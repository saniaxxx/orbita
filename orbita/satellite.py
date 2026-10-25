"""One voice satellite: the ESPHome API connection and its voice pipeline runs.

orbita plays Home Assistant's side of the ESPHome voice assistant protocol: the satellite
detects the wake word and streams 16 kHz audio over the API; orbita finds the end of the
command, transcribes it, picks the answer, synthesizes it and hands the satellite a URL:

  RUN_START, STT_START, STT_VAD_START, STT_VAD_END, STT_END{text}, INTENT_START,
  INTENT_END{conversation_id, continue_conversation}, TTS_START{text}, TTS_END{url}, RUN_END
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import time
import uuid
from collections.abc import Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from aioesphomeapi import (
    APIClient,
    ReconnectLogic,
    VoiceAssistantAnnounceFinished,
    VoiceAssistantAudioSettings,
    VoiceAssistantFeature,
)
from aioesphomeapi import (
    VoiceAssistantEventType as E,
)
from aioesphomeapi import (
    VoiceAssistantTimerEventType as T,
)

from .intents import Request
from .vad import Segmenter, VadEvent

if TYPE_CHECKING:
    from .app import Services

_LOG = logging.getLogger(__name__)

# A continued conversation (the answer asked a question) keeps its id this long.
CONTINUE_SECONDS = 30.0


@dataclass
class Timer:
    id: str
    name: str
    total: int
    ends: float  # monotonic
    task: asyncio.Task[None] | None = None

    def left(self) -> int:
        return max(0, round(self.ends - time.monotonic()))


class Satellite:
    def __init__(self, cfg: Any, services: Services) -> None:
        self.cfg = cfg
        self.name: str = cfg.name
        self._s = services
        self._client = APIClient(
            cfg.host,
            cfg.port,
            None,
            noise_psk=cfg.noise_psk or None,
            client_info="orbita",
            timezone=services.config.timezone or None,
        )
        self._reconnect: ReconnectLogic | None = None
        self._unsubscribe: Callable[[], None] | None = None
        self.connected = False
        self._run: asyncio.Task[None] | None = None
        self._audio: asyncio.Queue[bytes | None] | None = None
        self._continue_cid: tuple[str, float] | None = None
        self.timers: dict[str, Timer] = {}

    # ---------------- connection ----------------

    async def start(self) -> None:
        self._reconnect = ReconnectLogic(
            client=self._client,
            on_connect=self._on_connect,
            on_disconnect=self._on_disconnect,
            on_connect_error=self._on_connect_error,
            name=None,
        )
        await self._reconnect.start()

    async def stop(self) -> None:
        self._cancel_run()
        for t in list(self.timers.values()):
            if t.task:
                t.task.cancel()
        if self._reconnect:
            await self._reconnect.stop()
        with contextlib.suppress(Exception):
            await self._client.disconnect()

    async def _on_connect(self) -> None:
        info = await self._client.device_info()
        flags = info.voice_assistant_feature_flags_compat(self._client.api_version)
        _LOG.info(
            "%s: connected to %s (%s %s, voice flags 0x%x)",
            self.name,
            info.name,
            info.model or info.manufacturer,
            info.esphome_version,
            flags,
        )
        if not flags & VoiceAssistantFeature.VOICE_ASSISTANT:
            _LOG.warning("%s: the device has no voice assistant", self.name)
            return
        if not flags & VoiceAssistantFeature.API_AUDIO:
            _LOG.warning("%s: the device streams audio over UDP only, not supported", self.name)
            return
        self._unsubscribe = self._client.subscribe_voice_assistant(
            handle_start=self._handle_start,
            handle_stop=self._handle_stop,
            handle_audio=self._handle_audio,
            handle_announcement_finished=self._handle_announcement_finished,
        )
        self.connected = True
        await self._s.publish_status(self.name, "online")
        # Timers survive a reconnect on our side; tell the device what is still running.
        for t in self.timers.values():
            self._timer_event(T.VOICE_ASSISTANT_TIMER_STARTED, t)

    async def _on_disconnect(self, expected: bool) -> None:
        _LOG.info("%s: disconnected%s", self.name, "" if expected else " (unexpected)")
        self.connected = False
        self._unsubscribe = None
        self._cancel_run()
        await self._s.publish_status(self.name, "offline")

    async def _on_connect_error(self, err: Exception) -> None:
        _LOG.warning(
            "%s: cannot connect to %s:%d: %s", self.name, self.cfg.host, self.cfg.port, err
        )

    # ---------------- voice protocol callbacks ----------------

    async def _handle_start(
        self,
        conversation_id: str,
        flags: int,
        audio_settings: VoiceAssistantAudioSettings,
        wake_word_phrase: str | None,
    ) -> int | None:
        _LOG.info("%s: voice start (wake word %r)", self.name, wake_word_phrase or "")
        self._cancel_run()
        cid = self._conversation_id(conversation_id)
        audio: asyncio.Queue[bytes | None] = asyncio.Queue()
        self._audio = audio
        self._run = asyncio.create_task(self._pipeline(cid, audio), name=f"run-{self.name}")
        return 0  # port 0: audio comes over the API connection

    async def _handle_audio(self, data: bytes, *_: Any) -> None:
        if self._audio is not None:
            self._audio.put_nowait(data)

    async def _handle_stop(self, abort: bool) -> None:
        if abort:
            _LOG.info("%s: voice run aborted by the device", self.name)
            self._cancel_run()
        elif self._audio is not None:
            self._audio.put_nowait(None)  # end of audio: transcribe what came

    async def _handle_announcement_finished(self, msg: VoiceAssistantAnnounceFinished) -> None:
        _LOG.debug("%s: announcement finished (success=%s)", self.name, msg.success)

    def _cancel_run(self) -> None:
        if self._run and not self._run.done():
            self._run.cancel()
        self._run = None
        self._audio = None

    def _conversation_id(self, from_device: str) -> str:
        brain = self._s.brain
        if from_device and brain.conversations.known(from_device):
            return from_device
        if self._continue_cid and time.monotonic() < self._continue_cid[1]:
            cid = self._continue_cid[0]
            self._continue_cid = None
            return cid
        self._continue_cid = None
        return from_device or uuid.uuid4().hex

    def _send(self, ev: E, data: dict[str, str] | None = None) -> None:
        if self.connected:
            self._client.send_voice_assistant_event(ev, data)

    def _fail(self, code: str, message: str) -> None:
        _LOG.warning("%s: %s: %s", self.name, code, message)
        self._send(E.VOICE_ASSISTANT_ERROR, {"code": code, "message": message})
        self._send(E.VOICE_ASSISTANT_RUN_END)

    # ---------------- pipeline ----------------

    async def _pipeline(self, cid: str, audio: asyncio.Queue[bytes | None]) -> None:
        try:
            await self._pipeline_inner(cid, audio)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            _LOG.exception("%s: pipeline failed", self.name)
            self._fail("pipeline-failed", str(e))

    async def _pipeline_inner(self, cid: str, audio: asyncio.Queue[bytes | None]) -> None:
        s = self._s
        lang = self.cfg.language or s.config.stt.language or None
        self._send(E.VOICE_ASSISTANT_RUN_START)
        self._send(E.VOICE_ASSISTANT_STT_START)
        t0 = time.monotonic()

        text, why = await self._listen(audio, lang)
        if text is None:
            return self._fail("stt-stream-failed", why)
        if not text:
            return self._fail("stt-no-text-recognized", why or "No text recognized")
        _LOG.info("%s: heard %r (%.1f s)", self.name, text, time.monotonic() - t0)
        self._send(E.VOICE_ASSISTANT_STT_END, {"text": text})
        await s.publish_json(self.name, "transcript", {"text": text, "conversation_id": cid})

        self._send(E.VOICE_ASSISTANT_INTENT_START)
        reply = await s.brain.answer(
            Request(text=text, satellite=self.name, conversation_id=cid, language=lang, timers=self)
        )
        _LOG.info("%s: answer by %s: %r", self.name, reply.handler, reply.text)
        self._send(
            E.VOICE_ASSISTANT_INTENT_END,
            {
                "conversation_id": cid,
                "continue_conversation": "1" if reply.continue_conversation else "0",
            },
        )
        if reply.continue_conversation:
            self._continue_cid = (cid, time.monotonic() + CONTINUE_SECONDS)
        await s.publish_json(
            self.name,
            "reply",
            {"text": reply.text, "conversation_id": cid, "handler": reply.handler},
        )
        if not reply.text:
            self._send(E.VOICE_ASSISTANT_RUN_END)
            return

        self._send(E.VOICE_ASSISTANT_TTS_START, {"text": reply.text})
        try:
            url = await self.speak_url(reply.text)
        except Exception as e:  # noqa: BLE001
            return self._fail("tts-failed", str(e))
        self._send(E.VOICE_ASSISTANT_TTS_END, {"url": url})
        self._send(E.VOICE_ASSISTANT_RUN_END)
        _LOG.info("%s: run done in %.1f s", self.name, time.monotonic() - t0)

    async def _listen(
        self, audio: asyncio.Queue[bytes | None], lang: str | None
    ) -> tuple[str | None, str]:
        """-> (text, reason); text None: STT failed, "": nothing recognized."""
        cfg = self._s.config.vad
        seg = Segmenter(cfg, self._s.make_vad())
        stt = self._s.stt.open(lang)
        try:
            await stt.start()
        except Exception as e:  # noqa: BLE001
            return None, f"speech to text unavailable: {e}"
        try:
            deadline = time.monotonic() + cfg.timeout_seconds + 2.0
            ended_by_device = False
            while not seg.done:
                left = deadline - time.monotonic()
                try:
                    chunk = await asyncio.wait_for(audio.get(), max(0.0, left))
                except TimeoutError:
                    break  # the device went quiet
                if chunk is None:
                    ended_by_device = True
                    break
                await stt.feed(chunk)
                for ev in seg.process(chunk):
                    if ev is VadEvent.SPEECH_START:
                        self._send(E.VOICE_ASSISTANT_STT_VAD_START)
                    elif ev is VadEvent.NO_SPEECH:
                        self._send(E.VOICE_ASSISTANT_STT_VAD_END)
                        return "", "No speech detected"
                    else:
                        self._send(E.VOICE_ASSISTANT_STT_VAD_END)
            if not seg.done and not ended_by_device:
                self._send(E.VOICE_ASSISTANT_STT_VAD_END)
            if not seg.in_speech and not ended_by_device:
                return "", "No speech detected"
            try:
                return await stt.finish(), ""
            except Exception as e:  # noqa: BLE001
                return None, f"speech to text failed: {e}"
        finally:
            await stt.close()

    async def speak_url(self, text: str) -> str:
        data, ext = await self._s.tts.synthesize(text, self.cfg.voice)
        return self._s.store.put(data, self.cfg.host, ext)

    # ---------------- announcements ----------------

    async def announce(
        self, text: str = "", media_url: str = "", start_conversation: bool = False
    ) -> bool:
        if not self.connected:
            _LOG.warning("%s: announce: not connected", self.name)
            return False
        url = media_url or await self.speak_url(text)
        if start_conversation:
            self._continue_cid = (uuid.uuid4().hex, time.monotonic() + CONTINUE_SECONDS)
        res = await self._client.send_voice_assistant_announcement_await_response(
            url, 120.0, text, "", start_conversation
        )
        return res.success

    # ---------------- timers (the clock of a Yandex Station counts them down) ----------------

    def timer_start(self, seconds: int, name: str = "") -> Timer:
        t = Timer(uuid.uuid4().hex[:8], name, seconds, time.monotonic() + seconds)
        t.task = asyncio.create_task(self._timer_wait(t), name=f"timer-{t.id}")
        self.timers[t.id] = t
        self._timer_event(T.VOICE_ASSISTANT_TIMER_STARTED, t)
        return t

    def timer_cancel(self, timer_id: str | None = None) -> list[Timer]:
        ids = [timer_id] if timer_id else list(self.timers)
        out = []
        for tid in ids:
            t = self.timers.pop(tid, None)
            if t is None:
                continue
            if t.task:
                t.task.cancel()
            self._timer_event(T.VOICE_ASSISTANT_TIMER_CANCELLED, t)
            out.append(t)
        return out

    async def _timer_wait(self, t: Timer) -> None:
        await asyncio.sleep(max(0.0, t.ends - time.monotonic()))
        self.timers.pop(t.id, None)
        _LOG.info("%s: timer %s (%d s) finished", self.name, t.name or t.id, t.total)
        self._timer_event(T.VOICE_ASSISTANT_TIMER_FINISHED, t)
        await self._s.publish_json(
            self.name, "timer", {"id": t.id, "name": t.name, "event": "finished"}
        )

    def _timer_event(self, ev: T, t: Timer) -> None:
        if self.connected:
            active = ev in (T.VOICE_ASSISTANT_TIMER_STARTED, T.VOICE_ASSISTANT_TIMER_UPDATED)
            self._client.send_voice_assistant_timer_event(
                ev, t.id, t.name, t.total, t.left(), active
            )
