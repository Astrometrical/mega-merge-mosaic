# Panels, Plan, ROIs and Export Implementation Plan (phase 3 of the standalone app)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the v1 app workflow from the spec: the Panels tab (add files, automatic and manual grouping), the Plan tab (panel previews in place, user compositions, selection, checks), automatic background preview and analysis, Blend ROIs and composition views, and export of the blended result. A per-session cross-process lock guards it all.

**Architecture:**
- **Core:** a session lock, a busy-aware `set_inputs` edit, and a `preview` module. The preview module builds a small (≤512 px) linear preview per panel, placed in the shared reference frame and cached in the session. Export reads the tile store.
- **`mega-ui`:** gains a generic `PlanCanvas` widget (placed images over the phase-2 `View` springs) and a `checks_list` widget.
- **`mmm-app`:** gains the Panels and Plan tabs, an automatic pipeline (resolve groups → previews → analysis, restarted when inputs change), a checks engine, ROIs and composite views in Blend, and an Export job.

**Tech Stack:** Rust 2024, egui/eframe 0.36 (glow), egui_kittest 0.36, rfd 0.17, mmm-core, mpp-core (`tiff` feature for TIFF export).

**Spec:** `docs/superpowers/specs/2026-11-02-standalone-app-design.md`, §5.2, §5.3, §5.4, §5.5, §6, §7. Phase-2 follow-ups are in the "Outcome and carried follow-ups" section of `docs/superpowers/plans/2026-11-03-app-shell-and-viewer.md`.

## Global Constraints

- All phase-1 and phase-2 constraints still hold:
  - No core algorithm changes.
  - Linear data. Zero is no-data.
  - No `test_data/` in tests.
  - `missing_docs` in mmm-core and mega-ui.
  - fmt, clippy (`-D warnings`) and `cargo doc --no-deps` stay warning-free.
  - CLI behaviour is unchanged except for the additions named here.
  - "display stretch", never "STF".
  - egui/eframe/egui_kittest stay at 0.36.
  - **`mega-ui` must not depend on mmm-core.**
  - The Astrometrical palette, exactly.
- **The UI thread never waits** (spec §8). It does no header reads, no pixel decoding and no preview building. Small `mosaic.json` saves and state-table invalidation are the only documented exceptions.
- **GUI in automated checks:** run it only through `--selftest`, which closes itself. Never kill a GUI process on a timer.
- **Concurrency contract** (mmm-core mosaic module docs): plan on the owner thread, run on a worker, apply on the owner thread. Before calling `set_*` or `set_inputs`, cancel and join any job that holds a store.
- Export formats in this phase are FITS and TIFF (32-bit float). XISF export waits for an XISF writer in mpp-core (ruling; a follow-up).
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Run the full check before every commit.

## Review Focus

1. **The same session opened by two processes.** The second open is refused with "open in another window or process" and changes nothing (Task 1, `second_process_open_is_refused`).
2. **Editing inputs while a job runs.** The edit cancels the job, applies once it has stopped, and restarts automatic analysis. It never leaves stale tiles marked valid (Task 2 `set_inputs_stales_everything`, Task 6 `edit_during_analysis_restarts_pipeline`).
3. **100+ panels in Plan.** Previews build in parallel, the work is bounded and the UI stays responsive. Previews appear as each one finishes (Task 3 `previews_report_each`, Task 8 `plan_draws_partial_previews`).
4. **Raw solved panels in Plan before analysis.** They are placed correctly in the shared frame by a WCS warp, and the panel at an expected offset lands there (Task 3 `solved_preview_lands_in_frame`).
5. **Export of a stale or partially blended result.** The user is warned before writing, and the written file has the correct dimensions, WCS and provenance (Task 10 `export_warns_on_stale`, `fits_export_matches_streaming_blend`).

---

### Task 1: Cross-process session lock

**Files:** `crates/mmm-core/src/mosaic/lock.rs` (new), `mosaic/mod.rs`, `crates/mmm/src/mosaic_cmd.rs`, `crates/mmm-app/src/session.rs`. Tests are in each file.

**Interfaces:**
- **Produces:**
  - `pub const LOCK_FILE: &str = "session.lock";`
  - `pub struct SessionLock` (RAII). Dropping it releases the lock.
  - `pub fn lock_session(dir: &Path) -> Result<SessionLock>` takes an exclusive `File::try_lock` on `<dir>/session.lock`, creating the file if missing. If another holder has it, it returns `Error::Busy { path }`. The message names the session and says it is "open in another window or process".
  - CLI: `mmm init`, `set` and `run` take the lock for their whole duration.
  - App: `open_blocking` and `create_blocking` take the lock on the worker and keep it in `OpenSession` for the session's lifetime. A busy lock is shown on the start screen as an actionable error.
- [ ] **Step 1 — failing tests:**
  - `second_process_open_is_refused`: lock, then lock again in the same process from another handle. The second gives `is_busy()`; after the first drops, it succeeds.
  - CLI `run_refused_while_locked`.
  - App `open_refused_while_locked`.
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `feat: per-session lock across processes`.

### Task 2: `set_inputs`, busy-aware input edits

**Files:** `crates/mmm-core/src/mosaic/run.rs` and the module re-exports. Tests in `tests/mosaic.rs`.

**Interfaces:**
- **Consumes:** `lock_stores`, `MosaicConfig { panels, grouping, group_dirs, needs_analyze, … }`.
- **Produces:** `pub fn set_inputs(m: &mut Mosaic, panels: Vec<PanelEntry>, grouping: Grouping) -> Result<()>`.
  - If panels or grouping are unchanged, it is a no-op returning `Ok`.
  - Otherwise it locks every existing store first; a busy store refuses the whole edit and changes nothing.
  - It then invalidates every store, marks every group with a recorded dir as `needs_analyze`, sets panels and grouping, and pushes provenance `"inputs"` with counts. All of this is one save.
  - A `PanelEntry`'s `manual_group` survives a rule change, because it is part of the entry.
- [ ] **Step 1 — failing tests:**
  - `set_inputs_stales_everything`
  - `set_inputs_busy_changes_nothing`
  - `set_inputs_noop_when_unchanged`
  - `manual_group_survives_rule_change`
- [ ] **Step 2–4:** run (`cargo test -p mmm-core --test mosaic`), implement, run.
- [ ] **Step 5 — commit** `feat(core): busy-aware set_inputs for panel and grouping edits`.

### Task 3: Panel previews

**Files:** `crates/mmm-core/src/preview.rs` (new) and `lib.rs`. Unit tests in `preview.rs`, plus `tests/preview.rs`.

**Interfaces:**
- **Consumes:** `formats::InputPanel`; `reference::{ReferenceFrame, derive}`; `align::MosaicFrame`; `astrometry::WcsModel` (`sky_to_pixel` and its pixel→sky counterpart); `job::JobCtl`; `mosaic::{Mosaic, fingerprint, FRAME_FILE}`.
- **Produces:**
  - `pub const PREVIEW_MAX_SIDE: u32 = 512;` and `pub const PREVIEWS_DIR: &str = "previews";`
  - `#[derive(Clone, Debug, PartialEq)] pub struct Preview { pub path: PathBuf, pub rect: [f64; 4], pub width: u32, pub height: u32, pub channels: u32, pub data: Vec<f32> }`.
    - `rect` is the preview's extent in shared-frame canvas pixels, `[x0, y0, x1, y1]`.
    - `data` is planar and linear, and zero means no-data.
  - `pub fn build_preview(path: &Path, frame: &ReferenceFrame, max_side: u32, ctl: &JobCtl) -> Result<Preview>`:
    - **Aligned frame** (full-canvas registered panel): find the covered bbox, then take a block mean of covered pixels into ≤ `max_side` on the longer side.
    - **Solved frame:** map the panel's footprint into the frame, size the preview grid to ≤ `max_side`, and fill each preview pixel by the inverse mapping (frame pixel → sky → panel pixel) with a bilinear sample of a block-averaged panel copy. This is a low-resolution warp only; it is not the analysis reprojection.
    - `ctl.check()` once per preview row.
  - `pub fn cache_path(session_dir: &Path, path: &Path, fp: &Fingerprint) -> PathBuf` → `previews/<16-hex hash of path + len + mtime>.mmmp`.
  - `pub fn save_preview(p: &Preview, file: &Path) -> Result<()>` / `pub fn load_preview(file: &Path) -> Result<Preview>`. Format: a little-endian u32 JSON header length, a JSON header, then raw f32 LE. Writes are atomic (temp + rename).
  - `pub struct PreviewPlan { pub session_dir: PathBuf, pub frame_path: PathBuf, pub panels: Vec<(PathBuf, Option<Fingerprint>)>, pub input: InputSelect }` and `pub fn plan_previews(m: &Mosaic) -> PreviewPlan`.
  - `pub fn run_previews(plan: &PreviewPlan, ctl: &JobCtl, on_ready: &(dyn Fn(Preview) + Sync)) -> Result<()>`:
    1. Load `FRAME_FILE`, or derive it header-only and save it atomically if it is missing.
    2. For each panel, load the cached preview or build and cache it, on a bounded rayon pool of at most 4 threads (one decoded panel per thread).
    3. Call `on_ready` per panel as it finishes and report `("previews", done, total)`.
    - One panel's failure is logged through the report and does not fail the run. Record it in an `errors` list returned through `on_ready`'s companion `Result`. The implementer may use `run_previews(...) -> Result<Vec<(PathBuf, String)>>` for per-panel errors.
- [ ] **Step 1 — failing tests:**
  - `aligned_preview_block_mean_and_rect`
  - `solved_preview_lands_in_frame`: two synth solved panels with a known offset; their preview rects differ by the offset ±1 px, and a synth star lands in the right preview pixel.
  - `preview_cache_roundtrip_and_hit`: a second run doesn't rebuild (counter).
  - `previews_report_each`
  - `preview_cancels`
  - `bad_panel_does_not_fail_run`
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `feat(core): cached low-resolution panel previews in the shared frame`.

### Task 4: `mega-ui` `PlanCanvas` and `checks_list`

**Files:** `crates/mega-ui/src/plan.rs`, `crates/mega-ui/src/checks.rs`, `lib.rs`. Tests are kittest and unit tests.

**Interfaces:**
- **Consumes:** `viewer::View`, `Spring`, `PyramidGeometry` (only for `fit`), and the theme.
- **Produces:**
  - `pub struct Layer { pub texture: egui::TextureHandle, pub tint: Color32, pub additive: bool }`.
    - Additive layers use egui's premultiplied-alpha blending with alpha 0, which adds the colour. Document this trick on the type.
  - `pub struct PlacedItem { pub id: u64, pub rect: [f64; 4], pub layers: Vec<Layer>, pub outline: Color32, pub label: String, pub marker: Option<Color32>, pub selected: bool }`.
  - `pub struct PlanCanvas` with:
    - `new()`
    - `show(&mut self, ui, extent: [f64; 4], items: &[PlacedItem], show_outlines: bool, show_labels: bool) -> PlanResponse`
    - `home()` and `zoom_to(rect)`
  - `pub struct PlanResponse { pub clicked: Option<u64>, pub double_clicked: Option<u64>, pub hovered: Option<u64>, pub clicked_empty: bool }`.
  - **Behaviour:**
    - Spring pan and zoom as in the viewer.
    - Hit-testing uses the topmost item containing the cursor; items are drawn in slice order.
    - A selected item gets a 2.5 px outline in `ACCENT_400`; a marker is a filled circle with "!" at the item's top-right.
    - Overlay buttons: `+`, `−`, Home, and zoom to selection.
    - Double-click zooms to the item.
  - `pub enum Severity` is the existing one in theme.
  - `pub struct CheckItem { pub severity: Severity, pub title: String, pub detail: String, pub accepted: bool, pub acceptable: bool }`, `pub enum CheckAction { Accept(usize) }`, and `pub fn checks_list(ui, heading: &str, items: &[CheckItem], show_all: Option<&mut bool>) -> Vec<CheckAction>`.
    - Problems and notes are shown as cards with a severity bar.
    - Passed checks collapse into one green summary line with an expander.
    - An "Accept" button appears on acceptable, unaccepted problems.
- [ ] **Step 1 — failing tests:**
  - `click_selects_topmost`
  - `double_click_zooms_to_item`
  - `additive_layer_mesh_has_zero_alpha`
  - `checks_list_accept_emits_action`
  - `passed_checks_collapse`
- [ ] **Step 2–4:** run (`cargo test -p mega-ui`), implement, run.
- [ ] **Step 5 — commit** `feat(ui): PlanCanvas for placed previews; checks list`.

### Task 5: Panels tab

**Files:** `crates/mmm-app/src/panels_tab.rs` (new), `app.rs`. Tests are kittest.

**Interfaces:**
- **Consumes:** `set_inputs`, `MosaicConfig { panels, grouping }`, `mosaic::{PanelEntry, Grouping, GroupRule, UNGROUPED}`, `OpenSession.groups` (the cached `Groups`), and the job runner's group resolution.
- **Produces (spec §5.2):**
  - **Add files.**
    - "Add files…": an rfd multi-file picker on a thread, filtered to `xisf` and `fits`/`fit`/`fts`.
    - Drag-and-drop of files anywhere in the window (`ctx.input(|i| i.raw.dropped_files)`).
    - A typed-path field that adds one file or a glob.
    - Duplicates are ignored. No add-folder.
  - **Grouping switch**, off by default.
    - Off: a plain grid of file cards showing name and dimensions/channels once known.
    - On: a rule picker with Header (key text, default `FILTER`) or Filename token (keyword text), plus group columns with **"No group" first** (headed "N to sort" in the accent colour). Each other group shows a colour dot and its count.
  - **Cards** are multi-selectable: click, Ctrl/Cmd-click and Shift-click range. Esc clears the selection.
  - **Action bar** when something is selected:
    - **Move to ▾** (sets `manual_group` on each selected entry)
    - **Add to new group…** (name prompt)
    - **Remove**
  - It must not look like a spreadsheet (spec anti-goal).
  - **Applying an edit.** Every edit builds new `panels`/`grouping` and calls `PanelsEdit::apply`. If the runner holds a store, that cancels running jobs and queues the edit; the edit is applied in `poll` once the runner is idle (newer work wins).
  - **Group colours** are stable per group name, from a fixed palette derived from the accents: the first three groups get Hα red, OIII green and OIII blue, then further hues.
  - **Grouping suggestion.** When grouping is off and the `FiltersDifferUngrouped` check fires, a banner suggests turning grouping on.
- [ ] **Step 1 — failing tests:**
  - `add_files_dedupes`
  - `grouping_off_shows_plain_grid`
  - `no_group_column_first`
  - `move_to_sets_manual_group`
  - `add_to_new_group_creates_group`
  - `remove_drops_entries`
  - `edit_while_job_running_is_queued_then_applied`
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `feat(app): Panels tab — add files, grouping rules, manual placement`.

### Task 6: Automatic pipeline and checks engine

**Files:** `crates/mmm-app/src/pipeline.rs` (new), `crates/mmm-app/src/checks.rs` (new), `jobs.rs`, `app.rs`.

**Interfaces:**
- **Consumes:**
  - Task 3 `plan_previews`/`run_previews`;
  - phase-2 `JobRunner` (`start_analyze` with group resolution);
  - `checks::{panel_facts, input_checks, file_checks, analysis_checks, is_accepted}`.
- **Produces:**
  - **`JobRunner::start_previews(&mut self, session: &OpenSession) -> u64`.** A job that streams `Preview`s back over the event channel as `JobEvent::Preview(Preview)`. It is cancellable and repaints on each event.
  - **`Pipeline`**, owned by the shell (spec §5.2, analysis runs by itself):
    - On session open, and 1.5 s after the last input edit, it starts previews and then analysis.
    - Previews first: they are cheap and feed Plan. Analysis starts when previews finish, or at once if they're all cached.
    - A newer input edit cancels the running pipeline jobs and starts again.
    - Groups in `needs_analyze` with no running analysis trigger analysis automatically.
    - The user can pause automatic analysis with a toggle; the setting lives in `ui.auto_analyze`, default on.
  - **`ChecksEngine`:**
    - Runs on a worker after group resolution, after previews and after each analysis.
    - Computes `panel_facts` for all panels, `input_checks`, `file_checks`, and `analysis_checks` for each group with a `session.json`.
    - Produces `Vec<Check>` plus counts by severity. The status bar shows "Checks: N problems · N notes · N passed", coloured.
    - Accepting a check adds its keys to `accepted_checks`: a small save on the UI thread, which is the documented exception.
- [ ] **Step 1 — failing tests:**
  - `pipeline_runs_previews_then_analysis_on_open`
  - `edit_during_analysis_restarts_pipeline`
  - `auto_analyze_toggle_respected`
  - `checks_engine_counts_and_accept_persists`
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `feat(app): automatic preview and analysis pipeline; checks engine`.

### Task 7: Shared display-stretch panel and compositions model

**Files:** `crates/mmm-app/src/stretch_panel.rs` (new; moved out of `blend_tab.rs`), `crates/mmm-app/src/compositions.rs` (new), `blend_tab.rs`. Move `blend_tab.rs`'s tests to a sibling `blend_tab_tests.rs` (phase-2 follow-up).

**Interfaces:**
- **Produces:**
  - `pub fn stretch_panel(ui, state: &mut StretchState) -> StretchChange`. This is the existing Blend panel (Auto, Linked, per-channel black/white/midtones) extracted unchanged, so Blend and Plan share it.
  - `pub struct Composition { pub name: String, pub channels: [Option<(String /*group*/, f32 /*weight*/)>; 3] }`, stored in `ui.compositions`.
  - `pub fn load_compositions(ui: &Value) -> Vec<Composition>` and `pub fn save_compositions(ui: &mut Value, c: &[Composition])`.
  - `pub enum ViewKind { Group(String), Composition(usize) }`.
  - `pub fn composition_editor(ui, c: &mut Composition, groups: &[String]) -> bool` (changed).
  - Each view's display stretch is keyed by view name, stored separately for Plan (`ui.plan_stretch`) and Blend (`ui.blend_stretch`; the Blend format is unchanged).
- [ ] **Step 1 — failing tests:**
  - `compositions_roundtrip`
  - `stretch_panel_auto_emits_change`
  - `blend_tab_tests_still_pass` (moved)
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `refactor(app): shared display stretch panel; compositions model`.

### Task 8: Plan tab

**Files:** `crates/mmm-app/src/plan_tab.rs` (new), `app.rs`. Tests are kittest.

**Interfaces:**
- **Consumes:** Task 4 `PlanCanvas`/`checks_list`, Task 6 preview events and checks, Task 7 stretch panel and compositions.
- **Produces (spec §5.3):**
  - **View selector** in the top bar: each group, then the compositions, then "+ composition". It is hidden when grouping is off.
  - **Previews to textures.** Previews are converted to RGBA8 with the Plan display stretch on a small worker pool, never on the UI thread, and turned into textures as they arrive.
    - A group view uses the group colour for mono, or RGB for colour panels.
    - A composition draws each mapped group's previews as additive layers tinted to its channel colour (red, green, blue) times its weight.
  - **Auto display stretch** is computed from the statistics of the view's previews, on a worker.
  - **Overlays:** panel outlines (accent colour, or Hα for panels with an unaccepted problem) and panel numbers (toggles). Panels whose previews are still loading are drawn as dashed outlines.
  - **Selection.**
    - Click selects. Double-click zooms. Esc deselects.
    - The side panel shows: file name, full path with **Show in folder** (spawns the platform opener on the parent directory — `xdg-open`, `explorer /select,`, or `open -R`) and **Copy path**; dimensions with MP; channels; solution kind and scale; and the panels it overlaps (from the overlap graph once analysed).
    - Then the checks list filtered to the selected panel, with "show all".
  - **Status bar** keeps the canvas size with MP and the check counts.
- [ ] **Step 1 — failing tests:**
  - `plan_draws_partial_previews`: 2 of 4 previews arrive, giving 2 textured items and 2 dashed.
  - `selection_filters_checks`
  - `composition_view_uses_additive_layers`
  - `copy_path_puts_path_on_clipboard`
  - `group_selector_hidden_when_grouping_off`
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — selftest:** extend `make_selftest_session` with grouping on and two groups; `--selftest` exercises the Plan tab (switch tabs in the self-test frames) and exits 0.
- [ ] **Step 6 — commit** `feat(app): Plan tab — previews in place, compositions, selection, checks`.

### Task 9: Blend ROIs and composition views

**Files:** `crates/mmm-app/src/rois.rs` (new), `blend_tab.rs`, `tile_source.rs`; `crates/mega-ui/src/viewer/widget.rs` (ROI tool).

**Interfaces:**
- **Consumes:** phase-2 `TiledViewer`, `MosaicTileSource`, `JobRunner::start_blend(group, Some(rect))`, the frame `linear_wcs` (from `reference.mmm-frame.json`), and `TileReader::state`.
- **Produces:**
  - **mega-ui:**
    - `TiledViewer::set_tool(Tool::Navigate | Tool::DrawRect)`. `ViewerResponse` gains `drawn_rect: Option<[f64; 4]>` (image coordinates).
    - `TiledViewer::set_overlays(Vec<Overlay { rect, color, label, dashed }>)` draws rectangles in image coordinates.
  - **ROIs:**
    - `pub struct Roi { pub name: String, pub note: String, pub sky: Option<[[f64; 2]; 2]> /*RA,Dec corners*/, pub canvas: [u64; 4] }`.
    - They are stored in `ui.rois` in sky coordinates when the frame has a WCS (spec §7.1), otherwise in canvas coordinates. They are reprojected to canvas coordinates on load through the current frame.
    - The ROI list panel shows name, note, size with MP, and fresh/stale (the fraction of level-0 tiles inside that are Valid, computed on a worker).
    - Actions: "+ new ROI" (switches to the draw tool), rename and note edit, delete, and select (zooms to it).
  - **Blend ROI** blends the selected ROI for each group in the current view.
  - **Composition views in Blend.**
    - `CompositeTileSource` implements `TileSource` by fetching each mapped group's `MosaicTileSource` tile on the loader thread, then mixing tinted, weighted RGB.
    - Its generation is the sum of the member generations.
    - Its geometry must match across groups (shared frame); otherwise the composition is disabled with an explanation.
- [ ] **Step 1 — failing tests:**
  - `drawn_rect_reported_in_image_coords`
  - `roi_sky_roundtrip`
  - `roi_blend_starts_one_blend_per_view_group`
  - `roi_fresh_fraction`
  - `composite_source_mixes_channels`
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `feat(app): Blend ROIs (sky-anchored) and composition views`.

### Task 10: Export

**Files:** `crates/mmm-core/src/export.rs` (new) and `lib.rs`; `crates/mmm-core/Cargo.toml` (enable the mpp-core `tiff` feature); `crates/mmm/src/mosaic_cmd.rs` (`mmm export`); `crates/mmm-app/src/export.rs` (new).

**Interfaces:**
- **Produces:**
  - `pub enum ExportFormat { Fits, Tiff }`.
  - `pub struct ExportReport { pub stale_tiles: usize, pub missing_tiles: usize, pub width: u64, pub height: u64 }`.
  - `pub fn export_check(result: &Path) -> Result<ExportReport>` counts level-0 tiles that are not Valid, without writing anything.
  - `pub fn export_group(session: &Path, group: &str, out: &Path, format: ExportFormat, ctl: &JobCtl) -> Result<ExportReport>`:
    - Streams the level-0 tiles by tile row into a `RowSink` (FITS: the existing `FitsSink`) or mpp-core `write_tiff_with_progress` (32-bit float, planar to chunky).
    - Output is linear, full precision, and covers the whole canvas.
    - FITS headers carry WCS cards for the frame (as `blend_cmd` does) and `HISTORY` provenance lines: the mmm version and the group's blend settings from `mosaic.json`'s latest blend provenance.
    - Stale tiles are exported as they are, and missing tiles as zeros; the report counts both.
  - CLI: `mmm export -s S --group G -o OUT [--format fits|tiff]` prints the report and warns if anything is stale or missing.
  - App: "Export…" in the Blend tab.
    - A dialog chooses groups (current view), format and destination: rfd save dialog plus a typed path.
    - It runs `export_check` first. If anything is stale or missing, it shows a warning with counts and offers "Blend whole mosaic first" or "Export anyway".
    - The export runs as a job with progress.
- [ ] **Step 1 — failing tests:**
  - `fits_export_matches_streaming_blend`: synth group, blend to tiles, export FITS; the pixels equal a direct streaming blend with `Extent::Canvas` within 1e-6, and the WCS cards are present.
  - `tiff_export_dimensions_and_values`
  - `export_warns_on_stale`: the report counts are correct and CLI stderr has the warning.
  - `export_cancels`
  - App `export_dialog_warns_then_runs`.
- [ ] **Step 2–4:** run, implement, run.
- [ ] **Step 5 — commit** `feat: export the blended result to FITS and TIFF with provenance`.

### Task 11: Docs

**Files:** `docs/DESIGN.md` ("Standalone app (phase 3)": session lock, `set_inputs`, previews, the automatic pipeline, checks, Plan/Panels/ROIs/compositions, export, the XISF-export follow-up), `README.md` (app section: the workflow across the tabs; `mmm export`), `CHANGELOG.md`, `CLAUDE.md` (layout lines for the new modules if needed), `lib.rs` API map.
- [ ] Write the docs and check them against the code and `--help`. Run the full check, including `cargo doc --no-deps`.
- [ ] Commit `docs: standalone app phase 3`.
