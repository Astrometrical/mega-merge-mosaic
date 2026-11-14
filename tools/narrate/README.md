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
```

A plain `uv sync` after `uv sync --extra qwen` removes torch again, so on the
GPU machine use `uv sync --extra qwen` every time (and `uv run --extra qwen`).

```sh
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
second), duration. A second `build` reuses cached takes and takes seconds.

```sh
# Listen, approve, reject, annotate and rebuild in a local web page.
uv run narrate review path/to/x.narration.toml [--no-browser] [--no-build] [--port N]
                                               [--out DIR] [--voices DIR] [--cache DIR] [--approved DIR]
# The pronunciation lab: hear a word in carrier sentences under candidate respellings.
uv run narrate try Kappa --voice dan --spell "KAP-uh" --in "Say {} again." [--to terms.json]
# Mix the approved clips onto a silent video at cue times.
uv run narrate mux --timeline tour/timeline.json -o tour-narrated.mp4
uv run narrate mux --video silent.mp4 --script x.narration.toml -o tour-narrated.mp4   # `at` times from the script
```

`review` serves on 127.0.0.1 only (any free port unless `--port`), builds first
unless `--no-build`, and opens the page unless `--no-browser`. `try` serves a
small page that auditions respellings and writes the chosen one into a
pronunciation list. `mux` needs ffmpeg (`FFMPEG` names it, else it is looked
up on PATH).

## Approval workflow: build, review, then mux

1. `narrate build` generates and checks takes; each line comes out `new`
   (nothing approved yet), `flagged` (failed a check) or `approved`.
2. `narrate review` plays each take beside its text and checks. Approve a
   take to pin it, reject one to bar it from being picked again, add a note, or
   rebuild from the page. "Approve unflagged" approves every `new` line whose
   take is not rejected.
3. `narrate mux` puts only approved lines on the video.

A pin is bound to the sha256 of the approved audio, so a changed or replaced
take is no longer approved until it is approved again. Two stores keep the
text and the voice apart:

- The **lock file**, `<script>.lock.json` next to the script, holds pins,
  rejections and notes. It has no audio: commit it.
- The **approved store**, `NARRATE_APPROVED` (default `~/.narrate/approved/`),
  holds the approved WAVs. It is a real person's voice: private, never
  pruned, never committed.

`mux` refuses, with exit 2 and a message naming the lines: a draft manifest
(no audio); cues for unknown lines; lines with no clip; lines that are not
approved (`--allow-unapproved` overrides); and clips that run past the next
cue's start (`--allow-overlap` overrides; the message gives each overrun in
seconds). Missing clip files and an ffmpeg failure are also exit 2.

Exit codes: 0 ok; 1 a line is flagged (`build` only, unless
`--allow-flagged`); 2 a user error (bad script, profile, timeline or option, a
mux refusal, ffmpeg missing or failing, or the `qwen` extra missing); 3 an
internal error (traceback on stderr). 1 only ever means flagged.

Checks are English-only: Whisper runs in English and text normalisation assumes
English.

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
voice.toml              backend, model, language, precision, checker
                        (default `whisper`), default_style,
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

| What | Where |
|---|---|
| Takes, transcripts | `NARRATE_CACHE`, default `~/.narrate/cache` (safe to delete) |
| Approved takes | `NARRATE_APPROVED`, default `~/.narrate/approved/` (never pruned) |
| Clips and `narration.json` | `--out`, default `<script dir>/out/<script stem>/`; the folder carries its own `.gitignore` (`*`) |
| Approvals | `<script>.lock.json` next to the script (no audio; commit it) |

## Privacy

The reference clips, the profile, the cache and every generated clip are a
real person's voice. They stay out of git and out of any upload: keep
`out/` gitignored (the repo ignores `crates/mmm-app/tours/out/` and `/voice/`),
never commit anything under `~/.narrate`, and never send clips or references
to an outside service. Tests use synthetic audio only.
