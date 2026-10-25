"""Wires the services and the satellites together."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable
from typing import Any

from .bus import Bus
from .config import Config
from .http_store import AudioStore
from .intents import Brain, build_brain
from .satellite import Satellite
from .speech import Stt, Tts
from .vad import make_detector

_LOG = logging.getLogger(__name__)


class Services:
    def __init__(self, config: Config) -> None:
        self.config = config
        self.stt = Stt(config.stt, config.openai)
        self.tts = Tts(config.tts, config.openai)
        h = config.http
        self.store = AudioStore(h.host, h.port, h.base_url, h.ttl_seconds)
        self.bus: Bus | None = None
        self.brain: Brain  # set by App: its handlers may need the bus
        self.satellites: dict[str, Satellite] = {}
        make_detector(config.vad)  # fail at start, not on the first command

    def make_vad(self) -> Callable[[bytes], bool]:
        return make_detector(self.config.vad)

    async def publish_status(self, satellite: str, status: str) -> None:
        if self.bus:
            await self.bus.publish(self.bus.topic(satellite, "status"), status, retain=True)

    async def publish_json(self, satellite: str, leaf: str, data: dict[str, Any]) -> None:
        if self.bus:
            await self.bus.publish_json(satellite, leaf, data)


class App:
    def __init__(self, config: Config) -> None:
        self.s = Services(config)
        if config.mqtt.host:
            self.s.bus = Bus(config.mqtt, [c.name for c in config.satellites], self._announce)
            self.s.brain = build_brain(config, self.s.bus.publish, self.s.bus.ask)
        else:
            self.s.brain = build_brain(config, None, None)
        for c in config.satellites:
            self.s.satellites[c.name] = Satellite(c, self.s)

    async def _announce(self, satellite: str, data: dict[str, Any]) -> None:
        sat = self.s.satellites[satellite]
        text = str(data.get("text") or "")
        media = str(data.get("media_url") or "")
        if not text and not media:
            return
        try:
            ok = await sat.announce(text, media, bool(data.get("start_conversation", False)))
            _LOG.info("%s: announcement %s", satellite, "played" if ok else "not played")
        except Exception as e:  # noqa: BLE001
            _LOG.warning("%s: announcement failed: %s", satellite, e)

    async def run(self, stop: asyncio.Event) -> None:
        await self.s.store.start()
        if self.s.bus:
            self.s.bus.start()
        for sat in self.s.satellites.values():
            await sat.start()
        _LOG.info("orbita: %d satellite(s)", len(self.s.satellites))
        try:
            await stop.wait()
        finally:
            for sat in self.s.satellites.values():
                await sat.stop()
            if self.s.bus:
                await self.s.bus.stop()
            await self.s.store.stop()
