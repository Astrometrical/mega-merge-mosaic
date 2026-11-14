from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _isolate(monkeypatch, tmp_path):
    """No test may touch the real ~/.narrate."""
    monkeypatch.setenv("NARRATE_CACHE", str(tmp_path / "cache dir"))
    monkeypatch.setenv("NARRATE_VOICES", str(tmp_path / "voices dir"))
    monkeypatch.setenv("NARRATE_APPROVED", str(tmp_path / "approved dir"))


@pytest.fixture
def root(tmp_path) -> Path:
    d = tmp_path / "with space"
    d.mkdir()
    return d


@pytest.fixture
def write_script():
    def _write(root: Path, body: str, name: str = "demo.narration.toml") -> Path:
        p = root / name
        p.write_text(body, encoding="utf-8")
        return p

    return _write


def write_wav(path: Path, seconds: float, rate: int = 8000) -> None:
    """A small synthetic reference clip; only its duration matters."""
    import numpy as np
    import soundfile as sf

    path.parent.mkdir(parents=True, exist_ok=True)
    t = np.arange(int(seconds * rate)) / rate
    sf.write(path, (0.1 * np.sin(2 * np.pi * 220 * t)).astype("float32"), rate, subtype="PCM_16")


@pytest.fixture
def voice_library(root, monkeypatch) -> Path:
    """A voice library holding a profile named `test` (no `story` style)."""
    lib = root / "library"
    prof = lib / "test"
    write_wav(prof / "refs" / "explainer-10.wav", 10.0)
    write_wav(prof / "refs" / "explainer-full.wav", 49.0)
    (prof / "voice.toml").write_text(
        'backend = "fake"\n'
        'checker = "fake"\n'
        'model = "fake/model"\n'
        'default_style = "explainer"\n'
        "pronunciations = []\n"
        "\n"
        "[style.explainer]\n"
        'short = { audio = "refs/explainer-10.wav", text = "short text" }\n'
        'long = { audio = "refs/explainer-full.wav", text = "long text" }\n',
        encoding="utf-8",
    )
    monkeypatch.setenv("NARRATE_VOICES", str(lib))
    return lib
