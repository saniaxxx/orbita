"""End of speech detection on the satellite's 16 kHz mono 16-bit stream.

The satellite streams until the server reports STT_VAD_END: the server decides when the
command is over, as Home Assistant does.
"""

from __future__ import annotations

import array
import enum
import logging
import math
import sys
from collections.abc import Callable

from .config import VadConfig

_LOG = logging.getLogger(__name__)

RATE = 16000
FRAME_BYTES = RATE // 100 * 2  # 10 ms of 16-bit mono
FRAME_SECONDS = 0.01


class VadEvent(enum.Enum):
    SPEECH_START = "speech_start"
    # Speech, then silence_seconds of silence.
    END = "end"
    # timeout_seconds passed (speech still going).
    TIMEOUT = "timeout"
    # no_speech_seconds without speech.
    NO_SPEECH = "no_speech"


def frame_rms(frame: bytes) -> float:
    samples = array.array("h", frame)
    if sys.byteorder != "little":
        samples.byteswap()
    if not samples:
        return 0.0
    return math.sqrt(sum(s * s for s in samples) / len(samples))


def make_detector(cfg: VadConfig) -> Callable[[bytes], bool]:
    """Returns a function: 10 ms frame -> is speech."""
    engine = cfg.engine
    if engine in ("auto", "micro"):
        try:
            from pymicro_vad import MicroVad
        except ImportError:
            if engine == "micro":
                raise
            _LOG.warning("pymicro-vad not available, using the energy detector")
        else:
            vad = MicroVad()
            last = [False]

            def micro(frame: bytes) -> bool:
                p = vad.process_10ms(frame)
                # < 0: the model has not seen enough audio yet; keep the last decision.
                if p >= 0:
                    last[0] = p >= cfg.threshold
                return last[0]

            return micro
    if engine not in ("auto", "energy"):
        raise ValueError(f"vad.engine: unknown engine {engine!r}")
    return lambda frame: frame_rms(frame) >= cfg.energy_threshold


class Segmenter:
    """Feeds audio, reports SPEECH_START once and then one terminal event."""

    def __init__(self, cfg: VadConfig, is_speech: Callable[[bytes], bool] | None = None) -> None:
        self._cfg = cfg
        self._is_speech = is_speech or make_detector(cfg)
        self._buf = b""
        self._elapsed = 0.0
        self._speech_run = 0.0
        self._silence_run = 0.0
        self.in_speech = False
        self.done = False

    def process(self, audio: bytes) -> list[VadEvent]:
        if self.done:
            return []
        events: list[VadEvent] = []
        self._buf += audio
        while len(self._buf) >= FRAME_BYTES and not self.done:
            frame, self._buf = self._buf[:FRAME_BYTES], self._buf[FRAME_BYTES:]
            self._elapsed += FRAME_SECONDS
            speech = self._is_speech(frame)
            if speech:
                self._speech_run += FRAME_SECONDS
                self._silence_run = 0.0
            else:
                self._silence_run += FRAME_SECONDS
                if not self.in_speech:
                    self._speech_run = 0.0
            if not self.in_speech and self._speech_run >= self._cfg.speech_seconds - 1e-9:
                self.in_speech = True
                events.append(VadEvent.SPEECH_START)
            if self.in_speech and self._silence_run >= self._cfg.silence_seconds - 1e-9:
                events.append(self._finish(VadEvent.END))
            elif not self.in_speech and self._elapsed >= self._cfg.no_speech_seconds - 1e-9:
                events.append(self._finish(VadEvent.NO_SPEECH))
            elif self._elapsed >= self._cfg.timeout_seconds - 1e-9:
                events.append(self._finish(VadEvent.TIMEOUT))
        return events

    def _finish(self, ev: VadEvent) -> VadEvent:
        self.done = True
        self._buf = b""
        return ev
