"""Voice timers in Russian: "поставь таймер на пять минут", "отмени таймер", "сколько осталось".

The satellite gets the ESPHome timer events (a Yandex Station counts down on its clock and
rings when the timer finishes; "стоп" or the button silences it there).
"""

from __future__ import annotations

import re
from typing import Any

from .intents import Conversation, Handler, Reply, Request

UNITS = [
    (re.compile(r"^(час|часа|часов|ч)$"), 3600),
    (re.compile(r"^(минута|минуту|минуты|минут|мин)$"), 60),
    (re.compile(r"^(секунда|секунду|секунды|секунд|сек|с)$"), 1),
]
HALF_UNITS = {"полчаса": 1800, "полминуты": 30, "полсекунды": 1}

ONES = {
    "ноль": 0, "один": 1, "одна": 1, "одну": 1, "два": 2, "две": 2, "три": 3, "четыре": 4,
    "пять": 5, "шесть": 6, "семь": 7, "восемь": 8, "девять": 9, "десять": 10,
    "одиннадцать": 11, "двенадцать": 12, "тринадцать": 13, "четырнадцать": 14,
    "пятнадцать": 15, "шестнадцать": 16, "семнадцать": 17, "восемнадцать": 18,
    "девятнадцать": 19,
}  # fmt: skip
TENS = {
    "двадцать": 20, "тридцать": 30, "сорок": 40, "пятьдесят": 50, "шестьдесят": 60,
    "семьдесят": 70, "восемьдесят": 80, "девяносто": 90, "сто": 100,
}  # fmt: skip
FRACTIONS = {"полтора": 1.5, "полторы": 1.5}

START = re.compile(r"(таймер|засеки|засечь|напомни через|будильник через)")
CANCEL = re.compile(r"(отмени|отменить|выключи|удали|сбрось|останови|убери).*таймер")
STATUS = re.compile(r"(сколько|когда).*(осталось|таймер)|таймер.*осталось")


def _tokens(text: str) -> list[str]:
    text = text.lower().replace("ё", "е")
    # "5мин" -> "5 мин", "1,5" -> "1.5"
    text = re.sub(r"(\d)([а-я])", r"\1 \2", text)
    text = re.sub(r"(\d),(\d)", r"\1.\2", text)
    return re.findall(r"\d+(?:\.\d+)?|[а-я]+", text)


def parse_duration(text: str) -> int:
    """Seconds named in text ("1 час 20 минут", "две с половиной минуты"), 0 if none."""
    total = 0.0
    number: float | None = None
    words = _tokens(text)
    i = 0
    while i < len(words):
        w = words[i]
        if w in HALF_UNITS:
            total += HALF_UNITS[w]
            number = None
        elif re.fullmatch(r"\d+(?:\.\d+)?", w):
            number = (number or 0) + float(w) if number is not None and number >= 20 else float(w)
        elif w in TENS:
            number = TENS[w]
        elif w in ONES:
            number = (number or 0) + ONES[w] if number is not None and number >= 20 else ONES[w]
        elif w in FRACTIONS:
            number = FRACTIONS[w]
        elif w == "с" and i + 1 < len(words) and words[i + 1].startswith("половин"):
            number = (number or 0) + 0.5
            i += 1
        else:
            for pattern, seconds in UNITS:
                # A bare "с" is only a unit right after a number ("30 с").
                if pattern.match(w) and (w != "с" or number is not None):
                    n = 1.0 if number is None else number
                    # "минуту с половиной"
                    if (
                        i + 2 < len(words)
                        and words[i + 1] == "с"
                        and words[i + 2].startswith("половин")
                    ):
                        n += 0.5
                        i += 2
                    total += n * seconds
                    number = None
                    break
        i += 1
    return round(total)


def _plural(n: int, one: str, few: str, many: str) -> str:
    if n % 10 == 1 and n % 100 != 11:
        return one
    if 2 <= n % 10 <= 4 and not 12 <= n % 100 <= 14:
        return few
    return many


def say_duration(seconds: int, accusative: bool = False) -> str:
    h, rest = divmod(seconds, 3600)
    m, s = divmod(rest, 60)
    parts = []
    if h:
        parts.append(f"{h} {_plural(h, 'час', 'часа', 'часов')}")
    if m:
        one = "минуту" if accusative else "минута"
        parts.append(f"{m} {_plural(m, one, 'минуты', 'минут')}")
    if s or not parts:
        one = "секунду" if accusative else "секунда"
        parts.append(f"{s} {_plural(s, one, 'секунды', 'секунд')}")
    return " ".join(parts)


class TimersHandler(Handler):
    name = "timers"

    def __init__(self, spec: dict[str, Any]) -> None:
        self._max = int(spec.get("max_seconds", 24 * 3600))

    async def handle(self, req: Request, conv: Conversation) -> Reply | None:
        sat = req.timers
        if sat is None:
            return None
        text = req.text.lower().replace("ё", "е")
        if CANCEL.search(text):
            cancelled = sat.timer_cancel()
            if not cancelled:
                return Reply("Таймеров нет.", handler=self.name)
            if len(cancelled) == 1:
                return Reply("Таймер отменён.", handler=self.name)
            return Reply("Все таймеры отменены.", handler=self.name)
        if STATUS.search(text) and not parse_duration(text):
            timers = sorted(sat.timers.values(), key=lambda t: t.left())
            if not timers:
                return Reply("Таймеров нет.", handler=self.name)
            return Reply(f"Осталось {say_duration(timers[0].left())}.", handler=self.name)
        if START.search(text):
            seconds = parse_duration(text)
            if seconds <= 0:
                return Reply("На сколько поставить таймер?", True, self.name)
            if seconds > self._max:
                return Reply("Слишком долго для таймера.", handler=self.name)
            sat.timer_start(seconds)
            return Reply(f"Таймер на {say_duration(seconds, True)} запущен.", handler=self.name)
        # The answer to "На сколько поставить таймер?": just a duration.
        if conv.messages and conv.messages[-1].get("content") == "На сколько поставить таймер?":
            seconds = parse_duration(text)
            if 0 < seconds <= self._max:
                sat.timer_start(seconds)
                return Reply(f"Таймер на {say_duration(seconds, True)} запущен.", handler=self.name)
        return None
