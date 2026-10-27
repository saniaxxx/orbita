#!/usr/bin/env python3
"""Voice-assistant questions through an LLM the way orbita asks them: which model to run.

Every question goes through orbita's own path: its system prompt, the date and time in the last
message, the dialog history, a streamed answer cut into phrases (orbita/sentences.py). For each
answer: Latin letters inside Russian words, markup and emoji in the raw model text, whether the
fact is right, time to the first text and to the first phrase (what the user waits for, before
synthesis), the whole answer. Grammar is for a human: the report has every answer.

    orbita-venv/bin/python llm_eval.py --base-url http://192.168.250.102:8080/v1 \\
        --model ornith-1.5-9b [--no-think] [--runs 2] [--out report.md]

The API key comes from OPENAI_API_KEY. --no-think sends chat_template_kwargs.enable_thinking
false (orbita's config does, for speed).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import re
import statistics
import time
from dataclasses import dataclass, field

from orbita.config import parse
from orbita.intents import Request, build_brain, now_in, time_vars
from orbita.sentences import PhraseSplitter


@dataclass
class Case:
    turns: list[str]
    # Regular expression the last answer must match (case-insensitive); None: read it yourself.
    expect: str | None = None
    note: str = ""


def _weekday() -> str:
    return time_vars(now_in(""))["weekday"]


CASES = [
    # Numbers are checked too: models name the right mountain with a wrong height.
    Case(
        ["Какая самая высокая гора в России?"],
        r"эльбрус.*(5\s?642|пять тысяч шестьсот сорок два)",
    ),
    Case(
        ["Какая длина реки Волги?"],
        r"3\s?530|три тысячи пятьсот тридцать|3\s?690|три тысячи шестьсот девяносто",
    ),
    Case(["Какая столица Австралии?"], r"канберр"),
    Case(["Сколько планет в Солнечной системе?"], r"\b8\b|восемь"),
    Case(["Кто написал «Войну и мир»?"], r"толст"),
    Case(["При какой температуре кипит вода?"], r"\b100\b|\bсто\b"),
    Case(["Какая самая длинная река в Европе?"], r"волг"),
    Case(["Сколько дней в високосном году?"], r"366|триста шестьдесят шест"),
    Case(["В каком году Гагарин полетел в космос?"], r"1961|шестьдесят перв"),
    Case(["Сколько будет семнадцать умножить на три?"], r"\b51\b|пятьдесят од"),
    Case(
        ["Сколько будет двести сорок семь плюс триста восемьдесят девять?"],
        r"636|шестьсот тридцать шест",
    ),
    Case(["Сколько минут в трёх часах?"], r"180|сто восемьдесят"),
    Case(["Кто автор «Евгения Онегина»?"], r"пушкин"),
    Case(["Какой океан самый большой?"], r"тих"),
    Case(["Какой химический элемент обозначается буквой O?"], r"кислород"),
    Case(["Сколько сейчас времени?"], r"\d{1,2}[:.]\d\d|час|минут", "время из сообщения"),
    Case(["Какой сегодня день недели?"], _weekday(), "день из сообщения"),
    Case(["Какая самая высокая гора в России?", "А в мире?"], r"эверест|джомолунгм", "диалог"),
    Case(
        ["Кто написал «Мастера и Маргариту»?", "А в каком году он умер?"], r"1940|сороков", "диалог"
    ),
    Case(["Какая столица Японии?", "А сколько там примерно жителей?"], None, "диалог"),
    Case(["Расскажи коротко, почему небо голубое."]),
    Case(["Расскажи в трёх предложениях про Луну."]),
    Case(["Объясни простыми словами, что такое инфляция."]),
    Case(["Посоветуй, что приготовить на ужин из курицы и риса."]),
    Case(["Как подключить колонку к Wi-Fi?"], None, "латинские термины должны остаться"),
    Case(["Что такое Zigbee?"], None, "латинские термины должны остаться"),
    Case(["Придумай короткое поздравление с днём рождения для мамы."]),
    Case(["Какая погода будет завтра?"], None, "не должна выдумывать прогноз"),
    Case(["Включи свет на кухне."], None, "не должна делать вид, что включила"),
    Case(["Мне скучно."], None, "может спросить в ответ"),
    Case(["Переведи на английский: доброе утро."], r"good morning", "латиница уместна"),
    Case(["Сколько стоит биткоин?"], None, "не должна выдумывать курс"),
]

MIXED = re.compile(r"\b(?=\w*[а-яёА-ЯЁ])(?=\w*[a-zA-Z])\w+\b")
MARKUP = re.compile(r"[*_`#]|^\s*[-•]\s|^\s*\d+\.\s", re.MULTILINE)
EMOJI = re.compile("[\U0001f300-\U0001faff☀-➿]")
# Letters of other alphabets: Chinese, Greek... (the TTS cannot read them).
FOREIGN = re.compile(r"[^\W\d_a-zA-Zа-яёА-ЯЁ]+")


@dataclass
class Result:
    turns: list[str]
    note: str
    raw: str = ""
    spoken: str = ""
    ok: bool | None = None
    mixed: list[str] = field(default_factory=list)
    foreign: list[str] = field(default_factory=list)
    markup: bool = False
    emoji: bool = False
    first_text: float | None = None
    first_phrase: float | None = None
    total: float = 0.0
    error: str = ""


class Turn:
    """One question as orbita streams it: the raw text and when its first text and phrase came."""

    def __init__(self) -> None:
        self.parts: list[str] = []
        self.first_text: float | None = None
        self.first_phrase: float | None = None
        self._splitter = PhraseSplitter()
        self._t0 = time.monotonic()

    async def on_text(self, delta: str) -> None:
        now = time.monotonic() - self._t0
        if self.first_text is None:
            self.first_text = now
        self.parts.append(delta)
        if self._splitter.feed(delta) and self.first_phrase is None:
            self.first_phrase = now

    def elapsed(self) -> float:
        return time.monotonic() - self._t0


async def ask_case(brain, case: Case, cid: str) -> Result:
    r = Result(case.turns, case.note)
    for text in case.turns:  # the earlier turns only build the dialog history
        turn = Turn()
        reply = await brain.answer(Request(text, "eval", cid, on_text=turn.on_text))
    r.total = turn.elapsed()
    r.raw = "".join(turn.parts)
    r.spoken = reply.text
    r.first_text = turn.first_text
    # A short answer is one phrase: it is spoken when the text ends.
    r.first_phrase = turn.first_phrase or (r.total if r.raw else None)
    if reply.handler != "openai":
        r.error = f"answered by {reply.handler}: {reply.text}"
    r.mixed = MIXED.findall(r.raw)
    r.foreign = FOREIGN.findall(r.raw)
    r.markup = bool(MARKUP.search(r.raw))
    r.emoji = bool(EMOJI.search(r.raw))
    if case.expect is not None:
        r.ok = bool(re.search(case.expect, r.spoken, re.IGNORECASE))
    return r


def report(model: str, runs: list[list[Result]]) -> str:
    flat = [r for run in runs for r in run]
    checked = [r for r in flat if r.ok is not None]
    phrase = [r.first_phrase for r in flat if r.first_phrase is not None]
    first = [r.first_text for r in flat if r.first_text is not None]

    def pct(n: int, d: int) -> str:
        return f"{n}/{d}" if d else "—"

    def q(xs: list[float], p: float) -> str:
        return f"{statistics.quantiles(xs, n=100)[int(p) - 1]:.2f}" if len(xs) > 1 else "—"

    n = len(flat)
    rows = [
        f"| Факты верны | {pct(sum(r.ok for r in checked), len(checked))} |",
        f"| Латиница в русских словах | {pct(sum(bool(r.mixed) for r in flat), n)} ответов |",
        f"| Чужие алфавиты (иероглифы…) | {pct(sum(bool(r.foreign) for r in flat), n)} ответов |",
        f"| Разметка / эмодзи | {pct(sum(r.markup for r in flat), len(flat))} / "
        f"{pct(sum(r.emoji for r in flat), len(flat))} |",
        f"| Ошибки | {sum(bool(r.error) for r in flat)} |",
        f"| До первого текста, медиана | {statistics.median(first):.2f} с |" if first else "",
        f"| До первой фразы, медиана / p90 | {statistics.median(phrase):.2f} / {q(phrase, 90)} с |"
        if phrase
        else "",
        f"| Ответ целиком, медиана | {statistics.median(r.total for r in flat):.2f} с |",
        f"| Длина ответа, медиана | {statistics.median(len(r.spoken) for r in flat):.0f} симв. |",
    ]
    lines = [
        f"# {model}",
        "",
        f"Вопросов: {len(CASES)}, прогонов: {len(runs)}, дата: {time.strftime('%Y-%m-%d %H:%M')}",
        "",
        "| Метрика | Значение |",
        "|---|---|",
        *[row for row in rows if row],
        "",
        "## Ответы",
        "",
    ]
    for r in runs[0]:
        mark = {True: "✅", False: "❌", None: "👁"}[r.ok]
        flags = []
        if r.mixed:
            flags.append("латиница: " + ", ".join(r.mixed))
        if r.foreign:
            flags.append("чужие буквы: " + ", ".join(r.foreign))
        if r.markup:
            flags.append("разметка")
        if r.emoji:
            flags.append("эмодзи")
        if r.error:
            flags.append(r.error)
        head = " → ".join(r.turns)
        timing = f"{r.first_phrase:.2f} с до фразы" if r.first_phrase is not None else "нет ответа"
        lines.append(f"- {mark} **{head}**" + (f" _({r.note})_" if r.note else ""))
        lines.append(f"  {r.spoken}")
        lines.append(f"  `{timing}`" + (" · " + "; ".join(flags) if flags else ""))
    return "\n".join(lines) + "\n"


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--no-think", action="store_true")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--out", help="markdown report (also <out>.json)")
    args = ap.parse_args()

    intent: dict = {"type": "openai", "model": args.model, "temperature": 0.4, "max_tokens": 200}
    if args.no_think:
        intent["extra_body"] = {"chat_template_kwargs": {"enable_thinking": False}}
    cfg = parse(
        {
            "satellites": [{"name": "eval", "host": "127.0.0.1"}],
            "openai": {"base_url": args.base_url, "timeout": 120},
            "intents": [intent],
        }
    )
    runs = []
    for n in range(args.runs):
        brain = build_brain(cfg, None, None)  # fresh history every run
        run = []
        for i, case in enumerate(CASES):
            res = await ask_case(brain, case, f"r{n}c{i}")
            run.append(res)
            ok = {True: "ok", False: "WRONG", None: "-"}[res.ok]
            fp = f"{res.first_phrase:.2f}" if res.first_phrase is not None else "-"
            print(f"run {n + 1} #{i + 1:2} {ok:5} {fp:>5} s  {res.spoken[:70]}", flush=True)
        runs.append(run)
    text = report(args.model, runs)
    print(text)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text)
        with open(args.out + ".json", "w") as f:
            json.dump([[r.__dict__ for r in run] for run in runs], f, ensure_ascii=False, indent=1)


if __name__ == "__main__":
    asyncio.run(main())
