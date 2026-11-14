import json
from pathlib import Path

import pytest

from narrate import NarrateError
from narrate.lock import Lock, lock_path

SHA = "ab" * 32


def _pin(lock: Lock, line="intro", key="k1", take=1, cer=0.0, pace=1.0):
    lock.pin(line, key, take, 2.5, "heard text", cer, pace, SHA)


def test_lock_path():
    assert lock_path(Path("a/ngc7000.narration.toml")) == Path("a/ngc7000.narration.lock.json")


def test_missing_lock_is_empty(root):
    lock = Lock.load(root / "x.narration.lock.json")
    assert lock.pins == {} and lock.rejected == {} and lock.notes == {}


def test_lock_round_trip(root):
    path = root / "x.narration.lock.json"
    lock = Lock.load(path)
    _pin(lock, "intro", "k1", 2, cer=0.05, pace=1.1)
    _pin(lock, "outro", "k2", 1, cer=float("nan"), pace=float("inf"))
    lock.reject("k1", 5)
    lock.reject("k1", 3)
    lock.note("intro", "slower")
    lock.save()

    raw = json.loads(path.read_text(encoding="utf-8"))
    assert list(raw)[0] == "version" and raw["version"] == 1
    assert raw["rejected"] == {"k1": [3, 5]}
    assert raw["lines"]["outro"]["cer"] is None

    back = Lock.load(path)
    assert back.pins["intro"] == lock.pins["intro"]
    assert back.pins["outro"].cer is None and back.pins["outro"].pace is None
    assert back.pins["intro"].approved_at.endswith("Z")
    assert back.pins["intro"].cer == 0.05
    assert back.rejected == {"k1": {3, 5}}
    assert back.notes == {"intro": "slower"}


def test_reject_unpins(root):
    lock = Lock.load(root / "l.json")
    _pin(lock, "a", "k1", 1)
    _pin(lock, "b", "k1", 2)
    lock.reject("k1", 1)
    assert "a" not in lock.pins and "b" in lock.pins
    assert lock.rejected == {"k1": {1}}


def test_pin_clears_rejection(root):
    lock = Lock.load(root / "l.json")
    lock.reject("k1", 1)
    _pin(lock, "a", "k1", 1)
    assert lock.rejected.get("k1", set()) == set()
    assert "a" in lock.pins


def test_unpin_and_note_removal(root):
    lock = Lock.load(root / "l.json")
    _pin(lock, "a")
    lock.unpin("a")
    lock.unpin("a")
    assert lock.pins == {}
    lock.note("a", "x")
    lock.note("a", "")
    assert lock.notes == {}


def test_lock_version_guard(root):
    p = root / "l.json"
    p.write_text(json.dumps({"version": 2, "lines": {}}), encoding="utf-8")
    with pytest.raises(NarrateError, match="l.json"):
        Lock.load(p)


_GOOD = '{"key": "k", "take": 1, "duration": 1, "heard": "", "cer": null, "pace": null, "approved_at": "t"}'


@pytest.mark.parametrize(
    "doc",
    [
        "[]",
        "not json",
        '{"version": 1, "lines": []}',
        '{"version": 1, "lines": {"Bad Id": ' + _GOOD + "}}",
        '{"version": 1, "lines": {"a": ' + _GOOD.replace('"take": 1', '"take": "1"') + "}}",
        '{"version": 1, "lines": {"a": ' + _GOOD.replace('"take": 1', '"take": true') + "}}",
        '{"version": 1, "rejected": {"k": ["x"]}}',
        '{"version": 1, "notes": {"a": 3}}',
    ],
)
def test_malformed_lock_raises(root, doc):
    p = root / "l.json"
    p.write_text(doc, encoding="utf-8")
    with pytest.raises(NarrateError, match="l.json"):
        Lock.load(p)


def test_pin_sha256_is_validated(root):
    p = root / "l.json"
    lock = Lock.load(p)
    _pin(lock)
    lock.save()
    assert Lock.load(p).pins["intro"].sha256 == SHA
    raw = json.loads(p.read_text(encoding="utf-8"))
    raw["lines"]["intro"]["sha256"] = "XYZ"
    p.write_text(json.dumps(raw), encoding="utf-8")
    with pytest.raises(NarrateError, match="sha256"):
        Lock.load(p)
