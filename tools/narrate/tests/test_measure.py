import math

import numpy as np
import pytest

from narrate.measure import pace, pauses, speech_span

SR = 24000


def tone(secs):
    t = np.arange(int(secs * SR)) / SR
    return (0.3 * np.sin(2 * np.pi * 220 * t)).astype(np.float32)


def silence(secs):
    return np.zeros(int(secs * SR), dtype=np.float32)


def test_speech_span():
    y = np.concatenate([silence(0.5), tone(2.0), silence(0.5)])
    start, end = speech_span(y, SR)
    assert start == pytest.approx(0.5, abs=0.1)
    assert end == pytest.approx(2.5, abs=0.1)


def test_pauses():
    y = np.concatenate([tone(1.0), silence(0.6), tone(1.0)])
    found = pauses(y, SR)
    assert len(found) == 1
    assert found[0][0] == pytest.approx(1.0, abs=0.1)
    assert found[0][1] == pytest.approx(1.6, abs=0.1)
    short = np.concatenate([tone(1.0), silence(0.1), tone(1.0)])
    assert pauses(short, SR) == []


def test_pace():
    assert pace(tone(2.6), SR, "The North America Nebula is enormous.") == pytest.approx(5.0, rel=0.03)


def test_pace_of_silence_is_inf():
    assert pace(silence(1.0), SR, "Hello there.") == math.inf
    assert speech_span(silence(1.0), SR) == (0.0, 0.0)
