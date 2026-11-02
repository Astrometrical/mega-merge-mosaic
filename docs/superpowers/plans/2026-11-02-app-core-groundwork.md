# App Core Groundwork Implementation Plan (phase 1 of the standalone app)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give mmm-core everything the standalone app needs from the engine (cancellable, progress-reporting jobs; a memory-mapped tiled result with per-tile validity; a multi-group mosaic session with a shared configuration; structured checks; health metrics; display-stretch maths), and expose the configuration to the CLI, without changing any core algorithm.

**Architecture:** New mmm-core modules wrap the existing pipeline rather than alter it. A multi-group mosaic is a `.mmm-session` folder holding `mosaic.json` (configuration) plus one existing-format `Session` directory per group under `groups/` (cache), all on one shared reference frame. Blending into the tiled result goes through the existing `blend` entry point with a tile-aligned ROI and `Extent::Canvas`, streamed into a new `RowSink` that writes tiles; cancellation and progress for blend are a wrapping `RowSink`, for analyze a `JobCtl` threaded through the internal functions.

**Tech Stack:** Rust 2024, mmm-core, serde/serde_json, memmap2 (already a dependency), rayon, clap (CLI).

**Spec:** `docs/superpowers/specs/2026-11-02-standalone-app-design.md` (§6, §7, §9 above all). Read it before starting any task.

## Global Constraints

- **No core algorithm changes** (user ruling). Analyze, photometry, seams, blend maths stay as they are. `crates/mmm-core/tests/regression_guard.rs` (byte-identical blend hashes) must stay green untouched.
- Linear data end to end; zero (all channels zero) is the no-data sentinel.
- Never process dense over the canvas beyond what the pipeline already does.
- Tests must not depend on `test_data/`; synthesize inputs with `mmm_core::synth`.
- Every public item in mmm-core has a doc comment (`missing_docs` warns); `cargo fmt`, `cargo clippy --all-targets`, `cargo doc` stay warning-free. Update the "Public API map" in `crates/mmm-core/src/lib.rs` for every new public module.
- Existing CLI flags keep their meaning; existing commands keep working unchanged.
- Sessions written by earlier versions are not migrated (spec §7.1); a legacy single-group session dir (has `session.json`, no `mosaic.json`) keeps working with `analyze`/`blend`/`report`.
- Terminology: "display stretch", never "STF".
- Run `source ~/.cargo/env` if `cargo` is not on PATH. Full check before every commit: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Cancel mid-blend** must leave every tile in the blended rect *invalid*, never a half-written tile marked valid (Task 5 test `cancelled_blend_leaves_rect_invalid`).
2. **A reader racing a writer** on the same tile must never get a torn tile reported as valid (Task 4 test `reader_sees_invalid_while_tile_rewritten`).
3. **Canvas sizes that are not multiples of the tile size**: edge tiles are padded with zeros, parents built correctly, rects clamped to the canvas (Task 4 `edge_tiles_are_zero_padded`, Task 5 `odd_canvas_tiles_match_full_blend`).
4. **Group names from headers that are not filesystem-safe** ("Ha 3nm", "R/G", "", two names sanitising to the same string) get distinct, safe directory names (Task 7 `group_dirs_are_safe_and_unique`).
5. **Input files moved or rewritten between runs** produce `FileMissing` / `FileChanged` checks, never a panic or an opaque error (Task 8 `moved_and_modified_inputs_are_checks`).

---

### Task 1: Job control: cancellation, progress, `Error::Cancelled`

**Files:**
- Create: `crates/mmm-core/src/job.rs`
- Modify: `crates/mmm-core/src/error.rs` (new variant), `crates/mmm-core/src/lib.rs` (module + API map)
- Test: unit tests in `job.rs`

**Interfaces:**
- Produces:
  - `Error::Cancelled` (`#[error("cancelled")]`), plus `Error::is_cancelled(&self) -> bool`.
  - `#[derive(Clone, Default, Debug)] pub struct CancelToken` (an `Arc<AtomicBool>`): `new() -> Self`, `cancel(&self)`, `is_cancelled(&self) -> bool`, `check(&self) -> Result<()>` (returns `Err(Error::Cancelled)` once cancelled).
  - `pub type ProgressFn<'a> = &'a (dyn Fn(&str, u64, u64) + Sync);` (same shape as `analyze::AnalyzeProgress`).
  - `pub struct JobCtl<'a>`: `new(cancel: CancelToken, progress: Option<ProgressFn<'a>>) -> Self`, `none() -> JobCtl<'static>` (never cancelled, no progress), `check(&self) -> Result<()>`, `report(&self, stage: &str, done: u64, total: u64) -> Result<()>` (checks cancellation first, then forwards to the observer), `cancel_token(&self) -> &CancelToken`.
  - `pub struct CtlSink<'s, 'c>`: `new(inner: &'s mut dyn RowSink, ctl: &'s JobCtl<'c>, stage: &'static str) -> Self`, implementing `blend::RowSink`. `begin` records geometry and forwards; `band` calls `ctl.check()?`, forwards, then `ctl.report(stage, rows_done, h)` where `rows_done = y0 + rows.len() / (ch * w)`; `finish` forwards.

- [ ] **Step 1: Write failing tests** in `job.rs`: `token_check_errors_after_cancel` (fresh token `check()` is `Ok`; after `cancel()`, `is_cancelled()` and `check()` is `Err` with `is_cancelled()`), `clones_share_state` (cancel a clone, original reports cancelled), `report_forwards_then_stops` (a recording observer sees `("blend", 1, 4)`; after cancel `report` returns `Err(Cancelled)` and the observer records nothing more), `ctl_sink_reports_and_cancels` (a `MemSink`-style test sink receiving `begin(4, 4, 1)` and two 2-row bands records progress `(2,4)` and `(4,4)`; with the token cancelled before the second band, `band` returns `Err(Cancelled)` and the inner sink received only one band).
- [ ] **Step 2: Run** `cargo test -p mmm-core job::` — expect compile failure / FAIL.
- [ ] **Step 3: Implement** the items above. Check no exhaustive `match` on `Error` elsewhere breaks (`cargo build --workspace`; `crates/mmm-ipc-worker` included).
- [ ] **Step 4: Run** `cargo test -p mmm-core job::` — PASS; then the full check.
- [ ] **Step 5: Commit** `feat(core): job control — cancel token, progress, cancellable row sink`.

### Task 2: Settings types and change impact

**Files:**
- Create: `crates/mmm-core/src/settings.rs`
- Modify: `crates/mmm-core/src/analyze.rs` (serde derives on `InputSelect`), `crates/mmm-core/src/photometry.rs` (serde derives on `GainMode` if missing), `crates/mmm-core/src/blend.rs` (serde derives on `BlendMode`), `lib.rs`
- Test: unit tests in `settings.rs`

**Interfaces:**
- Consumes: `analyze::InputSelect`, `photometry::GainMode`, `blend::{BlendMode, BlendParams}`.
- Produces:
  - serde names exactly as the CLI spells them: `InputSelect` → `"auto" | "aligned" | "solved"`; `GainMode` → `"fit" | "unity"`; `BlendMode` → `"pyramid" | "twoband" | "feather"` (use `#[serde(rename_all = "lowercase")]`).
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct AnalyzeSettings { pub input: InputSelect, pub surface: Option<u32>, pub gain: GainMode }` with `Default` = the CLI defaults (`Auto`, `Some(2)`, `Fit`; verify against `AnalyzeOpts` in `crates/mmm/src/main.rs`).
  - `pub struct BlendSettings { pub mode: BlendMode, pub feather_px: f32, pub defect_veto: bool, pub flatten: Option<u32> }` (same derives), `Default` = values from `BlendParams::default()`, and `to_params(&self) -> BlendParams` (other `BlendParams` fields at their defaults).
  - `pub struct AnalyzeOverride { pub input: Option<InputSelect>, pub surface: Option<Option<u32>>, pub gain: Option<GainMode> }` and `pub struct BlendOverride { pub mode: Option<BlendMode>, pub feather_px: Option<f32>, pub defect_veto: Option<bool>, pub flatten: Option<Option<u32>> }`, all fields `#[serde(default, skip_serializing_if = "Option::is_none")]`; `pub struct GroupOverride { #[serde(default)] pub analyze: AnalyzeOverride, #[serde(default)] pub blend: BlendOverride }`. `AnalyzeSettings::with(&self, o: &AnalyzeOverride) -> AnalyzeSettings`, `BlendSettings::with(&self, o: &BlendOverride) -> BlendSettings`, `GroupOverride::is_empty(&self) -> bool`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)] pub enum ChangeImpact { None, Reblend, Reanalyze }`; `pub fn analyze_change(old: &AnalyzeSettings, new: &AnalyzeSettings) -> ChangeImpact` (any difference → `Reanalyze`); `pub fn blend_change(old: &BlendSettings, new: &BlendSettings) -> ChangeImpact` (any difference → `Reblend`). Doc comments state the spec's classification: analyze settings are *global* (analysis reruns, every result tile goes stale); blend settings are *blend-local* (analysis kept; existing result tiles go stale and an ROI reblend shows the effect).

- [ ] **Step 1: Write failing tests:** `defaults_match_cli` (`BlendSettings::default().to_params()` equals `BlendParams::default()` field by field for mode, feather_px, defect_veto, flatten); `serde_names_match_cli` (round-trip JSON of `AnalyzeSettings { input: Solved, surface: None, gain: Unity }` contains `"solved"`, `null`, `"unity"`; `BlendMode::TwoBand` serialises to `"twoband"`); `override_applies_only_set_fields` (`BlendOverride { mode: Some(Feather), ..Default::default() }` changes only mode); `empty_override_serialises_to_empty_object` (`{"analyze":{},"blend":{}}`); `change_impact_classifies` (feather change → `Reblend`; gain change → `Reanalyze`; identical → `None`).
- [ ] **Step 2: Run** `cargo test -p mmm-core settings::` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** tests — PASS; full check.
- [ ] **Step 5: Commit** `feat(core): serialisable analyze/blend settings, per-group overrides, change impact`.

### Task 3: Cancellable analyze (`analyze_ctl`)

**Files:**
- Modify: `crates/mmm-core/src/analyze.rs`
- Test: unit tests in `analyze.rs` (next to `file_analyze_reports_progress`)

**Interfaces:**
- Consumes: `job::JobCtl`, `settings::AnalyzeSettings`.
- Produces: `pub fn analyze_ctl(paths: &[PathBuf], session_dir: &Path, settings: &AnalyzeSettings, reference: Option<&ReferenceFrame>, ctl: &JobCtl) -> Result<Session>`. `analyze_full` keeps its signature and delegates to it (wrapping its optional `AnalyzeProgress` in a `JobCtl` with a fresh token). Progress stages and ordering are unchanged (`"reproject"`, `"analyze"`).

- [ ] **Step 1: Write failing tests:** `analyze_ctl_cancels_from_progress` (two synthetic full-canvas panels as in `synth_two_full_canvas_panels`; the observer cancels the token on its first event; `analyze_ctl` returns an error with `is_cancelled()`, and `session.json` does not exist in the session dir); `analyze_ctl_matches_analyze_full` (same inputs, uncancelled: the resulting `Session`'s panels' `bbox` and `ch_mean` equal those from `analyze_full`).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement.** Replace the internal `progress: Option<AnalyzeProgress>` parameters of `analyze_aligned` / `analyze_solved` with `ctl: &JobCtl`; the private `report` helper becomes `ctl.report(..)?` (inside the rayon `map` closures return the error so `collect::<Result<_>>` stops); call `ctl.check()?` before `finish_session` and once per reprojected panel. `session.json` must only be written after the final check (verify where `finish_session` saves).
- [ ] **Step 4: Run** tests (`cargo test -p mmm-core analyze`) — PASS; full check, including `regression_guard`.
- [ ] **Step 5: Commit** `feat(core): cancellable analyze via analyze_ctl`.

### Task 4: Tiled result store (`tiles` module)

**Files:**
- Create: `crates/mmm-core/src/tiles.rs`
- Modify: `lib.rs`
- Test: unit tests in `tiles.rs`

**Interfaces:**
- Produces:
  - `pub const TILE: u32 = 256;`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct TileGeometry { pub width: u64, pub height: u64, pub channels: u32, pub tile: u32 }` with `levels(&self) -> u32`, `level_dims(&self, level: u32) -> (u64, u64)`, `tiles(&self, level: u32) -> (u32, u32)`, `align_rect(&self, r: [u64; 4]) -> [u64; 4]` (expand outward to level-0 tile boundaries, clamp to the canvas; exclusive `[x0, y0, x1, y1]`), `tiles_in_rect(&self, level: u32, r: [u64; 4]) -> Vec<(u32, u32)>` (`r` in level-0 coords).
  - `pub struct TileStore` (read-write): `create(path: &Path, g: TileGeometry) -> Result<Self>` (all tiles invalid), `open(path: &Path) -> Result<Self>`, `geometry(&self) -> TileGeometry`, `is_valid(&self, level: u32, tx: u32, ty: u32) -> bool`, `write_tile(&mut self, level, tx, ty, data: &[f32]) -> Result<()>` (marks valid), `read_tile(&self, level, tx, ty, out: &mut [f32]) -> bool`, `invalidate_rect(&mut self, r: [u64; 4])` (level-0 coords; invalidates the covering tiles at every level), `invalidate_all(&mut self)`, `valid_count(&self, level) -> usize`, `flush(&self) -> Result<()>`.
  - `pub struct TileReader` (read-only, separately mapped, `Send + Sync`): `open(path) -> Result<Self>`, `geometry()`, `read_tile(&self, level, tx, ty, out: &mut [f32]) -> bool`.
- Format (decided; document it in the module docs):
  - Header, 4096 bytes: magic `b"MMMTILE\0"`, `u32` version `1`, `u32` tile, `u64` width, `u64` height, `u32` channels, `u32` levels, little endian, zero-padded.
  - Validity table: one byte per tile, level 0 first, row-major; `0` invalid, `1` valid; padded to a 4096 multiple.
  - Tile data, level by level, row-major: each tile `channels × tile × tile` little-endian `f32`, planar. Edge tiles are stored full size, zero beyond the image.
  - Levels: level 0 is full size; level `l` is `ceil(w / 2^l) × ceil(h / 2^l)`; add levels until both dimensions fit in one tile.
  - Torn-read protection: the writer stores flag `0` (Release), writes the data, then stores `1` (Release). The reader loads the flag (Acquire), copies, and loads it again; the tile counts as valid only if both loads read `1`. Access the flag bytes as `AtomicU8` through the mapping.

- [ ] **Step 1: Write failing tests:** `geometry_levels` (`1000×600`, tile 256 → level dims `(1000,600),(500,300),(250,150)`, 3 levels, `tiles(0) == (4,3)`); `align_rect_expands_and_clamps` (`[300,10,700,20]` on 1000×600 → `[256,0,768,256]`; `[900,500,1000,600]` → `[768,512,1000,600]`); `create_open_roundtrip` (write tile (0,1,2), reopen, read back equal, others invalid); `edge_tiles_are_zero_padded` (level-0 tile (3,2) of 1000×600 written from a buffer whose out-of-image part is non-zero: reading it back returns zeros beyond column 232 / row 88; document that `write_tile` zeroes beyond the image); `invalidate_rect_clears_all_levels` (all tiles valid, `invalidate_rect([0,0,10,10])` → tile (0,0) invalid at every level, tile (1,0) at level 0 still valid); `reader_sees_invalid_while_tile_rewritten` (a writer clears the flag of a valid tile, as `write_tile` does first, simulated through a test-only hook or by calling `invalidate_rect`; a `TileReader` opened on the same file returns `false` until the write completes, then `true` with the new data).
- [ ] **Step 2: Run** `cargo test -p mmm-core tiles::` — FAIL.
- [ ] **Step 3: Implement** with `memmap2::MmapMut` (writer) and `memmap2::Mmap` (reader). `create` sizes the file with `set_len`.
- [ ] **Step 4: Run** — PASS; full check.
- [ ] **Step 5: Commit** `feat(core): memory-mapped tiled result store with per-tile validity`.

### Task 5: Blend into the tiled result

**Files:**
- Create: `crates/mmm-core/src/tiles/sink.rs` (move `tiles.rs` to `tiles/mod.rs`)
- Test: `crates/mmm-core/tests/tiles_blend.rs`

**Interfaces:**
- Consumes: Task 1 `JobCtl`, `CtlSink`; Task 4 `TileStore`, `TileGeometry`; `blend::{blend, BlendParams, Extent, RowSink}`.
- Produces:
  - `pub struct TileSink<'a>`: `new(store: &'a mut TileStore, rect: [u64; 4]) -> Result<Self>` (`rect` must be tile-aligned per `align_rect`; it calls `invalidate_rect(rect)` immediately). Implements `RowSink`: `begin` checks `w, h, ch` equal the rect size and the store's channels; `band` accumulates rows and writes each level-0 tile row once its 256 rows are complete; `finish` writes the last partial tile row and fails if rows are missing.
  - `pub fn build_parents(store: &mut TileStore, rect: [u64; 4]) -> Result<()>`: for each level ≥ 1, recompute the tiles covering `rect` from their four children. Each parent pixel is the per-channel mean of the child pixels that are covered (any channel non-zero), or zero if none are. A parent is valid only if every child inside the image is valid.
  - `pub fn blend_tiles(session: &Session, phot: &Photometry, surfaces: Option<&Surfaces>, graph: &OverlapGraph, settings: &BlendSettings, rect: Option<[u64; 4]>, store: &mut TileStore, ctl: &JobCtl) -> Result<[u64; 4]>`. Rect `None` means the whole canvas. It aligns the rect and builds `BlendParams` from `settings.to_params()` with `downsample = 1`, `extent = Some(Extent::Canvas)` and `roi = Some(aligned)`. It blends through `CtlSink(TileSink)` with stage `"blend"`, then runs `build_parents`, then reports `("pyramid", 1, 1)`, and returns the aligned rect. It errors if the store geometry differs from `session.canvas`.

- [ ] **Step 1: Write failing tests** in `tests/tiles_blend.rs`. Use a synthetic mosaic from `mmm_core::synth` (as `tests/e2e.rs` does) and a `MemSink` like the one in `tests/e2e.rs`. Tests:
  - `tiles_match_full_blend`: whole-canvas `blend_tiles` matches a direct `blend` with `Extent::Canvas` into `MemSink`, bit-for-bit on every level-0 tile.
  - `roi_tiles_match_full_blend`: an ROI blend validates only the aligned rect's tiles, and they equal the full blend's tiles.
  - `odd_canvas_tiles_match_full_blend`: a synth canvas whose size is not a multiple of 256 still matches.
  - `parents_are_covered_means`: a level-1 pixel equals the mean of its covered children, computed by hand from the level-0 tiles.
  - `cancelled_blend_leaves_rect_invalid`: cancel from the progress observer at the first `"blend"` event. The result is `Err` with `is_cancelled()`, and `valid_count` over the rect's tiles is 0 at every level.
  - `wrong_geometry_errors`.
- [ ] **Step 2: Run** `cargo test -p mmm-core --test tiles_blend` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS; full check (regression guard untouched and green).
- [ ] **Step 5: Commit** `feat(core): blend into the tiled result with ROI, parents and cancellation`.

### Task 6: Display stretch maths

**Files:**
- Create: `crates/mmm-core/src/stretch.rs`
- Test: unit tests in `stretch.rs`

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)] pub struct ChannelStats { pub median: f32, pub mad: f32 }`.
  - `pub fn channel_stats(planar: &[f32], pixels: usize, channels: usize) -> Vec<ChannelStats>`. It ignores no-data pixels (all channels zero). With no covered pixels it returns zeros. `mad` is the raw median absolute deviation.
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)] pub struct StretchParams { pub black: f32, pub white: f32, pub midtones: f32 }`.
  - `pub const SHADOWS_CLIP: f32 = -2.8; pub const TARGET_BACKGROUND: f32 = 0.25; pub const MAD_TO_SIGMA: f32 = 1.4826;`
  - `pub fn mtf(m: f32, x: f32) -> f32`: the midtones transfer function. It returns 0 at x = 0, 1 at x = 1 and 0.5 at x = m; general form `((m − 1)·x) / ((2m − 1)·x − m)`.
  - `pub fn auto_stretch(stats: &[ChannelStats], linked: bool) -> Vec<StretchParams>`, one per channel; linked mode gives every channel the same parameters, computed from the mean of the channels' medians and MADs. Per channel:
    - `black = clamp(median + SHADOWS_CLIP·MAD_TO_SIGMA·mad, 0, 1)`
    - `white = 1`
    - `midtones = mtf(TARGET_BACKGROUND, median − black)`
    - If `mad == 0`, `black = 0`.
  - `pub fn apply(p: &StretchParams, x: f32) -> f32`: `mtf(midtones, clamp((x − black)/(white − black), 0, 1))`.
  - Module docs: this follows the publicly described auto-stretch rule and is written independently; it is used for display only (data stays linear); the term is "display stretch".
- [ ] **Step 1: Write failing tests:**
  - `mtf_fixed_points`: m = 0.25 gives 0 at 0, 1 at 1, 0.5 at 0.25.
  - `stats_ignore_no_data`: three channels, half the pixels all-zero; the median uses only covered pixels.
  - `auto_puts_median_at_target`: for synthetic stats (median 0.1, mad 0.01), `apply(p, 0.1)` is within 1e-4 of 0.25.
  - `unlinked_differs_per_channel_linked_does_not`.
  - `flat_data_is_safe`: mad 0 yields finite parameters.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core stretch::`).
- [ ] **Step 5: Commit** `feat(core): display stretch statistics and auto stretch`.

### Task 7: Mosaic configuration, grouping, layout

**Files:**
- Create: `crates/mmm-core/src/mosaic/mod.rs` (config + layout), `crates/mmm-core/src/mosaic/grouping.rs`
- Modify: `lib.rs`; `crates/mmm-core/Cargo.toml` if `serde_json` needs a feature (no new crates)
- Test: unit tests in both files

**Interfaces:**
- Consumes: Task 2 settings types.
- Produces (`mmm_core::mosaic`):
  - Layout constants:
    - `pub const CONFIG_FILE: &str = "mosaic.json";`
    - `pub const FRAME_FILE: &str = "reference.mmm-frame.json";`
    - `pub const GROUPS_DIR: &str = "groups";`
    - `pub const RESULT_FILE: &str = "result.mmmt";`
    - `pub const CONFIG_FORMAT: u32 = 1;`
    - `pub const UNGROUPED: &str = "No group";`
    - `pub const SINGLE_GROUP: &str = "mosaic";` (the group name when grouping is off).
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct Fingerprint { pub len: u64, pub modified_unix_ms: i64 }`.
  - `pub fn fingerprint(path: &Path) -> Result<Fingerprint>`.
  - `pub struct PanelEntry { pub path: PathBuf, #[serde(default)] pub fingerprint: Option<Fingerprint>, #[serde(default)] pub manual_group: Option<String> }`.
  - `#[serde(tag = "kind", rename_all = "snake_case")] pub enum GroupRule { Header { key: String }, FilenameToken { keyword: String } }` (default `Header { key: "FILTER" }`).
  - `pub struct Grouping { pub enabled: bool, pub rule: GroupRule }` (default disabled).
  - `pub struct ProvenanceEntry { pub unix_ms: i64, pub mmm_version: String, pub action: String, #[serde(default)] pub group: Option<String>, #[serde(default)] pub detail: serde_json::Value }`.
  - `pub struct MosaicConfig { pub format: u32, pub panels: Vec<PanelEntry>, #[serde(default)] pub grouping: Grouping, #[serde(default)] pub analyze: AnalyzeSettings, #[serde(default)] pub blend: BlendSettings, #[serde(default)] pub group_overrides: BTreeMap<String, GroupOverride>, #[serde(default)] pub accepted_checks: Vec<crate::checks::CheckKey>, #[serde(default)] pub provenance: Vec<ProvenanceEntry>, #[serde(default)] pub ui: serde_json::Value }`.
    - `accepted_checks` refers to Task 8's `CheckKey`. To keep this task self-contained, define `CheckKey` and `CheckCode` in `checks.rs` here, as a stub module holding just those types; Task 8 fills that module in.
    - `ui` is opaque app state (compositions, display stretches, ROIs, view state). The core stores it but never interprets it.
  - `MosaicConfig` methods:
    - `new(paths: Vec<PathBuf>) -> Self`
    - `load(dir: &Path) -> Result<Self>`: rejects a `format` above `CONFIG_FORMAT` with an actionable `Error::Format`.
    - `save(&self, dir: &Path) -> Result<()>`: atomic; writes `mosaic.json.tmp` then renames over `mosaic.json`.
    - `settings_for(&self, group: &str) -> (AnalyzeSettings, BlendSettings)`: base settings with the group's override applied.
    - `push_provenance(&mut self, action: &str, group: Option<&str>, detail: serde_json::Value)`.
  - `pub fn is_mosaic_dir(dir: &Path) -> bool` (has `mosaic.json`).
  - `grouping.rs`:
    - `pub fn token_after(file_name: &str, keyword: &str) -> Option<String>`: splits the file stem on `_`; the keyword matches case-insensitively; returns the next non-empty token.
    - `pub fn header_value(panel: &InputPanel, key: &str) -> Option<String>`: `FILTER` goes through `filter_name()`; other keys come from `fits_keywords()` with quotes and whitespace trimmed; empty → `None`.
    - `#[derive(Clone, Debug, Default, PartialEq)] pub struct Groups { pub groups: BTreeMap<String, Vec<PathBuf>>, pub ungrouped: Vec<PathBuf> }`.
    - `pub fn resolve_groups(cfg: &MosaicConfig, header: &dyn Fn(&Path) -> Option<String>) -> Groups`. With grouping disabled, every panel goes into `SINGLE_GROUP`. With it enabled, `manual_group` wins over the rule, and panels the rule cannot place go to `ungrouped`.
    - `Groups::processing_groups(&self) -> Vec<(String, Vec<PathBuf>)>`: the groups, plus `UNGROUPED` when non-empty (ungrouped panels are processed as their own group; spec §5.2).
    - `pub fn group_dirs(names: &[String]) -> BTreeMap<String, String>`: maps each name to a directory name. Characters outside `[A-Za-z0-9._-]` become `_`; an empty result becomes `group`; collisions get `-2`, `-3`, … in name order. The mapping is deterministic.
- [ ] **Step 1: Write failing tests:**
  - `token_after_examples`: `"P11_filter_Ha_int.xisf"` with `filter` → `Ha`; with `FILTER` → `Ha`; with `nope` → `None`; `"x_filter_.xisf"` → `None`.
  - `grouping_off_is_single_group`.
  - `manual_beats_rule_and_ungrouped_collects`: driven by a closure header lookup.
  - `processing_groups_include_ungrouped_last`.
  - `group_dirs_are_safe_and_unique`: `["Ha 3nm", "Ha_3nm", "R/G", ""]` → `Ha_3nm`, `Ha_3nm-2`, `R_G`, `group`, with no duplicates.
  - `config_roundtrip_and_atomic_save`: save, load, equal; no `.tmp` left behind.
  - `future_format_rejected`.
  - `settings_for_applies_override`.
  - `header_value_reads_synth_filter`: a synth XISF written with a `FILTER` keyword via `synth::write_xisf_with_header_xml` or the FITS writer, whichever supports keywords.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core mosaic::`).
- [ ] **Step 5: Commit** `feat(core): mosaic configuration, grouping rules and session layout`.

### Task 8: Structured checks

**Files:**
- Modify: `crates/mmm-core/src/checks.rs` (fill in the stub from Task 7), `crates/mmm-core/src/astrometry/mod.rs` (accessor)
- Test: unit tests in `checks.rs`

**Interfaces:**
- Consumes: Task 7 `MosaicConfig`, `Groups`, `fingerprint`; `formats::InputPanel`; `session::Session`; `overlap::OverlapGraph`.
- Produces:
  - `WcsModel::has_distortion(&self) -> bool`: `false` only for the linear-only model.
  - `LinearWcs::pixel_scale_arcsec(&self) -> f64`: `sqrt(|det CD|)·3600`. Add it only if no equivalent exists.
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)] #[serde(rename_all = "snake_case")] pub enum Severity { Problem, Note, Passed }`.
  - `pub enum CheckCode` (same derives, snake_case, **stable: never rename**): `SolutionPresent, DistortionModel, ScaleMismatch, MixedChannels, GroupCountMismatch, FiltersDifferUngrouped, Ungrouped, FileChanged, FileMissing, NoOverlap, SkyLevelMismatch`.
  - `pub struct Check { pub code: CheckCode, pub severity: Severity, pub panels: Vec<PathBuf>, pub groups: Vec<String>, pub message: String, pub remedy: Option<String> }`, with `keys(&self) -> Vec<CheckKey>`: one key per panel, or one panel-less key if the check names no panels.
  - `pub struct CheckKey { pub code: CheckCode, pub panel: Option<PathBuf> }`, which defines the identity of an accepted check.
  - `pub fn is_accepted(cfg: &MosaicConfig, check: &Check) -> bool`: true when every key is in `accepted_checks`.
  - `pub struct PanelFacts { pub path: PathBuf, pub channels: u64, pub filter: Option<String>, pub solution: SolutionKind, pub scale_arcsec: Option<f64> }`, with `pub enum SolutionKind { None, Linear, Distorted }`.
  - `pub fn panel_facts(path: &Path) -> Result<PanelFacts>`: header-only.
  - `pub fn input_checks(cfg: &MosaicConfig, facts: &[PanelFacts], groups: &Groups) -> Vec<Check>`.
  - `pub fn file_checks(cfg: &MosaicConfig) -> Vec<Check>`: compares each panel's stored fingerprint with the file on disk.
  - `pub fn analysis_checks(group: &str, session: &Session, graph: &OverlapGraph) -> Vec<Check>`.
- Check rules. Messages are plain English, lead with the panel's file name, and the remedy says what to do:

  | Code | Problem when… | Note when… | Passed when… |
  | --- | --- | --- | --- |
  | `SolutionPresent` | — | no solution ("fine for registered panels; raw panels need one") | present |
  | `DistortionModel` | `Linear` (message: "missing distortion model"; remedy: re-solve with distortion correction) | — | `Distorted`; not emitted when there is no solution |
  | `ScaleMismatch` | scale differs >1% from the group median | — | within 1% |
  | `MixedChannels` | channel counts differ within a group (one check per group) | — | — |
  | `GroupCountMismatch` | — | grouping on and group panel counts differ (names the groups and counts) | — |
  | `FiltersDifferUngrouped` | — | grouping off and >1 distinct `filter` (suggest turning grouping on) | — |
  | `Ungrouped` | — | grouping on and `ungrouped` non-empty | — |
  | `FileMissing` | path no longer exists | — | — |
  | `FileChanged` | — | fingerprint differs ("it will be reanalysed") | — |
  | `NoOverlap` | a group of >1 panel has a panel with no overlap-graph edge | — | — |
  | `SkyLevelMismatch` | — | a panel's mean over channels (`PanelMeta::ch_mean`) is >2× or <0.5× the median of its overlap neighbours' means | — |

- [ ] **Step 1: Write failing tests**, using synth solved panels (`synth::write_xisf_solved` or `write_fits_solved`, with and without a SIP or distortion model):
  - `distortion_missing_is_problem`, `distortion_present_passes`
  - `scale_mismatch_flagged`
  - `group_count_mismatch_note`
  - `filters_differ_while_ungrouped_note`
  - `moved_and_modified_inputs_are_checks`: one file deleted gives `FileMissing`; one rewritten with a new length gives `FileChanged`; neither panics.
  - `accepted_requires_every_key`
  - `no_overlap_flagged`: a synth group with one disjoint panel.
  - `keys_are_stable_json`: `CheckCode::DistortionModel` serialises to `"distortion_model"`.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core checks::`).
- [ ] **Step 5: Commit** `feat(core): structured input, file and analysis checks`.

### Task 9: Mosaic orchestration: analyze all groups, blend a group into tiles

**Files:**
- Create: `crates/mmm-core/src/mosaic/run.rs`
- Test: `crates/mmm-core/tests/mosaic.rs`

**Interfaces:**
- Consumes: Tasks 1–8; `reference::{derive, ReferenceFrame}`; `analyze::analyze_ctl`; `tiles::{TileStore, TileGeometry, blend_tiles, TILE}`.
- Produces (re-exported from `mosaic`):
  - `pub struct Mosaic { pub dir: PathBuf, pub config: MosaicConfig }` with:
    - `create(dir: &Path, config: MosaicConfig) -> Result<Self>`: creates the dir, then saves.
    - `open(dir: &Path) -> Result<Self>`.
    - `save(&self) -> Result<()>`.
    - `groups(&self) -> Result<Groups>`: reads headers via `header_value` with the configured rule.
    - `group_session_dir(&self, group: &str, groups: &Groups) -> PathBuf`: `<dir>/groups/<group_dirs name>`.
    - `result_path(&self, group: &str, groups: &Groups) -> PathBuf`.
  - `pub fn analyze_all(m: &mut Mosaic, ctl: &JobCtl) -> Result<Groups>`. Steps:
    1. Resolve the groups.
    2. Derive the shared reference over **all** panels with `config.analyze.input` and save it to `FRAME_FILE`.
    3. `analyze_ctl` each processing group into its session dir, using `settings_for(group).0` and the reference. Progress stage names are prefixed with the group (`"Ha: analyze"`).
    4. Refresh every panel's fingerprint.
    5. Push provenance `action = "analyze"`, with `detail` holding the resolved settings per group.
    6. Save.
  - `pub fn blend_group(m: &mut Mosaic, group: &str, rect: Option<[u64; 4]>, ctl: &JobCtl) -> Result<[u64; 4]>`: opens the group session and loads the graph, photometry and surfaces exactly as `blend_cmd` in `crates/mmm/src/main.rs` does. It opens the result store, or creates it if it is missing or its geometry differs, with `TileGeometry { width, height, channels: canvas.2 as u32, tile: TILE }`. It then calls `blend_tiles` with `settings_for(group).1`, pushes provenance `action = "blend"` (with the group, rect and settings in `detail`), saves, and returns the aligned rect.
  - `pub fn set_blend_settings(m: &mut Mosaic, new: BlendSettings) -> Result<ChangeImpact>`: on `Reblend`, `invalidate_all` on every existing group result store; saves.
  - `pub fn set_analyze_settings(m: &mut Mosaic, new: AnalyzeSettings) -> Result<ChangeImpact>`: on `Reanalyze`, invalidates every result store too, and the next `analyze_all` reruns; saves.
  - `pub fn set_group_override(m: &mut Mosaic, group: &str, o: GroupOverride) -> Result<ChangeImpact>`: the same, but only for that group's store. An empty override removes the entry.
- [ ] **Step 1: Write failing tests** in `tests/mosaic.rs`:
  - `two_group_mosaic_end_to_end`: two synth groups ("Ha", "OIII") of aligned panels on one canvas, distinguished by a `FILTER` header, with grouping on and the header rule. `analyze_all` creates `groups/Ha/session.json` and `groups/OIII/session.json` plus `reference.mmm-frame.json`. `blend_group("Ha", None)` validates every level-0 tile, and the tiles equal a direct `blend` of the Ha session with `Extent::Canvas`.
  - `grouping_off_uses_single_group`.
  - `override_only_stales_its_group`: after both groups are blended, an Ha override sets `mode: Feather`; the Ha store has 0 valid tiles and OIII is unchanged.
  - `blend_settings_change_stales_all`.
  - `provenance_records_runs`: one `analyze` entry and one `blend` entry with `mmm_version == env!("CARGO_PKG_VERSION")`.
  - `cancel_analyze_all_is_cancelled`.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core --test mosaic`).
- [ ] **Step 5: Commit** `feat(core): multi-group mosaic orchestration over the tiled result`.

### Task 10: Mosaic health metrics

**Files:**
- Create: `crates/mmm-core/src/health.rs`
- Test: unit tests in `health.rs` (synthetic session via `synth`, analysed in a temp dir)

**Interfaces:**
- Consumes: `diag::{load_owner_map, seam_deltas}`, `photometry::Photometry`.
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct SeamStep { pub a: usize, pub b: usize, pub delta: f64, pub cells: usize, pub flagged: bool }`.
  - `pub struct Health { pub seams: Vec<SeamStep>, pub seam_delta_median: f64, pub seam_delta_max: f64, pub gain_range: Vec<(f64, f64)>, pub offset_range: Vec<(f64, f64)> }`. The gain and offset ranges are `(min, max)` per channel across panels.
  - `pub fn health(session: &Session, graph: &OverlapGraph, phot: &Photometry, surfaces: Option<&Surfaces>, feather_px: f32) -> Result<Health>`.
  - Rules: an edge is flagged when its delta > 3 × the median, the same rule `mmm report` uses (see `report` in `crates/mmm/src/main.rs`); edges with `n == 0` are omitted.
  - Optionally refactor `mmm report` to use `health`, as long as its output is unchanged.
- [ ] **Step 1: Write failing tests:**
  - `health_matches_report_rule`: on a synthetic mosaic, the medians equal `diag::MosaicSummary`'s `seam_delta_median` and `seam_delta_max` from `write_session_reports`.
  - `gain_range_spans_panels`: the ranges contain every panel's gain.
  - `outlier_edge_flagged`: an artificially inflated delta through a constructed `SeamDelta` map, via a pure helper `flag_steps(deltas) -> Vec<SeamStep>`.
- [ ] **Step 2–4:** Run, implement, run.
- [ ] **Step 5: Commit** `feat(core): mosaic health metrics as data`.

### Task 11: CLI: `init`, `set`, `check`, `run`

**Files:**
- Modify: `crates/mmm/src/main.rs` (consider moving the new commands to `crates/mmm/src/mosaic_cmd.rs` to keep `main.rs` from growing)
- Test: `crates/mmm/tests/cli.rs`

**Interfaces:**
- Consumes: `mmm_core::mosaic::*`, `checks::*`, `settings::*`, plus the existing `AnalyzeOpts` and `BlendOpts` parsing.
- Produces these commands. Every flag that is not passed leaves the setting unchanged.
  - `mmm init <panels…> -s S [--group-by header:KEY | --group-by token:KEYWORD] [analyze opts] [blend opts]`: creates a mosaic session at `S`, with grouping enabled when `--group-by` is given. It refuses an existing `mosaic.json` unless `--force` is passed.
  - `mmm set -s S [--group NAME] [analyze opts] [blend opts]`: sets mosaic-wide settings, or a per-group override with `--group`. It prints the `ChangeImpact`. This needs the existing option structs to report which flags were explicitly passed; use `Option<String>` fields or clap's `ArgMatches::value_source`.
  - `mmm check -s S`: prints the groups with their counts, then every check grouped by severity (problems, notes, then a passed count).
  - `mmm run -s S -o OUTDIR [--png] [--wcs-frame …]`: `analyze_all`, then for each processing group the existing streaming `blend_cmd` from that group's session to `OUTDIR/<group dir>.fits`, using the group's resolved blend settings. It does not export from the tile store; that is phase 3. It prints the same summary as `batch`.
  - `analyze`, `blend` and `report` given an `-s` that is a mosaic dir (`is_mosaic_dir`) fail with an actionable message pointing at `mmm run` and `mmm check`. Legacy session dirs work as before.
- [ ] **Step 1: Write failing tests** in `tests/cli.rs`, following that file's existing synth-based patterns:
  - `init_run_two_groups`: `init --group-by header:FILTER` on two synth groups, then `run`; both FITS outputs exist with identical dimensions.
  - `set_group_override_persists`: `set --group Ha --mode feather`; `mosaic.json` has `group_overrides.Ha.blend.mode == "feather"`, and the mosaic-wide mode is unchanged.
  - `check_lists_missing_distortion`
  - `blend_on_mosaic_dir_points_to_run`
  - `init_refuses_existing`
- [ ] **Step 2–4:** Run (`cargo test -p mmm --test cli`), implement, run.
- [ ] **Step 5: Commit** `feat(cli): mosaic sessions — init, set, check, run`.

### Task 12: Documentation

**Files:**
- Modify: `docs/DESIGN.md` ("Session directory": the mosaic layout next to the legacy layout; "CLI surface": the new commands; a new section "Standalone app groundwork (2026-11)" covering job control, the tile store format, checks, health and display stretch, with a pointer to the spec), `README.md` (CLI reference for `init`/`set`/`check`/`run`, plus a short "mosaic sessions" example), `crates/mmm-core/src/lib.rs` (API map complete), `CHANGELOG.md` (an Unreleased section).
- [ ] **Step 1:** Write the docs; check every command and flag you document against `mmm --help` output.
- [ ] **Step 2:** `cargo doc --no-deps -p mmm-core` has no warnings; full check.
- [ ] **Step 3: Commit** `docs: mosaic sessions, tile store and app groundwork`.

---

## Later plans (not part of this one)

- **Plan 2: app shell and viewer.** WSLg backend spike (glow vs wgpu); generic UI crate (theme, tiled pyramid viewer with OpenSeadragon-style springs, job panel, status bar); app crate (start screen, session lifecycle, background job runner over `JobCtl`).
- **Plan 3: tabs.** Panels, Plan (preview pyramids per panel, including the low-resolution WCS warp for raw panels; compositions; selection; checks UI), Blend (settings, ROIs, staleness display, health, export from the tile store to FITS/XISF/TIFF with provenance headers).
- **Plan 4: packaging and signing** (its own design).

## Outcome and carried follow-ups (2026-11-03)

Executed on `feat/app-groundwork` (subagent-driven, per-task review plus a
whole-branch review and one fix wave). Deviations from this plan, ruled
against the spec:

- `align_rect([900,500,1000,600])` is `[768,256,1000,600]` (outward
  expansion); the plan's example was wrong.
- Tile state is a per-tile little-endian u32 seqlock (bit 31 valid, odd
  sequence while writing), not a 0/1 byte; `TileState {Missing, Stale,
  Valid}` and `read_tile_any` expose stale tiles for dimmed display.
- On cancel, finished level-0 tiles stay valid (spec §8), replacing the
  plan's "whole rect invalid" test.
- Display stretch midtones use the normalised median
  `(median − black)/(white − black)`.
- Group directories are persisted in `MosaicConfig.group_dirs` and never
  reassigned; `Mosaic::group_session_dir/result_path` take only the group.
- `MosaicConfig.needs_analyze` gates `blend_group` after analyze-setting
  changes and during/after an interrupted re-analysis.

Must do in Plan 2, before the viewer ships:
- Two-writer guard on a tile store (advisory lock or in-process registry);
  the concurrency contract in `tiles`/`mosaic` docs and DESIGN.md is
  documentation only today.
- Split job computation from config mutation (a running job holds
  `&mut Mosaic` and saves at the end — lost-update hazard), and make
  `set_*` edits atomic (the CLI's mosaic-wide `set` makes two saves).
- Build parents per completed tile row so zoomed-out views keep a coarse
  level during a blend; make `build_parents` cancellable.
- A test reading through `TileReader` while `blend_tiles` runs on another
  thread.
- Store replacement without SIGBUS risk (temp + rename + header
  generation; readers closed first on Windows).
- `analyze_all` leaves groups it never reached unmarked after a cancel.
- Reprojection cancel is checked only per panel (~1 s target missed on
  2 GB panels).

Before the Checks UI (Plan 3): `panel_facts` reports a rejected/inconsistent
solution as "no solution" (a Note); give it its own Problem.

Can wait: `check` exit-code policy; unreadable header not a Check;
`merge_override` lists fields by hand; `blend_group` recreates on any open
error; empty tiles stored dense; `channel_stats` per-pixel allocation;
seqlock sequence wrap at 2^31 reports a stale tile as Missing; tests that
read synth files at fixed offsets.
