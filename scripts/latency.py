#!/usr/bin/env python3
"""Answer latency as the user hears it: end of the spoken phrase -> first sound of the answer.

Runs on a linux-voice-assistant satellite whose microphone is a PulseAudio virtual one: a
null sink `virtmic_sink` whose monitor LVA records (see docs/spec.md, §11). Plays an utterance
("wake word + command", scripts/make_utterance.py) into it and records the microphone and the
speaker at once; the latency is between the last 10 ms frame of speech in the microphone and
the first frame of the answer on the speaker.

The speaker is recorded through a temporary null sink `latency_tap`, made the default output
and looped back to the real one (+~30 ms to what you hear): the monitor of a hardware sink
loses time while the sink sleeps. The tap is removed at the end.

    PULSE_SERVER=unix:/var/run/pulse/native python3 latency.py utterance.wav [seconds]

Python standard library and pulseaudio-utils (pactl, parec, paplay, pacat) only.
"""

from __future__ import annotations

import array
import os
import subprocess
import sys
import tempfile
import time

RATE = 16000
FRAME = RATE // 100  # 10 ms
LOUD = 300.0  # RMS of a 10 ms frame that counts as sound
MIC = "virtmic_sink"
TAP = "latency_tap"


def pactl(*args: str) -> str:
    return subprocess.run(["pactl", *args], check=True, capture_output=True, text=True).stdout


class Recorder:
    """parec into a file: a pipe read only at the end fills up in ~2 s and stalls parec."""

    def __init__(self, source: str) -> None:
        self._file = tempfile.TemporaryFile()
        # Its own application name: module-stream-restore would put both recorders on one source.
        self._proc = subprocess.Popen(
            [
                "parec",
                "-d",
                source,
                "--format=s16le",
                f"--rate={RATE}",
                "--channels=1",
                "--latency-msec=10",
                f"--property=application.name=latency-{source}",
            ],
            stdout=self._file,
        )

    def frames(self) -> int:
        """10 ms frames recorded so far."""
        return os.fstat(self._file.fileno()).st_size // 2 // FRAME

    def stop(self, start: int) -> list[float]:
        """Levels of the 10 ms frames from `start` on."""
        self._proc.terminate()
        self._proc.wait()
        self._file.seek(start * FRAME * 2)
        raw = self._file.read()
        a = array.array("h", raw[: len(raw) // 2 * 2])
        return [
            (sum(x * x for x in a[i : i + FRAME]) / FRAME) ** 0.5
            for i in range(0, len(a) - FRAME + 1, FRAME)
        ]


def measure(path: str, seconds: float) -> None:
    default = pactl("get-default-sink").strip()
    modules = [
        pactl(
            "load-module", "module-null-sink", f"sink_name={TAP}", "rate=48000", "channels=2"
        ).strip(),
        pactl(
            "load-module",
            "module-loopback",
            f"source={TAP}.monitor",
            f"sink={default}",
            "latency_msec=30",
        ).strip(),
    ]
    pactl("set-default-sink", TAP)
    # Silence keeps the tap running (an idle sink suspends and its monitor stops).
    keep = subprocess.Popen(
        ["pacat", "-d", TAP, "--format=s16le", f"--rate={RATE}", "--channels=1"],
        stdin=open("/dev/zero", "rb"),
    )
    try:
        mic, spk = Recorder(f"{MIC}.monitor"), Recorder(f"{TAP}.monitor")
        # A recorder's first sample is when its source started delivering, which differs
        # between the two (a fresh sink takes a while): both timelines start at one moment,
        # once both are recording.
        deadline = time.monotonic() + 10
        while (mic.frames() < 50 or spk.frames() < 50) and time.monotonic() < deadline:
            time.sleep(0.1)
        m0, s0 = mic.frames(), spk.frames()
        subprocess.run(["paplay", "-d", MIC, path], check=True)
        time.sleep(seconds)
        m, s = mic.stop(m0), spk.stop(s0)
    finally:
        keep.terminate()
        pactl("set-default-sink", default)
        for mod in reversed(modules):
            pactl("unload-module", mod)

    speech = [i for i, v in enumerate(m) if v > LOUD]
    if not speech:
        sys.exit("no speech in the microphone recording")
    end = speech[-1]
    answer = next((i for i, v in enumerate(s) if i > end and v > LOUD), None)
    print(f"phrase ends at {end / 100:.2f} s of the recording")
    if answer is None:
        sys.exit("no answer heard: is orbita connected? was the wake word recognized?")
    print(f"answer starts at {answer / 100:.2f} s -> latency {(answer - end) / 100:.2f} s")


if __name__ == "__main__":
    measure(sys.argv[1], float(sys.argv[2]) if len(sys.argv) > 2 else 15.0)
