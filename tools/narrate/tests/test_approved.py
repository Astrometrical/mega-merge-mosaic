import hashlib
import os
from pathlib import Path

import numpy as np
import pytest
import soundfile as sf

from narrate import NarrateError
from narrate.approved import ApprovedStore, approved_root


def take_file(root: Path, name: str = "src.wav", amplitude: float = 0.3) -> tuple[Path, np.ndarray, str]:
    src = root / name
    audio = (amplitude * np.sin(np.arange(800) / 7)).astype("float32")
    sf.write(src, audio, 8000, subtype="FLOAT")
    return src, audio, hashlib.sha256(src.read_bytes()).hexdigest()


def test_store_is_content_addressed(root):
    src, audio, sha = take_file(root)
    store = ApprovedStore(root / "store")
    assert not store.has(sha)
    out = store.put(sha, src)
    assert out == store.path(sha) == root / "store" / sha[:2] / f"{sha}.wav"
    assert store.has(sha)
    back, sr = store.load(sha)
    assert sr == 8000 and np.array_equal(back, audio)
    assert sorted(p.name for p in out.parent.iterdir()) == [out.name]


def test_put_refuses_audio_that_does_not_hash_to_its_address(root):
    src, _, _ = take_file(root)
    store = ApprovedStore(root / "store")
    wrong = "0" * 64
    with pytest.raises(NarrateError, match="src.wav"):
        store.put(wrong, src)
    assert not store.has(wrong)


def test_put_leaves_an_existing_file_untouched(root):
    src, _, sha = take_file(root)
    store = ApprovedStore(root / "store")
    out = store.put(sha, src)
    os.utime(out, (1_000_000, 1_000_000))
    assert store.put(sha, src) == out
    assert out.stat().st_mtime == 1_000_000
    assert hashlib.sha256(out.read_bytes()).hexdigest() == sha
    assert sorted(p.name for p in out.parent.iterdir()) == [out.name]


def test_store_ignores_itself_in_git(root):
    src, _, sha = take_file(root)
    store = ApprovedStore(root / "store")
    store.put(sha, src)
    assert (root / "store" / ".gitignore").read_text(encoding="utf-8") == "*\n"
    (root / "store" / ".gitignore").write_text("mine\n", encoding="utf-8")
    other, _, other_sha = take_file(root, "other.wav", amplitude=0.2)
    store.put(other_sha, other)
    assert store.has(other_sha)
    assert (root / "store" / ".gitignore").read_text(encoding="utf-8") == "mine\n"


def test_approved_root_resolution(tmp_path, monkeypatch):
    assert approved_root(tmp_path / "o") == tmp_path / "o"
    assert approved_root() == tmp_path / "approved dir"
    monkeypatch.delenv("NARRATE_APPROVED")
    assert approved_root() == Path.home() / ".narrate" / "approved"
