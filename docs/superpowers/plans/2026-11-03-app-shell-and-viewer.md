# App Shell and Viewer Implementation Plan (phase 2 of the standalone app)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a runnable Mega Merge Mosaic desktop app. It opens or creates a mosaic session, runs analysis and whole-mosaic blends in the background with progress and cancellation, and shows the blended result in an OpenSeadragon-style tiled viewer with an automatic display stretch. The app is built on a generic egui crate that a later phase can lift into workflow-platform or Photometrical.

**Architecture:** Two parts:
- **Engine hardening first.** The phase-1 follow-ups the app depends on: a store writer lock, parent levels built incrementally, a job/config split, and reprojection cancellation.
- **Two new crates.**
  - `mega-ui` is generic. It holds the theme, the status bar, the job panel and the tiled viewer. It does not depend on mmm-core.
  - `mmm-app` is the eframe binary. It provides the start screen, the session lifecycle, the job runner, and a Blend tab vertical slice.

  The viewer reads tiles through a `TileSource` trait implemented on worker threads. The app's implementation reads `result.mmmt` through `TileReader` and applies the display stretch on the CPU into RGBA8 textures.

**Tech Stack:** Rust 2024, egui/eframe 0.36 (glow backend, matching workflow-platform), egui_kittest 0.36 for headless UI tests, rfd (file dialogs), sysinfo (memory/CPU), mmm-core.

**Spec:** `docs/superpowers/specs/2026-11-02-standalone-app-design.md`. Read §3, §4, §5.1, §5.4, §5.5, §8, §10, §11 and §13 before any task. Phase-1 follow-ups: the "Outcome and carried follow-ups" section of `docs/superpowers/plans/2026-11-02-app-core-groundwork.md`.

## Global Constraints

- Every phase-1 global constraint still holds:
  - No core algorithm changes.
  - Linear data end to end; zero is no-data.
  - Tests never read `test_data/`.
  - `missing_docs` is warned in mmm-core and mega-ui.
  - fmt, clippy (`-D warnings`) and doc stay warning-free.
  - Existing CLI behaviour is unchanged.
  - Use "display stretch", never "STF".
- Pin `egui = "0.36"`, `eframe = { version = "0.36", default-features = false, features = ["default_fonts", "glow", "wayland", "x11", "persistence"] }` and `egui_kittest = "0.36"`. These are the versions workflow-platform uses (spec §10).
- **`mega-ui` must not depend on mmm-core** (spec §10). Cargo enforces this boundary. Any mmm knowledge lives in `mmm-app`.
- Colours: Astrometrical palette exactly (spec §4).
  - Surfaces `#050505` / `#0e0e0e` / `#1a1a1a`; borders `#2a2a2a`.
  - Text: strong `#eeeeee`, body `#aaaaaa`, muted `#999999`, subtle `#8a8a8a`.
  - OIII accent `#4EBCD4` / `#2A95AB` / `#1D7588`.
  - Hα `#C83050` (problems).
  - OIII green `#4ED090` / `#2AB875` (success).
- The Angora font is used only through the logo and wordmark **images**. UI text uses egui's default fonts.
- **The UI thread never waits** (spec §8): no file I/O, decoding or processing on it. Jobs and tile loading run on other threads.
- **GUI processes in automated checks:** run them only through the app's own `--selftest` mode, which closes itself. Never kill a GUI process on a timer (WSLg).
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Before each commit, run the full check: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`.

## Review Focus

1. **A second writer on a result store.** It must get an actionable "in use" error and never write concurrently with the first (Task 1, `second_writer_is_refused`).
2. **A settings change while a blend job runs.** The change must not leave stale tiles marked valid. Through the app this means cancel and join, then apply. Through the core, `set_*` returns a "busy" error while a writer holds the store (Task 3, `set_while_blend_holds_store_is_busy`).
3. **The viewer on a canvas far larger than the screen** (e.g. 70 000 × 48 000). The work per frame and the tiles requested are bounded by the screen size, not the canvas size (Task 6, `visible_tiles_bounded_by_screen`; Task 7, `requests_independent_of_canvas_size`).
4. **Blending while the viewer is open.** Tiles appear progressively, stale tiles show dimmed rather than as holes, and no torn tile is ever drawn (Task 7 `stale_tiles_drawn_dimmed`; Task 2 `parents_valid_during_blend`).
5. **No file dialog on WSLg** (missing portal or GTK). The start screen must still work through a typed-path fallback (Task 8, `typed_path_opens_session`).

---

### Task 1: Exclusive writer lock on the tile store

**Files:**
- Modify: `crates/mmm-core/src/tiles/mod.rs`
- Test: unit tests there

**Interfaces:**
- Produces:
  - `TileStore::create` and `TileStore::open` take an exclusive advisory lock on the file with std `File::try_lock` (Rust ≥ 1.89). The lock is released when the store drops.
  - On contention they return `Error::Format` with the reason "result store is in use by another writer". Add `Error::is_busy(&self) -> bool`, which recognises this error. The implementer may instead add a dedicated `Error::Busy { path }` variant (preferred), plus `Error::busy(path)`.
  - `TileReader` takes no lock.
  - The concurrency-contract docs (tiles module docs and DESIGN.md) change from "documented only" to "enforced for writers".
- [ ] **Step 1: Write failing tests:**
  - `second_writer_is_refused`: `open` while a `TileStore` is live gives an error with `is_busy()`; after the first store drops, `open` succeeds.
  - `create_refused_while_open`.
  - `reader_unaffected_by_writer_lock`.
- [ ] **Step 2–4:** Run (`cargo test -p mmm-core tiles::`), implement, run.
- [ ] **Step 5: Commit** `feat(core): exclusive writer lock on the tile store`.

### Task 2: Parent levels built incrementally and cancellably

**Files:**
- Modify: `crates/mmm-core/src/tiles/sink.rs`
- Test: `crates/mmm-core/tests/tiles_blend.rs`

**Interfaces:**
- Consumes: `JobCtl` (job.rs), `TileStore`.
- Produces:
  - `build_parents(store: &mut TileStore, rect: [u64; 4], ctl: &JobCtl) -> Result<()>`: the signature gains `ctl`. It calls `ctl.check()?` once per parent tile row. Update every caller.
  - `TileSink` builds parents incrementally. After each completed level-0 tile row it calls `build_parents` over the region of the rect completed so far, so parents whose children are all valid become valid during the blend.
    - `TileSink::new` takes a `&JobCtl`.
    - The end-of-blend `build_parents` in `blend_tiles` stays as a final pass; it is cheap once tiles are valid. The implementer may restrict it to the rows not yet built.
  - Parent rules are unchanged: a parent pixel is the covered-mean of its children, and a parent is valid only if all its in-image children are valid.
- [ ] **Step 1: Write failing tests:**
  - `parents_valid_during_blend`: a progress observer at the `"blend"` stage that has passed the halfway point sees at least one valid level-1 tile, using a `TileReader` opened on the same file.
  - `parents_still_match_full_build`: the final parents equal a from-scratch `build_parents` within 1e-6.
  - `build_parents_cancels`: a pre-cancelled ctl returns `Cancelled`.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core --test tiles_blend`).
- [ ] **Step 5: Commit** `feat(core): build pyramid parents during the blend; cancellable`.

### Task 3: Split jobs from configuration; atomic settings edits

**Files:**
- Modify: `crates/mmm-core/src/mosaic/run.rs`, `crates/mmm-core/src/mosaic/mod.rs`, `crates/mmm/src/mosaic_cmd.rs`
- Test: `crates/mmm-core/tests/mosaic.rs`, `crates/mmm/tests/cli.rs`

**Interfaces:**
- Consumes: phase-1 `Mosaic`, `analyze_ctl`, `blend_tiles`, Task 1 `Error::is_busy`.
- Produces (all re-exported from `mosaic`):
  - **`pub struct AnalyzePlan`** (`Clone`, `Send`). It holds everything a background analysis needs, with no reference to `Mosaic`: the session dir, the reference-frame path, the panel list, and per group (name, session dir, result path, `AnalyzeSettings`, paths).
    - `pub fn plan_analyze(m: &mut Mosaic) -> Result<AnalyzePlan>` resolves groups and assigns dirs. It marks **every** processing group `needs_analyze` and invalidates each group's result store; a busy store returns the busy error. Then it saves.
  - **`pub struct AnalyzeOutcome { pub completed: Vec<String>, pub fingerprints: Vec<(PathBuf, Fingerprint)>, pub provenance: Vec<ProvenanceEntry> }`**.
    - `pub fn run_analyze(plan: &AnalyzePlan, ctl: &JobCtl) -> (AnalyzeOutcome, Result<()>)` derives and saves the reference frame, then analyses each group in turn. On cancel or error, `completed` lists the groups that finished and the error is returned alongside.
    - `impl Mosaic { pub fn apply_analyze(&mut self, o: &AnalyzeOutcome) -> Result<()> }` removes completed groups from `needs_analyze`, records the fingerprints of those groups' panels, appends provenance, and saves.
  - **`pub struct BlendPlan`** (`Clone`, `Send`): group, session dir, result path, `BlendSettings`, rect.
    - `pub fn plan_blend(m: &Mosaic, group: &str, rect: Option<[u64; 4]>) -> Result<BlendPlan>` refuses a group in `needs_analyze` or with no dir, exactly as phase 1 did.
    - `pub fn run_blend(plan: &BlendPlan, ctl: &JobCtl) -> Result<[u64; 4]>` loads the session, opens or creates the store under the writer lock (phase-1 recreate rules), and calls `blend_tiles`.
    - `impl Mosaic { pub fn apply_blend(&mut self, plan: &BlendPlan, rect: [u64; 4]) -> Result<()> }` appends provenance and saves.
  - `analyze_all` and `blend_group` stay as thin compositions of plan, run and apply, with the same observable behaviour, so the phase-1 tests keep passing.
  - **`pub fn set_settings(m: &mut Mosaic, analyze: AnalyzeSettings, blend: BlendSettings) -> Result<ChangeImpact>`**: one validation, one invalidation pass and one save. It returns the max impact. If any affected store is busy, it returns the busy error **before** changing anything.
    - `set_blend_settings`, `set_analyze_settings` and `set_group_override` keep their signatures and get the same busy-first behaviour.
    - The CLI's mosaic-wide `mmm set` uses `set_settings`.
  - Module docs: the concurrency contract becomes:
    - plan on the owner thread;
    - run on a worker with no `Mosaic`;
    - apply on the owner thread;
    - `set_*` is refused while a writer holds a store;
    - the app cancels and joins before applying settings.
- [ ] **Step 1: Write failing tests:**
  - `plan_run_apply_matches_analyze_all`
  - `cancelled_run_marks_all_unfinished_groups`: every group not completed stays in `needs_analyze` and is refused by `plan_blend`.
  - `run_blend_without_mosaic_borrow`: run on a spawned thread from a cloned plan while the main thread reads `Mosaic`.
  - `set_while_blend_holds_store_is_busy`: hold a `TileStore` open; `set_blend_settings` returns busy and `mosaic.json` is unchanged.
  - `set_settings_is_one_save`: the impact is the max of the two, and a failing blend validation leaves analyze settings unchanged.
  - CLI `set_mosaic_wide_is_atomic`.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core --test mosaic && cargo test -p mmm --test cli`).
- [ ] **Step 5: Commit** `feat(core): plan/run/apply job split; atomic, busy-aware settings edits`.

### Task 4: Reprojection observes cancellation within a panel

**Files:**
- Modify: `crates/mmm-core/src/align.rs`, `crates/mmm-core/src/analyze.rs`
- Test: unit test in `analyze.rs`

**Interfaces:**
- Consumes: `JobCtl`.
- Produces: the reprojection of one panel calls `ctl.check()?` at least once per 256 output rows. Thread `&JobCtl` into the reprojection entry point that `analyze_solved` calls; the IPC path passes `JobCtl::none()`. The pixel maths is unchanged, and `regression_guard` plus the WCS golden tests stay green.
- [ ] **Step 1: Write failing test** `reproject_cancels_mid_panel`. Use a synth solved panel tall enough for more than 2 row bands and a `JobCtl` whose token is cancelled from a test-only hook after the first band. A `#[cfg(test)]` counter or callback inside the band loop is acceptable. `analyze_ctl` returns `Cancelled`, and the hook shows fewer bands processed than the total.
- [ ] **Step 2–4:** Run, implement, run (`cargo test -p mmm-core analyze && cargo test -p mmm-core --test regression_guard --test wcs_golden`).
- [ ] **Step 5: Commit** `feat(core): cancellation inside panel reprojection`.

### Task 5: `mega-ui` crate: theme, status bar, job panel

**Files:**
- Create:
  - `crates/mega-ui/Cargo.toml` (deps: egui only; dev-dep `egui_kittest`)
  - `crates/mega-ui/src/lib.rs`
  - `crates/mega-ui/src/theme.rs`
  - `crates/mega-ui/src/status.rs`
  - `crates/mega-ui/src/jobs.rs`
- Modify: root `Cargo.toml` (workspace members; `[workspace.dependencies]` egui, eframe, egui_kittest, rfd, sysinfo)

**Interfaces:**
- Produces:
  - `theme`:
    - Palette constants with the hex values from Global Constraints: `pub mod palette { pub const SURFACE_900: Color32 … ACCENT_400, ACCENT_500, ACCENT_600, HA_500, GREEN_300, GREEN_400, TEXT_STRONG, TEXT_BODY, TEXT_MUTED, TEXT_SUBTLE, BORDER }`.
    - `pub fn apply(ctx: &egui::Context)` sets dark `Visuals` from the palette: panel and window fills, selection and hyperlink in OIII, widget strokes in the border colour, text in body.
    - `pub fn severity_color(s: Severity) -> Color32`, with mega-ui's own `pub enum Severity { Problem, Note, Passed }`, independent of mmm-core.
  - `status`:
    - `pub struct StatusItem { pub text: String, pub color: Option<Color32> }`.
    - `pub fn status_bar(ui: &mut Ui, left: &[StatusItem], right: &[StatusItem])`.
  - `jobs`:
    - `pub struct JobView { pub id: u64, pub label: String, pub stage: String, pub done: u64, pub total: u64, pub cancellable: bool }`.
    - `pub enum JobAction { Cancel(u64) }`.
    - `pub fn job_panel(ui: &mut Ui, jobs: &[JobView]) -> Vec<JobAction>`: a progress bar in the accent colour plus a Cancel button.
  - `lib.rs`:
    - `#![warn(missing_docs)]`.
    - Crate docs: the crate is generic, must not depend on mmm-core, and is intended to be lifted into workflow-platform or Photometrical later.
- [ ] **Step 1: Write failing tests:**
  - `palette_matches_brand`: assert each constant's RGB against the hex list.
  - `job_panel_cancel_returns_action`: an egui_kittest `Harness` renders one cancellable job; clicking the button labelled "Cancel" yields `JobAction::Cancel(id)`.
  - `status_bar_renders_items`: query the labels by text.
- [ ] **Step 2–4:** Run (`cargo test -p mega-ui`), implement, run.
- [ ] **Step 5: Commit** `feat(ui): mega-ui crate — Astrometrical theme, status bar, job panel`.

### Task 6: Viewer maths: viewport, springs, tile selection

**Files:**
- Create: `crates/mega-ui/src/viewer/mod.rs`, `crates/mega-ui/src/viewer/view.rs`
- Test: unit tests in `view.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct PyramidGeometry { pub width: u64, pub height: u64, pub tile: u32, pub levels: u32 }`, with `level_dims(level) -> (u64, u64)` and `tiles(level) -> (u32, u32)`. These are the same rules as mmm-core's `TileGeometry`: level `l` is `ceil(w / 2^l)`. Re-implement them here; do not depend on mmm-core.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct TileKey { pub level: u32, pub tx: u32, pub ty: u32 }`.
  - **`pub struct Spring`** (OpenSeadragon's model):
    - Fields `start`, `target`, `start_time`.
    - Value at time `t` is `start + (target − start) · (1 − e^(−k·s)) / (1 − e^(−k))`, where `s = min(1, (t − start_time) / ANIMATION_TIME)`.
    - Constants `pub const SPRING_STIFFNESS: f64 = 6.5;` and `pub const ANIMATION_TIME: f64 = 1.2;` (seconds), as in OSD.
    - Methods `new(v)`, `spring_to(target, now)`, `reset_to(v)`, `value(now)`, `is_settled(now)`.
  - **`pub struct View`**:
    - Springs for centre x and centre y (image pixels) and for zoom, with zoom in log space so it animates uniformly.
    - `fit(&mut self, geom, screen: Vec2, now)`: home, the whole image visible, centred.
    - `zoom_by(&mut self, factor, about_screen: Pos2, screen_rect: Rect, now)`: keeps the image point under the cursor fixed at the target.
    - `pan_by(&mut self, screen_delta: Vec2, now)`: immediate (`reset_to`) while dragging.
    - `zoom_to_rect(&mut self, image_rect, screen, now)`.
    - `image_to_screen` and `screen_to_image` at time `now`.
    - Zoom is clamped between `min = fit zoom / 2` and `max = 8` screen px per image px.
  - **`pub fn pick_level(zoom: f64, levels: u32) -> u32`**: the largest level whose tile pixels are at least 1 screen pixel, i.e. `floor(log2(1/zoom))` clamped to `[0, levels − 1]`.
  - **`pub fn visible_tiles(geom: &PyramidGeometry, level: u32, image_rect: [f64; 4]) -> Vec<TileKey>`**: the tiles of that level intersecting the visible image rect, clamped to the grid.
- [ ] **Step 1: Write failing tests:**
  - `spring_reaches_target_after_animation_time`
  - `spring_monotonic`
  - `fit_centres_and_contains_image`
  - `zoom_keeps_cursor_point_fixed`: compare `screen_to_image(cursor)` before and after, at a settled `now`.
  - `pick_level_examples`: zoom 1 → 0; 0.5 → 1; 0.24 → 2; tiny → `levels − 1`.
  - `visible_tiles_bounded_by_screen`: on a 70 000 × 48 000 geometry with a 1920 × 1080 screen, at every zoom from fit up to 8, the count is at most `(1920/256 + 2) · (1080/256 + 2)`.
- [ ] **Step 2–4:** Run (`cargo test -p mega-ui viewer::`), implement, run.
- [ ] **Step 5: Commit** `feat(ui): viewer viewport with OpenSeadragon-style springs and tile selection`.

### Task 7: Tiled viewer widget with background loading

**Files:**
- Create: `crates/mega-ui/src/viewer/source.rs`, `crates/mega-ui/src/viewer/widget.rs`, `crates/mega-ui/src/viewer/cache.rs`
- Test: unit and kittest tests in those files

**Interfaces:**
- Consumes: Task 6 types.
- Produces:
  - **`pub enum Fetched { Valid(egui::ColorImage), Stale(egui::ColorImage), Missing }`**.
  - **`pub trait TileSource: Send + Sync`**:
    - `fn geometry(&self) -> PyramidGeometry`.
    - `fn generation(&self) -> u64`: changes whenever any tile's content or appearance may have changed, e.g. a blend progressed or the display stretch changed.
    - `fn fetch(&self, key: TileKey) -> Fetched`: called on loader threads only.
  - **`pub struct TiledViewer`**:
    - `new(source: Arc<dyn TileSource>, loader_threads: usize)` and `set_source(..)`.
    - `show(&mut self, ui: &mut Ui) -> ViewerResponse`; `ViewerResponse { pub hovered_image_pos: Option<(f64, f64)> }`.
    - `home()` and `zoom_in()` / `zoom_out()` for buttons.
  - Behaviour:
    - **Loader threads.** They take requests from a queue that keeps only the newest visible set: requests not visible in the latest frame are dropped before fetching. They send results back over a channel. The UI thread uploads at most N textures per frame (N = 32) and calls `ctx.request_repaint()` while work is pending or springs are moving.
    - **Cache.** An LRU of `TextureHandle`, keyed by `(TileKey, generation)` and bounded (default 512 tiles). On a generation change, cached tiles stay drawable as a fallback until replaced.
    - **Drawing.** Coarse to fine: for each visible fine tile that is not loaded, draw the nearest loaded ancestor's sub-rect. Draw `Stale` tiles at 55% brightness with a diagonal hatch in the border colour (spec §5.4). Draw `Missing` as background.
    - **Input:**
      - drag pans;
      - scroll zooms about the cursor (factor 1.2 per notch);
      - double-click zooms ×2 about the point;
      - `+` / `−` / `Home` overlay buttons, top-left, in theme style (spec §5.3).
- [ ] **Step 1: Write failing tests**, using a procedural `TestSource` whose tiles are flat colours from the key, with an optional per-key `Stale`/`Missing` map and a fetch counter:
  - `requests_independent_of_canvas_size`: after the harness settles at the home view on a 1024 × 768 screen, the total fetches for a 10 000² source and a 1 000 000² source are each at most the Task 6 bound × levels shown.
  - `stale_tiles_drawn_dimmed`: a pure helper `tile_tint(&Fetched) -> Color32` returns ~55% grey for `Stale` and white for `Valid`.
  - `generation_change_refetches_visible_only`.
  - `home_button_fits`.
  - `drag_pans`: a kittest drag changes the view centre.
- [ ] **Step 2–4:** Run (`cargo test -p mega-ui`), implement, run.
- [ ] **Step 5: Commit** `feat(ui): tiled pyramid viewer — background loading, LRU cache, coarse-to-fine, stale display`.

### Task 8: `mmm-app`: shell, branding, start screen, session lifecycle, self-test

**Files:**
- Create:
  - `crates/mmm-app/Cargo.toml`: binary `mega-merge-mosaic`; deps mega-ui, mmm-core, eframe, egui, rfd, sysinfo.
  - `crates/mmm-app/src/main.rs`
  - `crates/mmm-app/src/app.rs`
  - `crates/mmm-app/src/start.rs`
  - `crates/mmm-app/src/session.rs`
  - `crates/mmm-app/assets/logo-white.png` and `crates/mmm-app/assets/wordmark-white.png`: downscaled copies of `../astrometrical/apps/web/src/assets/Logo-White.png` (to 128 px tall) and `Brand-White.png` (to 64 px tall). Commit the PNGs.
- Modify: root `Cargo.toml` (member)

**Interfaces:**
- Consumes: `mega-ui::theme`, `mmm_core::mosaic::{Mosaic, MosaicConfig, is_mosaic_dir}`.
- Produces:
  - **`main`**:
    - Window title "Mega Merge Mosaic"; the logo is the window icon.
    - `eframe::NativeOptions` with the glow renderer and persistence.
    - CLI args `[SESSION_DIR]` (open directly) and `--selftest`.
  - **`--selftest`**:
    - Renders 30 frames: start screen, then a session if one is given.
    - Logs the GL renderer and version strings from the glow context (`eframe::Frame::gl()`), at info level to stderr.
    - Then sends `ViewportCommand::Close` itself and exits 0. It exits 1 if any frame panicked or the GL context is missing.
    - This is the only way automated checks launch the GUI.
  - **`start` (spec §5.1)**:
    - Logo, wordmark and "by Astrometrical".
    - **New mosaic…** picks a parent folder and name, then creates `<name>.mmm-session` with an empty `MosaicConfig` via `Mosaic::create`.
    - **Open…** picks a `.mmm-session` folder.
    - Recent sessions: up to 10, persisted through eframe storage, showing name, path and last opened; missing paths are shown dimmed with "missing".
    - File dialogs use `rfd::FileDialog` on a background thread. If a dialog fails or is unavailable (e.g. no portal on WSLg), a typed-path text field is always present as a fallback (Review Focus 5).
  - **`session`**:
    - `pub struct OpenSession { pub mosaic: Mosaic, … }`. Opening runs on a background thread and reads `mosaic.json`; the UI shows a spinner.
    - A folder that is not a mosaic session gives an actionable error in the start screen, e.g. a legacy single-group session suggests `mmm init`.
    - One session per window. Opening another session while one is open starts a new process (`std::process::Command` on the current executable with the path) rather than replacing it (spec §3).
- [ ] **Step 1: Write failing tests** (kittest harness over `start::StartScreen` with a temp dir):
  - `typed_path_opens_session`: type an existing session path and press Enter; an open request for that path is emitted.
  - `new_mosaic_creates_session`
  - `recent_list_marks_missing`
  - `non_mosaic_folder_shows_error`
- [ ] **Step 2–4:** Run (`cargo test -p mmm-app`), implement, run.
- [ ] **Step 5: Run the self-test** once: `cargo run -p mmm-app -- --selftest`. Record the GL renderer line, the exit code, and whether it opened under WSLg in the task report. Do not commit the output.
- [ ] **Step 6: Commit** `feat(app): Mega Merge Mosaic shell — branding, start screen, sessions, self-test`.

### Task 9: Job runner and status bar in the app

**Files:**
- Create: `crates/mmm-app/src/jobs.rs`, `crates/mmm-app/src/status.rs`
- Modify: `crates/mmm-app/src/app.rs`

**Interfaces:**
- Consumes: Task 3 `plan_*`/`run_*`/`apply_*`, `JobCtl`, `CancelToken`, mega-ui `jobs` and `status`.
- Produces:
  - **`pub struct JobRunner`**:
    - `start_analyze(&mut self, session: &mut OpenSession)`: plans on the UI thread. The planning step is a short metadata write, which is allowed; document why it is not "processing".
    - `start_blend(&mut self, session: &mut OpenSession, group: &str, rect: Option<[u64; 4]>)`.
    - `cancel(id)`.
    - `poll(&mut self, session: &mut OpenSession) -> Vec<JobEvent>`: applies outcomes on the UI thread.
    - `views() -> Vec<JobView>`.
    - `busy(&self) -> bool`.
  - Each job runs on its own `std::thread` with a `JobCtl`. The progress observer sends `(stage, done, total)` over a channel, throttled to at most 30 updates per second.
  - **Newer work wins** (spec §8). Starting a blend while another blend runs cancels and joins the old one first. Joining happens on a helper thread so the UI never blocks; the new job starts when the old one ends. Analysis while a blend runs cancels the blend.
  - `pub enum JobEvent { Finished { id, label }, Cancelled { id }, Failed { id, message } }`. The app shows a toast-style notice for each and logs it.
  - **Status bar** (spec §5): the left side shows the running job's label and progress, or "Ready". The right side shows the canvas size with megapixels (e.g. "Canvas 9 255 × 18 310 (169 MP)"), group count, process memory (RSS) and CPU % from `sysinfo`, refreshed at most once per second off the UI thread.
- [ ] **Step 1: Write failing tests:**
  - `blend_job_runs_and_applies`: on a synth two-group mosaic, start analyze, poll until finished (bounded wait loop with sleeps in the test), start a blend, finish; provenance has both entries.
  - `cancel_reports_cancelled_and_session_consistent`
  - `newer_blend_cancels_older`
  - `megapixel_formatting` (`format_canvas(9255, 18310) == "9 255 × 18 310 (169 MP)"`).
- [ ] **Step 2–4:** Run, implement, run.
- [ ] **Step 5: Commit** `feat(app): background job runner (newer work wins) and status bar`.

### Task 10: Blend tab vertical slice: result viewer with automatic display stretch

**Files:**
- Create: `crates/mmm-app/src/blend_tab.rs`, `crates/mmm-app/src/tile_source.rs`
- Modify: `crates/mmm-app/src/app.rs` (tabs: Panels, Plan, Blend; Panels and Plan show "Coming in the next phase" in muted text)

**Interfaces:**
- Consumes: mega-ui `TiledViewer`, `TileSource`, `Fetched`, `PyramidGeometry`, `TileKey`; mmm-core `tiles::{TileReader, TileState}`, `stretch::{channel_stats, auto_stretch, apply, StretchParams}`; `JobRunner`.
- Produces:
  - **`MosaicTileSource`** implements `TileSource` over a group's `result.mmmt` via `TileReader`, using `read_tile_any` to map tiles to `Valid`, `Stale` or `Missing`.
    - It converts planar f32 to RGBA8 by applying per-channel `StretchParams`. Mono is replicated to grey; zero (no-data) pixels become transparent.
    - `generation` is an `AtomicU64`, bumped when the stretch changes and when the job runner reports blend progress for this group.
    - It reopens the reader when the store file is replaced (geometry changes); the viewer is reset in that case.
  - **Display stretch, automatic by default** (spec §5.5). When a result first opens, and whenever a blend finishes, compute `channel_stats` on a background thread from the coarsest valid or stale level's tiles, then `auto_stretch(.., linked = false)`.
    - The Blend tab has a compact display stretch panel: Auto button; black, white and midtones sliders per channel (linked sliders for mono); a Linked toggle.
    - Slider changes update the `StretchParams` and bump the generation.
    - Each view keeps its own stretch for Blend, persisted in `MosaicConfig.ui` under `"blend_stretch": {group: [StretchParams…]}` (spec §5.5: separate for Plan and Blend).
  - **Blend tab layout:**
    - Group selector (hidden when there is one group).
    - Buttons: **Analyze**, **Blend whole mosaic** and **Cancel**, disabled appropriately. Analyze is required when the group is in `needs_analyze`, with a muted explanation.
    - The viewer fills the centre. The display stretch panel is on the right, together with the phase-1 `health` metrics for the group when analysed: seam step median and max, flagged edges in Hα, gain range.
  - Spec §8 justification for CPU stretching: this phase stretches on the CPU per visible tile on loader threads. That keeps the viewer backend-agnostic for the later lift into workflow-platform (glow) or wgpu. The cost is that a slider drag re-converts the visible tiles (about 130 tiles at 1080p), which the loader threads do in parallel. A GPU path is a follow-up if drags fall short of "next frame".
- [ ] **Step 1: Write failing tests:**
  - `tile_source_maps_states`: a synth store with valid, invalidated and never-written tiles gives `Valid`, `Stale` and `Missing`.
  - `rgba_conversion_applies_stretch_and_transparency`
  - `auto_stretch_from_coarse_level`: the median pixel maps near 0.25 × 255.
  - `stretch_persisted_per_group_in_ui`
- [ ] **Step 2–4:** Run, implement, run.
- [ ] **Step 5: Self-test** `cargo run -p mmm-app -- --selftest <synth session dir>`, with a synth session created and blended by a test helper or by `mmm init` + the core API in a small example. It must exit 0. Record the output in the report.
- [ ] **Step 6: Commit** `feat(app): Blend tab — tiled result viewer, automatic display stretch, health`.

### Task 11: Docs and the WSLg record

**Files:**
- Modify:
  - `docs/DESIGN.md`: new section "Standalone app (phase 2, 2026-11)" covering the crates, the plan/run/apply contract, the writer lock, the viewer, CPU display stretch, and the WSLg self-test result with the GL renderer line from Tasks 8 and 10 reports.
  - `README.md`: "Desktop app (preview)": build and run, `--selftest`, WSL note.
  - `CHANGELOG.md`: Unreleased.
  - `CLAUDE.md`: layout lines for `crates/mega-ui` (generic, no mmm-core) and `crates/mmm-app`, the `--selftest` rule for GUI runs, and the egui pin.
  - `crates/mmm-core/src/lib.rs`: API map entries for any new items.
- [ ] **Step 1:** Write the docs. Check commands against `--help` and the code.
- [ ] **Step 2:** Run the full check.
- [ ] **Step 3: Commit** `docs: standalone app phase 2 — shell, viewer, job contract`.

---

## Later plans

- **Plan 3: tabs.** Panels (grouping UI), Plan (preview pyramids per panel with the low-resolution WCS warp, compositions, selection, checks UI), Blend ROIs (draw/save/notes, ROI blends, staleness cost estimate), export from the tile store (FITS/XISF/TIFF with provenance headers).
- **Plan 4: packaging and signing** (its own design).
