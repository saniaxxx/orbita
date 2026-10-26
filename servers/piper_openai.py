"""OpenAI-compatible text to speech over Piper: POST /v1/audio/speech.

    python piper_openai.py --data-dir /var/lib/piper --voice ru_RU-irina-medium [--port 8002]

Voices are <name>.onnx + <name>.onnx.json in --data-dir. The request's "voice" picks one by
name; an unknown voice (OpenAI's "alloy"...) gets the default. response_format: wav or pcm
(16-bit mono at the voice's rate). speed > 1 speaks faster.

Needs piper-tts and aiohttp.
"""

from __future__ import annotations

import argparse
import asyncio
import io
import logging
import wave
from pathlib import Path

from aiohttp import web
from piper import PiperVoice, SynthesisConfig

_LOG = logging.getLogger("piper_openai")


def error(status: int, message: str) -> web.Response:
    return web.json_response({"error": {"message": message}}, status=status)


class Voices:
    def __init__(self, data_dir: Path, default: str, use_cuda: bool) -> None:
        self._dir = data_dir
        self.default = default
        self._cuda = use_cuda
        self._loaded: dict[str, PiperVoice] = {}

    def names(self) -> list[str]:
        return sorted(p.name.removesuffix(".onnx") for p in self._dir.glob("*.onnx"))

    def get(self, name: str | None) -> tuple[str, PiperVoice]:
        if not name or not (self._dir / f"{name}.onnx").is_file():
            name = self.default
        if name not in self._loaded:
            _LOG.info("loading voice %s", name)
            self._loaded[name] = PiperVoice.load(self._dir / f"{name}.onnx", use_cuda=self._cuda)
        return name, self._loaded[name]


def synthesize(voice: PiperVoice, text: str, speed: float | None, fmt: str) -> bytes:
    cfg = SynthesisConfig(length_scale=1.0 / speed if speed else None)
    pcm = b"".join(c.audio_int16_bytes for c in voice.synthesize(text, cfg))
    if fmt == "pcm":
        return pcm
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(voice.config.sample_rate)
        w.writeframes(pcm)
    return buf.getvalue()


def make_app(voices: Voices, api_key: str | None) -> web.Application:
    lock = asyncio.Lock()

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
        try:
            speed = float(body["speed"]) if body.get("speed") is not None else None
        except (TypeError, ValueError):
            return error(400, "'speed' must be a number")
        if speed is not None and not 0.25 <= speed <= 4.0:
            return error(400, "'speed' must be between 0.25 and 4.0")
        async with lock:
            try:
                name, voice = await asyncio.to_thread(voices.get, body.get("voice"))
                audio = await asyncio.to_thread(synthesize, voice, text, speed, fmt)
            except Exception as e:  # noqa: BLE001 - the client gets the reason
                _LOG.exception("synthesis failed")
                return error(500, f"synthesis failed: {e}")
        _LOG.debug("%s: %d bytes for %r", name, len(audio), text[:60])
        ctype = "audio/wav" if fmt == "wav" else "audio/pcm"
        return web.Response(body=audio, content_type=ctype)

    async def models(request: web.Request) -> web.Response:
        data = [{"id": n, "object": "model", "owned_by": "piper"} for n in voices.names()]
        return web.json_response({"object": "list", "data": data})

    async def health(request: web.Request) -> web.Response:
        return web.json_response({"status": "ok"})

    app = web.Application(middlewares=[auth])
    app.router.add_post("/v1/audio/speech", speech)
    app.router.add_get("/v1/models", models)
    app.router.add_get("/health", health)
    return app


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=8002)
    p.add_argument("--data-dir", type=Path, required=True)
    p.add_argument("--voice", required=True, help="default voice, e.g. ru_RU-irina-medium")
    p.add_argument("--cuda", action="store_true")
    p.add_argument("--api-key", help="require Authorization: Bearer <key>")
    p.add_argument("--debug", action="store_true")
    args = p.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.debug else logging.INFO)

    voices = Voices(args.data_dir, args.voice, args.cuda)
    voices.get(args.voice)  # fail at start, not on the first request
    web.run_app(make_app(voices, args.api_key), host=args.host, port=args.port, print=None)


if __name__ == "__main__":
    main()
