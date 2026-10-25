"""What to answer: a chain of handlers, the first one that answers wins.

rules   regular expressions -> reply template, optionally an MQTT message (lights...)
timers  voice timers in Russian (timers.py)
openai  an LLM behind an OpenAI-compatible chat completions API, with the dialog history
mqtt    asks over MQTT (Node-RED...) and waits for the reply
"""

from __future__ import annotations

import datetime as dt
import logging
import re
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any
from zoneinfo import ZoneInfo

from .config import Config, ConfigError, OpenAIConfig
from .openai_io import OpenAIClient

_LOG = logging.getLogger(__name__)

WEEKDAYS = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"]
MONTHS = [
    "января", "февраля", "марта", "апреля", "мая", "июня",
    "июля", "августа", "сентября", "октября", "ноября", "декабря",
]  # fmt: skip

DEFAULT_SYSTEM = (
    "Ты голосовой ассистент умной колонки. Отвечай по-русски, коротко (одно-два предложения), "
    "без разметки, списков и эмодзи: ответ будет произнесён вслух. Числа, даты и единицы "
    "пиши так, как их произносят."
)

# Turns kept per conversation (user + assistant messages).
MAX_MESSAGES = 40

Publish = Callable[[str, str], Awaitable[None]]


@dataclass
class Request:
    text: str
    satellite: str
    conversation_id: str
    language: str | None = None
    # The satellite (Satellite.timer_start / timer_cancel / timers), for the timers handler.
    timers: Any = None


@dataclass
class Reply:
    text: str
    continue_conversation: bool = False
    handler: str = ""


@dataclass
class Conversation:
    id: str
    messages: list[dict[str, str]] = field(default_factory=list)
    used: float = field(default_factory=time.monotonic)


class Conversations:
    def __init__(self, ttl: float) -> None:
        self._ttl = ttl
        self._items: dict[str, Conversation] = {}

    def get(self, conversation_id: str) -> Conversation:
        now = time.monotonic()
        for cid in [c for c, v in self._items.items() if now - v.used > self._ttl]:
            del self._items[cid]
        conv = self._items.get(conversation_id)
        if conv is None:
            conv = self._items[conversation_id] = Conversation(conversation_id)
        conv.used = now
        return conv

    def known(self, conversation_id: str) -> bool:
        conv = self._items.get(conversation_id)
        return conv is not None and time.monotonic() - conv.used <= self._ttl


def now_in(tz: str) -> dt.datetime:
    return dt.datetime.now(ZoneInfo(tz)) if tz else dt.datetime.now().astimezone()


def time_vars(now: dt.datetime) -> dict[str, str]:
    return {
        "time": f"{now.hour}:{now.minute:02d}",
        "date": f"{now.day} {MONTHS[now.month - 1]}",
        "weekday": WEEKDAYS[now.weekday()],
        "year": str(now.year),
    }


class _Vars(dict[str, str]):
    def __missing__(self, key: str) -> str:
        return "{" + key + "}"


def fill(template: str, values: dict[str, str]) -> str:
    return template.format_map(_Vars(values))


class Handler:
    name = ""

    async def handle(self, req: Request, conv: Conversation) -> Reply | None:
        raise NotImplementedError


@dataclass
class Rule:
    pattern: re.Pattern[str]
    reply: str
    mqtt: list[tuple[str, str]]
    continue_conversation: bool


class RulesHandler(Handler):
    name = "rules"

    def __init__(self, spec: dict[str, Any], tz: str, publish: Publish | None) -> None:
        self._tz = tz
        self._publish = publish
        self._rules: list[Rule] = []
        for i, r in enumerate(spec.get("rules") or []):
            where = f"intents.rules[{i}]"
            if not isinstance(r, dict) or "match" not in r:
                raise ConfigError(f"{where}: expected a mapping with 'match'")
            try:
                pattern = re.compile(r["match"], re.IGNORECASE)
            except re.error as e:
                raise ConfigError(f"{where}.match: {e}") from None
            msgs = r.get("mqtt") or []
            if isinstance(msgs, dict):
                msgs = [msgs]
            mqtt = [(m["topic"], str(m.get("payload", ""))) for m in msgs]
            if mqtt and publish is None:
                raise ConfigError(f"{where}.mqtt: the mqtt section has no host")
            self._rules.append(
                Rule(pattern, str(r.get("reply", "")), mqtt, bool(r.get("continue", False)))
            )

    async def handle(self, req: Request, conv: Conversation) -> Reply | None:
        for rule in self._rules:
            m = rule.pattern.search(req.text)
            if not m:
                continue
            values = time_vars(now_in(self._tz))
            values.update({k: v for k, v in m.groupdict().items() if v is not None})
            values["text"] = req.text
            values["satellite"] = req.satellite
            for topic, payload in rule.mqtt:
                assert self._publish is not None
                await self._publish(fill(topic, values), fill(payload, values))
            return Reply(fill(rule.reply, values), rule.continue_conversation, self.name)
        return None


_MARKUP = re.compile(r"[*_`#>]+")


def speakable(text: str) -> str:
    """LLM output -> text for TTS: no markdown, one line."""
    text = _MARKUP.sub("", text)
    text = re.sub(r"^\s*[-•]\s+", "", text, flags=re.MULTILINE)
    return re.sub(r"\s+", " ", text).strip()


class OpenAIHandler(Handler):
    name = "openai"

    def __init__(self, spec: dict[str, Any], default_api: OpenAIConfig, tz: str) -> None:
        if not spec.get("model"):
            raise ConfigError("intents.openai: 'model' is required")
        api = OpenAIConfig(
            base_url=spec.get("base_url", default_api.base_url),
            api_key=spec.get("api_key", default_api.api_key),
            timeout=float(spec.get("timeout", default_api.timeout)),
        )
        self._client = OpenAIClient(api)
        self._model = str(spec["model"])
        self._system = str(spec.get("system", DEFAULT_SYSTEM))
        self._history = int(spec.get("max_history", 10))
        self._continue_on_question = bool(spec.get("continue_on_question", True))
        self._params: dict[str, Any] = {}
        for key in ("temperature", "max_tokens", "top_p"):
            if key in spec:
                self._params[key] = spec[key]
        self._params.update(spec.get("extra_body") or {})
        self._tz = tz

    def _system_message(self, req: Request) -> str:
        v = time_vars(now_in(self._tz))
        return (
            f"{self._system}\nСейчас {v['weekday']}, {v['date']} {v['year']}, {v['time']}. "
            f"Колонка: {req.satellite}."
        )

    async def handle(self, req: Request, conv: Conversation) -> Reply | None:
        history = conv.messages[-2 * self._history :] if self._history > 0 else []
        messages = [
            {"role": "system", "content": self._system_message(req)},
            *history,
            {"role": "user", "content": req.text},
        ]
        answer = speakable(await self._client.chat(self._model, messages, **self._params))
        if not answer:
            return None
        ask = self._continue_on_question and answer.rstrip().endswith("?")
        return Reply(answer, ask, self.name)


class MqttHandler(Handler):
    """Publishes {id, text, satellite, conversation_id} to <prefix>/<satellite>/intent and waits
    for {id, text, continue} on <prefix>/<satellite>/intent/reply. An empty text passes the
    request on to the next handler, as does the timeout."""

    name = "mqtt"

    def __init__(
        self, spec: dict[str, Any], ask: Callable[[Request, float], Awaitable[Any]]
    ) -> None:
        self._ask = ask
        self._timeout = float(spec.get("timeout", 5.0))

    async def handle(self, req: Request, conv: Conversation) -> Reply | None:
        data = await self._ask(req, self._timeout)
        if not isinstance(data, dict) or not str(data.get("text") or "").strip():
            return None
        return Reply(str(data["text"]).strip(), bool(data.get("continue", False)), self.name)


class Brain:
    def __init__(self, handlers: list[Handler], cfg: Config) -> None:
        self._handlers = handlers
        self._fallback = cfg.fallback_reply
        self._error = cfg.error_reply
        self.conversations = Conversations(cfg.conversation_ttl)

    async def answer(self, req: Request) -> Reply:
        conv = self.conversations.get(req.conversation_id)
        failed = False
        for h in self._handlers:
            try:
                reply = await h.handle(req, conv)
            except Exception as e:  # noqa: BLE001 - a broken handler must not end the dialog
                _LOG.warning("%s: handler %s failed: %s", req.satellite, h.name, e)
                failed = True
                continue
            if reply is not None:
                break
        else:
            reply = Reply(self._error if failed else self._fallback, False, "fallback")
        # Every turn, whoever answered it: the LLM sees what the rules and timers said.
        conv.messages += [
            {"role": "user", "content": req.text},
            {"role": "assistant", "content": reply.text},
        ]
        del conv.messages[:-MAX_MESSAGES]
        return reply


def build_brain(
    cfg: Config,
    publish: Publish | None,
    ask: Callable[[Request, float], Awaitable[Any]] | None,
) -> Brain:
    handlers: list[Handler] = []
    for i, spec in enumerate(cfg.intents):
        kind = spec["type"]
        if kind == "rules":
            handlers.append(RulesHandler(spec, cfg.timezone, publish))
        elif kind == "openai":
            handlers.append(OpenAIHandler(spec, cfg.openai, cfg.timezone))
        elif kind == "timers":
            from .timers import TimersHandler

            handlers.append(TimersHandler(spec))
        elif kind == "mqtt":
            if ask is None:
                raise ConfigError(f"intents[{i}]: type mqtt needs the mqtt section with a host")
            handlers.append(MqttHandler(spec, ask))
        else:
            raise ConfigError(f"intents[{i}]: unknown type {kind!r} (rules, timers, openai, mqtt)")
    return Brain(handlers, cfg)
