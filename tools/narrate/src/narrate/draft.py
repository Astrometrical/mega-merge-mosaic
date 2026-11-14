"""Draft manifests: timings estimated from syllables, no audio and no voice needed."""

from __future__ import annotations

from pathlib import Path

from narrate.build import default_out
from narrate.captions import Caption, share_captions
from narrate.lexicon import Lexicon
from narrate.manifest import Manifest, ManifestLine
from narrate.script import load_script
from narrate.text import infer_kind, syllables

PACE_TARGET = 4.8  # syllables per second


def draft(script_path: Path, *, out: Path | None = None, pace_target: float = PACE_TARGET) -> Manifest:
    """Write `<out>/narration.json` with estimated durations; only the script's own lists apply."""
    script = load_script(script_path)
    lexicon = Lexicon.load(list(script.pronunciations))
    lines = []
    for line in script.lines:
        spoken = line.say or lexicon.apply(line.text)
        kind = line.kind or infer_kind(line.text)
        duration = syllables(spoken) / pace_target
        captions = [Caption(line.text, 0.0, duration)] if kind == "line" else share_captions(line.text, duration)
        lines.append(
            ManifestLine(
                id=line.id,
                text=line.text,
                spoken=spoken,
                kind=kind,
                clip=None,
                duration=duration,
                at=line.at,
                status="draft",
                captions=captions,
            )
        )
    manifest = Manifest(script.path.name, script.voice, True, None, lines=lines)
    manifest.write((out if out is not None else default_out(script)) / "narration.json")
    return manifest
