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


def test_to_json_is_strict_json():
    m = sample()
    json.dumps(m.to_json(), allow_nan=False)
    json.dumps(m.lines[0].to_json(), allow_nan=False)
    m.lines[0].duration = float("inf")
    assert m.to_json()["lines"][0]["duration"] is None


def test_write_is_atomic(root):
    path = root / "m" / "narration.json"
    sample().write(path)
    assert sorted(p.name for p in path.parent.iterdir()) == ["narration.json"]


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


def _line():
    return {"id": "open", "text": "Hi.", "spoken": "Hi.", "kind": "line", "status": "ok"}


def _doc(**over):
    line = {"id": "open", "text": "Hi.", "spoken": "Hi.", "kind": "line", "status": "ok", "duration": 1.0}
    line.update(over)
    return json.dumps({"version": 1, "script": "s", "lines": [line]})


@pytest.mark.parametrize(
    ("content", "needle"),
    [
        ("{not json", "invalid JSON"),
        ('{"version": true}', "unsupported manifest version"),
        ('{"version": "1"}', "unsupported manifest version"),
        ('{"lines": []}', "unsupported manifest version"),
        ('{"version": 2}', "newer"),
        (json.dumps({"version": 1, "script": "s", "lines": [_line()]}), "duration"),
        (json.dumps({"version": 1, "lines": []}), "script"),
        (_doc(flags=5), "flags"),
        (_doc(duration="x"), "duration"),
        (_doc(duration=None), "duration"),
        (_doc(duration=-1), "duration"),
        (_doc(id=3), "'id'"),
        (_doc(captions=[{"text": "a", "start": "0", "end": 1}]), "start"),
        ('{"version": 1, "script": "s", "lead": "x", "lines": []}', "lead"),
        ('{"version": 0, "script": "s", "lines": []}', "unsupported manifest version"),
        ('{"version": 1, "script": "s", "lead": NaN, "lines": []}', "NaN"),
        ('{"version": 1, "script": "s", "lines": 5}', "lines"),
        (None, "cannot read"),
    ],
)
def test_read_manifest_errors(root, content, needle):
    path = root / "m.json"
    if content is not None:
        path.write_text(content, encoding="utf-8")
    with pytest.raises(NarrateError, match="m.json") as info:
        read_manifest(path)
    assert needle in str(info.value)


def test_read_manifest_missing_field_names_line(root):
    path = root / "m.json"
    path.write_text(json.dumps({"version": 1, "script": "s", "lines": [_line()]}), encoding="utf-8")
    with pytest.raises(NarrateError, match="'open'"):
        read_manifest(path)
