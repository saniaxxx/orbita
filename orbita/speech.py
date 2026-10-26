"""Speech to text and text to speech behind one interface: Wyoming or OpenAI-compatible."""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Protocol

from .config import OpenAIConfig, SttConfig, TtsConfig
from .openai_io import OpenAIClient
from .wyoming_io import Pcm, SttSession, from_wav, synthesize, synthesize_stream

# One transcription without an end-of-audio is cut at this size (60 s of 16 kHz 16-bit).
MAX_PCM = 60 * 16000 * 2


class Transcription(Protocol):
    async def start(self) -> None: ...
    async def feed(self, audio: bytes) -> None: ...
    async def finish(self) -> str: ...
    async def close(self) -> None: ...


class _OpenAITranscription:
    """The API takes a whole file: audio is collected and uploaded on finish."""

    def __init__(self, client: OpenAIClient, model: str, language: str | None) -> None:
        self._client = client
        self._model = model
        self._language = language
        self._pcm = bytearray()

    async def start(self) -> None:
        pass

    async def feed(self, audio: bytes) -> None:
        if len(self._pcm) < MAX_PCM:
            self._pcm += audio

    async def finish(self) -> str:
        return await self._client.transcribe(bytes(self._pcm), self._model, self._language)

    async def close(self) -> None:
        self._pcm.clear()


class Stt:
    def __init__(self, cfg: SttConfig, default_api: OpenAIConfig) -> None:
        self._cfg = cfg
        self._api = OpenAIClient(cfg.openai or default_api) if cfg.engine == "openai" else None

    def open(self, language: str | None = None) -> Transcription:
        lang = language or self._cfg.language or None
        if self._api is not None:
            return _OpenAITranscription(self._api, self._cfg.model, lang)
        return SttSession(self._cfg.host, self._cfg.port, lang)


class Tts:
    def __init__(self, cfg: TtsConfig, default_api: OpenAIConfig) -> None:
        self._cfg = cfg
        self._api = OpenAIClient(cfg.openai or default_api) if cfg.engine == "openai" else None

    async def synthesize(self, text: str, voice: str | None = None) -> tuple[bytes, str]:
        """-> (audio file, extension)."""
        voice = voice or self._cfg.voice
        if self._api is not None:
            fmt = self._cfg.response_format
            data = await self._api.speech(text, self._cfg.model, voice, fmt, self._cfg.speed)
            return data, fmt
        wav = await synthesize(self._cfg.host, self._cfg.port, text, voice, self._cfg.speaker)
        return wav, "wav"

    async def stream(self, text: str, voice: str | None = None) -> AsyncIterator[Pcm]:
        """Text -> PCM as soon as the engine has it (Wyoming: while it synthesizes; OpenAI: the
        whole phrase, requested as WAV)."""
        voice = voice or self._cfg.voice
        if self._api is not None:
            data = await self._api.speech(text, self._cfg.model, voice, "wav", self._cfg.speed)
            yield from_wav(data)
            return
        async for p in synthesize_stream(
            self._cfg.host, self._cfg.port, text, voice, self._cfg.speaker
        ):
            yield p
