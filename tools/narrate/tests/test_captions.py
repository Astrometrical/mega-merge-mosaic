import numpy as np
import pytest

from narrate.captions import Caption, sentence_captions, share_captions
from narrate.text import syllables

SR = 24000


def tone(secs):
    t = np.arange(round(secs * SR)) / SR
    return (0.3 * np.sin(2 * np.pi * 220 * t)).astype(np.float32)


def silence(secs):
    return np.zeros(round(secs * SR), dtype=np.float32)


THREE = "Ba ba ba ba ba ba ba ba. Da da da da da da da da. Ka ka ka ka ka ka ka ka."


def assert_contiguous(caps, duration):
    assert caps[0].start == 0.0
    assert caps[-1].end == pytest.approx(duration)
    for a, b in zip(caps, caps[1:], strict=False):
        assert a.end == b.start


def test_caption_to_json():
    assert Caption("Hi.", 0.12345, 1.0).to_json() == {"text": "Hi.", "start": 0.123, "end": 1.0}


def test_share_captions():
    text = "One two three. Four five six seven eight nine."
    caps = share_captions(text, 9.0)
    assert [c.text for c in caps] == ["One two three.", "Four five six seven eight nine."]
    assert caps[0].end == pytest.approx(9.0 * syllables("One two three.") / syllables(text))
    assert caps[0].end == pytest.approx(2.769, abs=0.001)
    assert_contiguous(caps, 9.0)


def test_pause_matching():
    y = np.concatenate(
        [silence(0.3), tone(1.2), silence(0.3), tone(1.2), silence(0.6), tone(2.4)] + [silence(0.5), tone(2.5)]
    )
    caps = sentence_captions(THREE, y, SR)
    assert len(caps) == 3
    assert caps[0].end == pytest.approx(3.3, abs=0.08)
    assert caps[1].end == pytest.approx(6.25, abs=0.08)
    assert_contiguous(caps, len(y) / SR)


def test_too_few_pauses_falls_back():
    y = tone(9.0)
    assert sentence_captions(THREE, y, SR) == share_captions(THREE, 9.0)


def test_silence_falls_back():
    y = silence(3.0)
    assert sentence_captions(THREE, y, SR) == share_captions(THREE, 3.0)


def test_single_sentence():
    y = np.concatenate([silence(0.5), tone(2.0)])
    caps = sentence_captions("Just one sentence here.", y, SR)
    assert len(caps) == 1
    assert (caps[0].start, caps[0].end) == (0.0, pytest.approx(2.5))
