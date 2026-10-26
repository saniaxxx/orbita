"""Speech to text and text to speech over the Wyoming protocol (wyoming-faster-whisper,
wyoming-piper and other Wyoming servers)."""

from __future__ import annotations

import asyncio
import io
import wave
from collections.abc import AsyncIterator
from dataclasses import dataclass

from wyoming.asr import Transcribe, Transcript
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.client import AsyncTcpClient
from wyoming.error import Error
from wyoming.event import Event
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


@dataclass(frozen=True)
class Pcm:
    """A piece of synthesized audio."""

    rate: int
    width: int
    channels: int
    data: bytes


async def synthesize_stream(
    host: str,
    port: int,
    text: str,
    voice: str | None = None,
    speaker: str | None = None,
    timeout: float = 60.0,
) -> AsyncIterator[Pcm]:
    """Text -> audio as the server produces it."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout

    async def read(client: AsyncTcpClient) -> Event | None:
        return await asyncio.wait_for(client.read_event(), max(0.0, deadline - loop.time()))

    async with AsyncTcpClient(host, port) as client:
        v = SynthesizeVoice(name=voice, speaker=speaker) if voice or speaker else None
        await client.write_event(Synthesize(text=text, voice=v).event())
        fmt: tuple[int, int, int] | None = None
        got = False
        while True:
            event = await read(client)
            if event is None:
                raise WyomingError("tts: connection closed before the end of the audio")
            if AudioStart.is_type(event.type):
                s = AudioStart.from_event(event)
                fmt = (s.rate, s.width, s.channels)
            elif AudioChunk.is_type(event.type):
                c = AudioChunk.from_event(event)
                if fmt is None:
                    fmt = (c.rate, c.width, c.channels)
                got = True
                yield Pcm(*fmt, c.audio)
            elif AudioStop.is_type(event.type):
                break
            elif Error.is_type(event.type):
                raise WyomingError(f"tts: {Error.from_event(event).text}")
        if not got:
            raise WyomingError("tts: no audio")


async def synthesize(
    host: str,
    port: int,
    text: str,
    voice: str | None = None,
    speaker: str | None = None,
    timeout: float = 60.0,
) -> bytes:
    """Text -> WAV file bytes."""
    pcm = io.BytesIO()
    fmt: tuple[int, int, int] | None = None
    async for p in synthesize_stream(host, port, text, voice, speaker, timeout):
        fmt = fmt or (p.rate, p.width, p.channels)
        pcm.write(p.data)
    assert fmt is not None
    return to_wav(pcm.getvalue(), *fmt)


def to_wav(pcm: bytes, rate: int, width: int, channels: int) -> bytes:
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(width)
        w.setframerate(rate)
        w.writeframes(pcm)
    return out.getvalue()


def from_wav(data: bytes) -> Pcm:
    """WAV file bytes -> PCM (raises WyomingError when it is not PCM WAV)."""
    try:
        with wave.open(io.BytesIO(data), "rb") as w:
            return Pcm(w.getframerate(), w.getsampwidth(), w.getnchannels(), w.readframes(1 << 30))
    except (wave.Error, EOFError) as e:
        raise WyomingError(f"tts: not a PCM WAV file: {e}") from None
