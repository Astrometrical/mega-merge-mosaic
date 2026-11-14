from pathlib import Path

import numpy as np
import soundfile as sf

from narrate.approved import ApprovedStore, approved_root


def test_store_put_and_load(root):
    src = root / "src.wav"
    audio = (0.3 * np.sin(np.arange(800) / 7)).astype("float32")
    sf.write(src, audio, 8000, subtype="FLOAT")
    store = ApprovedStore(root / "store")
    assert not store.has("abcdef", 2)
    out = store.put("abcdef", 2, src)
    assert out == store.path("abcdef", 2) == root / "store" / "ab" / "abcdef" / "take-2.wav"
    assert store.has("abcdef", 2)
    back, sr = store.load("abcdef", 2)
    assert sr == 8000 and np.array_equal(back, audio)
    assert not list(out.parent.glob("*.tmp"))


def test_approved_root_resolution(tmp_path, monkeypatch):
    assert approved_root(tmp_path / "o") == tmp_path / "o"
    assert approved_root() == tmp_path / "approved dir"
    monkeypatch.delenv("NARRATE_APPROVED")
    assert approved_root() == Path.home() / ".narrate" / "approved"
