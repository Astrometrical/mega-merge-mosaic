"""The narration manifest (`narration.json`), spec section 8.4."""

from __future__ import annotations

import json
import math
import os
from dataclasses import dataclass, field
from pathlib import Path

from narrate import NarrateError
from narrate.captions import Caption

VERSION = 1
LEAD = 0.15
BREATH = 0.4


@dataclass
class ManifestLine:
    """One narrated line as the recorder sees it."""

    id: str
    text: str
    spoken: str
    kind: str
    clip: str | None
    duration: float
    at: float | None
    status: str  # approved, new, flagged or draft
    flags: list[str] = field(default_factory=list)
    captions: list[Caption] = field(default_factory=list)
    check: dict | None = None

    def to_json(self) -> dict:
        """JSON form with the section 8.4 field names."""
        return _finite(self._raw_json())

    def _raw_json(self) -> dict:
        return {
            "id": self.id,
            "text": self.text,
            "spoken": self.spoken,
            "kind": self.kind,
            "clip": self.clip,
            "duration": round(self.duration, 3),
            "at": self.at,
            "status": self.status,
            "flags": list(self.flags),
            "captions": [c.to_json() for c in self.captions],
            "check": self.check,
        }


@dataclass
class Manifest:
    """All lines of a script, with the clips built for them."""

    script: str
    voice: str | None
    draft: bool
    sample_rate: int | None
    lead: float = LEAD
    breath: float = BREATH
    lines: list[ManifestLine] = field(default_factory=list)

    def to_json(self) -> dict:
        """JSON form, `version` first; non-finite numbers become null."""
        return _finite(
            {
                "version": VERSION,
                "script": self.script,
                "voice": self.voice,
                "draft": self.draft,
                "sample_rate": self.sample_rate,
                "lead": self.lead,
                "breath": self.breath,
                "lines": [ln.to_json() for ln in self.lines],
            }
        )

    def write(self, path: Path) -> None:
        """Write strict JSON (non-finite numbers become null), UTF-8, atomically."""
        path.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(self.to_json(), indent=1, ensure_ascii=False, allow_nan=False)
        tmp = path.with_name(path.name + ".tmp")
        tmp.write_text(text + "\n", encoding="utf-8")
        os.replace(tmp, path)

    def flagged(self) -> list[str]:
        """Ids of the flagged lines."""
        return [ln.id for ln in self.lines if ln.status == "flagged"]


def _finite(value):
    """Replace non-finite floats with None, recursively."""
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {k: _finite(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_finite(v) for v in value]
    return value


def _refuse_constant(name: str):
    raise ValueError(f"non-finite constant {name}")


def read_manifest(path: Path) -> Manifest:
    """Read a manifest; every defect raises a `NarrateError` naming the file."""
    path = Path(path)
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise NarrateError(f"{path}: cannot read ({e})") from e
    try:
        data = json.loads(text, parse_constant=_refuse_constant)
    except ValueError as e:
        raise NarrateError(f"{path}: invalid JSON ({e})") from e
    if not isinstance(data, dict):
        raise NarrateError(f"{path}: invalid JSON (top level must be an object)")
    version = data.get("version")
    if not isinstance(version, int) or isinstance(version, bool) or version < 1:
        raise NarrateError(f"{path}: unsupported manifest version {version!r}")
    if version > VERSION:
        raise NarrateError(f"{path}: manifest version {version!r} is newer than supported ({VERSION})")
    raw_lines = data.get("lines", [])
    if not isinstance(raw_lines, list):
        raise NarrateError(f"{path}: 'lines' must be a list")
    if "script" not in data:
        raise NarrateError(f"{path}: missing required field 'script'")
    return Manifest(
        script=_str(path, "", data, "script"),
        voice=_opt(_str, path, "", data, "voice"),
        draft=bool(data.get("draft", False)),
        sample_rate=_opt(_int, path, "", data, "sample_rate"),
        lead=_num(path, "", data, "lead", LEAD),
        breath=_num(path, "", data, "breath", BREATH),
        lines=[_read_line(path, d) for d in raw_lines],
    )


def _bad(path: Path, where: str, key: str, what: str, value) -> NarrateError:
    return NarrateError(f"{path}: {where}field {key!r} must be {what}, got {value!r}")


def _str(path, where, d, key):
    if key not in d:
        raise NarrateError(f"{path}: {where}missing required field {key!r}")
    if not isinstance(d[key], str):
        raise _bad(path, where, key, "a string", d[key])
    return d[key]


def _int(path, where, d, key):
    v = d[key]
    if not isinstance(v, int) or isinstance(v, bool):
        raise _bad(path, where, key, "an integer", v)
    return v


def _opt(fn, path, where, d, key):
    return None if d.get(key) is None else fn(path, where, d, key)


def _num(path, where, d, key, default=None):
    if key not in d:
        if default is None:
            raise NarrateError(f"{path}: {where}missing required field {key!r}")
        return default
    v = d[key]
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or v < 0:
        raise _bad(path, where, key, "a finite number >= 0", v)
    return v


def _finite_num(path, where, d, key):
    v = d.get(key)
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v):
        raise _bad(path, where, key, "a finite number", v)
    return v


def _read_line(path: Path, d) -> ManifestLine:
    if not isinstance(d, dict):
        raise NarrateError(f"{path}: each line must be an object")
    ident = _str(path, "a line: ", d, "id")
    where = f"line {ident!r}: "
    for key in ("text", "spoken", "kind", "status"):
        _str(path, where, d, key)
    flags = d.get("flags", [])
    if not isinstance(flags, list) or not all(isinstance(f, str) for f in flags):
        raise _bad(path, where, "flags", "a list of strings", flags)
    raw_caps = d.get("captions", [])
    if not isinstance(raw_caps, list) or not all(isinstance(c, dict) for c in raw_caps):
        raise _bad(path, where, "captions", "a list of objects", raw_caps)
    captions = [
        Caption(
            _str(path, where + "caption ", c, "text"),
            _finite_num(path, where + "caption ", c, "start"),
            _finite_num(path, where + "caption ", c, "end"),
        )
        for c in raw_caps
    ]
    check = d.get("check")
    if check is not None and not isinstance(check, dict):
        raise _bad(path, where, "check", "an object or null", check)
    at = d.get("at")
    if at is not None:
        at = _finite_num(path, where, d, "at")
    return ManifestLine(
        id=ident,
        text=d["text"],
        spoken=d["spoken"],
        kind=d["kind"],
        clip=_opt(_str, path, where, d, "clip"),
        duration=_num(path, where, d, "duration"),
        at=at,
        status=d["status"],
        flags=list(flags),
        captions=captions,
        check=check,
    )
