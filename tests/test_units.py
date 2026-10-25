import pytest

from orbita.config import ConfigError, VadConfig, load, parse
from orbita.intents import Request, build_brain, speakable
from orbita.timers import parse_duration, say_duration
from orbita.vad import FRAME_BYTES, Segmenter, VadEvent, frame_rms

# ---------------- config ----------------


def test_example_config_parses():
    cfg = load("config.example.yaml")
    assert cfg.satellites[0].name == "kitchen"
    assert [i["type"] for i in cfg.intents] == ["timers", "rules", "openai"]
    build_brain(cfg, None, None)


def test_config_errors():
    with pytest.raises(ConfigError, match="satellites"):
        parse({})
    with pytest.raises(ConfigError, match="unknown key"):
        parse({"satellites": [{"name": "a", "host": "h", "bogus": 1}]})
    with pytest.raises(ConfigError, match="unique"):
        parse({"satellites": [{"name": "a", "host": "h"}, {"name": "a", "host": "g"}]})
    with pytest.raises(ConfigError, match="engine"):
        parse({"satellites": [{"name": "a", "host": "h"}], "stt": {"engine": "x"}})


def test_config_nested_openai():
    cfg = parse(
        {
            "satellites": [{"name": "a", "host": "h"}],
            "stt": {"engine": "openai", "openai": {"base_url": "http://x/v1", "api_key": "k"}},
        }
    )
    assert cfg.stt.openai.base_url == "http://x/v1"
    assert cfg.stt.openai.key() == "k"


def test_api_key_from_env(monkeypatch):
    monkeypatch.setenv("OPENAI_API_KEY", "envkey")
    cfg = parse({"satellites": [{"name": "a", "host": "h"}]})
    assert cfg.openai.key() == "envkey"


# ---------------- vad ----------------


def frames(speech: list[bool]) -> bytes:
    # The fake detector reads the first byte.
    return b"".join((b"\x01" if s else b"\x00") * FRAME_BYTES for s in speech)


def seg(**kw) -> Segmenter:
    cfg = VadConfig(**{"speech_seconds": 0.05, "silence_seconds": 0.1, **kw})
    return Segmenter(cfg, lambda f: f[0] == 1)


def test_vad_end_after_silence():
    s = seg()
    assert s.process(frames([False] * 10)) == []
    assert s.process(frames([True] * 5)) == [VadEvent.SPEECH_START]
    assert s.process(frames([False] * 9)) == []
    assert s.process(frames([False])) == [VadEvent.END]
    assert s.process(frames([True] * 50)) == []


def test_vad_short_noise_is_not_speech():
    s = seg(no_speech_seconds=0.3)
    ev = s.process(frames([True, True, False] * 20))
    assert ev == [VadEvent.NO_SPEECH]


def test_vad_timeout_and_partial_frames():
    s = seg(timeout_seconds=0.2)
    audio = frames([True] * 30)
    out = []
    for i in range(0, len(audio), 100):  # chunks not aligned to frames
        out += s.process(audio[i : i + 100])
    assert out == [VadEvent.SPEECH_START, VadEvent.TIMEOUT]


def test_frame_rms():
    assert frame_rms(b"\x00\x00" * 160) == 0
    assert round(frame_rms(b"\xe8\x03" * 160)) == 1000


# ---------------- timers ----------------


@pytest.mark.parametrize(
    "text,seconds",
    [
        ("поставь таймер на 5 минут", 300),
        ("таймер на пять минут", 300),
        ("таймер на двадцать пять секунд", 25),
        ("таймер на 1 час 20 минут", 4800),
        ("засеки полчаса", 1800),
        ("таймер на полторы минуты", 90),
        ("таймер на две с половиной минуты", 150),
        ("таймер на минуту", 60),
        ("таймер на 30 с", 30),
        ("поставь таймер на 10мин", 600),
        ("таймер на час", 3600),
        ("таймер на сорок минут тридцать секунд", 2430),
        ("поставь таймер", 0),
    ],
)
def test_parse_duration(text, seconds):
    assert parse_duration(text) == seconds


def test_say_duration():
    assert say_duration(60, True) == "1 минуту"
    assert say_duration(300, True) == "5 минут"
    assert say_duration(3722) == "1 час 2 минуты 2 секунды"
    assert say_duration(22) == "22 секунды"
    assert say_duration(11 * 60, True) == "11 минут"


class FakeSat:
    def __init__(self):
        self.timers = {}
        self.started = []

    def timer_start(self, seconds, name=""):
        import time

        from orbita.satellite import Timer

        t = Timer(str(len(self.timers)), name, seconds, time.monotonic() + seconds)
        self.timers[t.id] = t
        self.started.append(seconds)
        return t

    def timer_cancel(self, timer_id=None):
        out = list(self.timers.values())
        self.timers.clear()
        return out


async def test_timers_handler_dialog():
    cfg = parse({"satellites": [{"name": "a", "host": "h"}], "intents": [{"type": "timers"}]})
    brain = build_brain(cfg, None, None)
    sat = FakeSat()

    async def say(text):
        return await brain.answer(Request(text, "a", "c1", timers=sat))

    r = await say("Поставь таймер на 5 минут")
    assert r.text == "Таймер на 5 минут запущен." and sat.started == [300]
    r = await say("сколько осталось?")
    assert r.text.startswith("Осталось 4 минуты") or r.text.startswith("Осталось 5 минут")
    r = await say("поставь таймер")
    assert r.continue_conversation and r.text == "На сколько поставить таймер?"
    r = await say("три минуты")
    assert r.text == "Таймер на 3 минуты запущен." and sat.started == [300, 180]
    r = await say("отмени таймер")
    assert r.text == "Все таймеры отменены." and not sat.timers
    r = await say("как дела")
    assert r.handler == "fallback"


# ---------------- rules ----------------


async def test_rules_templates_and_mqtt():
    published = []

    async def publish(topic, payload):
        published.append((topic, payload))

    cfg = parse(
        {
            "satellites": [{"name": "kitchen", "host": "h"}],
            "timezone": "Europe/Moscow",
            "intents": [
                {
                    "type": "rules",
                    "rules": [
                        {"match": r"который час|сколько времени", "reply": "Сейчас {time}."},
                        {
                            "match": r"(?P<action>включи|выключи) свет",
                            "reply": "Готово.",
                            "mqtt": {
                                "topic": "z2m/{satellite}_light/set",
                                "payload": '{{"state": "{action}"}}',
                            },
                        },
                    ],
                }
            ],
        }
    )
    brain = build_brain(cfg, publish, None)
    r = await brain.answer(Request("Который час", "kitchen", "c"))
    assert r.text.startswith("Сейчас ") and ":" in r.text and r.handler == "rules"
    r = await brain.answer(Request("Включи свет", "kitchen", "c"))
    assert r.text == "Готово."
    assert published == [("z2m/kitchen_light/set", '{"state": "Включи"}')]


def test_rules_mqtt_without_bus_is_an_error():
    cfg = parse(
        {
            "satellites": [{"name": "a", "host": "h"}],
            "intents": [{"type": "rules", "rules": [{"match": "x", "mqtt": {"topic": "t"}}]}],
        }
    )
    with pytest.raises(ConfigError, match="mqtt"):
        build_brain(cfg, None, None)


def test_speakable():
    assert speakable("**Привет**!\n- один\n- два") == "Привет! один два"
