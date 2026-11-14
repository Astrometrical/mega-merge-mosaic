"""Silence analysis: speech span, pauses and pace.

Mirrors `librosa.effects.split` (librosa 0.11) so the numbers stay on the scale
the pace limits were measured on, without depending on librosa.
"""

import math

import numpy as np

from narrate.text import syllables

TOP_DB = 35.0
FRAME = 2048
HOP = 512
AMIN = 1e-5
PAUSE_MIN = 0.25
_MIN_SPAN = 0.05


def _intervals(audio: np.ndarray, top_db: float) -> list[tuple[int, int]]:
    """Non-silent [start, end) sample intervals, as `librosa.effects.split`."""
    y = np.asarray(audio, dtype=np.float64)
    n = len(y)
    if n == 0:
        return []
    padded = np.pad(y, FRAME // 2, mode="constant")
    n_frames = 1 + (len(padded) - FRAME) // HOP
    idx = np.arange(FRAME)[None, :] + HOP * np.arange(n_frames)[:, None]
    rms = np.sqrt(np.mean(np.abs(padded[idx]) ** 2, axis=1))
    db = 20 * np.log10(np.maximum(AMIN, rms)) - 20 * np.log10(max(AMIN, rms.max()))
    if rms.max() <= AMIN:  # librosa would call an all-zero clip entirely non-silent
        return []
    non_silent = db > -top_db
    edges = np.flatnonzero(np.diff(non_silent.astype(int))) + 1
    if non_silent[0]:
        edges = np.concatenate([[0], edges])
    if non_silent[-1]:
        edges = np.concatenate([edges, [len(non_silent)]])
    samples = np.minimum(edges * HOP, n)
    return [(int(a), int(b)) for a, b in samples.reshape(-1, 2)]


def speech_span(audio: np.ndarray, sr: int, top_db: float = TOP_DB) -> tuple[float, float]:
    """(start, end) of the speech in seconds; (0.0, 0.0) for silence."""
    iv = _intervals(audio, top_db)
    if not iv:
        return (0.0, 0.0)
    return (iv[0][0] / sr, iv[-1][1] / sr)


def pauses(
    audio: np.ndarray, sr: int, min_secs: float = PAUSE_MIN, top_db: float = TOP_DB
) -> list[tuple[float, float]]:
    """Silences inside the speech span of at least `min_secs`, as (start, end) seconds."""
    iv = _intervals(audio, top_db)
    gaps = ((a[1] / sr, b[0] / sr) for a, b in zip(iv, iv[1:], strict=False))
    return [(s, e) for s, e in gaps if e - s >= min_secs]


def pace(audio: np.ndarray, sr: int, spoken: str) -> float:
    """Syllables per second over the speech span; `math.inf` if the span is under 0.05 s."""
    start, end = speech_span(audio, sr)
    if end - start < _MIN_SPAN:
        return math.inf
    return syllables(spoken) / (end - start)
