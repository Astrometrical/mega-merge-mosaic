"""The narration manifest (`narration.json`), spec section 8.4."""

from __future__ import annotations

import json
import math
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
        """JSON form, `version` first."""
        return {
            "version": VERSION,
            "script": self.script,
            "voice": self.voice,
            "draft": self.draft,
            "sample_rate": self.sample_rate,
            "lead": self.lead,
            "breath": self.breath,
            "lines": [ln.to_json() for ln in self.lines],
        }

    def write(self, path: Path) -> None:
        """Write strict JSON (non-finite numbers become null), UTF-8."""
        path.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(_finite(self.to_json()), indent=1, ensure_ascii=False, allow_nan=False)
        path.write_text(text + "\n", encoding="utf-8")

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
    """Read a manifest; a newer major version is refused."""
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    version = data.get("version")
    if not isinstance(version, int) or version > VERSION:
        raise NarrateError(f"{path}: manifest version {version!r} is newer than supported ({VERSION})")
    lines = [
        ManifestLine(
            id=d["id"],
            text=d["text"],
            spoken=d["spoken"],
            kind=d["kind"],
            clip=d.get("clip"),
            duration=d["duration"],
            at=d.get("at"),
            status=d["status"],
            flags=list(d.get("flags", [])),
            captions=[Caption(c["text"], c["start"], c["end"]) for c in d.get("captions", [])],
            check=d.get("check"),
        )
        for d in data.get("lines", [])
    ]
    return Manifest(
        script=data["script"],
        voice=data.get("voice"),
        draft=bool(data.get("draft", False)),
        sample_rate=data.get("sample_rate"),
        lead=data.get("lead", LEAD),
        breath=data.get("breath", BREATH),
        lines=lines,
    )
