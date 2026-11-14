import dataclasses
import json
from pathlib import Path

import numpy as np

from narrate.cache import TakeStore, cache_root, take_key
from narrate.profile import load_profile


def test_cache_root_override_env_default(monkeypatch, tmp_path):
    assert cache_root(tmp_path / "x") == tmp_path / "x"
    assert cache_root() == tmp_path / "cache dir"
    monkeypatch.delenv("NARRATE_CACHE")
    assert cache_root() == Path.home() / ".narrate" / "cache"


def test_key_is_stable_and_sensitive(voice_library):
    profile = load_profile("test")
    ref = profile.styles["explainer"]["short"]
    key = take_key(profile, ref, "hello")
    assert key == take_key(profile, ref, "hello")
    assert len(key) == 24 and int(key, 16) >= 0
    others = [
        take_key(profile, ref, "hello!"),
        take_key(profile, dataclasses.replace(ref, text="other"), "hello"),
        take_key(profile, dataclasses.replace(ref, sha256="f" * 64), "hello"),
        take_key(dataclasses.replace(profile, precision="fp32"), ref, "hello"),
        take_key(dataclasses.replace(profile, model="other/model"), ref, "hello"),
        take_key(dataclasses.replace(profile, language="French"), ref, "hello"),
    ]
    assert len({key, *others}) == 7


def test_store_round_trip(root):
    store = TakeStore(root)
    key = "ab" + "c" * 22
    a1 = np.linspace(-0.5, 0.5, 100, dtype=np.float32)
    a2 = np.linspace(0.1, 0.9, 50, dtype=np.float32)
    p = store.save(key, 1, a1, 24000)
    store.save(key, 2, a2, 24000)
    assert p == store.path(key, 1) == root / "takes" / "ab" / key / "take-1.wav"
    assert store.takes(key) == [1, 2]
    assert store.takes("zz" + "0" * 22) == []
    audio, sr = store.load(key, 1)
    assert sr == 24000 and audio.dtype == np.float32 and np.array_equal(audio, a1)
    assert np.array_equal(store.load(key, 2)[0], a2)
    assert store.heard(key, 1, "whisper") is None
    store.set_heard(key, 1, "whisper", "hello there")
    store.set_heard(key, 1, "other", "hi")
    assert store.heard(key, 1, "whisper") == "hello there"
    assert store.heard(key, 1, "other") == "hi"
    assert store.heard(key, 2, "whisper") is None
    assert store.takes(key) == [1, 2]


def test_regenerated_take_does_not_reuse_old_transcript(root):
    store = TakeStore(root)
    key = "ab" + "c" * 22
    store.save(key, 1, np.zeros(100, dtype=np.float32), 8000)
    store.set_heard(key, 1, "whisper", "old words")
    store.path(key, 1).unlink()
    store.save(key, 1, np.ones(100, dtype=np.float32) * 0.1, 8000)
    assert store.heard(key, 1, "whisper") is None
    assert not store.path(key, 1).with_suffix(".json").exists()


def test_sidecar_with_wrong_fingerprint_is_ignored(root):
    store = TakeStore(root)
    key = "ab" + "c" * 22
    store.save(key, 1, np.zeros(100, dtype=np.float32), 8000)
    store.set_heard(key, 1, "whisper", "words")
    assert store.heard(key, 1, "whisper") == "words"
    sidecar = store.path(key, 1).with_suffix(".json")
    data = json.loads(sidecar.read_text(encoding="utf-8"))
    data["sha256"] = "0" * 64
    sidecar.write_text(json.dumps(data), encoding="utf-8")
    assert store.heard(key, 1, "whisper") is None
    store.set_heard(key, 1, "other", "fresh")
    assert store.heard(key, 1, "whisper") is None and store.heard(key, 1, "other") == "fresh"


def test_legacy_sidecar_without_fingerprint_is_ignored(root):
    store = TakeStore(root)
    key = "ab" + "c" * 22
    store.save(key, 1, np.zeros(100, dtype=np.float32), 8000)
    store.path(key, 1).with_suffix(".json").write_text('{"heard": {"whisper": "x"}}', encoding="utf-8")
    assert store.heard(key, 1, "whisper") is None


def test_tmp_files_are_ignored(root):
    store = TakeStore(root)
    key = "ab" + "c" * 22
    store.save(key, 1, np.zeros(10, dtype=np.float32), 8000)
    store.path(key, 3).with_name("take-3.wav.tmp").write_bytes(b"x")
    assert store.takes(key) == [1]
