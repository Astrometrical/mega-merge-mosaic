# `narrate` Human Loop and Video Implementation Plan (narration project, plan 2 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The person who owns the voice hears every new line before it reaches a video. `narrate review` approves or rejects takes and pins them, `narrate try` picks respellings by ear, and `narrate mux` lays approved narration onto video.

**Architecture:**
- **The approval record:**
  - **The lock file:** it records approvals, rejected takes and notes. It sits next to the script and is committed (text only).
  - **The approved store:** private and outside git. It keeps a durable copy of every pinned take.
- **`build` honours both:**
  - a pinned line is `approved` and reuses its take, with no generation or checking;
  - rejected takes never count;
  - a missing pinned audio file is flagged, never silently replaced.
- **Pages:** `review` and `try` serve one local page each (stdlib `http.server`, 127.0.0.1) from a small shared server module, and write straight to the lock or the pronunciation list.
- **`mux`:** mixes the approved clips onto a silent video, from a timeline (narration-first) or from the script's `at` times (video-first). It checks for overruns and unapproved lines.

**Tech Stack:** the plan 1 stack (Python 3.11, uv, numpy, soundfile, pyloudnorm); stdlib `http.server`, `webbrowser` and `importlib.resources`; ffmpeg as an external tool.

**Spec:** `docs/superpowers/specs/2026-11-14-narration-pipeline-design.md`. This plan covers:
- §5.2 step 8 (approval);
- §5.5 `review`;
- §5.6 `try`;
- §5.7 `mux`;
- the §5.8 approved store and lock;
- §8.5 (timeline);
- §10 item 2.

Plan 1's code is on main: `tools/narrate/`. Its DESIGN.md section is "Narration and tours".

## Global Constraints

- **Plan 1's constraints hold:**
  - Core deps are numpy, soundfile and pyloudnorm only; heavy imports stay in `backends/qwen.py` and `checkers/whisper.py`.
  - Tests need no GPU, model weights, voice recordings or network. They use `narrate.fakes`, the `root` fixture (a path with a space) and the autouse `_isolate` fixture.
  - User errors raise `NarrateError` subclasses whose messages name the file. Exit codes: 0 ok, 1 flagged, 2 user error, 3 internal.
  - `ruff check`, `ruff format --check` and `pytest -q` are clean.
  - Commits use conventional style, ending with the committing model's Co-Authored-By line.
- **The autouse `_isolate` fixture** also sets `NARRATE_APPROVED` to `tmp_path / "approved dir"` (Task 2), so no test touches the real `~/.narrate`.
- **Servers bind 127.0.0.1 only.** They serve only the page, JSON and clips looked up by line id (`[a-z0-9-]+`, present in the manifest). Never a path taken from a request. Request bodies are capped at 64 KiB.
- **ffmpeg** is found through `$FFMPEG`, else `PATH`. A missing ffmpeg is a `NarrateError`, exit 2. Tests that need it are skipped when it's absent; CI installs it.
- **Pinned values:**
  - **Lock and timeline JSON** carry `"version": 1`; readers refuse a newer version with a `NarrateError` naming the file.
  - **Mux filter graph** (spike values): `aresample=48000,adelay={ms}:all=1` per clip, then `amix=inputs=N:normalize=0:dropout_transition=0,loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000`. Output: `-c:v copy -c:a aac -b:a 192k -movflags +faststart`.
  - **Overrun tolerance:** `OVERRUN_TOLERANCE = 0.001` s.
  - **`try`:** 2 takes per carrier sentence (`TRY_TAKES = Takes(count=2, max=2)`, passed as `BuildOptions.takes`). The default carriers are `"Next up is {}."`, `"Everyone here has heard of {}."` and `"That brings us back to {}, again."`. Ruled at the final review: the words before the term differ ("is", "of", "to"), which meets spec §5.6 better than the first pinned set, where "with" repeated.
- **Anti-goal (spec §2):** no unheard line reaches a video. `mux` refuses unapproved lines unless `--allow-unapproved` is passed.

## Review Focus

1. **A take deleted from the cache after approval:** the line stays `approved` (it plays from the approved store). Covered by Task 3's `test_approved_line_survives_cache_loss`.
2. **Pinned audio missing everywhere,** for example on a new machine: the line comes back `new` with the flag `pinned-audio-missing`, never silently approved. Covered by Task 3's `test_pinned_audio_missing_is_flagged`.
3. **Editing a line's text after approval:** the key changes, the pin no longer applies, and the line is `new` again. Covered by Task 3's `test_edit_unpins`.
4. **Request paths that try to escape** (`/clips/..%2Fnarration.json`, unknown ids, oversized bodies): 404 or 413, never a file outside `clips/`. Covered by Task 4's `test_clip_paths_cannot_escape` and `test_oversized_body_413`.
5. **Muxing a draft manifest, an unapproved line, or a clip that overruns the next cue:** a clear error naming the lines, unless the matching `--allow-*` flag is given. Covered by Task 7's `test_mux_refusals`.

---

### Task 1: Hardened manifest reading and profile names

**Files:**
- Modify: `tools/narrate/src/narrate/manifest.py` (`read_manifest`), `tools/narrate/src/narrate/profile.py` (`load_profile`)
- Test: `tools/narrate/tests/test_manifest.py`, `tools/narrate/tests/test_profile.py`

**Interfaces:**
- Produces: `read_manifest(path)`, which raises `NarrateError` naming the file for:
  - unreadable or invalid JSON;
  - a missing, non-integer or bool `version` ("unsupported manifest version …");
  - a newer version;
  - a missing required field (naming the field and, where it applies, the line id).

  `load_profile(name)` raises `ProfileError` unless `name` matches `[A-Za-z0-9_-]+`.

- [ ] **Step 1: Write the failing tests**
  - **`test_read_manifest_errors`,** parametrized: invalid JSON, `{"version": true}`, `{"version": "1"}`, `{"version": 2}`, and a line missing `duration`. Each raises a `NarrateError` matching the file name; the last also matches `duration`.
  - **`test_profile_name_must_be_simple`:** the names `../x`, `a/b` and `""` each raise `ProfileError`.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement the changes.**
- [ ] **Step 4: Run them.** `uv run pytest -q` and ruff are clean.
- [ ] **Step 5: Commit.** `fix(narrate): strict manifest reading; simple profile names`

### Task 2: The lock file and the approved store

**Files:**
- Create: `tools/narrate/src/narrate/lock.py`, `tools/narrate/src/narrate/approved.py`
- Modify: `tools/narrate/tests/conftest.py`: the autouse `_isolate` also sets `NARRATE_APPROVED`
- Test: `tools/narrate/tests/test_lock.py`, `tools/narrate/tests/test_approved.py`

**Interfaces:**
- Produces (`lock.py`):
  - **`lock_path(script_path: Path) -> Path`:** the script's file name with `.toml` replaced by `.lock.json`, so `ngc7000.narration.toml` gives `ngc7000.narration.lock.json`.
  - **`Pin`,** frozen: `key: str`, `take: int`, `duration: float`, `heard: str`, `cer: float | None`, `pace: float | None`, `approved_at: str` (UTC ISO 8601, ending `Z`).
  - **`Lock`,** with fields `path: Path`, `pins: dict[str, Pin]` (by line id), `rejected: dict[str, set[int]]` (by take key) and `notes: dict[str, str]` (by line id), and these methods:
    - `Lock.load(path) -> Lock`: a missing file gives an empty lock; a malformed file or newer version raises `NarrateError` naming the file.
    - `save()`: atomic (tmp then `os.replace`), `indent=1`, sorted keys, `"version": 1` first, rejected takes as sorted lists.
    - `pin(line_id, key, take, duration, heard, cer, pace)`: stamps `approved_at`, and removes `take` from `rejected[key]`.
    - `unpin(line_id)`.
    - `reject(key, take)`: adds the take, and unpins every line pinned to `(key, take)`.
    - `note(line_id, text)`: empty text removes the note.
  - **File shape:** `{"version": 1, "lines": {id: {pin fields}}, "rejected": {key: [n, …]}, "notes": {id: str}}`.
- Produces (`approved.py`):
  - `approved_root(override: Path | None = None) -> Path`: `override`, else `$NARRATE_APPROVED`, else `~/.narrate/approved`.
  - `ApprovedStore(root)`, with:
    - `path(key, take) -> Path`, which is `root / key[:2] / key / f"take-{take}.wav"`;
    - `has(key, take) -> bool`;
    - `put(key, take, src: Path) -> Path`: an atomic copy;
    - `load(key, take) -> tuple[np.ndarray, int]`.

- [ ] **Step 1: Write the failing tests**
  - **`test_lock_round_trip`:** pin, reject, note, then save, load and compare.
  - **`test_missing_lock_is_empty`.**
  - **`test_reject_unpins`.**
  - **`test_pin_clears_rejection`.**
  - **`test_lock_version_guard`.**
  - **`test_lock_path`.**
  - **`test_store_put_and_load`:** a FLOAT WAV copied with `put`, then read back identical.
  - **`test_approved_root_resolution`:** override, then env, then home.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `lock.py` and `approved.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): approval lock file and approved-take store`

### Task 3: `build` honours approvals and rejections

**Files:**
- Modify: `tools/narrate/src/narrate/build.py`, `tools/narrate/src/narrate/manifest.py` (status values documented)
- Test: `tools/narrate/tests/test_build.py`, extended (or `tests/test_build_approval.py`)

**Interfaces:**
- Consumes: `Lock`, `lock_path`, `ApprovedStore` and `approved_root` (Task 2).
- Produces:
  - **`BuildOptions`** gains `approved: Path | None = None`.
  - **Line status** is `approved`, `new`, `flagged` or `draft`.
  - **Flags** can now include `pinned-audio-missing`.
- **Rules** (spec §5.2 step 8; §5.8):
  1. Load `Lock.load(lock_path(script.path))` after planning.
  2. **A pin applies** when `lock.pins[line.id].key == plan.key`. Then:
     - if the approved store has `(key, take)`, use that audio: status `approved`, and the `check` comes from the pin (`heard`, `cer`, `pace`, `take`, `key`). Nothing is generated or checked for that line;
     - else, if the take cache has `(key, take)`, copy it into the approved store and use it, as above;
     - else, the line goes through normal selection and gets the flag `pinned-audio-missing`. Its status is `new` or `flagged`, as selection decides.
  3. **A pin that doesn't apply** (the key changed) is ignored, and the line is selected normally. The lock is not modified by `build`.
  4. **Rejected takes** (`lock.rejected[plan.key]`) are never generated over, never checked and never chosen.
     - **Round 1** considers the line's non-rejected takes until there are `takes.count` of them. Existing ones come first, by number; any missing ones are generated with numbers above the highest number in the cache or the rejected set.
     - **Round 2** does the same up to `takes.max` non-rejected takes.
     - Batching and closing work as in plan 1.
  5. **Pinned lines don't count toward the VRAM rounds:** they never cause generation or checks.

- [ ] **Step 1: Write the failing tests.** They use `voice_library` and a patched `FakeWorld`, and pin by writing a `Lock` directly.
  - **`test_pinned_line_is_approved_without_work`:** build, pin line A's chosen take, then rebuild. A is `approved` with the same `check.take`, and there are no `synthesize` or `transcribe` events for A's spoken text.
  - **`test_approved_line_survives_cache_loss`:** pin, delete the cache's `takes/` folder, rebuild. Still `approved`, with the clip audio equal to the approved copy, after the same trim and loudness.
  - **`test_pin_repairs_the_store_from_cache`:** pin with an empty approved store; the rebuild copies the take into the store, and the line is `approved`.
  - **`test_pinned_audio_missing_is_flagged`:** pin, then delete both the cache and the store. The line is `new` with flag `pinned-audio-missing`.
  - **`test_edit_unpins`:** pin, change the line's text, rebuild. The line is `new`, and the lock still holds the old pin.
  - **`test_rejected_take_is_never_chosen`:**
    - script heard values so that take 1 scores best;
    - reject take 1 and rebuild: the chosen take is 2 or 3, and take 4 is generated so that 3 non-rejected takes exist;
    - `world.calls` shows exactly one new generation for that line.
  - **`test_rejections_in_round_2`:** a garbled line with takes 1–3 rejected generates takes 4–6 in round 1, then 7–9 in round 2 if still flagged.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement the changes in `build.py`.**
- [ ] **Step 4: Run them.** `uv run pytest -q` and ruff are clean, and plan 1's build tests still pass.
- [ ] **Step 5: Commit.** `feat(narrate): build honours approvals, the approved store and rejected takes`

### Task 4: The local server and the review API

**Files:**
- Create: `tools/narrate/src/narrate/server.py` (shared), `tools/narrate/src/narrate/review.py`
- Test: `tools/narrate/tests/test_server.py`, `tools/narrate/tests/test_review.py`

**Interfaces:**
- Produces (`server.py`):
  - **`LocalServer(ThreadingHTTPServer)`:** binds `("127.0.0.1", port)` (port 0 picks a free one), and has a `url` property.
  - **`Routes`:** maps `(method, path)` to a handler. A handler gets the parsed JSON body (`POST`) or `None`, and returns `(status, content_type, bytes)`.
    - Unknown routes give 404.
    - Bodies over 64 KiB give 413.
    - Invalid JSON gives 400.
    - A `NarrateError` gives 400, as `{"error": msg}`.
    - Any other exception gives 500, as `{"error": "internal error"}`, with the traceback on stderr.
  - **`json_response(obj, status=200)`:** writes strict JSON (non-finite numbers become null).
  - **`serve_clip(out_dir, ids, request_path)`:** returns the WAV bytes only when the path is exactly `/clips/<id>.wav` and `<id>` is in `ids`; anything else gives 404.
- Produces (`review.py`):
  - `ReviewApp(script_path: Path, options: BuildOptions, backend=None, checker=None)` holds the out dir, manifest and lock.
  - `routes() -> Routes`, serving:

    | Route | What it does |
    |---|---|
    | `GET /` | The page (Task 5; until then a placeholder string) |
    | `GET /api/state` | `{"manifest": …, "lock": {"pins": {id: pin}, "notes": {…}, "rejected": {key: [n]}}, "building": bool}` |
    | `GET /clips/<id>.wav` | The clip, through `serve_clip` |
    | `POST /api/approve {"id"}` | Pins the line's current take; returns the state |
    | `POST /api/reject {"id"}` | Rejects the line's current take; returns the state |
    | `POST /api/note {"id", "note"}` | Saves the note; returns the state |
    | `POST /api/approve-unflagged` | Pins every line whose status is `new`; returns the state |
    | `POST /api/rebuild` | Runs `build` and re-reads the manifest; returns the state |

  - **Approving** copies the take from the take cache into the approved store, using the manifest's `check.key` and `check.take`, then calls `lock.pin` and `lock.save`. If the take is in neither the cache nor the store, it's a 400 error.
  - **Rebuild** holds a lock; a rebuild already running gives 409.
  - **Errors:** an unknown id, a draft manifest, or a line with no `check` is a 400 `NarrateError`.

- [ ] **Step 1: Write the failing tests.** These run a real `LocalServer` on port 0 in a thread, call it with `urllib.request`, and use fakes.
  - **`test_state_lists_lines`.**
  - **`test_approve_pins_and_copies`:** the lock file on disk has the pin, the approved store has the take, and the next `/api/state` shows it.
  - **`test_reject_then_rebuild_picks_another_take`.**
  - **`test_note_round_trip`.**
  - **`test_approve_unflagged_skips_flagged`.**
  - **`test_rebuild_conflict_409`:** a slow fake backend, with two concurrent rebuilds.
  - **`test_clip_paths_cannot_escape`:** `/clips/..%2Fnarration.json`, `/clips/nope.wav` and `/clips/open.wav/../x` all give 404.
  - **`test_oversized_body_413`.**
  - **`test_bad_json_400`.**
  - **`test_binds_localhost_only`.**
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `server.py` and `review.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): local review server and API`

### Task 5: The review page and `narrate review`

**Files:**
- Create: `tools/narrate/src/narrate/pages/review.html` (package data, read with `importlib.resources`)
- Modify: `tools/narrate/src/narrate/review.py` (serve the page), `tools/narrate/src/narrate/cli.py` (`review` subcommand), `tools/narrate/pyproject.toml` (if needed to include `pages/*.html`)
- Test: `tools/narrate/tests/test_review.py`, `tools/narrate/tests/test_cli.py`

**Interfaces:**
- **CLI:** `narrate review SCRIPT [--out DIR] [--voices DIR] [--cache DIR] [--approved DIR] [--port N] [--no-browser] [--no-build]`.
  - It builds first, with flagged lines allowed, unless `--no-build`.
  - It starts the server, prints `Reviewing <script> at <url> (Ctrl+C to stop)`, and opens the browser unless `--no-browser`.
  - It serves until interrupted, then exits 0. Build errors keep their usual exit codes.
- **The page:** one self-contained HTML file with inline CSS and JS; no external resources.
  - **Look:** light and dark themes (CSS custom properties with `prefers-color-scheme`), in the look of the listening pages (`voice/pages/round4/listen4.html` in the main checkout, for reference).
  - **Header:** counts (approved, new, flagged), "Approve all unflagged", and "Rebuild". Rebuild is enabled when any line's current take has been rejected since the last build. A status line shows the build state and any error.
  - **Order:** `flagged`, then `new`, then `approved`.
  - **Each line shows:**
    - id and status badge;
    - an `<audio>` player for `/clips/<id>.wav?take=<n>`, so a new take busts the cache;
    - `text`, and the spoken form when it differs;
    - what Whisper heard, pace and flags;
    - for passages, the sentence captions, with the current one highlighted from the player's `timeupdate`;
    - Approve and Reject buttons;
    - a note field saved on blur.
  - **Every action** posts to the API and re-renders from the returned state.

- [ ] **Step 1: Write the failing tests**
  - **`test_page_is_served`:** `GET /` returns HTML that references `/api/state`, `/api/approve`, `/api/reject`, `/api/note`, `/api/approve-unflagged` and `/api/rebuild`.
  - **`test_cli_review_no_browser`:** `main(["review", script, "--no-browser", "--port", "0"])` runs in a thread, the test reaches `/api/state`, then the server is stopped. Give `review.py` a stop hook for tests, for example `serve(…, ready=callback)` handing back the server.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement the page and the subcommand.**
- [ ] **Step 4: Run them; they pass.** Then a manual check: run `narrate review` on a fake-profile script with `--no-browser`, load the page headless with `msedge --headless=new --dump-dom <url>`, and confirm the lines render.
- [ ] **Step 5: Commit.** `feat(narrate): review page and the review command`

### Task 6: `narrate try`, the pronunciation lab

**Files:**
- Create: `tools/narrate/src/narrate/trylab.py`, `tools/narrate/src/narrate/pages/try.html`
- Modify: `tools/narrate/src/narrate/cli.py` (`try` subcommand)
- Test: `tools/narrate/tests/test_trylab.py`

**Interfaces:**
- **CLI:** `narrate try WORD --voice V [--spell S]... [--in CARRIER]... [--style S] [--to LIST] [--port N] [--no-browser]`.
  - **Candidates:** `WORD` as written first, then each `--spell`, deduplicated.
  - **Carriers:** the `--in` values (each must contain `{}`), else the three defaults.
  - **`--to`:** the list file to write, defaulting to the profile's first pronunciation list. A missing file is created as `{"about": "…", "entries": [], "fine_as_written": []}`.
- **`trylab.prepare(word, spellings, carriers, voice, style, workdir) -> Path`:** writes a generated script to `workdir`:
  - `voice = V`, with lines `c{i}-s{j}`;
  - each line's `text` is the carrier with `WORD` as written, and its `say` is the carrier with the candidate;
  - each line has `style` when given and `kind = "line"`.

  `prepare` returns the script path. Then `build` runs on it with plan 1 and Task 3's machinery, unchanged.
- **`TryApp(script, word, candidates, list_path)`** serves:
  - `GET /`;
  - `GET /api/state`: the candidates under blind labels A, B, C…, shuffled with a seed derived from `word` (not shown to the user), with each candidate's clips per carrier;
  - `GET /clips/<id>.wav`;
  - `POST /api/pick {"choice": "A" | … | "none"}`.
- **Pick rules:**
  - **The as-written word:** remove any entry whose `match` is `\bWORD\b` (with `WORD` escaped by `re.escape`), and add `WORD` to `fine_as_written`.
  - **A respelling:** replace or add the entry `{"match": "\\b<escaped WORD>\\b", "say": <spelling>, "why": "picked with narrate try on <YYYY-MM-DD> from N spellings in M sentences"}`.
  - **`none`:** change nothing.
  - **Write** atomically, and respond with the list's new contents.
- **The page:** for each blind candidate, its clips (one per carrier) and a "Closest" button; plus "None of these". Same look as the review page.

- [ ] **Step 1: Write the failing tests**
  - **`test_prepare_writes_a_script`:** loads with `load_script`; 3 carriers × 2 candidates gives 6 lines, with `say` set.
  - **`test_try_end_to_end_with_fakes`:** prepare, build, then the server's state has blind labels that don't reveal the spellings in the order given.
  - **`test_pick_respelling_writes_entry`.**
  - **`test_pick_as_written_marks_fine`.**
  - **`test_pick_none_changes_nothing`.**
  - **`test_bad_carrier_without_braces_is_a_user_error`.**
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `trylab.py`, the page and the subcommand.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): narrate try, the pronunciation lab`

### Task 7: `narrate mux`

**Files:**
- Create: `tools/narrate/src/narrate/mux.py`
- Modify: `tools/narrate/src/narrate/cli.py` (`mux` subcommand), `.github/workflows/ci.yml` (the narrate job runs `sudo apt-get install -y ffmpeg` before the tests)
- Test: `tools/narrate/tests/test_mux.py`

**Interfaces:**
- Produces:
  - `Cue(id: str, start: float)`, frozen.
  - **`load_timeline(path) -> tuple[Path, Path, list[Cue]]`:** returns the video, the manifest and the cues. Paths are relative to the timeline file (spec §8.5); a newer version is refused.
  - **`script_cues(script: Script) -> list[Cue]`:** comes from each line's `at`. Lines without `at` are a `NarrateError` listing their ids.
  - **`overruns(cues, manifest) -> list[str]`:** sorts the cues by start, and gives one message per cue whose `start + duration` exceeds the next cue's start by more than `OVERRUN_TOLERANCE`, naming both ids and the overlap in seconds.
  - **`find_ffmpeg() -> str`.**
  - **`mux_command(ffmpeg, video, clips: list[tuple[Path, float]], out) -> list[str]`:** the exact pinned filter graph and output options. Clip delays are rounded to whole milliseconds.
  - **`mux(video, manifest_path, cues, out, *, allow_overlap=False, allow_unapproved=False) -> None`.**
    - **Refusals** (`NarrateError`): a draft manifest; a cue id not in the manifest; a line without a clip; unapproved lines (status other than `approved`) unless `allow_unapproved`; overruns unless `allow_overlap`. Every message names the lines.
    - **Running ffmpeg:** a non-zero exit is a `NarrateError` with the last 20 lines of stderr.
- **CLI:**
  - `narrate mux --timeline T -o OUT [--allow-overlap] [--allow-unapproved]`;
  - `narrate mux --video V --script S [--manifest M] -o OUT [...]`, where `--manifest` defaults to `default_out(script) / "narration.json"`.
  - Exactly one of `--timeline` or `--video`/`--script` is allowed.

- [ ] **Step 1: Write the failing tests**
  - **`test_mux_command_is_exact`:** the full argv for 2 clips at 0.15 s and 3.4 s.
  - **`test_overruns`:** a clip of 3.0 s at 0.0, with the next cue at 2.5, gives one message naming both ids and 0.5 s.
  - **`test_load_timeline_and_script_cues`.**
  - **`test_mux_refusals`,** parametrized: draft, unknown id, unapproved, overrun. Each raises, and each passes with its `allow_*` flag where one exists.
  - **`test_mux_end_to_end`:** skipped when ffmpeg is missing. Make a 3 s black video with ffmpeg (`-f lavfi -i color=c=black:s=320x240:d=3`), plus two synthetic clips and an approved manifest, then mux. The output exists and `ffmpeg -i` reports an audio stream.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `mux.py`, the subcommand and the CI change.**
- [ ] **Step 4: Run them.** With ffmpeg found through `$FFMPEG` (on this machine: `C:/Users/monar/AppData/Local/Microsoft/WinGet/Packages/Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe/ffmpeg-9.0.2-full_build/bin/ffmpeg.exe`), the end-to-end test passes.
- [ ] **Step 5: Commit.** `feat(narrate): mux narration onto video, with overrun and approval checks`

### Task 8: Docs and the manual smoke run

**Files:**
- Modify: `tools/narrate/README.md`, `docs/DESIGN.md` (the "Narration and tours" section), `CLAUDE.md` (Commands, if a line helps)

- [ ] **Step 1: Smoke in a throwaway copy, never the real lock.** Copy `crates/mmm-app/tours/ngc7000.narration.toml` and `terms.json` to a scratch folder outside the repo, and build with `uv sync --extra qwen`. The takes are cached from plan 1, so this is near-instant.
  - Start `narrate review --no-browser --no-build` on the copy, and approve all 8 lines through `POST /api/approve-unflagged`.
  - Confirm: the copy's lock has 8 pins; the approved store has 8 takes; and a rebuild reports all `approved`, with no synthesize or transcribe.
- [ ] **Step 2: Mux smoke.** Make a timeline from the spike's render:
  - **Video:** `C:/Users/monar/AppData/Local/Temp/claude/0ba026e137bbc718/4dcec9ee-9220-4b98-96ef-57c468a54d18/scratchpad/tts/tour-qwen3-tts/out/tour_silent.mp4`.
  - **Cues:** that folder's `tour.vtt` gives the cue starts; each cue's start is its VTT start plus 0.15 s, with ids in script order.
  - Run `narrate mux --timeline … -o <scratch>/tour-narrated.mp4 --allow-overlap`; the spike's pacing used older clips, so overlaps are expected and get reported.
  - Report the overruns, the output's duration, and that it has an audio stream.
  - If the spike files are gone, say so and skip this step.
- [ ] **Step 3: Docs.**
  - **README:** the review, try and mux commands; the lock file (committed) against the approved store (private); the approval workflow; and the mux refusals.
  - **DESIGN.md:** what plan 2 built, and its rulings.
- [ ] **Step 4: Run the suites.** `uv sync` (back to core), then `uv run pytest -q` and ruff are clean.
- [ ] **Step 5: Commit.** `docs(narrate): review, try and mux`
