"""LLM text deltas -> phrases to speak one by one, so the first one plays while the model writes
the rest."""

from __future__ import annotations

import re

# A phrase ends at . ! ? … ; followed by whitespace and a word that does not start in lower case
# (not "3.5", not "т.е. немного"), or at a line break. The word must have arrived: until then the
# end is not known.
_END = re.compile(r"(?<=[.!?…;])\s+(?=\S)|\n+")
# A first phrase this long without an end is cut at its last comma.
_LONG = 160


class PhraseSplitter:
    def __init__(self, min_chars: int = 20) -> None:
        self._min = min_chars
        self._buf = ""
        self._first = True

    def feed(self, delta: str) -> list[str]:
        self._buf += delta
        out: list[str] = []
        while True:
            phrase = self._take()
            if phrase is None:
                break
            out.append(phrase)
        return out

    def flush(self) -> list[str]:
        rest = self._buf.strip()
        self._buf = ""
        return [rest] if rest else []

    def _take(self) -> str | None:
        # The first ending past min_chars: short phrases ("Да.") go out with the next one.
        for m in _END.finditer(self._buf):
            if "\n" not in m.group() and self._buf[m.end()].islower():
                continue
            if len(self._buf[: m.start()].strip()) >= self._min:
                phrase, self._buf = self._buf[: m.start()].strip(), self._buf[m.end() :]
                self._first = False
                return phrase
        if self._first and len(self._buf) > _LONG:
            cut = self._buf.rfind(",", 0, _LONG)
            if cut >= self._min:
                phrase, self._buf = self._buf[: cut + 1].strip(), self._buf[cut + 1 :]
                self._first = False
                return phrase
        return None
