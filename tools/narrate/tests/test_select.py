import math

import numpy as np

from narrate.profile import Pace
from narrate.select import Choice, TakeResult, choose, evaluate

P = Pace(max=5.3, target=4.8)


def T(n, cer=0.0, punct=0, pace=4.8):
    return TakeResult(n, "h", cer, punct, pace, 3.0)


def test_lowest_error_then_punctuation_then_pace_then_take():
    assert choose("line", [T(1, cer=0.02), T(2, cer=0.0, punct=5), T(3, cer=0.0, punct=1)], P).take.n == 3
    assert choose("line", [T(1, pace=5.2), T(2, pace=4.7)], P).take.n == 2
    assert choose("line", [T(2), T(1)], P).take.n == 1


def test_line_prefers_pace_but_never_fails_on_it():
    assert choose("line", [T(1, pace=6.0), T(2, cer=0.03, pace=4.6)], P).take.n == 2
    c = choose("line", [T(1, pace=6.4), T(2, pace=6.0)], P)
    assert (c.status, c.flags, c.take.n) == ("new", (), 2)


def test_passage_fails_on_pace():
    c = choose("passage", [T(1, pace=5.6), T(2, pace=5.4)], P)
    assert (c.status, c.flags, c.take.n) == ("flagged", ("pace",), 2)


def test_garbled_everywhere_flags_words():
    c = choose("line", [T(1, cer=0.3), T(2, cer=1.0)], P)
    assert (c.status, c.flags, c.take.n) == ("flagged", ("words",), 1)


def test_passage_passing_take_is_new():
    c = choose("passage", [T(1, pace=5.6), T(2, pace=5.0)], P)
    assert isinstance(c, Choice)
    assert (c.status, c.flags, c.take.n) == ("new", (), 2)


def test_passage_garbled_flags_words():
    c = choose("passage", [T(1, cer=0.5), T(2, cer=0.6)], P)
    assert (c.status, c.flags, c.take.n) == ("flagged", ("words",), 1)


def test_infinite_pace_sorts_last_and_does_not_crash():
    c = choose("line", [T(1, pace=math.inf), T(2, pace=5.0)], P)
    assert c.take.n == 2
    c = choose("passage", [T(1, pace=math.inf)], P)
    assert (c.status, c.flags) == ("flagged", ("pace",))


def test_evaluate_uses_the_best_key():
    keys = [
        "Data courtesy of Adam Sbarra.",
        "Dahta courtesy of Adam Sbarra.",
        "Data courtesy of Adam Zabara.",
        "Dahta courtesy of Adam Zabara.",
    ]
    sr = 24000
    t = np.arange(sr * 2) / sr
    audio = 0.3 * np.sin(2 * np.pi * 220 * t)
    r = evaluate(3, keys, "Dahta courtesy of Adam Zabara.", "Data courtesy of Adam Zabara.", audio, sr)
    assert r.n == 3
    assert r.cer == 0.0
    assert r.punct == 0
    assert r.duration == 2.0
    assert r.pace > 0


def test_evaluate_empty_heard_fails_words():
    audio = np.zeros(24000)
    r = evaluate(1, ["Hello there."], "Hello there.", "", audio, 24000)
    assert r.cer >= 1.0
    assert r.pace == math.inf
    assert choose("line", [r], P).flags == ("words",)
