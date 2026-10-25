"""Speech to text and text to speech over the Wyoming protocol (wyoming-faster-whisper,
wyoming-piper and other Wyoming servers)."""

from __future__ import annotations

import asyncio
import io
import wave

from wyoming.asr import Transcribe, Transcript
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.client import AsyncTcpClient
from wyoming.error import Error
from wyoming.tts import Synthesize, SynthesizeVoice

RATE = 16000
WIDTH = 2
CHANNELS = 1


class WyomingError(RuntimeError):
    pass


class SttSession:
    """One transcription: open, feed audio as it arrives, finish -> text."""

    def __init__(self, host: str, port: int, language: str | None, timeout: float = 30.0) -> None:
        self._client = AsyncTcpClient(host, port)
        self._language = language
        self._timeout = timeout
        self._open = False

    async def start(self) -> None:
        await asyncio.wait_for(self._client.connect(), self._timeout)
        self._open = True
        await self._client.write_event(Transcribe(language=self._language).event())
        await self._client.write_event(
            AudioStart(rate=RATE, width=WIDTH, channels=CHANNELS).event()
        )

    async def feed(self, audio: bytes) -> None:
        await self._client.write_event(
            AudioChunk(rate=RATE, width=WIDTH, channels=CHANNELS, audio=audio).event()
        )

    async def finish(self) -> str:
        await self._client.write_event(AudioStop().event())
        try:
            return await asyncio.wait_for(self._read_transcript(), self._timeout)
        finally:
            await self.close()

    async def _read_transcript(self) -> str:
        while True:
            event = await self._client.read_event()
            if event is None:
                raise WyomingError("stt: connection closed before the transcript")
            if Transcript.is_type(event.type):
                return Transcript.from_event(event).text.strip()
            if Error.is_type(event.type):
                raise WyomingError(f"stt: {Error.from_event(event).text}")
            # TranscriptStart / TranscriptChunk of streaming servers: the final Transcript
            # carries the whole text.

    async def close(self) -> None:
        if self._open:
            self._open = False
            await self._client.disconnect()


async def synthesize(
    host: str,
    port: int,
    text: str,
    voice: str | None = None,
    speaker: str | None = None,
    timeout: float = 60.0,
) -> bytes:
    """Text -> WAV file bytes."""

    async def run() -> bytes:
        async with AsyncTcpClient(host, port) as client:
            v = SynthesizeVoice(name=voice, speaker=speaker) if voice or speaker else None
            await client.write_event(Synthesize(text=text, voice=v).event())
            fmt: tuple[int, int, int] | None = None
            pcm = io.BytesIO()
            while True:
                event = await client.read_event()
                if event is None:
                    raise WyomingError("tts: connection closed before the end of the audio")
                if AudioStart.is_type(event.type):
                    s = AudioStart.from_event(event)
                    fmt = (s.rate, s.width, s.channels)
                elif AudioChunk.is_type(event.type):
                    c = AudioChunk.from_event(event)
                    if fmt is None:
                        fmt = (c.rate, c.width, c.channels)
                    pcm.write(c.audio)
                elif AudioStop.is_type(event.type):
                    break
                elif Error.is_type(event.type):
                    raise WyomingError(f"tts: {Error.from_event(event).text}")
            if fmt is None:
                raise WyomingError("tts: no audio")
            return to_wav(pcm.getvalue(), *fmt)

    return await asyncio.wait_for(run(), timeout)


def to_wav(pcm: bytes, rate: int, width: int, channels: int) -> bytes:
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(width)
        w.setframerate(rate)
        w.writeframes(pcm)
    return out.getvalue()
