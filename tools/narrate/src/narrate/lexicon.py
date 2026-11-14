"""Pronunciation lists: ordered regex respellings applied before synthesis."""

from __future__ import annotations

import itertools
import json
import re
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from narrate import NarrateError


class LexiconError(NarrateError):
    """A pronunciation list is missing or malformed."""


@dataclass(frozen=True)
class Entry:
    """One respelling: `match` is replaced by `say`."""

    match: re.Pattern[str]
    say: str
    why: str


def _load_one(path: Path) -> list[Entry]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except OSError as e:
        raise LexiconError(f"{path}: cannot read pronunciation list ({e})") from e
    except ValueError as e:
        raise LexiconError(f"{path}: invalid JSON ({e})") from e
    if not isinstance(data, dict) or not isinstance(data.get("entries"), list):
        raise LexiconError(f"{path}: expected an object with an 'entries' list")
    out = []
    for i, raw in enumerate(data["entries"], start=1):
        where = f"{path}: entry {i}"
        if not isinstance(raw, dict):
            raise LexiconError(f"{where} must be an object")
        for key in ("match", "say"):
            if not isinstance(raw.get(key), str):
                raise LexiconError(f"{where}: '{key}' must be a string")
        why = raw.get("why", "")
        if not isinstance(why, str):
            raise LexiconError(f"{where}: 'why' must be a string")
        try:
            pattern = re.compile(raw["match"])
        except re.error as e:
            raise LexiconError(f"{where}: bad regex {raw['match']!r} ({e})") from e
        out.append(Entry(pattern, raw["say"], why))
    return out


@dataclass(frozen=True)
class Lexicon:
    """An ordered list of respellings."""

    entries: tuple[Entry, ...]

    @classmethod
    def load(cls, paths: Sequence[Path]) -> Lexicon:
        """Load lists, concatenating them in the given order."""
        entries: list[Entry] = []
        for p in paths:
            entries.extend(_load_one(Path(p)))
        return cls(tuple(entries))

    def apply(self, text: str) -> str:
        """Apply every entry once, in order."""
        for e in self.entries:
            text = e.match.sub(e.say, text)
        return text

    def variants(self, text: str, limit: int = 64) -> list[str]:
        """Acceptable transcriptions of `text` once spoken (the word-check key).

        Whisper writes some respellings as the intended word and others as
        spoken, so every subset of the entries that match is applied in list
        order. `text` comes first. Over `limit` subsets, only the unrespelled
        and fully respelled forms are returned.
        """
        hits = [e for e in self.entries if e.match.search(text)]
        full = self.apply(text)
        seen = {text: None, full: None}
        if 2 ** len(hits) > limit:
            return list(seen)
        for mask in itertools.product((False, True), repeat=len(hits)):
            t = text
            for e, on in zip(hits, mask, strict=True):
                if on:
                    t = e.match.sub(e.say, t)
            seen.setdefault(t)
        return list(seen)
