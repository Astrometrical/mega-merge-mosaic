# `narrate` Core Implementation Plan (narration project, plan 1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `narrate build SCRIPT` turns a narration script into checked voice clips and a `narration.json` manifest, and `narrate draft SCRIPT` writes a timing-only manifest.

**Architecture:** a Python package at `tools/narrate/`, with its CLI behind small focused modules. The modules cover:
- parsing: script, profile and pronunciation lists;
- text: spoken forms, sentences, syllables, normalisation and error rates;
- audio: speech span, pauses, pace, trimming and loudness;
- sentence captions;
- a content-addressed take cache, and batching within a padded-size budget;
- take selection, and the build orchestration.

The model (Qwen3-TTS) and the checker (Whisper) sit behind two small interfaces, with deterministic fakes for tests. Heavy imports (torch, transformers, qwen_tts) live only in `backends/qwen.py` and `checkers/whisper.py`.

**Tech Stack:**
- **Runtime:** Python 3.11 under uv; numpy, soundfile, pyloudnorm.
- **`[qwen]` extra:** qwen-tts 0.1.1, torch 2.8.0 (cu126), transformers 4.57.3.
- **Dev:** pytest, ruff.

**Spec:** `docs/superpowers/specs/2026-11-14-narration-pipeline-design.md`. This plan covers:
- §5.1–§5.4 (inputs, build, boundaries, pace);
- §5.7 `draft`;
- §5.8 for the cache and outputs;
- §8.1–§8.4;
- §10 item 1.

The lock file, the approved store, `review`, `try` and `mux` are plan 2. `mega-tour` is plan 3.

**Reference code:** the spike scripts in the main checkout's `voice/tools/` (untracked):
- `gen2_qwen.py`: Qwen loading, prompts, batching and seeds;
- `eval4.py`: Whisper, normalisation, error rate and tie-break;
- `voice/README.md`.

Read them for API details; don't copy their structure.

## Global Constraints

- **Location and tooling:** everything new lives under `tools/narrate/`, except the CI job, the docs and Task 13's tour files.
  - **Commands:** run `uv` from `tools/narrate/`. On this machine uv is `~/AppData/Roaming/Python/Python313/Scripts/uv.exe` when it's not on `PATH`.
- **Dependencies:**
  - **Core:** `numpy>=2`, `soundfile>=0.13` and `pyloudnorm>=0.1.1` only.
  - **Heavy imports** (`torch`, `transformers`, `qwen_tts`) happen only inside `narrate/backends/qwen.py` and `narrate/checkers/whisper.py`. Importing anything else must not import torch.
- **Tests:**
  - **Inputs:** no GPU, model weights, voice recordings or network. Use `narrate.fakes`. Build test directories under a path containing a space (the `root` fixture, Task 2).
  - **GPU tests** are marked `@pytest.mark.gpu` and skipped unless `NARRATE_GPU_TESTS=1`.
- **Clean:** `uv run ruff check`, `uv run ruff format --check` and `uv run pytest -q` all pass. The Rust workspace stays untouched.
- **Privacy:** voice audio is written only to the cache and voice library (both outside the repo) and to gitignored `out/` dirs. Nothing is uploaded.
- **Pinned values** (spec §3, §5): named module constants, exactly these:
  - **Checks:** `MAX_CER = 0.04`.
  - **Pace:** `pace.max` defaults to 5.3 and `pace.target` to 4.8.
  - **Takes:** `takes.count` defaults to 3 and `takes.max` to 6.
  - **Batching:** `MAX_ROWS = 6`, and `budget(ref_secs) = int(56_000 / (ref_secs + 13.4))` characters (rows × longest text).
  - **Pacing defaults:** `LEAD = 0.15` s and `BREATH = 0.4` s, written to the manifest.
  - **Clips:** `LOUDNESS_LUFS = -20.0`, `PEAK_DBFS = -1.0`, `TRIM_PAD = 0.05` s.
  - **Silence analysis:**
    - `PAUSE_MIN = 0.25` s.
    - `TOP_DB = 35.0`, `FRAME = 2048`, `HOP = 512`, `AMIN = 1e-5`.
  - **Kind inference:** `PASSAGE_SENTENCES = 3`, `PASSAGE_CHARS = 250`.
- **File formats:**
  - **Manifest:** `"version": 1`, with the exact §8.4 field names.
  - **Takes:** stored as 32-bit float WAV, so they round-trip exactly.
  - **Clips:** 16-bit PCM WAV.
- **Error types and exit codes:**
  - **Error types:** user errors (bad script, profile or list) raise `ScriptError`, `ProfileError` or `LexiconError` (all subclasses of `NarrateError(ValueError)`). Messages name the file, and the line id or entry where there is one.
  - **Exit codes:** 0 ok, 1 flagged lines, 2 user error.
- **Commits:** conventional style, e.g. `feat(narrate): …`. End messages with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **A build killed mid-generation:** no half-written take is ever read back, and the next build resumes. Covered by Task 9's `test_tmp_files_are_ignored`.
2. **Typographic characters in a line** (curly apostrophes, `…`, em dashes): they normalise the same as plain ASCII, and captions keep the original text. Covered by Task 3's `test_norm_typography`.
3. **A silent or garbled take** (empty transcript, all-silent audio): it fails cleanly, with error 1.0 and pace `inf`, and never crashes. Covered by Task 6's `test_pace_of_silence_is_inf` and Task 11's `test_empty_transcript_flags_line`.
4. **A missing voice, or a style or length the profile lacks:** a clear error before any generation, naming the line, style or searched path. Covered by Task 5's tests and Task 11's `test_missing_style_fails_before_generation`.
5. **A one-word line** (under 0.4 s of speech): loudness normalisation, pace and captions all work. Covered by Task 6's `test_normalise_short_clip` and Task 11's `test_one_word_line`.

---

### Task 1: Package skeleton and CI job

**Files:**
- Create: `tools/narrate/pyproject.toml`, `tools/narrate/.python-version` (`3.11`), `tools/narrate/.gitignore` (`.venv/`, `.pytest_cache/`, `.ruff_cache/`), `tools/narrate/uv.lock`
- Create: `tools/narrate/src/narrate/__init__.py` (`__version__ = "0.1.0"`, plus `NarrateError(ValueError)`), `tools/narrate/src/narrate/__main__.py`, `tools/narrate/src/narrate/cli.py`
- Create: `tools/narrate/tests/test_cli.py`
- Modify: `.github/workflows/ci.yml`: add a `narrate` job

**Interfaces:**
- Produces:
  - `narrate.NarrateError`;
  - `narrate.cli.main(argv: list[str] | None = None) -> int`, an argparse CLI with subcommands added by later tasks;
  - the console script `narrate = "narrate.cli:main"`.

- [ ] **Step 1: Write `pyproject.toml`**
  - **Build and project:** hatchling build. `[project]` has name `narrate`, version `0.1.0`, `requires-python = ">=3.11"` and the three core dependencies.
  - **`[project.optional-dependencies]`:** `qwen = ["qwen-tts==0.1.1", "torch==2.8.0", "torchaudio==2.8.0", "transformers==4.57.3", "accelerate>=1.12"]`.
  - **Dev group:** `[dependency-groups] dev = ["pytest>=8", "ruff>=0.8"]`.
  - **Torch index:** `[[tool.uv.index]] name = "pytorch-cu126"`, `url = "https://download.pytorch.org/whl/cu126"`, `explicit = true`.
  - **Torch sources:** `[tool.uv.sources]` sets `torch` and `torchaudio` to `{ index = "pytorch-cu126", marker = "sys_platform == 'win32' or sys_platform == 'linux'" }`.
  - **Ruff:** `line-length = 120`, `target-version = "py311"`, lint `select = ["E", "F", "I", "B", "UP"]`.
  - **Pytest:** `testpaths = ["tests"]`, and a marker `gpu: needs a CUDA GPU, model weights and a voice profile (NARRATE_GPU_TESTS=1)`.

- [ ] **Step 2: Write the failing test**

```python
def test_version(capsys):
    assert main(["--version"]) == 0
    assert capsys.readouterr().out.strip() == "narrate 0.1.0"

def test_no_command_prints_usage_and_returns_2(capsys):
    assert main([]) == 2
```

- [ ] **Step 3: Implement `cli.main`.** It uses argparse with `--version`. With no subcommand it prints usage and returns 2. `__main__.py` calls `sys.exit(main())`.

- [ ] **Step 4: Lock, lint, test**

Run: `uv lock && uv sync && uv run ruff check && uv run ruff format --check && uv run pytest -q`
Expected: the lock resolves (the qwen extra included) and 2 passed.

- [ ] **Step 5: Add the CI job.** Add it to `.github/workflows/ci.yml` beside `test`:
  - **Job:** `narrate`, named `narrate (python)`, on `ubuntu-latest`, with `defaults.run.working-directory: tools/narrate`.
  - **Steps:** `actions/checkout@v4`, then `astral-sh/setup-uv@v6` with `enable-cache: true`.
  - **Commands:** `uv sync --locked`, `uv run ruff check`, `uv run ruff format --check`, `uv run pytest -q`.

- [ ] **Step 6: Commit**

```bash
git add tools/narrate .github/workflows/ci.yml
git commit -m "feat(narrate): package skeleton, CLI entry point and CI job"
```

### Task 2: Narration scripts

**Files:**
- Create: `tools/narrate/src/narrate/script.py`
- Create: `tools/narrate/tests/conftest.py`, `tools/narrate/tests/test_script.py`

**Interfaces:**
- Produces:
  - `Line(id: str, text: str, style: str | None, kind: str | None, at: float | None, say: str | None)`, a frozen dataclass.
  - `Script(path: Path, voice: str | None, style: str | None, pronunciations: tuple[Path, ...], lines: tuple[Line, ...])`, frozen. Its `pronunciations` are absolute, resolved against the script's folder. Its `stem` property is the file name up to the first `.`, so `ngc7000.narration.toml` gives `ngc7000`.
  - `ScriptError(NarrateError)`.
  - `load_script(path: Path) -> Script`.
- Produces (fixtures in `conftest.py`, reused by later tasks):
  - `root(tmp_path) -> Path`, which is `tmp_path / "with space"`, created;
  - `write_script(root, body: str, name="demo.narration.toml") -> Path`;
  - **an autouse `_isolate(monkeypatch, tmp_path)`.** It sets `NARRATE_CACHE` to `tmp_path / "cache dir"` and `NARRATE_VOICES` to `tmp_path / "voices dir"`, so no test ever reads or writes the real `~/.narrate`.

- [ ] **Step 1: Write the failing tests**

```python
def test_loads_lines_and_resolves_lists(root, write_script):
    p = write_script(root, '''
voice = "test"
style = "explainer"
pronunciations = ["terms.json"]
[[line]]
id = "open"
text = "Hello there."
[[line]]
id = "story-1"
text = "A. B. C."
style = "story"
kind = "passage"
at = 1.5
say = "Ay. Bee. See."
''')
    s = load_script(p)
    assert s.voice == "test" and s.style == "explainer" and s.stem == "demo"
    assert s.pronunciations == ((root / "terms.json").resolve(),)
    assert [l.id for l in s.lines] == ["open", "story-1"]
    assert s.lines[1] == Line("story-1", "A. B. C.", "story", "passage", 1.5, "Ay. Bee. See.")

@pytest.mark.parametrize("body, needle", [
    ('[[line]]\nid = "Open"\ntext = "x"', "'Open'"),                         # id pattern [a-z0-9-]+
    ('[[line]]\nid = "a"\ntext = "x"\n[[line]]\nid = "a"\ntext = "y"', "duplicate"),
    ('[[line]]\nid = "a"\ntext = "…"', "'a'"),                                # no letter or digit
    ('[[line]]\nid = "a"\ntext = "x"\nkind = "chapter"', "kind"),
    ('[[line]]\nid = "a"\ntext = "x"\nat = -1', "at"),
    ('[[line]]\nid = "a"\ntext = "x"\ntxet = "typo"', "txet"),               # unknown keys are errors
    ('voice = "v"', "no lines"),
])
def test_rejects_bad_scripts(root, write_script, body, needle):
    with pytest.raises(ScriptError, match=re.escape(needle)):
        load_script(write_script(root, body))
```

Also: `test_bad_toml_names_file`. Invalid TOML raises a `ScriptError` whose message contains the file path.

- [ ] **Step 2: Run them to make sure they fail.** `uv run pytest tests/test_script.py -q` fails at import.

- [ ] **Step 3: Implement `load_script`.** Use `tomllib`. Every message is prefixed with `f"{path}: "`, plus `f"line {id!r}: "` where it applies.

- [ ] **Step 4: Run the tests.** `uv run pytest tests/test_script.py -q` passes.

- [ ] **Step 5: Commit.** `feat(narrate): narration script format`

### Task 3: Text: sentences, syllables, kinds and normalisation

**Files:**
- Create: `tools/narrate/src/narrate/text.py`, `tools/narrate/tests/test_text.py`

**Interfaces:**
- Produces:
  - `sentences(text: str) -> list[str]`. It splits after `.`, `?`, `!` or `…` (optionally followed by a closing quote) when whitespace and then an uppercase letter, digit or opening quote follows. Pieces are stripped.
  - `syllables(text: str) -> int`. The spec §5.4 count: `[aeiouy]+` matches per word of `[a-z]+` in `text.lower()`, at least 1 per word, summed.
  - `infer_kind(text: str) -> str`: `"passage"` if `len(sentences(text)) >= PASSAGE_SENTENCES` or `len(text) >= PASSAGE_CHARS`, else `"line"`.
  - `norm(text: str) -> str`. Applied in order:
    1. lowercase;
    2. `’`, `‘` → `'`;
    3. `α` → `alpha`;
    4. `&` → ` and `;
    5. a digit, then `x` or `×`, then a digit (spaces optional) → `<d> by <d>`;
    6. word-level: `sulfur` → `sulphur`, `color*` → `colour*`, `gigabytes?` → `gb`;
    7. hyphens → spaces; anything outside `[a-z0-9 ]` → space;
    8. per-word: `ii`, `iii`, `iv` → `2`, `3`, `4`;
    9. number-word runs → digits, so `two hundred and fifty six` → `256` and `nine thousand two hundred and fifty five` → `9255`. `and` is consumed only between number words;
    10. join the words with no separator.
  - `lev(a: str, b: str) -> int`: Levenshtein distance.
  - `cer(ref: str, hyp: str) -> float`: `lev(norm(ref), norm(hyp)) / max(1, len(norm(ref)))`.
  - `light(text: str) -> str`: lowercase; `&` → ` and `; keep `[a-z0-9, ]`; collapse spaces.
  - `punct_distance(ref: str, hyp: str) -> int`: `lev(light(ref), light(hyp))`.

- [ ] **Step 1: Write the failing tests.** These use real strings from the listening rounds.

```python
TWELVE = "Twelve panels, each 9,255 by 18,310 pixels, about 2 GB apiece."
def test_whisper_formatting_normalises_away():
    assert cer(TWELVE, "12 panels each 9,255 x 18,310 pixels about 2GB a piece") == 0.0
    assert cer("Three narrowband filters: H-alpha, O III and S II.", "Three narrowband filters, Hα, O3 and S2.") == 0.0
    assert cer("Hi, I'm Daniel Paull, and this is Mega Merge Mosaic.", "Hi, I'm Daniel Paull and this is MegaMerge Mosaic.") == 0.0
    assert norm("two hundred and fifty-six") == "256" and norm("nine thousand two hundred and fifty five") == "9255"
    assert norm("R, G and B") == "rgandb"

def test_real_mistakes_fail():
    for text, heard in [
        ("Hi, I'm Daniel Paull, and this is Mega Merge Mosaic.", "Hi, I'm Daniel Pohl and this is MegaMerge Mosaic."),
        ("Data courtesy of Adam Sbarra.", "Data courtesy of Adam Sparrow."),
        ("Save it as an XISF file.", "Save it as an .exis file."),
        ("SHO, HOO and RGB are built in, and you can add your own.", "Show, Who and RGB are built in and you can add your own."),
    ]:
        assert cer(text, heard) >= 0.04, heard

def test_punctuation_breaks_ties():
    text = "...and natural colour from R, G and B."
    apart, merged = "and natural colour from R, G and B.", "and Natural Color from RG&B."
    assert cer(text, apart) == cer(text, merged) == 0.0
    assert punct_distance(text, apart) < punct_distance(text, merged)

def test_norm_typography():
    assert norm("what’s new — here…") == norm("what's new - here...")

def test_sentences_and_kinds():
    showcase = ("The North America Nebula is enormous. From a dark site, it spans about four full Moons across the sky, far "
                "too big for most telescopes to capture in a single frame. So imagers do what map makers have always done: "
                "they photograph it in pieces, and then they stitch the pieces together.")
    assert len(sentences(showcase)) == 3 and infer_kind(showcase) == "passage"
    assert sentences("Composites combine filters. Here is SHO, the Hubble palette...") == [
        "Composites combine filters.", "Here is SHO, the Hubble palette..."]
    assert infer_kind("Each filter is a group of its own. Pick one to see its panels.") == "line"
    assert infer_kind("word " * 52) == "passage"            # 260 characters, one sentence

def test_syllables():
    assert syllables("The North America Nebula is enormous.") == 13
    assert syllables("X I S F") == 4 and syllables("XISF") == 1
```

- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `text.py`** to the interface above. `lev` is the standard two-row dynamic programme.
- [ ] **Step 4: Run them.** `uv run pytest tests/test_text.py -q` passes.
- [ ] **Step 5: Commit.** `feat(narrate): text normalisation, error rate, sentences and syllables`

### Task 4: Pronunciation lists

**Files:**
- Create: `tools/narrate/src/narrate/lexicon.py`, `tools/narrate/tests/test_lexicon.py`

**Interfaces:**
- Produces:
  - `Entry(match: re.Pattern[str], say: str, why: str)`, frozen.
  - `Lexicon(entries: tuple[Entry, ...])`, with:
    - `Lexicon.load(paths: Sequence[Path]) -> Lexicon`, which concatenates the lists in the given order;
    - `apply(text: str) -> str`, which applies `re.sub` once per entry, in order.
  - `LexiconError(NarrateError)`.
- File format (spec §8.3): `{"about": str, "entries": [{"match", "say", "why"}], "fine_as_written": [str]}`. `about` and `fine_as_written` are optional.

- [ ] **Step 1: Write the failing tests**

```python
ENTRIES = [  # from voice/pronunciations.json
    {"match": r"\bAstrometrical\b", "say": "Astro-metrical", "why": "stress"},
    {"match": r"\bData\b", "say": "Dahta", "why": "American as written"},
    {"match": r"\bdata\b", "say": "dahta", "why": "as above"},
    {"match": r"\b(\d+) GB\b", "say": r"\1 gigabytes", "why": "gigibytes"},
    {"match": r"\bSHO\b", "say": "S H O", "why": "Show"},
]
def test_applies_in_order_with_word_bounds(root):
    lx = Lexicon.load([write_list(root / "a.json", ENTRIES)])
    assert lx.apply("From Astrometrical: OSC data, about 2 GB apiece. SHO!") == \
        "From Astro-metrical: OSC dahta, about 2 gigabytes apiece. S H O!"
    assert lx.apply("Datapoints and SHOW") == "Datapoints and SHOW"

def test_lists_chain_in_given_order(root):
    a = write_list(root / "a.json", [{"match": "Foo", "say": "Bar", "why": ""}])
    b = write_list(root / "b.json", [{"match": "Bar", "say": "Baz", "why": ""}])
    assert Lexicon.load([a, b]).apply("Foo") == "Baz"
    assert Lexicon.load([b, a]).apply("Foo") == "Bar"
```

Also these error cases, each raising `LexiconError` whose message names the file:
- **`test_bad_regex_names_entry`:** the message also names the entry number (`entry 2`).
- **`test_missing_file`.**
- **`test_missing_say`.**

`write_list(path, entries)` is a test helper in the test module.

- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `lexicon.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): pronunciation lists`

### Task 5: Voice profiles and the voice library

**Files:**
- Create: `tools/narrate/src/narrate/profile.py`, `tools/narrate/tests/test_profile.py`
- Modify: `tools/narrate/tests/conftest.py`: add `voice_library`

**Interfaces:**
- Produces:
  - `Reference(audio: Path, text: str, seconds: float, sha256: str)`, frozen. `seconds` comes from `soundfile.info`, and `sha256` is of the file bytes.
  - `Pace(max: float = 5.3, target: float = 4.8)` and `Takes(count: int = 3, max: int = 6)`.
  - `VoiceProfile`, frozen:
    - **Fields:** `name`, `root: Path`, `backend: str`, `model: str`, `language: str = "English"`, `precision: str = "fp16"`, `checker: str = "whisper"`, `default_style: str`, `pronunciations: tuple[Path, ...]`, `styles: dict[str, dict[str, Reference]]` (style to `{"short": …, "long": …}`), `pace: Pace`, `takes: Takes`.
    - **`reference(style: str, kind: str) -> Reference`:** `kind == "line"` gives `"short"` and `"passage"` gives `"long"`. It raises `ProfileError` when the style, or that length, is missing; the message names the style and the length.
  - `voice_library(override: Path | None = None) -> Path`: `override`, else `$NARRATE_VOICES`, else `Path.home() / ".narrate" / "voices"`.
  - `load_profile(name: str, library: Path | None = None) -> VoiceProfile`. It reads `<library>/<name>/voice.toml` (spec §8.2) and resolves relative paths against the profile folder. It raises `ProfileError` for:
    - a missing profile, with the message including the path it looked for;
    - a missing audio file;
    - an unknown key;
    - `default_style` not in `styles`.
  - `ProfileError(NarrateError)`.
- Produces (fixture): `voice_library(root) -> Path`. It is a library holding a profile named `test`:
  - `backend = "fake"`, `checker = "fake"`, `model = "fake/model"`, `default_style = "explainer"`;
  - `explainer.short`: a 10.0 s synthetic WAV at 24 kHz;
  - `explainer.long`: a 49.0 s one;
  - no `story` style;
  - an empty profile pronunciation list.

  The fixture also sets `NARRATE_VOICES` to the library through `monkeypatch`.

- [ ] **Step 1: Write the failing tests**
  - **`test_loads_profile`:** the fields, defaults (`Pace(5.3, 4.8)`, `Takes(3, 6)`), `Reference.seconds == pytest.approx(10.0)`, and a 64-hex `sha256` that is stable across two loads.
  - **`test_reference_choice`:** `("explainer", "line")` gives the short reference and `("explainer", "passage")` the long one.
  - **`test_missing_style_names_it`:** `reference("story", "line")` raises `ProfileError`, matching `"story"`.
  - **`test_missing_length_names_it`:** a profile whose style has only `short` raises for `("explainer", "passage")`, matching `"long"`.
  - **`test_library_resolution`:** an override beats `NARRATE_VOICES`, which beats home (monkeypatch `Path.home`).
  - **`test_missing_profile_says_where_it_looked`:** the message contains `str(library / "nobody" / "voice.toml")`.
  - **`test_missing_audio`.**

- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `profile.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): voice profiles and the voice library`

### Task 6: Audio: files, speech span, pauses, pace, trim and loudness

**Files:**
- Create: `tools/narrate/src/narrate/audio.py`, `tools/narrate/src/narrate/measure.py`
- Create: `tools/narrate/tests/test_audio.py`, `tools/narrate/tests/test_measure.py`

**Interfaces:**
- Produces (`audio.py`):
  - `read_wav(path: Path) -> tuple[np.ndarray, int]`: mono float32, averaging any channels.
  - `write_wav(path: Path, audio: np.ndarray, sr: int, subtype: str = "PCM_16") -> None`: creates parent folders.
  - `trim(audio, sr, pad: float = TRIM_PAD) -> np.ndarray`: cut to the speech span ± `pad`, clamped to the clip.
  - `normalise_loudness(audio, sr, lufs: float = LOUDNESS_LUFS, peak_dbfs: float = PEAK_DBFS) -> np.ndarray`:
    - measures with `pyloudnorm.Meter(sr)`, zero-padding audio shorter than 0.4 s for the measurement only;
    - limits the gain so the peak stays at or below `peak_dbfs`;
    - returns all-silent input unchanged.
- Produces (`measure.py`):
  - `speech_span(audio, sr, top_db: float = TOP_DB) -> tuple[float, float]` in seconds, `(0.0, 0.0)` for silence.
  - `pauses(audio, sr, min_secs: float = PAUSE_MIN, top_db: float = TOP_DB) -> list[tuple[float, float]]`: silences inside the speech span of at least `min_secs`.
  - `pace(audio, sr, spoken: str) -> float`: `syllables(spoken) / (end - start)`, or `math.inf` when the span is under 0.05 s.
- **Silence analysis** mirrors `librosa.effects.split` so the numbers stay on the rounds' scale (spec §5.4):
  - RMS per frame, `FRAME = 2048` and `HOP = 512`, with centre padding of zeros;
  - `db = 20*log10(max(rms, AMIN)) - 20*log10(max(max(rms), AMIN))`, and non-silent means `db > -top_db`;
  - interval edges are `frame * HOP`, clamped to the length.

- [ ] **Step 1: Write the failing tests.** Use synthetic signals at 24 kHz: a 220 Hz sine at amplitude 0.3 is "speech" and zeros are silence.
  - **`test_speech_span`:** 0.5 s silence, 2.0 s tone, then 0.5 s silence gives `(0.5, 2.5)` ± 0.05.
  - **`test_pauses`:** tone 1.0 s, silence 0.6 s, tone 1.0 s gives one pause ≈ `(1.0, 1.6)` ± 0.1. A 0.1 s gap gives none.
  - **`test_pace`:** a 2.6 s tone with `"The North America Nebula is enormous."` gives `pytest.approx(5.0, rel=0.03)`.
  - **`test_pace_of_silence_is_inf`.**
  - **`test_trim_keeps_pad`:** the trimmed length is ≈ span + 0.1 s.
  - **`test_float_round_trip_is_exact`:** a FLOAT WAV written then read gives identical bytes.
  - **`test_normalise_hits_target`:**
    - `pyloudnorm` measures the result at −20 ± 0.5.
    - A dense loud signal where −20 LUFS would need a peak above −1 dBFS keeps its peak at or below `10**(-1/20) + 1e-4`.
  - **`test_normalise_short_clip`:** a 0.3 s tone doesn't raise and comes back louder than its input.
  - **`test_normalise_silence_unchanged`.**
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `audio.py` and `measure.py`.** Constants live in `measure.py` (`TOP_DB`, `FRAME`, `HOP`, `AMIN`, `PAUSE_MIN`) and `audio.py` (`LOUDNESS_LUFS`, `PEAK_DBFS`, `TRIM_PAD`).
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): speech span, pauses, pace, trim and loudness`

### Task 7: Sentence captions

**Files:**
- Create: `tools/narrate/src/narrate/captions.py`, `tools/narrate/tests/test_captions.py`

**Interfaces:**
- Consumes: `sentences` and `syllables` (Task 3); `speech_span` and `pauses` (Task 6).
- Produces:
  - `Caption(text: str, start: float, end: float)`, frozen; `to_json() -> dict` gives `{"text", "start", "end"}` with times rounded to 3 decimals.
  - `share_captions(text: str, duration: float) -> list[Caption]`: one caption per sentence, with boundaries at the cumulative syllable shares of `[0, duration]`.
  - `sentence_captions(text: str, audio: np.ndarray, sr: int) -> list[Caption]`. Captions are contiguous, the first starts at 0.0, and the last ends at the clip's duration. A single sentence gives one caption.
- **Algorithm:** pause matching. With `n` sentences, a speech span `(s0, s1)` and syllable shares, each target boundary is `b_k = s0 + (s1 - s0) * cum_k / total` for k = 1..n−1.
  - Choose `n−1` pause midpoints, in increasing order, that minimise Σ|m − b_k|. This is a dynamic programme over (boundary, pause).
  - Boundaries go at the chosen midpoints.
  - **Fallback:** with fewer than `n−1` pauses, return `share_captions(text, duration)`.

- [ ] **Step 1: Write the failing tests**
  - **`test_share_captions`:** `"One two three. Four five six seven eight nine."` over 9.0 s puts the boundary at 9 × 3/9 = 3.0. The captions are contiguous and cover `[0, 9]`.
  - **`test_pause_matching`:** three sentences of 8 syllables each. The audio is tone 0.3–3.0, silence 3.0–3.6, tone 3.6–6.0, silence 6.0–6.5, tone 6.5–9.0, with a decoy 0.3 s silence at 1.5–1.8. The boundaries come out at 3.3 and 6.25 (± 0.08), and the decoy isn't chosen.
  - **`test_too_few_pauses_falls_back`:** the same text over a continuous tone equals `share_captions(text, duration)`.
  - **`test_single_sentence`:** one caption, covering `[0, duration]`.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `captions.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): sentence captions timed against pauses`

### Task 8: Backends, checkers and fakes

**Files:**
- Create: `tools/narrate/src/narrate/backends/__init__.py`, `tools/narrate/src/narrate/backends/qwen.py`
- Create: `tools/narrate/src/narrate/checkers/__init__.py`, `tools/narrate/src/narrate/checkers/whisper.py`
- Create: `tools/narrate/src/narrate/fakes.py`
- Create: `tools/narrate/tests/test_fakes.py`, `tools/narrate/tests/test_gpu.py`

**Interfaces:**
- Consumes: `VoiceProfile` and `Reference` (Task 5); `syllables` and `sentences` (Task 3).
- Produces:
  - `Backend`, a Protocol:
    - `sample_rate: int`;
    - `synthesize(texts: list[str], reference: Reference, seed: int) -> list[np.ndarray]`, returning mono float32 in input order;
    - `close() -> None`, which frees the model; the next `synthesize` reloads it lazily.
  - `get_backend(name: str, profile: VoiceProfile) -> Backend`: `"qwen3-tts"` or `"fake"`, else `ValueError` listing the known names.
  - `Checker`, a Protocol: `name: str`, `transcribe(audio: np.ndarray, sr: int) -> str` and `close() -> None`.
  - `get_checker(name: str) -> Checker`: `"whisper"` or `"fake"`.
  - `fakes.FakeWorld`, with:
    - `script(spoken: str, heard: list[str] | None = None, pace: list[float] | None = None) -> None`: per-take behaviour for that spoken text in generation order, with the last value repeating. The defaults are heard = the spoken text and pace = 4.5.
    - `events: list[str]`: `"synthesize"`, `"close-backend"`, `"transcribe"`, `"close-checker"`, in order.
    - `calls: list[list[str]]`: the texts of each `synthesize` call.
    - `default_world() -> FakeWorld`: a module global that tests replace with `monkeypatch.setattr(fakes, "_DEFAULT", FakeWorld())`.
  - `fakes.FakeBackend(world)`, with `sample_rate = 24000`.
  - `fakes.FakeChecker(world)`, with `name = "fake"`.
- **FakeBackend audio:**
  - 0.3 s of silence at each end.
  - Speech is a sine burst per word, with the frequency varied by seed and row.
  - Gaps are 0.12 s between words and 0.5 s between sentences. They are scaled down if they'd exceed the span.
  - Each take's span is `syllables(spoken) / pace`, so `measure.pace` gives back the scripted pace within 5%.
- **FakeChecker** returns the `heard` registered under the SHA-1 of the take's float32 bytes. Takes are saved as FLOAT WAV, so a round trip keeps the fingerprint.
- **Qwen backend** (`qwen.py`; not run in CI):
  - **Model:** `Qwen3TTSModel.from_pretrained(profile.model, device_map="cuda:0", dtype=torch.float16 if profile.precision == "fp16" else torch.float32, attn_implementation="sdpa")`.
  - **Prompts:** one `create_voice_clone_prompt(ref_audio=str(ref.audio), ref_text=ref.text)` per reference sha, kept in memory.
  - **Generation:** `torch.manual_seed(seed)`, then `generate_voice_clone(text=texts, language=profile.language, voice_clone_prompt=prompt)`. Non-finite samples become 0.
  - **`close()`:** deletes the model and prompts, runs `gc.collect()` and `torch.cuda.empty_cache()`.
- **Whisper checker** (`whisper.py`): the code from `eval4.py`.
  - **Model:** `WhisperProcessor` with `WhisperForConditionalGeneration.from_pretrained("openai/whisper-large-v3-turbo", dtype=torch.float16)` on `cuda:0`; `generate(…, language="english", task="transcribe")`.
  - **Input:** resample to 16 kHz with `numpy.interp`, so librosa isn't needed in core.
  - **Long audio:** over 28 s, split at the quietest 10 ms RMS point between 15 s and 28 s, recursively.

- [ ] **Step 1: Write the failing tests** (`test_fakes.py`)
  - **`test_fake_pace_round_trips`:** a scripted pace of 4.0 or 6.0 on a 3-sentence passage comes back within 5% through `measure.pace`. `measure.pauses` finds at least 2 pauses.
  - **`test_fake_checker_hears_what_was_scripted`:** `heard=["garbled", "ok text"]` gives "garbled" for take 1 and "ok text" for take 2. This holds after a FLOAT round trip through `write_wav` and `read_wav`.
  - **`test_events_order`:** the event log is recorded in order.
  - **`test_registry`:** `get_backend("fake", profile)` and `get_checker("fake")` are bound to `default_world()`. Unknown names raise a `ValueError` listing `qwen3-tts`, `fake`, `whisper`.
  - **`test_core_import_does_not_import_torch`:** a subprocess runs `python -c "import narrate.build, narrate.backends, narrate.checkers, sys; assert 'torch' not in sys.modules"`. The import of `narrate.build` is skipped until Task 11 exists.
- [ ] **Step 2: Write `test_gpu.py`.** It is marked `gpu` and skipped unless `NARRATE_GPU_TESTS=1`. It loads the profile named by `NARRATE_GPU_VOICE` (default `dan`), synthesizes `"Each filter is a group of its own."` with `QwenBackend`, and transcribes it with Whisper. It asserts `cer < 0.1`, and that `close()` frees the model (`torch.cuda.memory_allocated()` drops).
- [ ] **Step 3: Run the tests to make sure they fail.**
- [ ] **Step 4: Implement the backends, checkers and fakes.** Import `qwen.py` and `whisper.py` lazily inside `get_backend` and `get_checker`.
- [ ] **Step 5: Run them.** `uv run pytest -q` passes, with the GPU tests skipped.
- [ ] **Step 6: Commit.** `feat(narrate): backend and checker boundaries, Qwen3-TTS, Whisper and fakes`

### Task 9: The take cache and batching

**Files:**
- Create: `tools/narrate/src/narrate/cache.py`, `tools/narrate/src/narrate/batching.py`
- Create: `tools/narrate/tests/test_cache.py`, `tools/narrate/tests/test_batching.py`

**Interfaces:**
- Consumes: `VoiceProfile` and `Reference` (Task 5); `write_wav` and `read_wav` (Task 6).
- Produces (`cache.py`):
  - `cache_root(override: Path | None = None) -> Path`: `override`, else `$NARRATE_CACHE`, else `~/.narrate/cache`.
  - `take_key(profile: VoiceProfile, reference: Reference, spoken: str) -> str`: the first 24 hex characters of the sha256 of `json.dumps({"backend", "model", "precision", "ref_sha256", "ref_text", "spoken"}, sort_keys=True)`.
  - `TakeStore(root: Path)`:
    - `path(key, n) -> Path`, which is `root / "takes" / key[:2] / key / f"take-{n}.wav"`;
    - `takes(key) -> list[int]`, existing take numbers in ascending order, ignoring `*.tmp`;
    - `save(key, n, audio, sr) -> Path`, writing a FLOAT WAV to `<path>.tmp` then `os.replace`;
    - `load(key, n) -> tuple[np.ndarray, int]`;
    - `heard(key, n, checker: str) -> str | None`;
    - `set_heard(key, n, checker, text) -> None`, stored in `take-{n}.json` as `{"heard": {checker: text}}`, also written atomically.
- Produces (`batching.py`):
  - `Job(key: str, take: int, spoken: str, reference: Reference)`, frozen.
  - `MAX_ROWS = 6`.
  - `budget(reference_seconds: float) -> int`, which is `int(56_000 / (reference_seconds + 13.4))`.
  - `plan_batches(jobs: Sequence[Job]) -> list[list[Job]]`:
    - group by `reference.sha256`, in first-seen order, and sort each group by `len(spoken)`;
    - add jobs while `rows < MAX_ROWS` and `(rows + 1) * len(longest) <= budget`;
    - a job over budget on its own gets a batch of one.
  - `batch_seed(job: Job) -> int`: `int(sha256(f"{job.key}:{job.take}").hexdigest()[:8], 16)`.

- [ ] **Step 1: Write the failing tests**
  - **`test_key_is_stable_and_sensitive`:** the same inputs give the same 24-hex key. Changing `spoken`, `ref_text`, the reference audio bytes, `precision` or `model` each changes it.
  - **`test_store_round_trip`:** save takes 1 and 2, then `takes()` gives `[1, 2]`, `load()` returns identical float32 audio, and `heard`/`set_heard` round-trip per checker name.
  - **`test_tmp_files_are_ignored`:** a stray `take-3.wav.tmp` doesn't appear in `takes()`.
  - **`test_budget_values`:** `budget(10.0) == 2393`, `budget(49.0) == 897`, `budget(53.0) == 843`.
  - **`test_batches_follow_the_rounds`:**
    - three 787-character jobs on a 10 s reference make one batch of 3, but three batches of 1 on a 49 s reference;
    - six 144-character jobs on a 49 s reference make one batch;
    - six 557-character jobs on a 49 s reference make six batches (the 11-hour hang);
    - seven 10-character jobs make batches of 6 and 1;
    - two references never share a batch.
  - **`test_batch_seed_differs_per_take`.**
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `cache.py` and `batching.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): content-addressed take cache and VRAM-safe batching`

### Task 10: Choosing a take

**Files:**
- Create: `tools/narrate/src/narrate/select.py`, `tools/narrate/tests/test_select.py`

**Interfaces:**
- Consumes: `cer` and `punct_distance` (Task 3); `pace` (Task 6); `Pace` (Task 5).
- Produces:
  - `MAX_CER = 0.04`.
  - `TakeResult(n: int, heard: str, cer: float, punct: int, pace: float, duration: float)`, frozen.
  - `evaluate(n: int, text: str, spoken: str, heard: str, audio: np.ndarray, sr: int) -> TakeResult`:
    - `cer = min(cer(text, heard), cer(spoken, heard))`;
    - `punct = min(punct_distance(text, heard), punct_distance(spoken, heard))`;
    - `pace = measure.pace(audio, sr, spoken)`;
    - `duration = len(audio) / sr`.
  - `Choice(take: TakeResult, status: str, flags: tuple[str, ...])`, frozen, with `status` either `"new"` or `"flagged"`.
  - `choose(kind: str, takes: Sequence[TakeResult], pace: Pace, max_cer: float = MAX_CER) -> Choice`.
- **Rules** (spec §5.2, steps 5–6):
  - **Order:** `(round(cer, 4), punct, abs(pace - pace.target), n)`. Words pass when `cer < max_cer`.
  - **`line`:** among word-passing takes, prefer those with `pace <= pace.max`; if there are none, take any word-passing take. The status is `new`. With no word-passing take, it's flagged `("words",)` and the best by the order is picked.
  - **`passage`:**
    - a take passes when its words pass and `pace <= pace.max`, giving `new`;
    - otherwise it's flagged `("pace",)` when some take passes on words, or `("words",)` when none do;
    - the fallback pick orders by `(not words_ok, pace > max, *order)`.

- [ ] **Step 1: Write the failing tests**

```python
P = Pace(max=5.3, target=4.8)
T = lambda n, cer=0.0, punct=0, pace=4.8: TakeResult(n, "h", cer, punct, pace, 3.0)

def test_lowest_error_then_punctuation_then_pace_then_take():
    assert choose("line", [T(1, cer=0.02), T(2, cer=0.0, punct=5), T(3, cer=0.0, punct=1)], P).take.n == 3
    assert choose("line", [T(1, pace=5.2), T(2, pace=4.7)], P).take.n == 2
    assert choose("line", [T(2), T(1)], P).take.n == 1

def test_line_prefers_pace_but_never_fails_on_it():
    assert choose("line", [T(1, pace=6.0), T(2, cer=0.03, pace=4.6)], P).take.n == 2
    c = choose("line", [T(1, pace=6.4), T(2, pace=6.0)], P)
    assert (c.status, c.flags, c.take.n) == ("new", (), 2)

def test_passage_fails_on_pace():
    c = choose("passage", [T(1, pace=5.6), T(2, pace=5.4)], P)
    assert (c.status, c.flags, c.take.n) == ("flagged", ("pace",), 2)

def test_garbled_everywhere_flags_words():
    c = choose("line", [T(1, cer=0.3), T(2, cer=1.0)], P)
    assert (c.status, c.flags, c.take.n) == ("flagged", ("words",), 1)
```

Also: `test_evaluate_uses_spoken_or_text`. With text `"Data courtesy of Adam Sbarra."`, spoken `"Dahta courtesy of Adam Zabara."` and heard `"Data courtesy of Adam Zabara."`, `cer < 0.04` (so `cer(text)` alone would fail). The audio is a synthetic tone.

- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `select.py`.**
- [ ] **Step 4: Run them; they pass.**
- [ ] **Step 5: Commit.** `feat(narrate): take selection with word, punctuation and pace rules`

### Task 11: Build and the manifest

**Files:**
- Create: `tools/narrate/src/narrate/manifest.py`, `tools/narrate/src/narrate/build.py`
- Create: `tools/narrate/tests/test_manifest.py`, `tools/narrate/tests/test_build.py`

**Interfaces:**
- Consumes: Tasks 2–10.
- Produces (`manifest.py`):
  - `LEAD = 0.15`, `BREATH = 0.4`.
  - `ManifestLine`: `id`, `text`, `spoken`, `kind`, `clip: str | None` (relative to the manifest), `duration: float`, `at: float | None`, `status: str`, `flags: list[str]`, `captions: list[Caption]`, `check: dict | None` (`{"heard", "cer", "pace", "take", "key"}`).
  - `Manifest`: `script: str` (the file name), `voice: str | None`, `draft: bool`, `sample_rate: int | None`, `lead: float`, `breath: float`, `lines: list[ManifestLine]`. It has:
    - `to_json() -> dict`, with `"version": 1` first and the §8.4 field names;
    - `write(path: Path) -> None`, UTF-8 with `indent=1` and `ensure_ascii=False`;
    - `flagged() -> list[str]`, the ids of flagged lines.
  - `read_manifest(path: Path) -> Manifest`: raises `NarrateError` when `version > 1`.
- Produces (`build.py`):
  - `BuildOptions(out: Path | None = None, voices: Path | None = None, cache: Path | None = None)`.
  - `default_out(script: Script) -> Path`, which is `script.path.parent / "out" / script.stem`.
  - `build(script_path: Path, *, backend: Backend | None = None, checker: Checker | None = None, options: BuildOptions = BuildOptions()) -> Manifest`. It writes `<out>/clips/<id>.wav` and `<out>/narration.json`, and returns the manifest.
- **Flow** (spec §5.2):
  1. Load the script; a missing `voice` is a `ScriptError`. Load the profile, and `Lexicon.load(script.pronunciations + profile.pronunciations)`, in that order.
  2. Resolve every line's spoken form, kind, reference and key **before any generation**, so a missing style or length fails first.
  3. **Round 1:** jobs for takes `1..takes.count` that the cache lacks go through `plan_batches`. Run `backend.synthesize(texts, ref, batch_seed(batch[0]))` per batch and `store.save` each take. Then `backend.close()`.
  4. **Check** every take: cached `heard` for `checker.name`, else `checker.transcribe` and `set_heard`. Then `evaluate` and `choose`.
  5. **Round 2,** only if some line is flagged: jobs for takes `count+1..takes.max` of the flagged lines. Generate, close the backend, check, then choose again over all of that line's takes.
  6. Then `checker.close()`.
  7. **Write each clip:** `normalise_loudness(trim(audio))`, as `PCM_16`. Captions are one caption over `[0, duration]` for a `line`, or `sentence_captions(text, clip, sr)` for a `passage`.
  8. Write the manifest, with statuses `new` or `flagged` (plan 2 adds `approved`).
  - **Defaults:** with no `backend` or `checker` given, use `get_backend(profile.backend, profile)` and `get_checker(profile.checker)`.

- [ ] **Step 1: Write the failing tests.** They use the `voice_library` fixture and `fakes.default_world()` patched to a fresh `FakeWorld`.
  - **`test_build_writes_clips_and_manifest`:** a line (`"Each filter is a group of its own."`) and a three-sentence passage give two `new` lines, each with:
    - a clip on disk;
    - a matching `duration`;
    - `check.take` in 1..3;
    - `kind` values `line` and `passage`;
    - captions: 1 for the line, 3 for the passage, contiguous over `[0, duration]`.

    `narration.json` round-trips through `read_manifest`.
  - **`test_rebuild_is_all_cache`:** a second build makes no `synthesize` and no `transcribe` events.
  - **`test_editing_one_line_regenerates_only_it`:** after changing one line's text, `world.calls` flattens to exactly that line's spoken text, 3 times.
  - **`test_pronunciation_change_regenerates_affected_lines`:** adding a script list entry that touches line A only regenerates only A.
  - **`test_failing_line_gets_more_takes_then_flags`:** with `heard=["garbled"]`, takes 1–6 exist and the status is `flagged` with `["words"]`.
  - **`test_second_round_rescues`:** with `heard=["garbled", "garbled", "garbled", "<the text>"]`, the result is `new` with `check.take == 4`.
  - **`test_passage_too_fast_flags_pace`:** `pace=[6.0]` flags `["pace"]`.
  - **`test_fast_line_is_not_flagged`:** `pace=[6.0, 6.0, 4.6]` picks take 3, and `pace=[6.0]` stays `new`.
  - **`test_backend_closed_before_checking`:** in `world.events`, every `"transcribe"` comes after a `"close-backend"` with no `"synthesize"` between them.
  - **`test_missing_style_fails_before_generation`:** a line with `style = "story"` raises `ProfileError` and `world.events == []`.
  - **`test_one_word_line`:** `"Next."` builds as `new` with a finite `check.pace`.
  - **`test_empty_transcript_flags_line`:** `heard=[""]` flags `["words"]` without raising.
  - **`test_respelled_line_passes`:** with a script list `Sbarra` → `Zabara`, text `"Data courtesy of Adam Sbarra."` and `heard` = the spoken form, the line is `new`, and its `spoken` contains `"Zabara"`.
  - **`test_manifest_version_guard`:** reading a manifest with `"version": 2` raises `NarrateError`.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `manifest.py` and `build.py`.**
- [ ] **Step 4: Run them.** `uv run pytest -q` passes, and `test_core_import_does_not_import_torch` now covers `narrate.build`.
- [ ] **Step 5: Commit.** `feat(narrate): build: takes, checks, picks, clips, captions and the manifest`

### Task 12: `draft`, and the `build` and `draft` commands

**Files:**
- Create: `tools/narrate/src/narrate/draft.py`
- Modify: `tools/narrate/src/narrate/cli.py`, `tools/narrate/tests/test_cli.py`
- Create: `tools/narrate/tests/test_draft.py`

**Interfaces:**
- Consumes: `load_script`, `Lexicon`, `syllables`, `infer_kind`, `share_captions`, `Manifest`, `build`, `default_out`.
- Produces:
  - `draft(script_path: Path, *, out: Path | None = None, pace_target: float = 4.8) -> Manifest`. It needs no profile; its lists are the script's only. Each line gets:
    - `duration = syllables(spoken) / pace_target`;
    - `clip = None`, `status = "draft"`, `check = None`;
    - captions: one for a line, `share_captions` for a passage.

    The manifest has `draft = true` and `sample_rate = None`.
  - **CLI:**
    - `narrate build SCRIPT [--out DIR] [--voices DIR] [--cache DIR] [--allow-flagged]`;
    - `narrate draft SCRIPT [--out DIR] [--pace FLOAT]`.
  - **Output:** both print one row per line (`id`, `status`, take, `cer`, `pace`, `duration`) and the manifest path. `build` also prints what was heard for each flagged line.
  - **Exit codes:** 0, 1 (flagged, without `--allow-flagged`), 2 (`NarrateError`, with the message on stderr).

- [ ] **Step 1: Write the failing tests**
  - **`test_draft_needs_no_voice`:** `draft` with no voice library at all, and the three-sentence passage, gives `draft == True`, durations of `syllables / 4.8`, and three `share_captions` captions.
  - **`test_cli_build_end_to_end`:** with the `test` profile (fake backend), `main(["build", str(script)])` returns 0, and `narration.json` lands in `default_out`.
  - **`test_cli_build_flagged_exit_codes`:** a scripted garbled line gives 1, or 0 with `--allow-flagged`.
  - **`test_cli_user_error_exit_2`:** a missing voice gives 2, and stderr names the path.
  - **`test_cli_draft`:** returns 0 with `--pace 4.0`.
- [ ] **Step 2: Run them to make sure they fail.**
- [ ] **Step 3: Implement `draft.py` and the subcommands.**
- [ ] **Step 4: Run them.** `uv run pytest -q` and ruff are clean.
- [ ] **Step 5: Commit.** `feat(narrate): draft manifests and the build/draft commands`

### Task 13: Dan's profile, the tour script, docs and the GPU smoke

**Files:**
- Create (outside git): `~/.narrate/voices/dan/voice.toml`, `~/.narrate/voices/dan/refs/{explainer-10,explainer-full,story-10,story-full}.wav`, `~/.narrate/voices/dan/pronunciations.json`
- Create: `crates/mmm-app/tours/ngc7000.narration.toml`, `crates/mmm-app/tours/terms.json`
- Create: `tools/narrate/README.md`
- Modify: `.gitignore` (add `/crates/mmm-app/tours/out/`), `CLAUDE.md` (Layout and Commands), `docs/DESIGN.md` (a new "Narration and tours (2026-11)" section)

**Interfaces:**
- Consumes: the CLI (Task 12).
- Produces: the tour script and terms list that plans 2 and 3 use, with ids `open`, `analyse`, `groups`, `merge`, `h-mosaic`, `sho`, `rgb`, `rendered`.

- [ ] **Step 1: Assemble Dan's profile.** Copy the four reference clips from the main checkout's `voice/refs/`. Take the transcripts from `voice/refs/ref_text.json` (keys `explainer-10`, `explainer-full`, `story-10`, `story-full`).
  - **`voice.toml`:** per spec §8.2, with `backend = "qwen3-tts"`, `model = "Qwen/Qwen3-TTS-12Hz-1.7B-Base"` and `default_style = "explainer"`.
  - **`pronunciations.json`** (the speaker's list): the `Paull`, `Data`, `data` and `(\d+) GB` entries from `voice/pronunciations.json`, with their `why`.
- [ ] **Step 2: Write `terms.json`** (the project's list). It holds the remaining entries of `voice/pronunciations.json`: `Sbarra`, `SHO`, `HOO`, `MosaicByCoordinates`, `Astrometrical`, `XISF`, `Composites` and `composites`.
- [ ] **Step 3: Write `ngc7000.narration.toml`.** It has `voice = "dan"`, `style = "explainer"` and `pronunciations = ["terms.json"]`, and eight lines. The text is exactly round 2's phrases 21–28, in `voice/tools/make_jobs2.py`'s `PHRASES[20:28]`:
  - `open`: "A real mosaic of NGC 7000: two panels, shot through six filters: S, H, O, R, G and B."
  - `analyse`: "Mega Merge Mosaic reads every panel, places it on the sky and checks the overlaps."
  - `groups`: "Each filter is a group of its own. Pick one to see its panels."
  - `merge`: "One click merges all six filters."
  - `h-mosaic`: "The merged H-alpha mosaic: gains matched, seams blended away."
  - `sho`: "Composites combine filters. Here is SHO, the Hubble palette..."
  - `rgb`: "...and natural colour from R, G and B."
  - `rendered`: "Rendered frame by frame from the running app: no screen recording."
- [ ] **Step 4: Check the draft with no GPU.**

Run: `uv run narrate draft ../../crates/mmm-app/tours/ngc7000.narration.toml`
Expected: exit 0, 8 `draft` lines, and `crates/mmm-app/tours/out/ngc7000/narration.json`. `git status` must not show `out/`.

- [ ] **Step 5: GPU smoke (manual, on the reference machine).**

Run: `uv sync --extra qwen && uv run narrate build ../../crates/mmm-app/tours/ngc7000.narration.toml`
Expected:
- 8 lines `new`, or the flagged ones listed with what Whisper heard;
- clips in `out/ngc7000/clips/`;
- a second `build` that is all cache and finishes in seconds.

Record the timings and any flagged lines in the report.
- [ ] **Step 6: Calibration check (manual).** Use `narrate.measure.pace` on round 4's audio, in the main checkout's `voice/pages/round4/audio/`, with `syllables` of the passage texts in `voice/results/round4-texts.json`:
  - `real-showcase.mp3` should come out at 3.85 ± 0.15;
  - `real-tutorial.mp3` at 3.64 ± 0.15.

  If they're off, fix the measure (Task 6) before going on.
- [ ] **Step 7: Run the GPU tests.** `NARRATE_GPU_TESTS=1 uv run pytest -m gpu -q` passes.
- [ ] **Step 8: Docs**
  - **`tools/narrate/README.md`:** install (`uv sync`, plus `--extra qwen` for the GPU), the voice library and its profile format, the `build` and `draft` commands, where files go (spec §5.8), and the privacy rules.
  - **CLAUDE.md:** a Layout line for `tools/narrate` (Python; the narration design spec), and the command `cd tools/narrate && uv run pytest -q`.
  - **DESIGN.md:** a "Narration and tours (2026-11)" section with what this plan built, its defaults, and a link to the spec.
- [ ] **Step 9: Commit**

```bash
git add .gitignore CLAUDE.md docs/DESIGN.md tools/narrate/README.md crates/mmm-app/tours
git commit -m "feat(narrate): the NGC 7000 tour script, project terms and docs"
```
