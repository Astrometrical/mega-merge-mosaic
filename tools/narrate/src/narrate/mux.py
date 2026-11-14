"""Mix approved narration clips onto a silent video at cue times (spec section 8.5)."""

from __future__ import annotations

import json
import math
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

from narrate import NarrateError
from narrate._fs import read_text_retry, replace_with_retry
from narrate.manifest import Manifest, read_manifest
from narrate.script import Script

TIMELINE_VERSION = 1
#: Seconds a clip may run past the next cue's start before it counts as an overrun.
OVERRUN_TOLERANCE = 0.001
SAMPLE_RATE = 48000
LOUDNORM = "loudnorm=I=-16:TP=-1.5:LRA=11"
AUDIO_BITRATE = "192k"
STDERR_TAIL = 20


@dataclass(frozen=True)
class Cue:
    """A narration line and the time (seconds) its clip starts in the video."""

    id: str
    start: float


def _is_num(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def _refuse_constant(name: str):
    raise ValueError(f"non-finite constant {name}")


def load_timeline(path: Path) -> tuple[Path, Path, list[Cue]]:
    """Read a timeline file; returns the video, the manifest and the cues. Paths are relative to the file."""
    path = Path(path)
    try:
        text = read_text_retry(path)
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
        raise NarrateError(f"{path}: unsupported timeline version {version!r}")
    if version > TIMELINE_VERSION:
        raise NarrateError(f"{path}: timeline version {version!r} is newer than supported ({TIMELINE_VERSION})")
    for key in ("video", "narration"):
        if not isinstance(data.get(key), str) or not data[key]:
            raise NarrateError(f"{path}: field {key!r} must be a non-empty string, got {data.get(key)!r}")
    raw = data.get("cues")
    if not isinstance(raw, list):
        raise NarrateError(f"{path}: field 'cues' must be a list, got {raw!r}")
    cues: list[Cue] = []
    seen: set[str] = set()
    for i, c in enumerate(raw):
        if not isinstance(c, dict):
            raise NarrateError(f"{path}: cue #{i + 1} must be an object")
        id_, start = c.get("id"), c.get("start")
        if not isinstance(id_, str) or not id_:
            raise NarrateError(f"{path}: cue #{i + 1} field 'id' must be a non-empty string, got {id_!r}")
        if not _is_num(start) or not math.isfinite(start) or start < 0:
            raise NarrateError(f"{path}: cue {id_!r} field 'start' must be a finite number >= 0, got {start!r}")
        if id_ in seen:
            raise NarrateError(f"{path}: duplicate cue id {id_!r}")
        seen.add(id_)
        cues.append(Cue(id_, float(start)))
    base = path.parent
    return base / data["video"], base / data["narration"], cues


def script_cues(script: Script) -> list[Cue]:
    """Cues from each line's `at`; lines without one are an error."""
    missing = [ln.id for ln in script.lines if ln.at is None]
    if missing:
        raise NarrateError(f"{script.path}: lines without 'at': {', '.join(repr(i) for i in missing)}")
    for ln in script.lines:
        if not math.isfinite(ln.at) or ln.at < 0:
            raise NarrateError(f"{script.path}: line {ln.id!r}: 'at' must be a finite number >= 0, got {ln.at!r}")
    return [Cue(ln.id, float(ln.at)) for ln in script.lines]


def overruns(cues: list[Cue], manifest: Manifest) -> list[str]:
    """One message per cue whose clip runs into the next cue. The last cue is not checked."""
    durations = {ln.id: ln.duration for ln in manifest.lines}
    ordered = sorted(cues, key=lambda c: c.start)
    out = []
    for cur, nxt in zip(ordered, ordered[1:], strict=False):
        if cur.id not in durations:
            continue
        over = cur.start + durations[cur.id] - nxt.start
        if over > OVERRUN_TOLERANCE:
            out.append(
                f"line {cur.id!r} (starts {cur.start:.2f} s, runs {durations[cur.id]:.2f} s) "
                f"overlaps {nxt.id!r} (starts {nxt.start:.2f} s) by {over:.2f} s"
            )
    return out


def find_ffmpeg() -> str:
    """`$FFMPEG` if set, else `ffmpeg` on PATH."""
    env = os.environ.get("FFMPEG")
    if env:
        return env
    found = shutil.which("ffmpeg")
    if found is None:
        raise NarrateError("ffmpeg not found; install it or set FFMPEG to its path")
    return found


def mux_command(ffmpeg: str, video: Path, clips: list[tuple[Path, float]], out: Path) -> list[str]:
    """The ffmpeg argv: delay each clip to its start, mix, loudness-normalise, copy the video."""
    cmd = [ffmpeg, "-y", "-loglevel", "error", "-i", str(video)]
    parts = []
    for i, (clip, start) in enumerate(clips, 1):
        cmd += ["-i", str(clip)]
        parts.append(f"[{i}:a]aresample={SAMPLE_RATE},adelay={round(start * 1000)}:all=1[a{i}]")
    labels = "".join(f"[a{i}]" for i in range(1, len(clips) + 1))
    parts.append(
        f"{labels}amix=inputs={len(clips)}:normalize=0:dropout_transition=0,{LOUDNORM},aresample={SAMPLE_RATE}[aout]"
    )
    cmd += ["-filter_complex", ";".join(parts), "-map", "0:v", "-map", "[aout]"]
    cmd += ["-c:v", "copy", "-c:a", "aac", "-b:a", AUDIO_BITRATE, "-movflags", "+faststart", str(out)]
    return cmd


def _names(ids: list[str]) -> str:
    return ", ".join(repr(i) for i in ids)


def mux(
    video: Path,
    manifest_path: Path,
    cues: list[Cue],
    out: Path,
    *,
    allow_overlap: bool = False,
    allow_unapproved: bool = False,
) -> None:
    """Mix the cued clips onto `video`, writing `out`. Refuses drafts, unheard lines and overruns."""
    manifest_path = Path(manifest_path)
    manifest = read_manifest(manifest_path)
    if manifest.draft:
        raise NarrateError(f"{manifest_path}: draft manifest has no audio; build it first")
    if not cues:
        raise NarrateError("no cues to mux")
    lines = {ln.id: ln for ln in manifest.lines}
    unknown = [c.id for c in cues if c.id not in lines]
    if unknown:
        raise NarrateError(f"{manifest_path}: cues for unknown lines: {_names(unknown)}")
    noclip = [c.id for c in cues if not lines[c.id].clip]
    if noclip:
        raise NarrateError(f"{manifest_path}: lines with no clip: {_names(noclip)}")
    unapproved = [c.id for c in cues if lines[c.id].status != "approved"]
    if unapproved and not allow_unapproved:
        raise NarrateError(
            f"{manifest_path}: lines not approved: {_names(unapproved)} (--allow-unapproved to mux anyway)"
        )
    over = overruns(cues, manifest)
    if over and not allow_overlap:
        raise NarrateError("clips overlap:\n  " + "\n  ".join(over) + "\n(--allow-overlap to mux anyway)")
    clips = [((manifest_path.parent / lines[c.id].clip).resolve(), c.start) for c in cues]
    missing = [str(p) for p, _ in clips if not p.is_file()]
    if missing:
        raise NarrateError(f"clip files missing: {', '.join(missing)}")
    out = Path(out).resolve()
    video = Path(video).resolve()
    tmp = out.with_name(out.name + ".tmp.mp4")
    cmd = mux_command(find_ffmpeg(), video, clips, tmp)
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
        if result.returncode != 0:
            tail = "\n".join((result.stderr or "").splitlines()[-STDERR_TAIL:])
            raise NarrateError(f"ffmpeg failed (exit {result.returncode}):\n{tail}")
        replace_with_retry(tmp, out)
    finally:
        tmp.unlink(missing_ok=True)
