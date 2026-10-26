"""An answer spoken while it is being written: LLM deltas -> phrases -> TTS one phrase after
another -> one growing WAV stream the satellite is already playing."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable

from .http_store import AudioStream
from .intents import speakable
from .sentences import PhraseSplitter
from .speech import Tts

_LOG = logging.getLogger(__name__)


class StreamingAnswer:
    def __init__(
        self,
        tts: Tts,
        voice: str | None,
        stream: AudioStream,
        on_first_audio: Callable[[], None],
        min_chars: int = 20,
        name: str = "",
    ) -> None:
        self._tts = tts
        self._voice = voice
        self._stream = stream
        self._on_first_audio = on_first_audio
        self._splitter = PhraseSplitter(min_chars)
        self._phrases: asyncio.Queue[str | None] = asyncio.Queue()
        self._worker: asyncio.Task[None] | None = None
        self._name = name
        self.phrases = 0
        self.failed = 0

    @property
    def started(self) -> bool:
        return self._worker is not None

    async def on_text(self, delta: str) -> None:
        for phrase in self._splitter.feed(delta):
            self._put(phrase)

    async def finish(self) -> None:
        """All text is in: speak the rest, then end the stream."""
        for phrase in self._splitter.flush():
            self._put(phrase)
        if self._worker is not None:
            self._phrases.put_nowait(None)
            await self._worker
        await self._stream.close()

    async def cancel(self) -> None:
        if self._worker is not None and not self._worker.done():
            self._worker.cancel()
        await self._stream.close()

    def _put(self, phrase: str) -> None:
        if self._worker is None:
            self._worker = asyncio.create_task(self._speak(), name=f"tts-stream-{self._name}")
        self._phrases.put_nowait(phrase)

    async def _speak(self) -> None:
        first = True
        while (phrase := await self._phrases.get()) is not None:
            text = speakable(phrase)
            if not text:
                continue
            self.phrases += 1
            try:
                async for pcm in self._tts.stream(text, self._voice):
                    await self._stream.write(pcm)
                    if first:
                        first = False
                        self._on_first_audio()
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001 - one phrase lost, the answer goes on
                self.failed += 1
                _LOG.warning("%s: tts of %r failed: %s", self._name, text[:40], e)
