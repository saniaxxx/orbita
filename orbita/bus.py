"""MQTT: what orbita hears and says, announcements and intents for Node-RED and the like.

<prefix>/<satellite>/status        online / offline (retained)
<prefix>/<satellite>/transcript    {"text", "conversation_id"}
<prefix>/<satellite>/reply         {"text", "conversation_id", "handler"}
<prefix>/<satellite>/timer         {"id", "name", "event": "finished"}
<prefix>/<satellite>/announce      <- text, or {"text", "media_url", "start_conversation"}
<prefix>/<satellite>/intent        {"id", "text", "satellite", "conversation_id", "language"}
<prefix>/<satellite>/intent/reply  <- {"id", "text", "continue"}
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import uuid
from collections.abc import Awaitable, Callable
from typing import Any

import aiomqtt

from .config import MqttConfig
from .intents import Request

_LOG = logging.getLogger(__name__)

AnnounceFn = Callable[[str, dict[str, Any]], Awaitable[None]]


def parse_announce(payload: bytes) -> dict[str, Any]:
    raw = payload.decode("utf-8", "replace").strip()
    if raw.startswith("{"):
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = None
        if isinstance(data, dict):
            return data
    return {"text": raw}


class Bus:
    def __init__(self, cfg: MqttConfig, satellites: list[str], on_announce: AnnounceFn) -> None:
        self._cfg = cfg
        self._prefix = cfg.prefix.rstrip("/")
        self._satellites = set(satellites)
        self._on_announce = on_announce
        self._client: aiomqtt.Client | None = None
        self._pending: dict[str, asyncio.Future[Any]] = {}
        self._task: asyncio.Task[None] | None = None
        self._tasks: set[asyncio.Task[None]] = set()

    def start(self) -> None:
        self._task = asyncio.create_task(self._run(), name="mqtt")

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task

    def topic(self, satellite: str, leaf: str) -> str:
        return f"{self._prefix}/{satellite}/{leaf}"

    async def publish(self, topic: str, payload: str, retain: bool = False) -> None:
        if self._client is None:
            _LOG.debug("mqtt not connected, dropped %s", topic)
            return
        try:
            await self._client.publish(topic, payload, retain=retain)
        except aiomqtt.MqttError as e:
            _LOG.warning("mqtt publish %s: %s", topic, e)

    async def publish_json(self, satellite: str, leaf: str, data: dict[str, Any]) -> None:
        await self.publish(self.topic(satellite, leaf), json.dumps(data, ensure_ascii=False))

    async def ask(self, req: Request, timeout: float) -> Any:
        """Intent over MQTT: publish the request, wait for the reply with the same id."""
        if self._client is None:
            return None
        rid = uuid.uuid4().hex
        fut: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self._pending[rid] = fut
        try:
            await self.publish_json(
                req.satellite,
                "intent",
                {
                    "id": rid,
                    "text": req.text,
                    "satellite": req.satellite,
                    "conversation_id": req.conversation_id,
                    "language": req.language,
                },
            )
            return await asyncio.wait_for(fut, timeout)
        except TimeoutError:
            _LOG.info("%s: no intent reply over mqtt in %.1f s", req.satellite, timeout)
            return None
        finally:
            self._pending.pop(rid, None)

    async def _run(self) -> None:
        backoff = 1.0
        while True:
            try:
                async with aiomqtt.Client(
                    hostname=self._cfg.host,
                    port=self._cfg.port,
                    username=self._cfg.username,
                    password=self._cfg.password,
                    identifier=f"orbita-{uuid.uuid4().hex[:6]}",
                    will=aiomqtt.Will(f"{self._prefix}/status", "offline", retain=True),
                ) as client:
                    self._client = client
                    backoff = 1.0
                    _LOG.info("mqtt connected to %s:%d", self._cfg.host, self._cfg.port)
                    await client.publish(f"{self._prefix}/status", "online", retain=True)
                    await client.subscribe(f"{self._prefix}/+/announce")
                    await client.subscribe(f"{self._prefix}/+/intent/reply")
                    async for msg in client.messages:
                        self._dispatch(str(msg.topic), bytes(msg.payload or b""))
            except aiomqtt.MqttError as e:
                _LOG.warning("mqtt: %s; reconnecting in %.0f s", e, backoff)
            finally:
                self._client = None
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 60.0)

    def _dispatch(self, topic: str, payload: bytes) -> None:
        parts = topic[len(self._prefix) + 1 :].split("/")
        if len(parts) < 2 or parts[0] not in self._satellites:
            return
        sat, leaf = parts[0], "/".join(parts[1:])
        if leaf == "announce":
            task = asyncio.create_task(self._on_announce(sat, parse_announce(payload)))
            self._tasks.add(task)
            task.add_done_callback(self._tasks.discard)
        elif leaf == "intent/reply":
            try:
                data = json.loads(payload)
            except json.JSONDecodeError:
                _LOG.warning("%s: intent reply is not JSON", sat)
                return
            fut = self._pending.get(str(data.get("id"))) if isinstance(data, dict) else None
            if fut and not fut.done():
                fut.set_result(data)
