"""Choosing the best take of a line: word accuracy, punctuation, then pace."""

from collections.abc import Sequence
from dataclasses import dataclass

import numpy as np

from narrate.measure import pace as measure_pace
from narrate.profile import Pace
from narrate.text import cer as text_cer
from narrate.text import punct_distance

MAX_CER = 0.04


@dataclass(frozen=True)
class TakeResult:
    """One take's measurements."""

    n: int
    heard: str
    cer: float
    punct: int
    pace: float
    duration: float


@dataclass(frozen=True)
class Choice:
    """The chosen take, with its status (`new` or `flagged`) and flags."""

    take: TakeResult
    status: str
    flags: tuple[str, ...]


def evaluate(n: int, keys: Sequence[str], spoken: str, heard: str, audio: np.ndarray, sr: int) -> TakeResult:
    """Score a take against the best of the answer keys."""
    return TakeResult(
        n=n,
        heard=heard,
        cer=min(text_cer(k, heard) for k in keys),
        punct=min(punct_distance(k, heard) for k in keys),
        pace=measure_pace(audio, sr, spoken),
        duration=len(audio) / sr,
    )


def choose(kind: str, takes: Sequence[TakeResult], pace: Pace, max_cer: float = MAX_CER) -> Choice:
    """Pick the best take for a `line` or `passage`."""

    def order(t: TakeResult) -> tuple:
        return (round(t.cer, 4), t.punct, abs(t.pace - pace.target), t.n)

    def words_ok(t: TakeResult) -> bool:
        return t.cer < max_cer

    good = [t for t in takes if words_ok(t)]
    if kind == "line":
        if not good:
            return Choice(min(takes, key=order), "flagged", ("words",))
        quick = [t for t in good if t.pace <= pace.max]
        return Choice(min(quick or good, key=order), "new", ())
    passing = [t for t in good if t.pace <= pace.max]
    if passing:
        return Choice(min(passing, key=order), "new", ())
    best = min(takes, key=lambda t: (not words_ok(t), t.pace > pace.max, *order(t)))
    return Choice(best, "flagged", ("pace",) if good else ("words",))
