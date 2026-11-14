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
    status: str
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


def read_manifest(path: Path) -> Manifest:
    """Read a manifest; every defect raises a `NarrateError` naming the file."""
    path = Path(path)
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ValueError) as e:
        raise NarrateError(f"{path}: invalid JSON ({e})") from e
    if not isinstance(data, dict):
        raise NarrateError(f"{path}: invalid JSON (top level must be an object)")
    version = data.get("version")
    if not isinstance(version, int) or isinstance(version, bool):
        raise NarrateError(f"{path}: unsupported manifest version {version!r}")
    if version > VERSION:
        raise NarrateError(f"{path}: manifest version {version!r} is newer than supported ({VERSION})")
    raw_lines = data.get("lines", [])
    if not isinstance(raw_lines, list):
        raise NarrateError(f"{path}: 'lines' must be a list")
    lines = [_read_line(path, d) for d in raw_lines]
    if "script" not in data:
        raise NarrateError(f"{path}: missing required field 'script'")
    return Manifest(
        script=data["script"],
        voice=data.get("voice"),
        draft=bool(data.get("draft", False)),
        sample_rate=data.get("sample_rate"),
        lead=data.get("lead", LEAD),
        breath=data.get("breath", BREATH),
        lines=lines,
    )


def _read_line(path: Path, d) -> ManifestLine:
    if not isinstance(d, dict):
        raise NarrateError(f"{path}: each line must be an object")
    where = f"line {d['id']!r}" if isinstance(d.get("id"), str) else "a line"
    for key in ("id", "text", "spoken", "kind", "duration", "status"):
        if key not in d:
            raise NarrateError(f"{path}: {where} is missing required field {key!r}")
    try:
        captions = [Caption(c["text"], c["start"], c["end"]) for c in d.get("captions", [])]
    except (KeyError, TypeError) as e:
        raise NarrateError(f"{path}: {where} has a malformed caption ({e!r})") from e
    return ManifestLine(
        id=d["id"],
        text=d["text"],
        spoken=d["spoken"],
        kind=d["kind"],
        clip=d.get("clip"),
        duration=d["duration"],
        at=d.get("at"),
        status=d["status"],
        flags=list(d.get("flags", [])),
        captions=captions,
        check=d.get("check"),
    )
