# narrate

Standalone narration tool: a script of lines in, loudness-normalised voice-over
clips and a `narration.json` manifest out. It is generic (no mmm dependency);
the design is in
[`docs/superpowers/specs/2026-11-14-narration-pipeline-design.md`](../../docs/superpowers/specs/2026-11-14-narration-pipeline-design.md).

## Install

```sh
cd tools/narrate
uv sync                 # core: script, text, audio, manifest, draft (no GPU)
uv sync --extra qwen    # adds torch, Qwen3-TTS and Whisper for real generation
uv run pytest -q        # tests need no GPU, weights or voice recordings
```

Generation needs an NVIDIA GPU (developed on an RTX 2080 Ti, 11 GB). The first
`build` downloads Qwen3-TTS and whisper-large-v3-turbo into the Hugging Face
cache. Qwen3-TTS wants SoX on the PATH; without it you get a harmless warning.

## Commands

```sh
# Timing-only manifest from a syllable-rate estimate: no GPU, no audio.
uv run narrate draft path/to/x.narration.toml [--pace 4.8] [--out DIR]

# Generate takes, check each with Whisper, pick, write clips and manifest.
uv run narrate build path/to/x.narration.toml [--out DIR] [--voices DIR]
                                              [--cache DIR] [--allow-flagged]
```

`build` prints one row per line: id, status, take, CER, pace (syllables per
second), duration. It exits non-zero if any line is flagged, unless
`--allow-flagged`. A second `build` reuses cached takes and takes seconds.

Try it on the tour script:
`uv run narrate build ../../crates/mmm-app/tours/ngc7000.narration.toml`.

## Script

A `.narration.toml` names a `voice`, a `style` and optional `pronunciations`
(JSON lists, paths relative to the script), then `[[line]]` tables with a
unique `id` (`[a-z0-9-]+`) and `text`. See spec section 8.1.

## Voice library

Profiles live in `~/.narrate/voices/<name>/` (override with `NARRATE_VOICES`
or `--voices`):

```
voice.toml              backend, model, language, precision, default_style,
                        [style.<name>] short/long reference (audio + exact
                        transcript), [pace], [takes]
refs/*.wav              the reference clips
pronunciations.json     the speaker's own list
```

Pronunciation lists are JSON, `{"about", "entries": [{"match", "say", "why"}]}`:
`match` is a case-sensitive regex applied in order, `say` the replacement
(group references allowed). The speaker's list holds fixes that follow the
voice; the project's list (for example `crates/mmm-app/tours/terms.json`) holds
the project's names. Spec sections 8.2 and 8.3.

## Where files go

The approval rows describe the design (spec section 5.8); the approvals
commands are not built yet, so `build` does not write them.

| What | Where |
|---|---|
| Takes, transcripts | `NARRATE_CACHE`, default `~/.narrate/cache` (safe to delete) |
| Approved takes | `NARRATE_APPROVED`, default `~/.narrate/approved/` (never pruned) |
| Clips and `narration.json` | `--out`, default `<script dir>/out/<script stem>/` |
| Approvals | `<script>.lock.json` next to the script (no audio; commit it) |

## Privacy

The reference clips, the profile, the cache and every generated clip are a
real person's voice. They stay out of git and out of any upload: keep
`out/` gitignored (the repo ignores `crates/mmm-app/tours/out/` and `/voice/`),
never commit anything under `~/.narrate`, and never send clips or references
to an outside service. Tests use synthetic audio only.
