"""HTTP server the satellites fetch spoken answers from (TTS_END carries the URL)."""

from __future__ import annotations

import logging
import secrets
import socket
import time

from aiohttp import web

_LOG = logging.getLogger(__name__)

CONTENT_TYPES = {
    "wav": "audio/wav",
    "mp3": "audio/mpeg",
    "flac": "audio/flac",
    "opus": "audio/ogg",
    "aac": "audio/aac",
    "pcm": "application/octet-stream",
}


class AudioStore:
    def __init__(self, host: str, port: int, base_url: str, ttl: float) -> None:
        self._host = host
        self._port = port
        self._base_url = base_url.rstrip("/")
        self._ttl = ttl
        self._items: dict[str, tuple[float, bytes, str]] = {}
        self._runner: web.AppRunner | None = None

    @property
    def port(self) -> int:
        return self._port

    async def start(self) -> None:
        app = web.Application()
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

    def base_url_for(self, satellite_host: str) -> str:
        if self._base_url:
            return self._base_url
        return f"http://{local_address_for(satellite_host)}:{self._port}"

    def _expire(self) -> None:
        now = time.monotonic()
        for name in [n for n, (t, _, _) in self._items.items() if now - t > self._ttl]:
            del self._items[name]

    async def _get(self, request: web.Request) -> web.StreamResponse:
        self._expire()
        item = self._items.get(request.match_info["name"])
        if item is None:
            raise web.HTTPNotFound()
        _, data, ctype = item
        return web.Response(body=data, content_type=ctype)


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
