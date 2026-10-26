"""Speech to text and text to speech over OpenAI-compatible APIs (/audio/transcriptions,
/audio/speech)."""

from __future__ import annotations

from collections.abc import AsyncIterator

from .audio import Pcm, from_wav
from .config import OpenAIConfig, SttConfig, TtsConfig
from .openai_io import OpenAIClient

# One transcription without an end-of-audio is cut at this size (60 s of 16 kHz 16-bit).
MAX_PCM = 60 * 16000 * 2


class Transcription:
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
        self._api = OpenAIClient(cfg.openai or default_api)

    def open(self, language: str | None = None) -> Transcription:
        return Transcription(self._api, self._cfg.model, language or self._cfg.language or None)


class Tts:
    def __init__(self, cfg: TtsConfig, default_api: OpenAIConfig) -> None:
        self._cfg = cfg
        self._api = OpenAIClient(cfg.openai or default_api)

    async def synthesize(self, text: str, voice: str | None = None) -> tuple[bytes, str]:
        """-> (audio file, extension)."""
        fmt = self._cfg.response_format
        data = await self._api.speech(
            text, self._cfg.model, voice or self._cfg.voice, fmt, self._cfg.speed
        )
        return data, fmt

    async def stream(self, text: str, voice: str | None = None) -> AsyncIterator[Pcm]:
        """Text -> PCM: the whole phrase, requested as WAV."""
        data = await self._api.speech(
            text, self._cfg.model, voice or self._cfg.voice, "wav", self._cfg.speed
        )
        yield from_wav(data)
