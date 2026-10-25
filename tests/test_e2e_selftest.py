"""End to end against pulsar's own ESPHome API server (api_selftest from pulsar's
satellite/crates/pulsar-esphome, built for this host with
`cargo build -p pulsar-esphome --bin api_selftest`).

api_selftest starts a voice run as soon as a client subscribes (wake word "Алиса", 0.64 s of a
tone over the API, then a stop request ~0.75 s in) and prints every event it receives. orbita
must finish the run before that stop: a 0.5 s VAD timeout ends listening.

ORBITA_SELFTEST_BIN=/path/to/api_selftest; skipped when it is not there.
"""

from __future__ import annotations

import asyncio
import os
import re
import socket
from pathlib import Path

import aiohttp
import pytest

from orbita.app import App
from orbita.config import parse
from tests.fakes import FakeOpenAI, FakeWyoming

DEFAULT_BIN = (
    Path(__file__).resolve().parents[2] / "saniaxxx/pulsar/satellite/target/debug/api_selftest"
)
BIN = Path(os.environ.get("ORBITA_SELFTEST_BIN", DEFAULT_BIN))

pytestmark = pytest.mark.skipif(not BIN.exists(), reason=f"no api_selftest at {BIN}")


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Selftest:
    def __init__(self, port: int, key_file: Path | None = None) -> None:
        self.port = port
        self.key_file = key_file
        self.lines: list[str] = []
        self.proc: asyncio.subprocess.Process | None = None
        self._reader: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self.proc = await asyncio.create_subprocess_exec(
            str(BIN), str(self.port), "pulsar-selftest", "30",
            *([str(self.key_file)] if self.key_file else []),
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
        )  # fmt: skip
        self._reader = asyncio.create_task(self._read())
        await self.wait_for(rf"listening|{self.port}", timeout=10, allow_missing=True)

    async def _read(self) -> None:
        assert self.proc and self.proc.stdout
        async for raw in self.proc.stdout:
            self.lines.append(raw.decode(errors="replace").rstrip())

    async def wait_for(self, pattern: str, timeout: float = 10, allow_missing: bool = False) -> str:
        rx = re.compile(pattern)
        loop = asyncio.get_running_loop()
        end = loop.time() + timeout
        seen = 0
        while loop.time() < end:
            for line in self.lines[seen:]:
                if rx.search(line):
                    return line
            seen = len(self.lines)
            await asyncio.sleep(0.05)
        if allow_missing:
            return ""
        raise AssertionError(f"no {pattern!r} in:\n" + "\n".join(self.lines))

    def events(self) -> list[str]:
        return [
            ln.split("HOOK voice_event ", 1)[1] for ln in self.lines if "HOOK voice_event " in ln
        ]

    async def stop(self) -> None:
        if self.proc and self.proc.returncode is None:
            self.proc.terminate()
            await self.proc.wait()
        if self._reader:
            await self._reader


async def run_app(cfg_dict: dict) -> tuple[App, asyncio.Event, asyncio.Task[None]]:
    app = App(parse(cfg_dict))
    stop = asyncio.Event()
    task = asyncio.create_task(app.run(stop))
    return app, stop, task


def base_cfg(api_port: int, http_port: int) -> dict:
    return {
        "satellites": [{"name": "selftest", "host": "127.0.0.1", "port": api_port}],
        "timezone": "Europe/Moscow",
        "http": {"host": "127.0.0.1", "port": http_port},
        # The tone is loud (RMS ~700): energy detector; 0.5 s: done before the stop request.
        "vad": {"engine": "energy", "speech_seconds": 0.1, "timeout_seconds": 0.5},
    }


async def test_voice_run_wyoming_rules():
    wy = FakeWyoming("Который час")
    await wy.start()
    st = Selftest(free_port())
    await st.start()
    http_port = free_port()
    cfg = base_cfg(st.port, http_port) | {
        "stt": {"engine": "wyoming", "port": wy.port},
        "tts": {"engine": "wyoming", "port": wy.port, "voice": "ru_RU-irina-medium"},
        "intents": [
            {"type": "timers"},
            {"type": "rules", "rules": [{"match": "который час", "reply": "Сейчас {time}."}]},
        ],
    }
    app, stop, task = await run_app(cfg)
    try:
        await st.wait_for(r"HOOK voice_event 2\b", timeout=15)  # RUN_END
        ev = st.events()
        codes = [e.split()[0] for e in ev]
        # RUN_START STT_START STT_VAD_START STT_VAD_END STT_END INTENT_START INTENT_END
        # TTS_START TTS_END RUN_END
        assert codes == ["1", "3", "11", "12", "4", "5", "6", "7", "8", "2"], ev
        assert "text=Который час" in ev[4]
        assert "continue_conversation=0" in ev[6]
        assert re.search(r"text=Сейчас \d+:\d\d\.", ev[7]), ev[7]
        url = re.search(r"url=(\S+)", ev[8]).group(1)
        assert url.startswith(f"http://127.0.0.1:{http_port}/tts/") and url.endswith(".wav")
        async with aiohttp.ClientSession() as s, s.get(url) as r:
            assert r.status == 200 and r.content_type == "audio/wav"
            assert (await r.read())[:4] == b"RIFF"
        assert wy.rec.transcribe_language == ["ru"]
        assert wy.rec.stt_bytes and wy.rec.stt_bytes[0] > 0
        assert wy.rec.synthesized[0][1] == "ru_RU-irina-medium"

        # Timers and announcements on the same connection.
        sat = app.s.satellites["selftest"]
        sat.timer_start(90, "pizza")
        line = await st.wait_for(r"HOOK timer event=0 ")
        assert "total=90" in line and "name=pizza" in line and "active=1" in line
        assert sat.timer_cancel()
        await st.wait_for(r"HOOK timer event=2 ")
        ok = await sat.announce("Обед готов")
        line = await st.wait_for(r"HOOK announce ")
        assert "text='Обед готов'" in line and "/tts/" in line and ok
    finally:
        stop.set()
        await task
        await st.stop()
        await wy.stop()


async def test_voice_run_openai_everything():
    oa = FakeOpenAI(answer="Привет! Чем помочь?", transcript="привет")
    await oa.start()
    st = Selftest(free_port())
    await st.start()
    cfg = base_cfg(st.port, free_port()) | {
        "openai": {"base_url": oa.base_url, "api_key": "sk-test"},
        "stt": {"engine": "openai", "model": "whisper-large-v3"},
        "tts": {"engine": "openai", "model": "tts-ru", "voice": "nova"},
        "intents": [{"type": "openai", "model": "qwen", "temperature": 0.2}],
    }
    app, stop, task = await run_app(cfg)
    try:
        await st.wait_for(r"HOOK voice_event 2\b", timeout=15)
        ev = st.events()
        assert "text=привет" in next(e for e in ev if e.startswith("4 "))
        # The answer is a question: the satellite listens again without the wake word.
        assert "continue_conversation=1" in next(e for e in ev if e.startswith("6 "))
        assert "text=Привет! Чем помочь?" in next(e for e in ev if e.startswith("7 "))
        assert oa.rec.uploads[0]["model"] == "whisper-large-v3"
        assert oa.rec.uploads[0]["language"] == "ru"
        assert oa.rec.uploads[0]["bytes"] > 44
        chat = oa.rec.chats[0]
        assert chat["model"] == "qwen" and chat["temperature"] == 0.2
        assert chat["messages"][0]["role"] == "system"
        assert chat["messages"][-1] == {"role": "user", "content": "привет"}
        assert set(oa.auth) == {"Bearer sk-test"}
        assert oa.rec.synthesized == [("Привет! Чем помочь?", "nova")]
    finally:
        stop.set()
        await task
        await st.stop()
        await oa.stop()


async def test_stt_down_reports_error():
    st = Selftest(free_port())
    await st.start()
    cfg = base_cfg(st.port, free_port()) | {"stt": {"engine": "wyoming", "port": free_port()}}
    app, stop, task = await run_app(cfg)
    try:
        await st.wait_for(r"HOOK voice_event 2\b", timeout=15)
        ev = st.events()
        err = next(e for e in ev if e.startswith("0 "))
        assert "code=stt-stream-failed" in err
    finally:
        stop.set()
        await task
        await st.stop()


async def test_noise_encryption(tmp_path):
    import base64

    key = base64.b64encode(os.urandom(32)).decode()
    key_file = tmp_path / "api_key"
    key_file.write_text(key + "\n")
    wy = FakeWyoming("Который час")
    await wy.start()
    st = Selftest(free_port(), key_file)
    await st.start()
    cfg = base_cfg(st.port, free_port()) | {
        "stt": {"engine": "wyoming", "port": wy.port},
        "tts": {"engine": "wyoming", "port": wy.port},
        "intents": [{"type": "rules", "rules": [{"match": ".", "reply": "Да."}]}],
    }
    cfg["satellites"][0]["noise_psk"] = key
    app, stop, task = await run_app(cfg)
    try:
        await st.wait_for(r"HOOK voice_event 2\b", timeout=15)
        assert any(e.startswith("8 url=") for e in st.events())
    finally:
        stop.set()
        await task
        await st.stop()
        await wy.stop()
