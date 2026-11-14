"""Sentence captions: one caption per sentence, timed against the pauses."""

import math
from dataclasses import dataclass

import numpy as np

from narrate.measure import pauses, speech_span
from narrate.text import sentences, syllables

CAPTION_TOP_DB = 30.0
CAPTION_PAUSE_MIN = 0.15
SNAP_WINDOW = 1.5  # seconds a boundary may move to reach a pause


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
    """Place each boundary on a nearby pause midpoint, or leave it at its target.

    Monotonic DP: a snap (|m - target| <= SNAP_WINDOW) costs the distance, no
    pause costs SNAP_WINDOW, and snapped pauses strictly increase across boundaries.
    """
    n, m = len(targets), len(mids)
    # f[k][j]: best cost placing the first k boundaries using only the first j pauses
    f = [[0.0] * (m + 1)] + [[SNAP_WINDOW * k] + [0.0] * m for k in range(1, n + 1)]
    for k in range(1, n + 1):
        for j in range(1, m + 1):
            best = min(f[k][j - 1], f[k - 1][j] + SNAP_WINDOW)
            d = abs(mids[j - 1] - targets[k - 1])
            if d <= SNAP_WINDOW:
                best = min(best, f[k - 1][j - 1] + d)
            f[k][j] = best
    out = list(targets)
    k, j = n, m
    while k > 0:
        d = abs(mids[j - 1] - targets[k - 1]) if j > 0 else math.inf
        if j > 0 and d <= SNAP_WINDOW and f[k][j] == f[k - 1][j - 1] + d:
            out[k - 1] = mids[j - 1]
            k, j = k - 1, j - 1
        elif j > 0 and f[k][j] == f[k][j - 1]:
            j -= 1
        else:
            k -= 1
    for i in range(1, n):
        out[i] = max(out[i], out[i - 1])
    return out


def sentence_captions(text: str, audio: np.ndarray, sr: int) -> list[Caption]:
    """Captions whose boundaries snap to nearby pauses, else sit at syllable shares.

    Each boundary's target is its cumulative syllable share of the speech span;
    it snaps to a pause midpoint within `SNAP_WINDOW` seconds when one fits the
    monotonic best assignment. With no pauses (or no speech) this is `share_captions`.
    """
    duration = len(audio) / sr
    sents = sentences(text)
    if len(sents) < 2:
        return _build(sents, [], duration)
    s0, s1 = speech_span(audio, sr)
    mids = [(a + b) / 2 for a, b in pauses(audio, sr, min_secs=CAPTION_PAUSE_MIN, top_db=CAPTION_TOP_DB)]
    if s1 <= s0 or not mids:
        return share_captions(text, duration)
    targets = [s0 + (s1 - s0) * f for f in _cumulative_fractions(sents)]
    return _build(sents, _match(targets, mids), duration)
