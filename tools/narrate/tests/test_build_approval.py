import hashlib
import shutil

import numpy as np
import pytest

from narrate import fakes
from narrate.approved import ApprovedStore, approved_root
from narrate.audio import normalise_loudness, read_wav, trim
from narrate.build import BuildOptions, build, default_out
from narrate.cache import TakeStore, cache_root
from narrate.fakes import FakeWorld
from narrate.lock import Lock, lock_path
from narrate.script import load_script

LINE = "Each filter is a group of its own."
OTHER = "Each filter is a group of its very own."


@pytest.fixture
def world(monkeypatch):
    w = FakeWorld()
    monkeypatch.setattr(fakes, "_DEFAULT", w)
    return w


def script_text(*lines):
    body = 'voice = "test"\n'
    for id_, text in lines:
        body += f'\n[[line]]\nid = "{id_}"\ntext = "{text}"\n'
    return body


@pytest.fixture
def make(root, write_script, voice_library):
    return lambda *lines: write_script(root, script_text(*lines))


def pin_line(path, line):
    lock = Lock.load(lock_path(path))
    c = line.check
    sha = hashlib.sha256(TakeStore(cache_root(None)).path(c["key"], c["take"]).read_bytes()).hexdigest()
    lock.pin(line.id, c["key"], c["take"], line.duration, c["heard"], c["cer"], c["pace"], sha)
    lock.save()
    return lock


def store_approved(line):
    key, take = line.check["key"], line.check["take"]
    ApprovedStore(approved_root(None)).put(key, take, TakeStore(cache_root(None)).path(key, take))


def test_pinned_line_is_approved_without_work(make, world):
    path = make(("a", LINE), ("b", OTHER))
    a = build(path).lines[0]
    store_approved(a)
    pin_line(path, a)
    world.calls.clear()
    world.events.clear()
    m2 = build(path)
    assert (m2.lines[0].status, m2.lines[0].flags) == ("approved", [])
    assert m2.lines[0].check["take"] == a.check["take"]
    assert LINE not in [t for c in world.calls for t in c]
    assert m2.lines[1].status == "new"
    assert world.events.count("transcribe") == 0


def test_approved_line_survives_cache_loss(make, world):
    path = make(("a", LINE))
    a = build(path).lines[0]
    store_approved(a)
    pin_line(path, a)
    shutil.rmtree(cache_root(None) / "takes")
    world.calls.clear()
    world.events.clear()
    m = build(path)
    assert world.calls == []
    assert "transcribe" not in world.events
    assert m.lines[0].status == "approved"
    audio, sr = ApprovedStore(approved_root(None)).load(a.check["key"], a.check["take"])
    expected = normalise_loudness(trim(audio, sr), sr)
    got, _ = read_wav(default_out(load_script(path)) / m.lines[0].clip)
    assert np.allclose(got, expected, atol=1e-3)


def test_pin_repairs_the_store_from_cache(make, world):
    path = make(("a", LINE))
    a = build(path).lines[0]
    pin_line(path, a)
    store = ApprovedStore(approved_root(None))
    assert not store.has(a.check["key"], a.check["take"])
    assert build(path).lines[0].status == "approved"
    assert store.has(a.check["key"], a.check["take"])


def test_pinned_audio_missing_is_flagged(make, world):
    path = make(("a", LINE))
    a = build(path).lines[0]
    pin_line(path, a)
    shutil.rmtree(cache_root(None) / "takes")
    m = build(path)
    assert (m.lines[0].status, m.lines[0].flags) == ("new", ["pinned-audio-missing"])


def test_edit_unpins(root, make, write_script, world):
    path = make(("a", LINE))
    pin_line(path, build(path).lines[0])
    before = lock_path(path).read_text(encoding="utf-8")
    m = build(write_script(root, script_text(("a", OTHER))))
    assert (m.lines[0].status, m.lines[0].flags) == ("new", [])
    assert lock_path(path).read_text(encoding="utf-8") == before
    assert "a" in Lock.load(lock_path(path)).pins


def test_rejected_take_is_never_chosen(make, world):
    world.script(LINE, heard=[LINE, "garbled words here", "garbled again", LINE])
    path = make(("a", LINE))
    first = build(path).lines[0]
    assert first.check["take"] == 1
    lock = Lock.load(lock_path(path))
    lock.reject(first.check["key"], 1)
    lock.save()
    world.calls.clear()
    m = build(path).lines[0]
    assert m.check["take"] == 4
    assert m.status == "new"
    assert world.calls == [[LINE]]
    assert TakeStore(cache_root(None)).takes(m.check["key"]) == [1, 2, 3, 4]


def test_rejections_in_round_2(make, world):
    world.script(LINE, heard=[LINE, LINE, LINE, "garbled"])
    path = make(("a", LINE))
    key = build(path).lines[0].check["key"]
    lock = Lock.load(lock_path(path))
    for n in (1, 2, 3):
        lock.reject(key, n)
    lock.save()
    world.calls.clear()
    m = build(path).lines[0]
    assert m.status == "flagged"
    assert world.calls == [[LINE] * 3, [LINE] * 3]
    assert TakeStore(cache_root(None)).takes(key) == list(range(1, 10))
    assert m.check["take"] >= 4


def test_regenerated_take_is_not_reapproved(make, world):
    path = make(("a", LINE))
    a = build(path).lines[0]
    pin_line(path, a)
    shutil.rmtree(cache_root(None) / "takes")
    store = ApprovedStore(approved_root(None))
    assert not store.has(a.check["key"], a.check["take"])
    assert build(path).lines[0].flags == ["pinned-audio-missing"]
    m = build(path).lines[0]
    assert (m.status, m.flags) == ("new", ["pinned-audio-missing"])
    assert not store.has(a.check["key"], a.check["take"])


def test_pinned_line_with_round_2_neighbour(make, world):
    path = make(("a", LINE), ("b", OTHER))
    a = build(path).lines[0]
    store_approved(a)
    pin_line(path, a)
    shutil.rmtree(cache_root(None) / "takes")
    world.script(OTHER, heard=["garbled"])
    world.events.clear()
    world.calls.clear()
    m = build(path)
    assert all(LINE not in c for c in world.calls)
    assert world.events == [
        "synthesize",
        "close-backend",
        *["transcribe"] * 3,
        "close-checker",
        "synthesize",
        "close-backend",
        *["transcribe"] * 3,
        "close-checker",
    ]
    assert [ln.status for ln in m.lines] == ["approved", "flagged"]


def test_pin_on_rejected_take_is_ignored(make, world):
    path = make(("a", LINE))
    a = build(path).lines[0]
    store_approved(a)
    lock = pin_line(path, a)
    lock.rejected.setdefault(a.check["key"], set()).add(a.check["take"])
    lock.save()
    m = build(path).lines[0]
    assert m.status == "new"
    assert m.check["take"] != a.check["take"]


def test_pin_with_mismatched_cache_audio_is_not_copied(make, world):
    path = make(("a", LINE))
    a = build(path).lines[0]
    pin_line(path, a)
    cached = TakeStore(cache_root(None)).path(a.check["key"], a.check["take"])
    data = bytearray(cached.read_bytes())
    data[-1] ^= 0xFF
    cached.write_bytes(bytes(data))
    m = build(path).lines[0]
    assert m.flags == ["pinned-audio-missing"]
    assert not ApprovedStore(approved_root(None)).has(a.check["key"], a.check["take"])


def test_build_options_approved_override(root, make, world):
    path = make(("a", LINE))
    a = build(path, options=BuildOptions(approved=root / "mine")).lines[0]
    pin_line(path, a)
    build(path, options=BuildOptions(approved=root / "mine"))
    assert ApprovedStore(root / "mine").has(a.check["key"], a.check["take"])
