"""Deterministic fake backend and checker so the pipeline can be tested without a GPU.

A `FakeWorld` scripts, per spoken text and in generation order, what each take
sounds like (its pace) and what the checker hears. Takes are fingerprinted by the
SHA-1 of their float32 bytes, so the checker recognises them after a FLOAT WAV
round trip.
"""

from __future__ import annotations

import hashlib

import numpy as np

from narrate.text import sentences, syllables

SAMPLE_RATE = 24000
EDGE_SILENCE = 0.3
WORD_GAP = 0.12
SENTENCE_GAP = 0.5
DEFAULT_PACE = 4.5
# librosa-style framing (2048-sample frames) makes the measured span reach past a
# tone's true ends; this much of the target span is taken off so pace round-trips.
MEASURE_EXTENSION = 0.09
MAX_GAP_FRACTION = 0.6
AMPLITUDE = 0.3
FADE = 0.005
BASE_HZ = 180.0


def _fingerprint(audio: np.ndarray) -> str:
    # `+ 0.0` turns -0.0 into 0.0: read_wav's channel mean does the same to a round-tripped take
    return hashlib.sha1((np.asarray(audio, dtype=np.float32) + np.float32(0.0)).tobytes()).hexdigest()


class FakeWorld:
    """Shared script and event log for a `FakeBackend` and `FakeChecker`."""

    def __init__(self) -> None:
        self.events: list[str] = []
        self.calls: list[list[str]] = []
        self._scripts: dict[str, tuple[list[str] | None, list[float] | None]] = {}
        self._takes: dict[str, int] = {}
        self._heard: dict[str, str] = {}

    def script(self, spoken: str, heard: list[str] | None = None, pace: list[float] | None = None) -> None:
        """Per-take heard text and pace for `spoken`, in generation order; the last value repeats."""
        self._scripts[spoken] = (heard, pace)

    def next_take(self, spoken: str) -> tuple[int, str, float]:
        """(take number from 1, heard, pace) for the next take of `spoken`."""
        n = self._takes.get(spoken, 0)
        self._takes[spoken] = n + 1
        heard, pace = self._scripts.get(spoken, (None, None))
        h = heard[min(n, len(heard) - 1)] if heard else spoken
        p = pace[min(n, len(pace) - 1)] if pace else DEFAULT_PACE
        return n + 1, h, p

    def register(self, audio: np.ndarray, heard: str) -> None:
        """Remember what the checker should hear for this exact audio."""
        self._heard[_fingerprint(audio)] = heard

    def heard(self, audio: np.ndarray) -> str:
        """What was registered for this audio, or an empty string."""
        return self._heard.get(_fingerprint(audio), "")


_DEFAULT = FakeWorld()


def default_world() -> FakeWorld:
    """The world the registry binds fakes to; tests replace `_DEFAULT`."""
    return _DEFAULT


def _burst(secs: float, hz: float) -> np.ndarray:
    n = max(1, round(secs * SAMPLE_RATE))
    t = np.arange(n) / SAMPLE_RATE
    y = AMPLITUDE * np.sin(2 * np.pi * hz * t)
    fade = min(n // 2, round(FADE * SAMPLE_RATE))
    if fade:
        ramp = np.linspace(0.0, 1.0, fade)
        y[:fade] *= ramp
        y[n - fade :] *= ramp[::-1]
    return y


def _render(spoken: str, pace: float, hz: float) -> np.ndarray:
    span = max(0.1, syllables(spoken) / pace - MEASURE_EXTENSION)
    words_by_sentence = [s.split() for s in sentences(spoken)] or [spoken.split()]
    weights = [[max(1, syllables(w)) for w in ws] for ws in words_by_sentence]
    gaps = sum(len(w) - 1 for w in weights) * WORD_GAP + (len(weights) - 1) * SENTENCE_GAP
    scale = min(1.0, MAX_GAP_FRACTION * span / gaps) if gaps else 1.0
    speech = span - gaps * scale
    total = sum(sum(w) for w in weights)
    pieces: list[np.ndarray] = [np.zeros(round(EDGE_SILENCE * SAMPLE_RATE))]
    for si, ws in enumerate(weights):
        for wi, weight in enumerate(ws):
            pieces.append(_burst(speech * weight / total, hz * (1 + 0.05 * (wi % 5))))
            if wi < len(ws) - 1:
                pieces.append(np.zeros(round(WORD_GAP * scale * SAMPLE_RATE)))
        if si < len(weights) - 1:
            pieces.append(np.zeros(round(SENTENCE_GAP * scale * SAMPLE_RATE)))
    pieces.append(np.zeros(round(EDGE_SILENCE * SAMPLE_RATE)))
    return np.concatenate(pieces).astype(np.float32)


class FakeBackend:
    """Synthesises tone bursts shaped like speech, per the world's script."""

    sample_rate = SAMPLE_RATE

    def __init__(self, world: FakeWorld) -> None:
        self.world = world

    def synthesize(self, texts: list[str], reference, seed: int) -> list[np.ndarray]:
        """One clip per text; each take is distinct, and its pace and heard text follow the script."""
        self.world.events.append("synthesize")
        self.world.calls.append(list(texts))
        out = []
        for row, text in enumerate(texts):
            take, heard, pace = self.world.next_take(text)
            audio = _render(text, pace, BASE_HZ + 7.0 * (seed % 40) + 11.0 * row + 3.0 * take)
            self.world.register(audio, heard)
            out.append(audio)
        return out

    def close(self) -> None:
        """Log the release; nothing is held."""
        self.world.events.append("close-backend")


class FakeChecker:
    """Hears whatever the world registered for the audio."""

    name = "fake"

    def __init__(self, world: FakeWorld) -> None:
        self.world = world

    def transcribe(self, audio: np.ndarray, sr: int) -> str:
        """The scripted heard text for this exact take."""
        self.world.events.append("transcribe")
        return self.world.heard(audio)

    def close(self) -> None:
        """Log the release; nothing is held."""
        self.world.events.append("close-checker")
