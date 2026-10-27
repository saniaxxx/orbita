"""What to answer: a chain of handlers, the first one that answers wins.

rules   regular expressions -> reply template, optionally an MQTT message (lights...)
timers  voice timers in Russian (timers.py)
openai  an LLM behind an OpenAI-compatible chat completions API, with the dialog history
mqtt    asks over MQTT (Node-RED...) and waits for the reply
"""

from __future__ import annotations

import datetime as dt
import logging
import math
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

# Modes: a request that matches one gets its instruction (in the last message, next to the time,
# so the system prompt and the prompt cache stay the same) and its own parameters. A mode is
# picked by its regular expression (`match`, first wins) or, with a classifier configured, by
# meaning: the request goes to the mode whose `examples` it is closest to, or to none if it is
# closest to the ordinary requests (`classifier.plain_examples`). `modes:` of the openai handler
# replaces these.
DEFAULT_MODES: list[dict[str, Any]] = [
    {
        "name": "long",
        "match": r"сказк|истори|сти[хш]|подробн|поподробн|развёрнут|развернут",
        "examples": [
            "Расскажи сказку про лису",
            "Расскажи подробно, как работает двигатель",
            "Прочитай стихотворение про зиму",
            "Расскажи длинную историю про пиратов",
            "Объясни развёрнуто, что такое квантовая физика",
            "Почитай мне что-нибудь интересное",
            "Сочини рассказ про дракона",
            "Расскажи что-нибудь подольше, хочу послушать",
        ],
        "prompt": (
            "сразу выполни просьбу, не переспрашивая: расскажи, прочитай или объясни развёрнуто, "
            "до пятнадцати предложений"
        ),
        "max_tokens": 600,
    },
]
# Ordinary requests: questions, smart home, timers, small talk.
DEFAULT_PLAIN_EXAMPLES = [
    "Какая столица Франции?",
    "Сколько лет Земле?",
    "Кто написал Муму?",
    "Сколько будет семь на восемь?",
    "Какая завтра погода?",
    "Включи свет в гостиной",
    "Выключи телевизор",
    "Сделай температуру двадцать два градуса",
    "Закрой шторы в спальне",
    "Поставь таймер на десять минут",
    "Разбуди меня в шесть утра",
    "Сколько осталось на таймере?",
    "Отмени будильник",
    "Напомни через час позвонить маме",
    "Привет",
    "Как у тебя дела?",
    "Спасибо большое",
    "Мне грустно",
    "Ты кто такая?",
    "Что ты умеешь?",
]
MODE_KEYS = {"name", "match", "examples", "prompt", "temperature", "max_tokens", "top_p"}


@dataclass
class Mode:
    name: str
    match: re.Pattern[str] | None
    examples: list[str]
    prompt: str
    params: dict[str, Any]


def _modes(specs: Any) -> list[Mode]:
    if not isinstance(specs, list):
        raise ConfigError("intents.openai.modes: expected a list")
    out = []
    for i, s in enumerate(specs):
        where = f"intents.openai.modes[{i}]"
        if not isinstance(s, dict) or not s.get("prompt"):
            raise ConfigError(f"{where}: 'prompt' is required")
        if not s.get("match") and not s.get("examples"):
            raise ConfigError(f"{where}: 'match' or 'examples' is required")
        unknown = set(s) - MODE_KEYS
        if unknown:
            raise ConfigError(f"{where}: unknown key(s) {', '.join(sorted(unknown))}")
        match = None
        if s.get("match"):
            try:
                match = re.compile(str(s["match"]), re.IGNORECASE)
            except re.error as e:
                raise ConfigError(f"{where}.match: {e}") from None
        examples = [str(x) for x in s.get("examples") or []]
        params = {k: s[k] for k in ("temperature", "max_tokens", "top_p") if k in s}
        out.append(Mode(str(s.get("name", i)), match, examples, str(s["prompt"]), params))
    return out


def _centroid(vectors: list[list[float]]) -> list[float]:
    c = [sum(xs) / len(vectors) for xs in zip(*vectors, strict=True)]
    norm = math.sqrt(sum(x * x for x in c)) or 1.0
    return [x / norm for x in c]


class Classifier:
    """Request -> mode by meaning, over an OpenAI-compatible /embeddings (servers/embed_openai.py):
    the nearest centroid of the modes' examples and of the ordinary requests. The examples are
    embedded on first use; while the service is down, no mode is picked (the regular expressions
    still work) and it is retried after RETRY seconds."""

    RETRY = 60.0

    def __init__(self, spec: Any, modes: list[Mode]) -> None:
        if not isinstance(spec, dict) or not spec.get("base_url"):
            raise ConfigError("intents.openai.classifier: 'base_url' is required")
        unknown = set(spec) - {"base_url", "api_key", "model", "timeout", "plain_examples"}
        if unknown:
            raise ConfigError(f"intents.openai.classifier: unknown key(s) {', '.join(unknown)}")
        self._client = OpenAIClient(
            OpenAIConfig(
                base_url=str(spec["base_url"]),
                api_key=str(spec.get("api_key", "")),
                timeout=float(spec.get("timeout", 1.0)),
            )
        )
        self._model = str(spec.get("model", "embeddings"))
        self._plain = [str(x) for x in spec.get("plain_examples") or DEFAULT_PLAIN_EXAMPLES]
        self._modes = [m for m in modes if m.examples]
        self._centroids: list[tuple[Mode | None, list[float]]] | None = None
        self._failed_at = -math.inf

    async def _load(self) -> list[tuple[Mode | None, list[float]]]:
        groups: list[tuple[Mode | None, list[str]]] = [(None, self._plain)]
        groups += [(m, m.examples) for m in self._modes]
        texts = [t for _, xs in groups for t in xs]
        vectors = await self._client.embeddings(texts, self._model)
        out, i = [], 0
        for mode, xs in groups:
            out.append((mode, _centroid(vectors[i : i + len(xs)])))
            i += len(xs)
        return out

    async def classify(self, text: str) -> tuple[Mode | None, float]:
        """-> (mode or None for an ordinary request, similarity)."""
        if not self._modes or time.monotonic() - self._failed_at < self.RETRY:
            return None, 0.0
        try:
            if self._centroids is None:
                self._centroids = await self._load()
            (v,) = await self._client.embeddings([text], self._model)
        except Exception as e:  # noqa: BLE001 - classification is a nicety, never a failure
            _LOG.warning(
                "classifier unavailable (%s); regular expressions only for %.0f s", e, self.RETRY
            )
            self._failed_at = time.monotonic()
            return None, 0.0
        mode, c = max(
            self._centroids, key=lambda mc: sum(a * b for a, b in zip(v, mc[1], strict=True))
        )
        return mode, sum(a * b for a, b in zip(v, c, strict=True))


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
    # Set when the satellite plays answers as they are written: a handler that can stream (the
    # LLM) passes its text deltas here and marks the reply streamed.
    on_text: Callable[[str], Awaitable[None]] | None = None


@dataclass
class Reply:
    text: str
    continue_conversation: bool = False
    handler: str = ""
    # The text already went to Request.on_text.
    streamed: bool = False


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
        self._modes = _modes(spec.get("modes", DEFAULT_MODES))
        self._classifier = (
            Classifier(spec["classifier"], self._modes) if spec.get("classifier") else None
        )
        self._tz = tz

    def _system_message(self, req: Request) -> str:
        # The same for every turn of a satellite: llama.cpp, vLLM, Ollama reuse the computed
        # prompt (system + history) only while its start does not change, so the time and the
        # mode's instruction go into the last message instead.
        return (
            f"{self._system}\nВ начале сообщения пользователя в квадратных скобках — текущие "
            f"дата и время, а иногда и указание, как ответить; дату и время называй, только "
            f"если спросят. Колонка: {req.satellite}."
        )

    async def _mode(self, req: Request) -> Mode | None:
        """The regular expressions first (no cost), then the classifier."""
        for m in self._modes:
            if m.match is not None and m.match.search(req.text):
                _LOG.info("%s: llm mode %s (match)", req.satellite, m.name)
                return m
        if self._classifier is None:
            return None
        mode, similarity = await self._classifier.classify(req.text)
        if mode is not None:
            _LOG.info("%s: llm mode %s (classifier, %.2f)", req.satellite, mode.name, similarity)
        return mode

    def _user_message(self, req: Request, mode: Mode | None) -> str:
        v = time_vars(now_in(self._tz))
        head = f"{v['weekday']}, {v['date']} {v['year']}, {v['time']}"
        if mode is not None:
            head += f"; {mode.prompt}"
        return f"[{head}]\n{req.text}"

    async def handle(self, req: Request, conv: Conversation) -> Reply | None:
        history = conv.messages[-2 * self._history :] if self._history > 0 else []
        mode = await self._mode(req)
        messages = [
            {"role": "system", "content": self._system_message(req)},
            *history,
            {"role": "user", "content": self._user_message(req, mode)},
        ]
        params = self._params
        if mode is not None:
            params = {**params, **mode.params}
        if req.on_text is None:
            answer = speakable(await self._client.chat(self._model, messages, **params))
            streamed = False
        else:
            answer, streamed = await self._stream(messages, req.on_text, params)
        if not answer:
            return None
        ask = self._continue_on_question and answer.rstrip().endswith("?")
        return Reply(answer, ask, self.name, streamed)

    async def _stream(
        self,
        messages: list[dict[str, str]],
        on_text: Callable[[str], Awaitable[None]],
        params: dict[str, Any],
    ) -> tuple[str, bool]:
        parts: list[str] = []
        try:
            async for delta in self._client.chat_stream(self._model, messages, **params):
                parts.append(delta)
                await on_text(delta)
        except Exception as e:  # noqa: BLE001
            if not parts:
                raise
            # Part of the answer is already playing: keep it rather than add another answer.
            _LOG.warning("llm stream broke off after %d chars: %s", len("".join(parts)), e)
        return speakable("".join(parts)), bool(parts)


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
