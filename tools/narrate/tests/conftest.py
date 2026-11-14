from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _isolate(monkeypatch, tmp_path):
    """No test may touch the real ~/.narrate."""
    monkeypatch.setenv("NARRATE_CACHE", str(tmp_path / "cache dir"))
    monkeypatch.setenv("NARRATE_VOICES", str(tmp_path / "voices dir"))


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
