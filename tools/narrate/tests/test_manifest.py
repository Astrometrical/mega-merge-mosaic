import json

import pytest

from narrate import NarrateError
from narrate.captions import Caption
from narrate.manifest import BREATH, LEAD, Manifest, ManifestLine, read_manifest


def sample() -> Manifest:
    line = ManifestLine(
        "open",
        "Hello.",
        "Hello.",
        "line",
        "clips/open.wav",
        1.5,
        None,
        "flagged",
        ["words"],
        [Caption("Hello.", 0.0, 1.5)],
        {"heard": "", "cer": 1.0, "pace": float("inf"), "take": 2, "key": "abc"},
    )
    return Manifest("demo.narration.toml", "test", False, 24000, lines=[line])


def test_json_shape_and_order():
    data = sample().to_json()
    assert list(data)[:2] == ["version", "script"]
    assert data["version"] == 1
    assert (data["lead"], data["breath"]) == (LEAD, BREATH)
    assert data["lines"][0]["captions"] == [{"text": "Hello.", "start": 0.0, "end": 1.5}]


def test_write_is_strict_json_with_null_for_non_finite(root):
    path = root / "narration.json"
    sample().write(path)

    def refuse(name):
        raise AssertionError(f"non-finite constant {name} in manifest")

    data = json.loads(path.read_text(encoding="utf-8"), parse_constant=refuse)
    assert data["lines"][0]["check"]["pace"] is None


def test_round_trip_and_flagged(root):
    path = root / "m" / "narration.json"
    m = sample()
    m.write(path)
    back = read_manifest(path)
    assert back.lines[0].captions == m.lines[0].captions
    assert back.lines[0].id == "open"
    assert back.flagged() == ["open"]


def test_unicode_is_not_escaped(root):
    m = sample()
    m.lines[0].text = "Café"
    path = root / "narration.json"
    m.write(path)
    assert "Café" in path.read_text(encoding="utf-8")


def test_manifest_version_guard(root):
    path = root / "narration.json"
    path.write_text(json.dumps({"version": 2, "script": "x", "lines": []}), encoding="utf-8")
    with pytest.raises(NarrateError):
        read_manifest(path)
