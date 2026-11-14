"""Narration scripts: a TOML file of lines to be voiced."""

from __future__ import annotations

import re
import tomllib
from dataclasses import dataclass
from pathlib import Path

from narrate import NarrateError

_ID_RE = re.compile(r"[a-z0-9-]+")
_KINDS = ("line", "passage")
_SCRIPT_KEYS = {"voice", "style", "pronunciations", "line"}
_LINE_KEYS = {"id", "text", "style", "kind", "at", "say"}


class ScriptError(NarrateError):
    """A narration script is malformed."""


@dataclass(frozen=True)
class Line:
    """One narrated line."""

    id: str
    text: str
    style: str | None = None
    kind: str | None = None
    at: float | None = None
    say: str | None = None


@dataclass(frozen=True)
class Script:
    """A parsed narration script."""

    path: Path
    voice: str | None
    style: str | None
    pronunciations: tuple[Path, ...]
    lines: tuple[Line, ...]

    @property
    def stem(self) -> str:
        """File name up to the first dot."""
        return self.path.name.split(".", 1)[0]


def _opt_str(d: dict, key: str, where: str) -> str | None:
    v = d.get(key)
    if v is not None and not isinstance(v, str):
        raise ScriptError(f"{where}{key} must be a string")
    return v


def _parse_line(raw: object, index: int, path: Path) -> Line:
    if not isinstance(raw, dict):
        raise ScriptError(f"{path}: line #{index + 1} must be a table")
    id_ = raw.get("id")
    if not isinstance(id_, str) or not _ID_RE.fullmatch(id_):
        raise ScriptError(f"{path}: line {id_!r}: id must match [a-z0-9-]+")
    where = f"{path}: line {id_!r}: "
    unknown = sorted(set(raw) - _LINE_KEYS)
    if unknown:
        raise ScriptError(f"{where}unknown key(s): {', '.join(unknown)}")
    text = raw.get("text")
    if not isinstance(text, str):
        raise ScriptError(f"{where}text must be a string")
    if not any(c.isalnum() for c in text):
        raise ScriptError(f"{where}text has no letter or digit")
    kind = _opt_str(raw, "kind", where)
    if kind is not None and kind not in _KINDS:
        raise ScriptError(f"{where}kind must be one of {', '.join(_KINDS)}, got {kind!r}")
    at = raw.get("at")
    if at is not None:
        if isinstance(at, bool) or not isinstance(at, int | float) or at < 0:
            raise ScriptError(f"{where}at must be a number >= 0")
        at = float(at)
    return Line(id_, text, _opt_str(raw, "style", where), kind, at, _opt_str(raw, "say", where))


def load_script(path: Path) -> Script:
    """Load and validate a narration script."""
    path = Path(path)
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as e:
        raise ScriptError(f"{path}: invalid TOML: {e}") from e
    except (OSError, UnicodeDecodeError) as e:
        raise ScriptError(f"{path}: cannot read: {e}") from e
    where = f"{path}: "
    unknown = sorted(set(data) - _SCRIPT_KEYS)
    if unknown:
        raise ScriptError(f"{where}unknown key(s): {', '.join(unknown)}")
    voice = _opt_str(data, "voice", where)
    style = _opt_str(data, "style", where)
    prons = data.get("pronunciations", [])
    if not isinstance(prons, list) or not all(isinstance(p, str) for p in prons):
        raise ScriptError(f"{where}pronunciations must be a list of strings")
    raw_lines = data.get("line", [])
    if not isinstance(raw_lines, list) or not raw_lines:
        raise ScriptError(f"{where}no lines (add [[line]] tables)")
    lines: list[Line] = []
    seen: set[str] = set()
    for i, raw in enumerate(raw_lines):
        ln = _parse_line(raw, i, path)
        if ln.id in seen:
            raise ScriptError(f"{path}: line {ln.id!r}: duplicate id")
        seen.add(ln.id)
        lines.append(ln)
    resolved = tuple((path.parent / p).resolve() for p in prons)
    return Script(path, voice, style, resolved, tuple(lines))
