"""OpenAI-compatible speech to text over GigaAM: POST /v1/audio/transcriptions.

    python gigaam_openai.py --model v3_e2e_rnnt --device cuda [--port 8001]

multipart/form-data: file (any format ffmpeg reads, up to 25 s), model (ignored: the loaded
model answers), language (Russian only), response_format json (the default) or text. Silence
gives an empty text, not an error.

Needs gigaam (https://github.com/salute-developers/GigaAM), torch, aiohttp and ffmpeg.
"""

from __future__ import annotations

import argparse
import asyncio
import io
import logging
import os
import tempfile
import time
import wave
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import gigaam
import torch
from aiohttp import web

_LOG = logging.getLogger("gigaam_openai")

RATE = 16000
# gigaam's transcribe() takes up to 25 s; longer needs transcribe_longform and a VAD model.
MAX_SECONDS = 25.0
# Shorter than this is nothing to recognize (and too short for the encoder).
MIN_SECONDS = 0.1
# On a GPU every new input length costs ~0.3 s (kernels picked for the shape) against ~0.05 s
# for a known one: audio is padded with zeros to a whole number of these, the encoder gets the
# real length (the text is the same), and every padded length is warmed up at start.
BUCKET_SECONDS = 2


def error(status: int, message: str) -> web.Response:
    return web.json_response({"error": {"message": message}}, status=status)


def tmp_dir() -> str | None:
    """tmpfs when there is one: the upload never touches the disk."""
    return "/dev/shm" if os.access("/dev/shm", os.W_OK) else None


def load_audio(data: bytes, suffix: str) -> torch.Tensor:
    """-> float samples at 16 kHz mono. 16 kHz 16-bit mono WAV (what orbita sends) is read
    as is; anything else goes through ffmpeg (~80 ms more)."""
    try:
        with wave.open(io.BytesIO(data), "rb") as w:
            if (w.getframerate(), w.getsampwidth(), w.getnchannels()) == (RATE, 2, 1):
                pcm = bytearray(w.readframes(w.getnframes()))
                return torch.frombuffer(pcm, dtype=torch.int16).float() / 32768.0
    except (wave.Error, EOFError):
        pass
    with tempfile.NamedTemporaryFile(suffix=suffix, dir=tmp_dir()) as f:
        f.write(data)
        f.flush()
        return gigaam.load_audio(f.name, RATE)


class Recognizer:
    def __init__(self, name: str, device: str, download_root: Path, warmup: Path | None) -> None:
        self.name = name
        # One thread for the model: requests queue up in it (the model is one), and CUDA's
        # per-thread setup (~0.3 s) is paid once, by the warm-up, not by requests.
        self._thread = ThreadPoolExecutor(max_workers=1, thread_name_prefix="gigaam")
        self._model = gigaam.load_model(name, device=device, download_root=str(download_root))
        self._pad = self._param().device.type != "cpu"
        self._thread.submit(self._warm_up, warmup).result()

    def _warm_up(self, speech: Path | None) -> None:
        """First runs are slow on a GPU: kernels for the encoder (~15 s, then ~0.3 s for each
        input length) and for the decoder once it emits text (~5 s). Noise emits nothing, so
        the decoder needs speech."""
        if speech is not None and speech.is_file():
            _LOG.info("warm-up: %r", self._recognize(load_audio(speech.read_bytes(), ".wav")))
        elif self._pad:
            _LOG.warning("no --warmup speech: the first request will take a few seconds more")
        if self._pad:
            bucket = BUCKET_SECONDS * RATE
            for n in range(bucket, int(MAX_SECONDS * RATE) + bucket, bucket):
                self._recognize(torch.randn(n) * 0.01)
                self._recognize(torch.randn(n - RATE // 2) * 0.01)

    def _param(self) -> torch.Tensor:
        return next(self._model.parameters())

    @torch.inference_mode()
    def _recognize(self, samples: torch.Tensor) -> str:
        """GigaAMASR.transcribe() from samples instead of a file."""
        m = self._model
        param = self._param()
        n = len(samples)
        if self._pad:
            bucket = BUCKET_SECONDS * RATE
            samples = torch.nn.functional.pad(samples, (0, -n % bucket))
        wav = samples.to(param.device).to(param.dtype).unsqueeze(0)
        length = torch.full([1], n, device=param.device)
        encoded, encoded_len = m.forward(wav, length)
        return m._decode(encoded, encoded_len, length)[0][0].strip()

    def _transcribe(self, audio: bytes, suffix: str) -> str:
        samples = load_audio(audio, suffix)
        seconds = len(samples) / RATE
        if seconds < MIN_SECONDS:
            return ""
        if seconds > MAX_SECONDS:
            raise ValueError(f"audio is {seconds:.1f} s, at most {MAX_SECONDS:.0f} s")
        return self._recognize(samples)

    async def transcribe(self, audio: bytes, suffix: str) -> str:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self._thread, self._transcribe, audio, suffix)


def make_app(rec: Recognizer, api_key: str | None) -> web.Application:
    @web.middleware
    async def auth(request: web.Request, handler):
        if api_key and request.path != "/health":
            if request.headers.get("Authorization") != f"Bearer {api_key}":
                return error(401, "invalid API key")
        return await handler(request)

    async def transcriptions(request: web.Request) -> web.Response:
        try:
            form = await request.post()
        except ValueError as e:
            return error(400, f"expected multipart/form-data: {e}")
        upload = form.get("file")
        if not isinstance(upload, web.FileField):
            return error(400, "'file' is required")
        fmt = str(form.get("response_format") or "json")
        if fmt not in ("json", "text"):
            return error(400, f"response_format {fmt!r}: only json and text")
        language = str(form.get("language") or "ru")
        if language != "ru":
            return error(400, f"language {language!r}: only ru")
        audio = upload.file.read()
        if not audio:
            return error(400, "'file' is empty")
        t = time.monotonic()
        try:
            text = await rec.transcribe(audio, Path(upload.filename or "").suffix or ".wav")
        except ValueError as e:
            return error(400, str(e))
        except RuntimeError as e:  # ffmpeg could not read it
            return error(400, f"cannot decode the audio: {e}")
        except Exception as e:  # noqa: BLE001 - the client gets the reason
            _LOG.exception("transcription failed")
            return error(500, f"transcription failed: {e}")
        _LOG.info("%d bytes in %.2f s: %r", len(audio), time.monotonic() - t, text)
        if fmt == "text":
            return web.Response(text=text)
        return web.json_response({"text": text})

    async def models(request: web.Request) -> web.Response:
        data = [{"id": rec.name, "object": "model", "owned_by": "gigaam"}]
        return web.json_response({"object": "list", "data": data})

    async def health(request: web.Request) -> web.Response:
        return web.json_response({"status": "ok"})

    # A phrase is well under a megabyte of WAV; 25 s of 16 kHz 16-bit mono is 800 KB.
    app = web.Application(middlewares=[auth], client_max_size=32 * 1024 * 1024)
    app.router.add_post("/v1/audio/transcriptions", transcriptions)
    app.router.add_get("/v1/models", models)
    app.router.add_get("/health", health)
    return app


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=8001)
    p.add_argument("--model", default="v3_e2e_rnnt", help="gigaam model name")
    p.add_argument("--device", default="cuda", help="cuda, cuda:N or cpu")
    p.add_argument("--data-dir", type=Path, default=Path("/var/lib/gigaam"))
    p.add_argument("--warmup", type=Path, help="WAV with a spoken phrase, recognized at start")
    p.add_argument("--api-key", help="require Authorization: Bearer <key>")
    p.add_argument("--debug", action="store_true")
    args = p.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.debug else logging.INFO)

    t = time.monotonic()
    rec = Recognizer(args.model, args.device, args.data_dir, args.warmup)
    _LOG.info("%s on %s loaded in %.1f s", args.model, args.device, time.monotonic() - t)
    web.run_app(make_app(rec, args.api_key), host=args.host, port=args.port, print=None)


if __name__ == "__main__":
    main()
