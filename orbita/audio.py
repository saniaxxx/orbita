"""PCM and WAV: the satellites' audio format and conversions."""

from __future__ import annotations

import io
import wave
from dataclasses import dataclass

# What the satellites send: 16 kHz, 16-bit, mono.
RATE = 16000
WIDTH = 2
CHANNELS = 1


class AudioError(ValueError):
    pass


@dataclass(frozen=True)
class Pcm:
    """A piece of synthesized audio."""

    rate: int
    width: int
    channels: int
    data: bytes


def to_wav(pcm: bytes, rate: int, width: int, channels: int) -> bytes:
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(width)
        w.setframerate(rate)
        w.writeframes(pcm)
    return out.getvalue()


def from_wav(data: bytes) -> Pcm:
    """WAV file bytes -> PCM (raises AudioError when it is not PCM WAV)."""
    try:
        with wave.open(io.BytesIO(data), "rb") as w:
            return Pcm(w.getframerate(), w.getsampwidth(), w.getnchannels(), w.readframes(1 << 30))
    except (wave.Error, EOFError) as e:
        raise AudioError(f"tts: not a PCM WAV file: {e}") from None
