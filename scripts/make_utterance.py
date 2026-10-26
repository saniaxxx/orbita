#!/usr/bin/env python3
"""A spoken test request "<wake word> ... <command>" as a 16 kHz mono WAV, synthesized by the
TTS service from orbita's config: input for scripts/latency.py on a satellite with a virtual
microphone.

    orbita-venv/bin/python make_utterance.py out.wav "Который час?" \\
        [--wake "Okay Nabu."] [--wake-voice en_US-lessac-medium] \\
        [--wake-tts http://host:8002/v1] [-c /etc/orbita/config.yaml]

The wake word needs an English voice (a Russian one reading "Okay Nabu" does not wake
linux-voice-assistant): --wake-tts is an OpenAI-compatible TTS that has --wake-voice, by
default orbita's own. The command is spoken with the configured voice.
"""

from __future__ import annotations

import argparse
import asyncio
import struct
import wave
from dataclasses import replace

from orbita.audio import CHANNELS, RATE, WIDTH, from_wav
from orbita.config import OpenAIConfig, load
from orbita.speech import Tts


def to_16k_mono(wav: bytes) -> bytes:
    p = from_wav(wav)
    samples = struct.unpack(f"<{len(p.data) // 2}h", p.data)[:: p.channels]
    n = int(len(samples) * RATE / p.rate)
    out = [samples[min(int(i * p.rate / RATE), len(samples) - 1)] for i in range(n)]
    return struct.pack(f"<{n}h", *out)


def silence(seconds: float) -> bytes:
    return b"\0\0" * int(RATE * seconds)


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("out")
    ap.add_argument("command")
    ap.add_argument("--wake", default="Okay Nabu.")
    ap.add_argument("--wake-voice", default="en_US-lessac-medium")
    ap.add_argument("--wake-tts", help="OpenAI-compatible TTS base URL for the wake word")
    ap.add_argument("-c", "--config", default="/etc/orbita/config.yaml")
    args = ap.parse_args()

    cfg = load(args.config)
    tts = Tts(cfg.tts, cfg.openai)
    wake_tts = tts
    if args.wake_tts:
        wake_tts = Tts(replace(cfg.tts, openai=OpenAIConfig(base_url=args.wake_tts)), cfg.openai)
    wake = to_16k_mono((await wake_tts.synthesize(args.wake, args.wake_voice))[0])
    command = to_16k_mono((await tts.synthesize(args.command))[0])
    with wave.open(args.out, "wb") as w:
        w.setnchannels(CHANNELS)
        w.setsampwidth(WIDTH)
        w.setframerate(RATE)
        w.writeframes(silence(0.5) + wake + silence(0.8) + command + silence(2.5))


if __name__ == "__main__":
    asyncio.run(main())
