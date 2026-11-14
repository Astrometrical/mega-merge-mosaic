"""Voice profiles: a folder holding `voice.toml` and its reference clips."""

from __future__ import annotations

import hashlib
import os
import tomllib
from dataclasses import dataclass
from pathlib import Path

import soundfile

from narrate import NarrateError

DEFAULT_PACE_MAX = 5.3
DEFAULT_PACE_TARGET = 4.8
DEFAULT_TAKES_COUNT = 3
DEFAULT_TAKES_MAX = 6

_TOP_KEYS = {
    "backend",
    "model",
    "language",
    "precision",
    "checker",
    "default_style",
    "pronunciations",
    "style",
    "pace",
    "takes",
}
_REQUIRED = ("backend", "model", "default_style")
_LENGTHS = ("short", "long")
_KIND_LENGTH = {"line": "short", "passage": "long"}


class ProfileError(NarrateError):
    """A voice profile is missing or malformed."""


@dataclass(frozen=True)
class Reference:
    """A reference clip and its exact transcript."""

    audio: Path
    text: str
    seconds: float
    sha256: str


@dataclass(frozen=True)
class Pace:
    """Speaking-pace limits in syllables per second."""

    max: float = DEFAULT_PACE_MAX
    target: float = DEFAULT_PACE_TARGET


@dataclass(frozen=True)
class Takes:
    """How many takes to try per line, and the ceiling."""

    count: int = DEFAULT_TAKES_COUNT
    max: int = DEFAULT_TAKES_MAX


@dataclass(frozen=True)
class VoiceProfile:
    """A loaded voice profile."""

    name: str
    root: Path
    backend: str
    model: str
    default_style: str
    pronunciations: tuple[Path, ...]
    styles: dict[str, dict[str, Reference]]
    language: str = "English"
    precision: str = "fp16"
    checker: str = "whisper"
    pace: Pace = Pace()
    takes: Takes = Takes()

    def reference(self, style: str, kind: str) -> Reference:
        """The reference for a style and line kind ("line" or "passage")."""
        length = _KIND_LENGTH[kind]
        if style not in self.styles:
            raise ProfileError(f"{self.root / 'voice.toml'}: no style {style!r}")
        if length not in self.styles[style]:
            raise ProfileError(f"{self.root / 'voice.toml'}: style {style!r} has no {length!r} reference")
        return self.styles[style][length]


def voice_library(override: Path | None = None) -> Path:
    """The voice library folder: override, else $NARRATE_VOICES, else ~/.narrate/voices."""
    if override is not None:
        return Path(override)
    env = os.environ.get("NARRATE_VOICES")
    if env:
        return Path(env)
    return Path.home() / ".narrate" / "voices"


def _check_keys(where: str, table: dict, allowed: set[str]) -> None:
    unknown = sorted(set(table) - allowed)
    if unknown:
        raise ProfileError(f"{where}: unknown key {unknown[0]!r}")


def _string(where: str, table: dict, key: str, default: str | None = None) -> str:
    value = table.get(key, default)
    if not isinstance(value, str) or not value:
        raise ProfileError(f"{where}: {key!r} must be a non-empty string")
    return value


def _number(where: str, table: dict, key: str, default, kind: type):
    value = table.get(key, default)
    ok = isinstance(value, int | float) if kind is float else isinstance(value, int)
    if isinstance(value, bool) or not ok or value <= 0:
        raise ProfileError(f"{where}: {key!r} must be a positive {kind.__name__}")
    return kind(value)


def _reference(where: str, root: Path, raw: object) -> Reference:
    if not isinstance(raw, dict):
        raise ProfileError(f"{where} must be a table with 'audio' and 'text'")
    _check_keys(where, raw, {"audio", "text"})
    audio = root / _string(where, raw, "audio")
    text = _string(where, raw, "text")
    if not audio.is_file():
        raise ProfileError(f"{where}: audio file not found: {audio}")
    try:
        seconds = float(soundfile.info(audio).duration)
        digest = hashlib.sha256(audio.read_bytes()).hexdigest()
    except (OSError, RuntimeError) as e:
        raise ProfileError(f"{where}: cannot read audio {audio} ({e})") from e
    return Reference(audio, text, seconds, digest)


def load_profile(name: str, library: Path | None = None) -> VoiceProfile:
    """Load `<library>/<name>/voice.toml`, resolving paths against the profile folder."""
    root = voice_library(library) / name
    path = root / "voice.toml"
    if not path.is_file():
        raise ProfileError(f"{path}: voice profile {name!r} not found")
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise ProfileError(f"{path}: cannot parse ({e})") from e
    where = str(path)
    _check_keys(where, data, _TOP_KEYS)
    for key in _REQUIRED:
        if key not in data:
            raise ProfileError(f"{where}: missing required key {key!r}")

    pron = data.get("pronunciations", [])
    if not isinstance(pron, list) or not all(isinstance(p, str) for p in pron):
        raise ProfileError(f"{where}: 'pronunciations' must be a list of paths")

    raw_styles = data.get("style", {})
    if not isinstance(raw_styles, dict):
        raise ProfileError(f"{where}: 'style' must be a table")
    styles: dict[str, dict[str, Reference]] = {}
    for style, table in raw_styles.items():
        swhere = f"{where}: [style.{style}]"
        if not isinstance(table, dict):
            raise ProfileError(f"{swhere} must be a table")
        _check_keys(swhere, table, set(_LENGTHS))
        styles[style] = {
            length: _reference(f"{swhere} {length}", root, table[length]) for length in _LENGTHS if length in table
        }

    default_style = _string(where, data, "default_style")
    if default_style not in styles:
        raise ProfileError(f"{where}: default_style {default_style!r} is not a defined style")

    pace_raw = data.get("pace", {})
    _check_keys(f"{where}: [pace]", pace_raw, {"max", "target"})
    pace = Pace(
        _number(f"{where}: [pace]", pace_raw, "max", DEFAULT_PACE_MAX, float),
        _number(f"{where}: [pace]", pace_raw, "target", DEFAULT_PACE_TARGET, float),
    )
    takes_raw = data.get("takes", {})
    _check_keys(f"{where}: [takes]", takes_raw, {"count", "max"})
    takes = Takes(
        _number(f"{where}: [takes]", takes_raw, "count", DEFAULT_TAKES_COUNT, int),
        _number(f"{where}: [takes]", takes_raw, "max", DEFAULT_TAKES_MAX, int),
    )

    return VoiceProfile(
        name=name,
        root=root,
        backend=_string(where, data, "backend"),
        model=_string(where, data, "model"),
        default_style=default_style,
        pronunciations=tuple(root / p for p in pron),
        styles=styles,
        language=_string(where, data, "language", "English"),
        precision=_string(where, data, "precision", "fp16"),
        checker=_string(where, data, "checker", "whisper"),
        pace=pace,
        takes=takes,
    )
