"""Sentence captions: one caption per sentence, timed against the pauses."""

from dataclasses import dataclass

import numpy as np

from narrate.measure import pauses, speech_span
from narrate.text import sentences, syllables


@dataclass(frozen=True)
class Caption:
    """A caption line with its start and end time in seconds."""

    text: str
    start: float
    end: float

    def to_json(self) -> dict:
        """JSON form, with times rounded to milliseconds."""
        return {"text": self.text, "start": round(self.start, 3), "end": round(self.end, 3)}


def _build(sents: list[str], bounds: list[float], duration: float) -> list[Caption]:
    edges = [0.0, *bounds, duration]
    return [Caption(s, edges[i], edges[i + 1]) for i, s in enumerate(sents)]


def _cumulative_fractions(sents: list[str]) -> list[float]:
    """Cumulative syllable share after each sentence except the last."""
    counts = [syllables(s) for s in sents]
    total = sum(counts) or 1
    out, run = [], 0
    for c in counts[:-1]:
        run += c
        out.append(run / total)
    return out


def share_captions(text: str, duration: float) -> list[Caption]:
    """One caption per sentence, split by cumulative syllable share of `duration`."""
    sents = sentences(text)
    return _build(sents, [duration * f for f in _cumulative_fractions(sents)], duration)


def _match(targets: list[float], mids: list[float]) -> list[float]:
    """Increasing choice of one midpoint per target minimising total distance."""
    n, m = len(targets), len(mids)
    inf = float("inf")
    cost = [[inf] * m for _ in range(n)]
    back = [[0] * m for _ in range(n)]
    for k in range(n):
        best, arg = inf, 0  # running min of the previous row over pauses before j
        for j in range(m):
            if k == 0:
                prev, parent = 0.0, 0
            else:
                if j > 0 and cost[k - 1][j - 1] < best:
                    best, arg = cost[k - 1][j - 1], j - 1
                prev, parent = best, arg
            if prev < inf:
                cost[k][j] = prev + abs(mids[j] - targets[k])
                back[k][j] = parent
    j = min(range(m), key=lambda x: cost[n - 1][x])
    chosen = []
    for k in range(n - 1, -1, -1):
        chosen.append(mids[j])
        j = back[k][j]
    return chosen[::-1]


def sentence_captions(text: str, audio: np.ndarray, sr: int) -> list[Caption]:
    """Captions timed by matching syllable-share boundaries to the nearest pauses."""
    duration = len(audio) / sr
    sents = sentences(text)
    if len(sents) < 2:
        return _build(sents, [], duration)
    s0, s1 = speech_span(audio, sr)
    mids = [(a + b) / 2 for a, b in pauses(audio, sr)]
    if s1 <= s0 or len(mids) < len(sents) - 1:
        return share_captions(text, duration)
    targets = [s0 + (s1 - s0) * f for f in _cumulative_fractions(sents)]
    return _build(sents, _match(targets, mids), duration)
