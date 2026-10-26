"""HTTP server the satellites fetch spoken answers from: whole files (TTS_END carries the URL)
and streams that grow while the answer is being synthesized (the URL goes in RUN_START, the
satellite starts playing on INTENT_PROGRESS tts_start_streaming=1)."""

from __future__ import annotations

import asyncio
import logging
import secrets
import socket
import struct
import time

from aiohttp import web

from .audio import Pcm

_LOG = logging.getLogger(__name__)

CONTENT_TYPES = {
    "wav": "audio/wav",
    "mp3": "audio/mpeg",
    "flac": "audio/flac",
    "opus": "audio/ogg",
    "aac": "audio/aac",
    "pcm": "application/octet-stream",
}


# A WAV header whose RIFF and data sizes are unknown yet: the player reads to the end.
_UNKNOWN = 0xFFFFFFFF
# A stream that ends without any audio still plays: this much silence.
_EMPTY_SECONDS = 0.1


def wav_header(rate: int, width: int, channels: int, size: int = _UNKNOWN) -> bytes:
    block = width * channels
    return (
        b"RIFF"
        + struct.pack("<I", size if size == _UNKNOWN else size + 36)
        + b"WAVEfmt "
        + struct.pack("<IHHIIHH", 16, 1, channels, rate, rate * block, block, width * 8)
        + b"data"
        + struct.pack("<I", size)
    )


class AudioStream:
    """One spoken answer as a WAV stream: PCM is appended as it is synthesized, every reader
    gets it from the start (a player that reconnects hears the whole answer)."""

    def __init__(self) -> None:
        self.created = time.monotonic()
        self._data = bytearray()
        self._fmt: tuple[int, int, int] | None = None
        self._done = False
        self._changed = asyncio.Condition()
        self.readers = 0

    @property
    def done(self) -> bool:
        return self._done

    @property
    def has_audio(self) -> bool:
        return self._fmt is not None

    async def write(self, pcm: Pcm) -> None:
        if self._done:
            return
        fmt = (pcm.rate, pcm.width, pcm.channels)
        if self._fmt is None:
            self._fmt = fmt
            self._data += wav_header(*fmt)
        elif fmt != self._fmt:
            _LOG.warning("tts stream: audio format %s after %s, dropped", fmt, self._fmt)
            return
        self._data += pcm.data
        async with self._changed:
            self._changed.notify_all()

    async def close(self) -> None:
        if self._done:
            return
        if self._fmt is None:
            self._fmt = (16000, 2, 1)
            self._data += wav_header(*self._fmt) + bytes(int(16000 * 2 * _EMPTY_SECONDS))
        self._done = True
        async with self._changed:
            self._changed.notify_all()

    async def chunks(self):  # -> AsyncIterator[bytes]
        sent = 0
        while True:
            if sent < len(self._data):
                chunk = bytes(self._data[sent:])
                sent += len(chunk)
                yield chunk
                continue
            if self._done:
                return
            async with self._changed:
                await self._changed.wait_for(lambda n=sent: self._done or len(self._data) > n)


class AudioStore:
    def __init__(self, host: str, port: int, base_url: str, ttl: float) -> None:
        self._host = host
        self._port = port
        self._base_url = base_url.rstrip("/")
        self._ttl = ttl
        self._items: dict[str, tuple[float, bytes, str]] = {}
        self._streams: dict[str, AudioStream] = {}
        self._runner: web.AppRunner | None = None

    @property
    def port(self) -> int:
        return self._port

    async def start(self) -> None:
        app = web.Application()
        app.router.add_get("/tts/stream/{name}", self._get_stream)
        app.router.add_get("/tts/{name}", self._get)
        app.router.add_get("/health", _health)
        self._runner = web.AppRunner(app, access_log=None)
        await self._runner.setup()
        site = web.TCPSite(self._runner, self._host, self._port)
        await site.start()
        if self._port == 0:  # tests: an ephemeral port
            self._port = site._server.sockets[0].getsockname()[1]  # type: ignore[union-attr]
        _LOG.info("answers served on %s:%d", self._host, self._port)

    async def stop(self) -> None:
        if self._runner:
            await self._runner.cleanup()

    def put(self, data: bytes, satellite_host: str, ext: str = "wav") -> str:
        """Stores an answer, returns the URL the satellite at satellite_host can fetch."""
        self._expire()
        name = f"{secrets.token_urlsafe(12)}.{ext}"
        ctype = CONTENT_TYPES.get(ext, "application/octet-stream")
        self._items[name] = (time.monotonic(), data, ctype)
        return f"{self.base_url_for(satellite_host)}/tts/{name}"

    def open_stream(self, satellite_host: str) -> tuple[AudioStream, str]:
        """A new answer stream and the URL the satellite at satellite_host can fetch."""
        self._expire()
        name = f"{secrets.token_urlsafe(12)}.wav"
        stream = self._streams[name] = AudioStream()
        return stream, f"{self.base_url_for(satellite_host)}/tts/stream/{name}"

    def base_url_for(self, satellite_host: str) -> str:
        if self._base_url:
            return self._base_url
        return f"http://{local_address_for(satellite_host)}:{self._port}"

    def _expire(self) -> None:
        now = time.monotonic()
        for name in [n for n, (t, _, _) in self._items.items() if now - t > self._ttl]:
            del self._items[name]
        for name in [n for n, st in self._streams.items() if now - st.created > self._ttl]:
            del self._streams[name]

    async def _get(self, request: web.Request) -> web.StreamResponse:
        self._expire()
        item = self._items.get(request.match_info["name"])
        if item is None:
            raise web.HTTPNotFound()
        _, data, ctype = item
        return web.Response(body=data, content_type=ctype)

    async def _get_stream(self, request: web.Request) -> web.StreamResponse:
        self._expire()
        stream = self._streams.get(request.match_info["name"])
        if stream is None:
            raise web.HTTPNotFound()
        resp = web.StreamResponse(
            headers={"Content-Type": "audio/wav", "Cache-Control": "no-cache"}
        )
        await resp.prepare(request)
        stream.readers += 1
        try:
            async for chunk in stream.chunks():
                await resp.write(chunk)
            await resp.write_eof()
        except (ConnectionResetError, asyncio.CancelledError):
            _LOG.debug("tts stream: reader went away")
            raise
        finally:
            stream.readers -= 1
        return resp


async def _health(_: web.Request) -> web.Response:
    return web.Response(text="ok")


def local_address_for(remote_host: str) -> str:
    """The local address the kernel picks to reach remote_host (no packet is sent)."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect((socket.gethostbyname(remote_host), 9))
            return s.getsockname()[0]
    except OSError:
        return socket.gethostbyname(socket.gethostname())
