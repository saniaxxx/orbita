"""Fake Wyoming STT/TTS and OpenAI-compatible servers for the tests and scripts/e2e_selftest.py."""

from __future__ import annotations

import json
import math
import struct
from dataclasses import dataclass, field
from typing import Any

from aiohttp import web
from wyoming.asr import Transcribe, Transcript
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.event import Event
from wyoming.server import AsyncEventHandler, AsyncTcpServer
from wyoming.tts import Synthesize


def tone(seconds: float, rate: int = 22050, hz: float = 440.0, amp: int = 8000) -> bytes:
    n = int(seconds * rate)
    return struct.pack(
        f"<{n}h", *(int(amp * math.sin(2 * math.pi * hz * i / rate)) for i in range(n))
    )


@dataclass
class Recorder:
    transcribe_language: list[str | None] = field(default_factory=list)
    stt_bytes: list[int] = field(default_factory=list)
    synthesized: list[tuple[str, str | None]] = field(default_factory=list)
    chats: list[dict[str, Any]] = field(default_factory=list)
    uploads: list[dict[str, Any]] = field(default_factory=list)


class FakeWyoming:
    """STT: answers `transcript` after AudioStop. TTS: 0.2 s of a 440 Hz tone at 22050 Hz."""

    def __init__(self, transcript: str = "который час") -> None:
        self.transcript = transcript
        self.rec = Recorder()
        self._server: AsyncTcpServer | None = None
        self.port = 0

    async def start(self) -> None:
        fake = self

        class Handler(AsyncEventHandler):
            def __init__(self, *a: Any, **kw: Any) -> None:
                super().__init__(*a, **kw)
                self.got = 0

            async def handle_event(self, event: Event) -> bool:
                if Transcribe.is_type(event.type):
                    fake.rec.transcribe_language.append(Transcribe.from_event(event).language)
                elif AudioChunk.is_type(event.type):
                    self.got += len(AudioChunk.from_event(event).audio)
                elif AudioStop.is_type(event.type):
                    fake.rec.stt_bytes.append(self.got)
                    await self.write_event(Transcript(text=fake.transcript).event())
                    return False
                elif Synthesize.is_type(event.type):
                    s = Synthesize.from_event(event)
                    fake.rec.synthesized.append((s.text, s.voice.name if s.voice else None))
                    await self.write_event(AudioStart(rate=22050, width=2, channels=1).event())
                    pcm = tone(0.2)
                    for i in range(0, len(pcm), 4096):
                        await self.write_event(
                            AudioChunk(
                                rate=22050, width=2, channels=1, audio=pcm[i : i + 4096]
                            ).event()
                        )
                    await self.write_event(AudioStop().event())
                    return False
                return True

        self._server = AsyncTcpServer("127.0.0.1", 0)
        await self._server.start(Handler)
        self.port = self._server._server.sockets[0].getsockname()[1]  # type: ignore[union-attr]

    async def stop(self) -> None:
        if self._server:
            await self._server.stop()


class FakeOpenAI:
    """/v1/chat/completions, /v1/audio/transcriptions, /v1/audio/speech."""

    def __init__(self, answer: str = "Привет! Чем помочь?", transcript: str = "привет") -> None:
        self.answer = answer
        self.transcript = transcript
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

    async def _chat(self, request: web.Request) -> web.Response:
        self.auth.append(request.headers.get("Authorization", ""))
        body = await request.json()
        self.rec.chats.append(body)
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
        return web.Response(body=b"RIFFfake" + json.dumps(body).encode(), content_type="audio/wav")
