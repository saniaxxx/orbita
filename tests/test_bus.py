import json

from orbita.bus import Bus, parse_announce
from orbita.config import MqttConfig
from orbita.intents import Request


def test_parse_announce():
    assert parse_announce(b"hello") == {"text": "hello"}
    assert parse_announce(b'{"text": "a", "start_conversation": true}') == {
        "text": "a",
        "start_conversation": True,
    }
    assert parse_announce(b"{broken") == {"text": "{broken"}


class FakeClient:
    def __init__(self):
        self.sent = []

    async def publish(self, topic, payload, retain=False):
        self.sent.append((topic, payload, retain))


async def test_ask_and_announce_dispatch():
    announced = []

    async def on_announce(sat, data):
        announced.append((sat, data))

    bus = Bus(MqttConfig(host="x"), ["kitchen"], on_announce)
    bus._client = FakeClient()

    async def reply_soon():
        import asyncio

        while not bus._client.sent:
            await asyncio.sleep(0.01)
        topic, payload, _ = bus._client.sent[0]
        assert topic == "orbita/kitchen/intent"
        req = json.loads(payload)
        bus._dispatch(
            "orbita/kitchen/intent/reply",
            json.dumps({"id": req["id"], "text": "Готово", "continue": False}).encode(),
        )

    import asyncio

    t = asyncio.create_task(reply_soon())
    data = await bus.ask(Request("включи свет", "kitchen", "c1", "ru"), 2)
    await t
    assert data["text"] == "Готово"

    bus._dispatch("orbita/kitchen/announce", "Обед".encode())
    bus._dispatch("orbita/unknown/announce", b"x")
    await asyncio.sleep(0.01)
    assert announced == [("kitchen", {"text": "Обед"})]


async def test_ask_timeout():
    async def on_announce(sat, data):
        pass

    bus = Bus(MqttConfig(host="x"), ["kitchen"], on_announce)
    bus._client = FakeClient()
    assert await bus.ask(Request("x", "kitchen", "c"), 0.05) is None
