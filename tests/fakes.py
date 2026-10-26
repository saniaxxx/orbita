"""Fake OpenAI-compatible server (chat, transcriptions, speech) for the tests."""

from __future__ import annotations

import asyncio
import io
import json
import math
import struct
import wave
from dataclasses import dataclass, field
from typing import Any

from aiohttp import web


def tone(seconds: float, rate: int = 22050, hz: float = 440.0, amp: int = 8000) -> bytes:
    n = int(seconds * rate)
    return struct.pack(
        f"<{n}h", *(int(amp * math.sin(2 * math.pi * hz * i / rate)) for i in range(n))
    )


@dataclass
class Recorder:
    synthesized: list[tuple[str, str | None]] = field(default_factory=list)
    chats: list[dict[str, Any]] = field(default_factory=list)
    uploads: list[dict[str, Any]] = field(default_factory=list)


class FakeOpenAI:
    """/v1/chat/completions, /v1/audio/transcriptions, /v1/audio/speech."""

    def __init__(
        self,
        answer: str = "Привет! Чем помочь?",
        transcript: str = "привет",
        sse: bool = True,
        delta_delay: float = 0.0,
    ) -> None:
        self.answer = answer
        self.transcript = transcript
        # stream=true is answered with server-sent events, one delta per word.
        self.sse = sse
        self.delta_delay = delta_delay
        self.sent_done_at: float | None = None
        self.rec = Recorder()
        self.auth: list[str] = []
        self._runner: web.AppRunner | None = None
        self.port = 0

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}/v1"

    async def start(self) -> None:
        app = web.Application()
        app.router.add_post("/v1/chat/completions", self._chat)
        app.router.add_post("/v1/audio/transcriptions", self._transcriptions)
        app.router.add_post("/v1/audio/speech", self._speech)
        self._runner = web.AppRunner(app)
        await self._runner.setup()
        site = web.TCPSite(self._runner, "127.0.0.1", 0)
        await site.start()
        self.port = site._server.sockets[0].getsockname()[1]  # type: ignore[union-attr]

    async def stop(self) -> None:
        if self._runner:
            await self._runner.cleanup()

    async def _chat(self, request: web.Request) -> web.StreamResponse:
        self.auth.append(request.headers.get("Authorization", ""))
        body = await request.json()
        self.rec.chats.append(body)
        if body.get("stream") and self.sse:
            resp = web.StreamResponse(headers={"Content-Type": "text/event-stream"})
            await resp.prepare(request)
            words = self.answer.split(" ")
            for i, w in enumerate(words):
                delta = w if i == 0 else " " + w
                event = {"choices": [{"index": 0, "delta": {"content": delta}}]}
                await resp.write(f"data: {json.dumps(event, ensure_ascii=False)}\n\n".encode())
                await asyncio.sleep(self.delta_delay)
            await resp.write(b"data: [DONE]\n\n")
            self.sent_done_at = asyncio.get_running_loop().time()
            await resp.write_eof()
            return resp
        return web.json_response(
            {"choices": [{"index": 0, "message": {"role": "assistant", "content": self.answer}}]}
        )

    async def _transcriptions(self, request: web.Request) -> web.Response:
        form = await request.post()
        f = form["file"]
        data = f.file.read()  # type: ignore[union-attr]
        self.rec.uploads.append(
            {"model": form.get("model"), "language": form.get("language"), "bytes": len(data)}
        )
        return web.json_response({"text": self.transcript})

    async def _speech(self, request: web.Request) -> web.Response:
        body = await request.json()
        self.rec.synthesized.append((body["input"], body.get("voice")))
        if body.get("response_format", "wav") != "wav":
            return web.Response(body=b"ID3fake-" + body["response_format"].encode())
        wav = io.BytesIO()
        with wave.open(wav, "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(24000)
            w.writeframes(tone(0.1, 24000))
        return web.Response(body=wav.getvalue(), content_type="audio/wav")
