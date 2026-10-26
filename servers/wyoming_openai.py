"""OpenAI-compatible text to speech in front of a Wyoming TTS server: POST /v1/audio/speech.

    python wyoming_openai.py --wyoming tcp://127.0.0.1:10200 --voices a,b [--port 8003]

For TTS servers that exist only as Wyoming services (the Russian wyoming-piper of
mitrokun/espeak-ng-data with stress and number normalization, TeraTTS, Vosk TTS...): orbita
speaks OpenAI-compatible APIs only. The request's "voice" picks one of --voices; any other
(OpenAI's "alloy"...) gets the first. Only these names reach the server: wyoming-piper lists its
whole catalog as installed and downloads a voice on its first use. response_format: wav or pcm
(16-bit at the server's rate). "speed" is not supported: set it on the Wyoming server.

Needs wyoming and aiohttp.
"""

from __future__ import annotations

import argparse
import asyncio
import io
import logging
import time
import wave
from urllib.parse import urlparse

from aiohttp import web
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.client import AsyncTcpClient
from wyoming.error import Error
from wyoming.info import Describe, Info
from wyoming.tts import Synthesize, SynthesizeVoice

_LOG = logging.getLogger("wyoming_openai")


def error(status: int, message: str) -> web.Response:
    return web.json_response({"error": {"message": message}}, status=status)


class Wyoming:
    def __init__(self, uri: str, timeout: float) -> None:
        u = urlparse(uri)
        if u.scheme != "tcp" or not u.hostname or not u.port:
            raise ValueError(f"--wyoming {uri!r}: expected tcp://host:port")
        self._host, self._port = u.hostname, u.port
        self._timeout = timeout

    async def ping(self) -> None:
        """Describe -> Info: the server is up."""
        async with AsyncTcpClient(self._host, self._port) as c:
            await c.write_event(Describe().event())
            while True:
                event = await asyncio.wait_for(c.read_event(), self._timeout)
                if event is None:
                    raise ConnectionError("wyoming: closed before info")
                if Info.is_type(event.type):
                    return

    async def synthesize(self, text: str, voice: str) -> tuple[bytes, int, int, int]:
        """-> (pcm, rate, width, channels)."""
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self._timeout
        async with AsyncTcpClient(self._host, self._port) as c:
            await c.write_event(Synthesize(text=text, voice=SynthesizeVoice(name=voice)).event())
            pcm = bytearray()
            fmt: tuple[int, int, int] | None = None
            while True:
                left = max(0.0, deadline - loop.time())
                event = await asyncio.wait_for(c.read_event(), left)
                if event is None:
                    raise ConnectionError("wyoming: closed before the end of the audio")
                if AudioStart.is_type(event.type):
                    s = AudioStart.from_event(event)
                    fmt = (s.rate, s.width, s.channels)
                elif AudioChunk.is_type(event.type):
                    ch = AudioChunk.from_event(event)
                    fmt = fmt or (ch.rate, ch.width, ch.channels)
                    pcm += ch.audio
                elif AudioStop.is_type(event.type):
                    break
                elif Error.is_type(event.type):
                    raise RuntimeError(f"wyoming: {Error.from_event(event).text}")
        if fmt is None or not pcm:
            raise RuntimeError("wyoming: no audio")
        return bytes(pcm), *fmt


def to_wav(pcm: bytes, rate: int, width: int, channels: int) -> bytes:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(width)
        w.setframerate(rate)
        w.writeframes(pcm)
    return buf.getvalue()


def make_app(wy: Wyoming, voices: list[str], api_key: str | None) -> web.Application:
    @web.middleware
    async def auth(request: web.Request, handler):
        if api_key and request.path != "/health":
            if request.headers.get("Authorization") != f"Bearer {api_key}":
                return error(401, "invalid API key")
        return await handler(request)

    async def speech(request: web.Request) -> web.Response:
        try:
            body = await request.json()
        except ValueError:
            return error(400, "body must be JSON")
        text = str(body.get("input") or "").strip()
        if not text:
            return error(400, "'input' is required")
        fmt = str(body.get("response_format") or "wav")
        if fmt not in ("wav", "pcm"):
            return error(400, f"response_format {fmt!r}: only wav and pcm")
        if body.get("speed") not in (None, 1, 1.0):
            return error(400, "'speed' is not supported: set it on the Wyoming server")
        voice = body.get("voice")
        if voice not in voices:
            voice = voices[0]
        t = time.monotonic()
        try:
            pcm, rate, width, channels = await wy.synthesize(text, voice)
        except (OSError, TimeoutError) as e:
            return error(502, f"wyoming server unavailable: {e}")
        except RuntimeError as e:
            return error(500, str(e))
        _LOG.info("%s: %.2f s for %r", voice, time.monotonic() - t, text[:60])
        if fmt == "pcm":
            return web.Response(body=pcm, content_type="audio/pcm")
        return web.Response(body=to_wav(pcm, rate, width, channels), content_type="audio/wav")

    async def models(request: web.Request) -> web.Response:
        data = [{"id": v, "object": "model", "owned_by": "wyoming"} for v in voices]
        return web.json_response({"object": "list", "data": data})

    async def health(request: web.Request) -> web.Response:
        try:
            await wy.ping()
        except (OSError, TimeoutError) as e:
            return error(503, f"wyoming server unavailable: {e}")
        return web.json_response({"status": "ok"})

    app = web.Application(middlewares=[auth])
    app.router.add_post("/v1/audio/speech", speech)
    app.router.add_get("/v1/models", models)
    app.router.add_get("/health", health)
    return app


async def warm_up(wy: Wyoming, voices: list[str], text: str) -> None:
    for _ in range(30):  # the Wyoming server may still be starting
        try:
            await wy.ping()
            break
        except OSError:
            await asyncio.sleep(2)
    else:
        _LOG.warning("warm-up skipped: the Wyoming server does not answer")
        return
    # The default voice last: a server that keeps one voice loaded keeps that one.
    for v in reversed(voices):
        t = time.monotonic()
        await wy.synthesize(text, v)
        _LOG.info("warm-up %s: %.2f s", v, time.monotonic() - t)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=8003)
    p.add_argument("--wyoming", default="tcp://127.0.0.1:10200", help="the Wyoming TTS server")
    p.add_argument("--voices", required=True, help="comma-separated; the first is the default")
    p.add_argument("--timeout", type=float, default=60.0)
    p.add_argument(
        "--warmup", help="text synthesized with every voice at start (first runs are slow)"
    )
    p.add_argument("--api-key", help="require Authorization: Bearer <key>")
    p.add_argument("--debug", action="store_true")
    args = p.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.debug else logging.INFO)

    voices = [v.strip() for v in args.voices.split(",") if v.strip()]
    wy = Wyoming(args.wyoming, args.timeout)
    if args.warmup:
        asyncio.run(warm_up(wy, voices, args.warmup))
    web.run_app(make_app(wy, voices, args.api_key), host=args.host, port=args.port, print=None)


if __name__ == "__main__":
    main()
