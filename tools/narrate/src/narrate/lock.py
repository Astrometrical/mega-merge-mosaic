"""The approval lock: the user's by-ear approvals, committed next to the script."""

from __future__ import annotations

import json
import math
import os
import re
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from pathlib import Path

from narrate import NarrateError

VERSION = 1
_ID_RE = re.compile(r"[a-z0-9-]+")


def lock_path(script_path: Path) -> Path:
    """`x.narration.toml` -> `x.narration.lock.json`."""
    name = script_path.name
    stem = name[: -len(".toml")] if name.endswith(".toml") else name
    return script_path.with_name(stem + ".lock.json")


@dataclass(frozen=True)
class Pin:
    """One line's approved take, with the measurements heard at approval time."""

    key: str
    take: int
    duration: float
    heard: str
    cer: float | None
    pace: float | None
    approved_at: str


def _finite(x: float | None) -> float | None:
    return x if x is not None and math.isfinite(x) else None


@dataclass
class Lock:
    """Approvals (`pins`), rejected takes and per-line notes."""

    path: Path
    pins: dict[str, Pin] = field(default_factory=dict)
    rejected: dict[str, set[int]] = field(default_factory=dict)
    notes: dict[str, str] = field(default_factory=dict)

    @classmethod
    def load(cls, path: Path) -> Lock:
        """Read a lock; a missing file is an empty lock."""
        lock = cls(path)
        if not path.is_file():
            return lock
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            raise NarrateError(f"{path}: cannot read lock file: {e}") from e
        if not isinstance(data, dict):
            raise NarrateError(f"{path}: lock must be a JSON object")
        version = data.get("version")
        if not _is_int(version):
            raise NarrateError(f"{path}: 'version' must be an integer")
        if version > VERSION:
            raise NarrateError(f"{path}: lock version {version} is newer than supported ({VERSION})")
        for id_, raw in _section(data, "lines", path).items():
            _check_id(id_, path)
            lock.pins[id_] = _read_pin(id_, raw, path)
        for key, takes in _section(data, "rejected", path).items():
            if not isinstance(takes, list) or not all(_is_int(t) for t in takes):
                raise NarrateError(f"{path}: rejected[{key!r}] must be a list of integers")
            lock.rejected[key] = set(takes)
        for id_, text in _section(data, "notes", path).items():
            _check_id(id_, path)
            if not isinstance(text, str):
                raise NarrateError(f"{path}: note {id_!r} must be a string")
            lock.notes[id_] = text
        return lock

    def save(self) -> None:
        """Write the lock atomically."""
        doc = {
            "version": VERSION,
            "lines": {i: _pin_json(self.pins[i]) for i in sorted(self.pins)},
            "notes": {i: self.notes[i] for i in sorted(self.notes)},
            "rejected": {k: sorted(self.rejected[k]) for k in sorted(self.rejected)},
        }
        text = json.dumps(doc, indent=1, allow_nan=False) + "\n"
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_name(self.path.name + ".tmp")
        tmp.write_text(text, encoding="utf-8")
        os.replace(tmp, self.path)

    def pin(
        self, line_id: str, key: str, take: int, duration: float, heard: str, cer: float | None, pace: float | None
    ) -> None:
        """Approve `take` for a line; this also un-rejects it."""
        stamp = datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.pins[line_id] = Pin(key, take, duration, heard, cer, pace, stamp)
        self.rejected.get(key, set()).discard(take)

    def unpin(self, line_id: str) -> None:
        """Forget a line's approval."""
        self.pins.pop(line_id, None)

    def reject(self, key: str, take: int) -> None:
        """Reject a take and unpin every line that used it."""
        self.rejected.setdefault(key, set()).add(take)
        for id_ in [i for i, p in self.pins.items() if (p.key, p.take) == (key, take)]:
            del self.pins[id_]

    def note(self, line_id: str, text: str) -> None:
        """Set a line's note; empty text removes it."""
        if text:
            self.notes[line_id] = text
        else:
            self.notes.pop(line_id, None)


def _pin_json(p: Pin) -> dict:
    d = dict(sorted(asdict(p).items()))
    d["cer"] = _finite(p.cer)
    d["pace"] = _finite(p.pace)
    return d


def _is_int(x: object) -> bool:
    return isinstance(x, int) and not isinstance(x, bool)


def _is_num(x: object) -> bool:
    return isinstance(x, (int, float)) and not isinstance(x, bool)


def _section(data: dict, name: str, path: Path) -> dict:
    value = data.get(name, {})
    if not isinstance(value, dict):
        raise NarrateError(f"{path}: {name!r} must be an object")
    return value


def _check_id(id_: str, path: Path) -> None:
    if not _ID_RE.fullmatch(id_):
        raise NarrateError(f"{path}: line id {id_!r} must match [a-z0-9-]+")


def _read_pin(id_: str, raw: object, path: Path) -> Pin:
    where = f"{path}: line {id_!r}"
    if not isinstance(raw, dict):
        raise NarrateError(f"{where}: must be an object")
    checks = {
        "key": lambda v: isinstance(v, str),
        "take": _is_int,
        "duration": _is_num,
        "heard": lambda v: isinstance(v, str),
        "cer": lambda v: v is None or _is_num(v),
        "pace": lambda v: v is None or _is_num(v),
        "approved_at": lambda v: isinstance(v, str),
    }
    for name, ok in checks.items():
        if name not in raw or not ok(raw[name]):
            raise NarrateError(f"{where}: field {name!r} is missing or has the wrong type")
    return Pin(
        key=raw["key"],
        take=raw["take"],
        duration=float(raw["duration"]),
        heard=raw["heard"],
        cer=None if raw["cer"] is None else float(raw["cer"]),
        pace=None if raw["pace"] is None else float(raw["pace"]),
        approved_at=raw["approved_at"],
    )
