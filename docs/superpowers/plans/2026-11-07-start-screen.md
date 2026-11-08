# Start Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace mmm's start screen with the "Hero stage" design. It covers:
- the last session's mosaic, crossfading through its groups and compositions;
- a session block with stats, notes and Resume;
- a shelf of session cards with pins, and an "All sessions" grid;
- new and open flows that start a session in one step.

**Architecture:**
- **mmm-core** gains three pieces:
  - a plain-file tile reader that can't fault when another process recreates a store;
  - a small per-session `summary.json`, written by the app and read by the start screen;
  - an overview renderer for cached previews.
- **mega-ui** gains:
  - a presentation mode for `TiledViewer`;
  - an in-memory `ImageTileSource`;
  - a starfield;
  - the `HeroStage` widget;
  - a session card;
  - an OS reduced-motion probe.
- **mmm-app** rebuilds `start.rs` as a `start/` module. The facts for the cards and the hero's views are loaded on background threads, the hero's first. The app writes the summary whenever what it reports changes.

**Tech Stack:** Rust 2024, egui/eframe/egui_kittest 0.36.2, rfd 0.17, mmm-core tile stores and previews, and the mega-ui standard from plan 1.

**Spec:** `docs/superpowers/specs/2026-11-07-ui-standard-and-start-screen-design.md`, §5. The mega-ui standard (§4) is already built, and `docs/DESIGN.md` "Mega UI standard" describes it.

## Global Constraints

- egui/eframe/egui_kittest stay at 0.36. No third-party egui crates. `mega-ui` must not depend on `mmm-core`.
- Tests never use `test_data/`; synthetic sessions come from `mmm-app` `testutil` / `synth_mosaic`. Every public item has a doc comment. fmt, `clippy --all-targets -D warnings` and doc must be warning-free.
- UI code takes colours, sizes, radii and durations from `mega_ui::theme` tokens; new tokens go in `theme::tokens` or `theme::palette`. Icons draw only in the icon font (`icons::font` / `icons::rich`).
- Harnesses that draw Mega widgets call `crate::testutil::themed(ui)` (mega-ui) or `crate::testutil::themed(ui)` (mmm-app), and draw nothing until it returns true.
- **The UI thread never does file I/O.** Facts, summaries, previews and tiles are read on background threads, and the UI polls channels.
- **The start screen never opens a session.** It takes no lock and runs no analysis. Live reads of a session's tiles use `FileTileReader`, never the memory-mapped `TileReader`. A session found busy is never read live.
- Spec values:
  - hero dwell 8 s and crossfade 2 s; with reduced motion, crossfades are cuts and there's no drift;
  - shelf cards: pinned first, then by last opened, as many as fit (about 5–10);
  - recent list kept: 100;
  - first hero image on screen in under 1 s from launch with a warm disk;
  - empty-state line: "No panels yet — resume to add them";
  - overline above the session name: "CONTINUE WHERE YOU LEFT OFF" for the most recent session, "SESSION" otherwise;
  - buttons: **Resume session** (large primary), **New mosaic** (ghost), **Open…** (quiet).

## Review Focus

1. **A recent session on an unplugged drive, or since deleted.** Its card says "Missing" with **Locate…** and **Remove from list**. It's never the default hero, and checking it never blocks the UI, even on a slow network path. Pinned in Task 6 (facts) and Task 12 (hero choice).
2. **A session open in another window or in the CLI.** It's never the default hero and never read live. If a store is recreated under the hero by another process, nothing crashes. Pinned in Task 1 (recreated-store read) and Task 12 (busy card).
3. **A recent session whose `mosaic.json` or `summary.json` is corrupt, or from a newer mmm.** Its card shows "Can't read this session", and nothing panics. Pinned in Tasks 2 and 6.
4. **A long recent list (100 sessions) on slow disks.** The screen draws at once, and the hero's facts load before the shelf's. Pinned in Task 6 (load order) and Task 12 (a frame before any facts).
5. **Notes edited while the session opens elsewhere, or typed quickly.** A busy session's notes are read-only, so a save never overwrites the other window's config. Typing is saved after a pause, not on every keystroke. Pinned in Task 6 (refused save) and Task 12 (debounce).

---

### Task 1: `FileTileReader`, a tile reader that survives store recreation

**Files:**
- Create: `crates/mmm-core/src/tiles/file_reader.rs` (a child module of `tiles`).
- Modify: `crates/mmm-core/src/tiles/mod.rs` (`mod file_reader; pub use file_reader::{FileTileReader, TileRead};`, and `impl TileRead for TileReader` by delegation). Factor the header parse out of `Layout::parse` if it needs the whole mapping.

**Interfaces:**
- Produces:
  - `pub trait TileRead: Send + Sync { fn geometry(&self) -> TileGeometry; fn state(&self, level: u32, tx: u32, ty: u32) -> TileState; fn read_tile_any(&self, level: u32, tx: u32, ty: u32, out: &mut [f32]) -> Option<TileState>; }`, with the same meaning as `TileReader`'s methods.
  - `pub struct FileTileReader` with `pub fn open(path: &Path) -> Result<Self>`, implementing `TileRead`.
- How it reads:
  - The file is read with positioned reads under a `Mutex<File>`.
  - Each tile uses the seqlock protocol: read the state `u32`; continue only if it is non-zero and even; read the samples; read the state again; accept only if the two are equal.
  - A short read, an I/O error or an out-of-range tile gives `None` / `TileState::Missing` and never panics.
  - The file is never mapped. Doc it as the reader for a store that another process may recreate.

- [ ] **Step 1: Write the failing tests** in `file_reader.rs`, building stores with `TileStore::create`, `write_tile` and `write_tile_stale` on a temp path:

```rust
#[test] fn file_reader_matches_mapped_reader()
// Level 0 and 1: some tiles valid, some stale, some never written. For every tile, FileTileReader's
// state() and read_tile_any() (state and samples) equal TileReader's.
#[test] fn file_reader_survives_recreation()
// Open FileTileReader; drop it from nothing; TileStore::create the same path with a different geometry
// (the file is truncated and rebuilt); every read on the old reader returns None or a complete tile —
// never panics.
#[test] fn file_reader_on_truncated_file_reads_none()
// After File::set_len(4096) under an open reader: state(..) == Missing and read_tile_any(..) == None.
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-core file_reader`. Expected: compile errors.
- [ ] **Step 3: Implement the trait, the delegation and `FileTileReader`.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mmm-core tiles`. Expected: PASS, with the existing tile tests unchanged.
- [ ] **Step 5: Commit.** `git commit -m "feat(core): FileTileReader, a store reader safe against recreation"`

---

### Task 2: The session summary file and the busy probe

**Files:**
- Create: `crates/mmm-core/src/session_summary.rs`.
- Modify:
  - `crates/mmm-core/src/lib.rs` (`pub mod session_summary;`, with a doc line in the API list);
  - `crates/mmm-core/src/mosaic/lock.rs` (`session_busy`) and its re-export in `mosaic/mod.rs`.

**Interfaces:**
- Produces:
  - `pub const SUMMARY_FILE: &str = "summary.json"`, at the session root, and `pub const SUMMARY_FORMAT: u32 = 1`.
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)] pub struct SessionSummary`, with fields:
    - `format: u32`, `written_at: u64` (unix seconds) and `panels: usize`;
    - `groups: Vec<GroupSummary>`;
    - `problems: Option<usize>` and `notes: Option<usize>` (`None` = not checked yet);
    - `canvas: Option<(u64, u64)>` and `last_view: Option<String>`.
  - `pub struct GroupSummary`, with fields:
    - `name: String` and `panels: Vec<PathBuf>`;
    - `result: ResultState`;
    - `blended_at: Option<u64>` and `exported_at: Option<u64>`.
  - `#[serde(rename_all = "snake_case")] pub enum ResultState { None, Complete, Stale, Partial }`.
  - `pub fn read_summary(dir: &Path) -> Option<SessionSummary>`: `None` when the file is absent, unreadable, unparsable, or `format > SUMMARY_FORMAT`.
  - `pub fn write_summary(dir: &Path, s: &SessionSummary) -> Result<()>`, atomic via `fsutil::write_atomic`.
  - `pub fn result_state(store: &Path) -> ResultState`, from `export::export_check`:
    - no file or an error: `None`;
    - every tile valid: `Complete`;
    - any stale tile: `Stale`;
    - some valid and some missing: `Partial`;
    - nothing valid: `None`.
  - `pub fn session_busy(dir: &Path) -> bool` in `mosaic::lock`. It opens `LOCK_FILE` without creating it (missing: `false`) and calls `try_lock_shared`. `WouldBlock` gives `true`. On success it unlocks at once and returns `false`. Doc the microseconds-long probe as the only time the start screen touches the lock.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn summary_roundtrips()                 // write_summary then read_summary == the value
#[test] fn summary_from_newer_mmm_is_ignored()  // format: SUMMARY_FORMAT + 1 → read_summary == None
#[test] fn corrupt_summary_is_ignored()         // "{not json" → None, no panic
#[test] fn result_state_follows_the_store()     // no file → None; all valid → Complete; one invalidated → Stale;
                                                // half written → Partial (stores made with TileStore)
#[test] fn busy_probe_sees_the_session_lock()   // no lock file → false; while lock_session(dir) held → true; after drop → false
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-core summary busy_probe`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mmm-core`.
- [ ] **Step 5: Commit.** `git commit -m "feat(core): session summary file and lock probe"`

---

### Task 3: An overview render of cached previews

**Files:**
- Modify: `crates/mmm-core/src/preview.rs` and its tests.

**Interfaces:**
- Produces:
  - `pub fn load_cached_previews(session_dir: &Path) -> Vec<Preview>`: every readable `.mmmp` under `previews/`, sorted by panel path. Unreadable files are skipped. Blocking.
  - `#[derive(Clone, Debug, PartialEq)] pub struct Overview { pub width: u32, pub height: u32, pub channels: u32, pub data: Vec<f32> }`: planar; zero means no data.
  - `pub fn render_overview(previews: &[&Preview], canvas: (u64, u64), max_side: u32) -> Option<Overview>`:
    - the canvas is scaled so its longer side is `max_side`;
    - each preview is resampled into its `rect` by nearest neighbour;
    - where previews overlap, each channel is averaged over the previews that have data there;
    - `channels` is the most any preview has, and mono previews fill channel 0 only;
    - `None` for no previews or an empty canvas.
  - The canvas comes from the previews' frame (`preview_frame_path`), loaded with the existing `ReferenceFrame` loader.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn overview_places_previews_in_their_rects()
// canvas (1000, 500), max_side 200 → overview 200×100; preview A rect [0,0,500,500] value 1.0,
// preview B rect [500,0,1000,500] value 3.0 → left half ≈1.0, right half ≈3.0.
#[test] fn overlap_is_averaged()     // A=1.0 and B=3.0 over the same rect → 2.0 there
#[test] fn uncovered_is_no_data()    // a pixel outside every rect is 0.0 in every channel
#[test] fn no_previews_no_overview() // render_overview(&[], (100,100), 64) == None
#[test] fn cached_previews_load_back() // save_preview ×2 into <tmp>/previews; load_cached_previews returns both, sorted
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-core overview cached_previews`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -m "feat(core): overview render of cached previews"`

---

### Task 4: The app writes the summary

**Files:**
- Create: `crates/mmm-app/src/summary_writer.rs`, with its tests.
- Modify:
  - `crates/mmm-app/src/main.rs` (module);
  - `crates/mmm-app/src/app.rs`, where the Shell owns a `SummaryWriter`, marks it dirty and records exports;
  - `crates/mmm-app/src/blend_tab.rs` and `plan_tab.rs`: `pub fn view_key(&self) -> Option<&str>`, the current view's stretch key, if they don't expose it yet.

**Interfaces:**
- Consumes: `session_summary::*` (Task 2) and `checks::CheckCounts`.
- Produces:
  - A pure function: `pub fn summarize(cfg: &MosaicConfig, groups: Option<&Groups>, checks: Option<CheckCounts>, canvas: Option<(u64, u64)>, last_view: Option<String>, results: &BTreeMap<String, ResultState>, stamps: &BTreeMap<String, (Option<u64>, Option<u64>)>) -> SessionSummary`. `stamps` maps a group to `(blended_at, exported_at)`; `written_at` is set by the writer.
  - `pub struct SummaryWriter` with:
    - `pub fn new(previous: Option<SessionSummary>) -> Self`, which seeds the stamps from the session's existing summary;
    - `pub fn mark_dirty(&mut self)`;
    - `pub fn record_blend(&mut self, group: &str, at: u64)` and `pub fn record_export(&mut self, group: &str, at: u64)`;
    - `pub fn poll(&mut self, session: &OpenSession, checks: Option<CheckCounts>, last_view: Option<String>)`. When dirty and no write is running, it clones what it needs and computes `result_state(mosaic.result_path(g))` for each processing group on a background thread. It writes the summary only if it differs from the last one written, ignoring `written_at`.
- The Shell:
  - creates the writer with each session (`read_summary` happens in the background open path, on `OpenSession`);
  - marks it dirty on open, on every job `Finished`/`Failed`, when the panels or grouping change, and when the check counts change (compare them each frame);
  - calls `record_blend` when a blend job finishes and `record_export` when an export job finishes. Keep a job-id → group map when queuing them; the time is `start::unix_now()`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn summarize_reports_groups_checks_and_results()
// two groups Ha (3 panels) and OIII (2); results {Ha: Complete, OIII: Stale}; checks {problems:1, notes:2};
// → panels 5, groups in processing order with their panel paths, problems Some(1), notes Some(2).
#[test] fn exports_survive_reopening()
// SummaryWriter::new(Some(summary with Ha exported_at 100)) then summarize keeps exported_at Some(100) for Ha.
#[test] fn shell_writes_summary_after_opening()
// Shell harness (crate::testutil::themed guard) opens testutil::two_group_session via typed path; step_until
// <session>/summary.json exists; read_summary has panels == the session's count and both group names.
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-app summar`.
- [ ] **Step 3: Implement the writer and the Shell hooks.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mmm-app`.
- [ ] **Step 5: Commit.** `git commit -m "feat(app): keep a session summary for the start screen"`

---

### Task 5: The recent library: pins, ordering, a bigger limit

**Files:**
- Create: `crates/mmm-app/src/start/mod.rs` (move `start.rs` here unchanged first, with `git mv`) and `crates/mmm-app/src/start/recent.rs`.
- Modify: `crates/mmm-app/src/app.rs`, where the storage key stays the same.

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)] pub struct RecentEntry { pub path: PathBuf, pub last_opened: u64, #[serde(default)] pub pinned: bool }`, moved from `start.rs`.
  - `pub const MAX_RECENT: usize = 100`.
  - `pub struct Library`, with:
    - `pub fn new(entries: Vec<RecentEntry>) -> Self`;
    - `pub fn shelf_order(&self) -> Vec<&RecentEntry>`: pinned first, then by `last_opened`, newest first;
    - `pub fn by_recency(&self) -> Vec<&RecentEntry>`: by `last_opened` alone, for choosing the hero;
    - `pub fn record_opened(&mut self, path: &Path, now: u64)`;
    - `pub fn set_pinned(&mut self, path: &Path, pinned: bool)`;
    - `pub fn remove(&mut self, path: &Path)`;
    - `pub fn relocate(&mut self, old: &Path, new: &Path)`, which keeps the pin and the time;
    - `pub fn entries(&self) -> &[RecentEntry]`, for persisting.
  - Over `MAX_RECENT`, the oldest unpinned entries are dropped.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn old_storage_without_pins_loads()   // serde_json from `[{"path":"a","last_opened":5}]` → pinned false
#[test] fn pinned_sessions_lead_the_shelf()   // pinned older entry comes before an unpinned newer one; by_recency ignores pins
#[test] fn limit_drops_oldest_unpinned()      // 101 entries, oldest pinned → kept; the oldest unpinned is dropped
#[test] fn relocate_keeps_pin_and_time()
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-app recent`.
- [ ] **Step 3: Implement `Library` and switch `StartScreen` and the Shell to it.** The persistence format stays a `Vec<RecentEntry>`.
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mmm-app`.
- [ ] **Step 5: Commit.** `git commit -m "feat(app): recent sessions with pins and a larger list"`

---

### Task 6: Card facts loaded in the background, and notes

**Files:**
- Create: `crates/mmm-app/src/start/facts.rs`.
- Modify: `crates/mmm-app/src/session.rs` (notes helpers).

**Interfaces:**
- Consumes: `read_summary`, `session_busy` and `ResultState` (Task 2); `RecentEntry` (Task 5).
- Produces, in `session.rs`:
  - `pub const NOTES_KEY: &str = "notes"`;
  - `pub fn notes(ui: &Value) -> String` and `pub fn set_notes(ui: &mut Value, text: &str)`, which keep the rest of `ui`, as `save_group_colors` does;
  - `pub fn save_notes_blocking(dir: &Path, text: &str) -> Result<(), String>`: take `lock_session(dir)` (busy: `Err(busy_message(dir))`), load `MosaicConfig`, `set_notes`, save, release.
- Produces, in `facts.rs`:
  - `pub enum Availability { Available, Missing, Busy, Unreadable(String) }`.
  - `pub enum CardState { NoPanels, PlanReady, Merged, MergedExported, ReMergeNeeded, Unknown }`.
  - `pub fn card_state(panels: usize, has_previews: bool, summary: Option<&SessionSummary>) -> CardState`, in this order:
    1. no panels: `NoPanels`;
    2. any group `Stale` or `Partial`: `ReMergeNeeded`;
    3. every group `Complete`, each with `exported_at >= blended_at`: `MergedExported`;
    4. every group `Complete`: `Merged`;
    5. `has_previews`: `PlanReady`;
    6. otherwise: `Unknown`.
  - `#[derive(Clone, Debug)] pub struct CardFacts { pub path: PathBuf, pub name: String, pub last_opened: u64, pub availability: Availability, pub panels: Option<usize>, pub groups: Vec<(String, usize)>, pub group_colors: BTreeMap<String, usize>, pub state: CardState, pub problems: Option<usize>, pub notes_count: Option<usize>, pub notes: String, pub summary: Option<SessionSummary> }`. `group_colors` comes from `panels_tab::load_group_colors(&cfg.ui)`, the session's saved palette indices.
  - `pub fn load_facts(entry: &RecentEntry) -> CardFacts` (blocking):
    - not a mosaic dir: `Missing`;
    - `session_busy`: `Busy`, with the config still read for the facts;
    - config unreadable: `Unreadable(message)`;
    - `has_previews` means `previews/` holds any `.mmmp`.
  - `pub struct FactsLoader` with:
    - `pub fn spawn(entries: Vec<RecentEntry>, first: Option<PathBuf>, ctx: &egui::Context) -> Self`: one background thread that loads `first` before the rest and requests a repaint per result;
    - `pub fn poll(&mut self) -> Vec<CardFacts>`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn facts_of_a_session()          // testutil::two_group_session → Available, panels Some(n), notes ""
#[test] fn missing_session_is_missing()  // a path that does not exist → Missing, no panic
#[test] fn busy_session_is_busy()        // while lock_session held → Busy
#[test] fn unreadable_session_says_so()  // mosaic.json = "{" → Unreadable(_)
#[test] fn card_state_rules()            // one assertion per row of card_state's order above
#[test] fn notes_roundtrip()             // save_notes_blocking then load_facts.notes == text; other ui keys kept
#[test] fn notes_save_refused_while_busy() // lock held → Err containing "open in another window", file unchanged
#[test] fn hero_facts_arrive_first()     // FactsLoader over 5 entries with first = the 4th → first poll result is the 4th
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-app facts notes_`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -m "feat(app): background card facts and session notes"`

---

### Task 7: Viewer presentation mode and `ImageTileSource`

**Files:**
- Create: `crates/mega-ui/src/viewer/image_source.rs`.
- Modify: `crates/mega-ui/src/viewer/widget.rs`, `crates/mega-ui/src/viewer/mod.rs` (export), and `widget_tests.rs`.

**Interfaces:**
- Produces:
  - `TiledViewer::set_presentation(&mut self, on: bool)`. When on:
    - the viewer only senses hover: no pan, zoom, double-click or drawing;
    - there are no overlay buttons and no background fill;
    - stale tiles draw at full brightness.
  - `TiledViewer::set_opacity(&mut self, opacity: f32)`: every tile's tint alpha is multiplied by it.
  - `TiledViewer::view_mut(&mut self) -> &mut View`.
  - `pub struct ImageTileSource` with `pub fn new(image: egui::ColorImage, tile: u32) -> Self`. It builds a pyramid by 2×2 box downsampling (transparent pixels don't count) until a level fits one tile, and implements `TileSource`: generation 0, and `Fetched::Valid` crops at edges.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn presentation_ignores_drag()        // drag across the viewer → view centre and zoom unchanged
#[test] fn presentation_has_no_buttons()      // no node labelled "Home" (or the existing overlay button labels)
#[test] fn opacity_scales_tile_tint()         // set_opacity(0.5) → every painted image mesh vertex alpha ≈ 128
#[test] fn image_source_builds_a_pyramid()    // 1000×600 image, tile 256 → levels until 1 tile; level 0 tile (0,0)
                                              // top-left pixel equals the image's; the top level is one tile
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui presentation image_source opacity`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mega-ui`.
- [ ] **Step 5: Commit.** `git commit -m "feat(mega-ui): viewer presentation mode and in-memory tile source"`

---

### Task 8: Starfield and reduced motion

**Files:**
- Create: `crates/mega-ui/src/starfield.rs` and `crates/mega-ui/src/motion.rs`.
- Modify:
  - `crates/mega-ui/src/lib.rs`;
  - `crates/mega-ui/Cargo.toml`: `[target.'cfg(windows)'.dependencies] windows-sys = { version = "0.61", features = ["Win32_UI_WindowsAndMessaging"] }`. 0.61.2 is already in `Cargo.lock`.

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct Star { pub pos: egui::Pos2, pub radius: f32, pub color: egui::Color32 }`, with `pos` relative to the field's top left.
  - `pub fn stars(size: egui::Vec2, seed: u64) -> Vec<Star>`, which is pure and deterministic:
    - `round(area / 900)` stars (one per 900 square points);
    - star types drawn by weight from the web's table, `(r, g, b, weight)`: O `(155,176,255,1)`, B `(170,191,255,3)`, A `(202,215,255,6)`, F `(248,247,255,10)`, G `(255,244,234,15)`, K `(255,210,161,20)`, M `(255,180,111,20)`;
    - alpha `u³·0.9 + 0.08`;
    - radius 0.7, or 1.4 for 3 % of stars;
    - generated with splitmix64, with no `rand` dependency.
  - `pub struct Starfield` with `pub fn new(seed: u64) -> Self` and `pub fn paint(&mut self, painter: &egui::Painter, rect: egui::Rect)`. It caches one mesh per size, rebuilt only when the size changes.
  - `pub fn reduced_motion() -> bool`. It blocks; call it once, off the UI thread.
    - Windows: `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)` reporting animations off gives `true`.
    - macOS: `defaults read com.apple.universalaccess reduceMotion` printing `1`.
    - Linux: `gsettings get org.gnome.desktop.interface enable-animations` printing `false`.
    - Any failure gives `false`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn stars_are_deterministic()     // stars(v, 7) == stars(v, 7); != stars(v, 8)
#[test] fn star_count_follows_area()     // size 900×300 → 300 stars
#[test] fn stars_stay_inside()           // every pos within [0, size]
#[test] fn star_colours_are_spectral()   // every rgb is one of the seven types; alpha in [0.08, 0.98]
#[test] fn reduced_motion_probe_returns() // reduced_motion() returns without panicking
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui star reduced_motion`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -m "feat(mega-ui): starfield and OS reduced-motion probe"`

---

### Task 9: `HeroStage`

**Files:**
- Create: `crates/mega-ui/src/hero.rs` and `crates/mega-ui/src/hero_tests.rs` (a `#[path]` child).
- Modify: `crates/mega-ui/src/lib.rs` and `crates/mega-ui/src/theme/tokens.rs`. Add `HERO_DWELL_S: f64 = 8.0`, `HERO_CROSSFADE_S: f64 = 2.0` and `KEN_BURNS_ZOOM: f64 = 1.08`.

**Interfaces:**
- Consumes: `TiledViewer` presentation and opacity, `view_mut`, `ImageTileSource` (Task 7); `Starfield` (Task 8); `icon_button`; `text::Type`.
- Produces:
  - `pub struct HeroView { pub label: String, pub source: Arc<dyn TileSource> }`.
  - `pub struct HeroStage` with:
    - `pub fn new(loader_threads: usize) -> Self` and, for tests only, `pub fn new_synchronous() -> Self`;
    - `pub fn set_views(&mut self, views: Vec<HeroView>, start: usize, now: f64)`, which replaces the views and shows `start` at once;
    - `pub fn set_message(&mut self, message: Option<String>)`, the empty-state line drawn over the starfield;
    - `pub fn set_reduced_motion(&mut self, on: bool)`;
    - `pub fn current_label(&self) -> Option<&str>`;
    - `pub fn first_image_drawn(&self) -> bool`: true once any tile of a view has been painted;
    - `pub fn show(&mut self, ui: &mut Ui, image_rect: Rect, now: f64)`.
- What `show` draws:
  - the starfield over all of `ui.max_rect()`;
  - each visible view's viewer in `image_rect`, in presentation mode, fitted, with a Ken Burns drift: zoom eases from 1.0 to `KEN_BURNS_ZOOM` (alternating direction per view) with a pan of 2 % of the view, over dwell plus crossfade;
  - after `HERO_DWELL_S`, a crossfade to the next view: the outgoing opacity is `1 − t` and the incoming `t` over `HERO_CROSSFADE_S`;
  - a left-to-right gradient (`SURFACE_900` at 0.92 alpha, falling to 0 at 60 % of the width), a bottom-to-top gradient (0.97 falling to 0 at 45 %), and a radial vignette (to 0.6 at the edges), drawn as vertex-coloured meshes;
  - at the image area's bottom right: the current label (`Type::Small`, `TEXT_EMPHASIS`) and ghost icon buttons "Pause slideshow" / "Resume slideshow" (`icons::PAUSE` / `PLAY`) and "Next image" (`icons::SKIP_FORWARD`);
  - with reduced motion, no drift and a cut instead of a crossfade. One view never cycles.

- [ ] **Step 1: Write the failing tests** (two `ImageTileSource` views labelled "Ha" and "OIII", synchronous, `now` injected through the harness state):

```rust
#[test] fn cycles_after_the_dwell()      // now 0 → "Ha"; now 8.0+ε → crossfading (both drawn); now 10.1 → "OIII"
#[test] fn pause_holds_and_next_advances() // click "Pause slideshow" → at 20.0 still "Ha"; click "Next image" → "OIII"
#[test] fn reduced_motion_cuts()         // set_reduced_motion(true): at 8.0+ε label "OIII", no crossfade frame;
                                         // view zoom constant across frames
#[test] fn one_view_never_cycles()       // single view → "Ha" at 30.0
#[test] fn message_shows_without_views() // set_message(Some("No panels yet — resume to add them")) → label present
#[test] fn first_image_reports_drawn()   // after one frame with views → first_image_drawn()
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui hero`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -m "feat(mega-ui): hero stage"`

---

### Task 10: Session card

**Files:**
- Create: `crates/mega-ui/src/widgets/session_card.rs` and its tests.
- Modify: `crates/mega-ui/src/widgets/mod.rs`.

**Interfaces:**
- Produces:
  - `pub const CARD_WIDTH: f32 = 220.0; pub const CARD_THUMB_HEIGHT: f32 = 96.0;`.
  - `pub struct SessionCardData<'a> { pub title: &'a str, pub status: Option<(Tone, &'a str)>, pub pinned: bool, pub dimmed: bool, pub selected: bool, pub thumbnail: Option<&'a egui::TextureHandle> }`.
  - `pub fn session_card(ui: &mut Ui, card: &SessionCardData) -> Response`, which senses click and double-click and uses the title as its accessible label. It draws:
    - a `SURFACE_800` fill and `radius::CARD`;
    - a `BORDER` border, or 2 pt `ACCENT_500` when selected;
    - a thumbnail band `CARD_THUMB_HEIGHT` high on `SURFACE_900`, with the image fitted inside, transparent no-data showing through, and a pin glyph at the top right when pinned;
    - under it the title (`TEXT_STRONG`) and `status(tone, text)`;
    - `dimmed` (a missing session) draws everything at 0.4 opacity.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn card_clicks_and_double_clicks() // click → clicked(); double-click → double_clicked()
#[test] fn card_is_labelled_by_title()     // get_by_label("Carina Nebula SHO")
#[test] fn pinned_card_paints_a_pin()      // painted text contains icons::PIN in the icon family
#[test] fn card_has_fixed_width()          // response.rect.width() == CARD_WIDTH
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mega-ui card`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.**
- [ ] **Step 5: Commit.** `git commit -m "feat(mega-ui): session card"`

---

### Task 11: Hero views and thumbnails for a session

**Files:**
- Create: `crates/mmm-app/src/start/hero_views.rs`.
- Modify: `crates/mmm-app/src/tile_source.rs`. `MosaicTileSource` holds a `Box<dyn TileRead>`; `new` takes `impl TileRead + 'static`. Add `pub fn open_plain(path: &Path, stretch: Option<Vec<StretchParams>>) -> mmm_core::Result<Self>` over `FileTileReader`, keeping `open`, which is mapped, for the Blend tab.

**Interfaces:**
- Consumes:
  - `CardFacts` and `CardState` (Task 6);
  - `FileTileReader` (Task 1); `load_cached_previews` and `render_overview` (Task 3);
  - `HeroView` (Task 9) and `ImageTileSource` (Task 7);
  - `blend_tab::saved_stretch`, `stretch_panel::{saved_stretch_at, PLAN_STRETCH_KEY}`, `compositions::load_compositions`, `CompositeTileSource`, `tile_image` and `auto_stretch`.
- Produces:
  - `pub struct HeroData { pub views: Vec<HeroView>, pub start: usize, pub message: Option<String> }`.
  - `pub fn build_hero(facts: &CardFacts) -> HeroData` (blocking). It loads the session's `MosaicConfig` itself, for result paths, `ui` stretches and compositions; it takes no lock. By state:
    - **Missing, Busy or Unreadable:** no views, and the message is "Missing" / "Open in another window" / "Can't read this session".
    - **Merged, MergedExported or ReMergeNeeded:**
      - one view per summary group whose result isn't `None`, labelled `"{group} · {n} panels"`, from `MosaicTileSource::open_plain(cfg result path, saved Blend stretch or the source's auto stretch)`;
      - plus every saved composition whose groups are all merged, labelled with the composition's name, as a `CompositeTileSource`;
      - `start` is the index of `summary.last_view`, if present.
    - **PlanReady:**
      - for each summary group (one "All panels" view without a summary), `render_overview` of its cached previews at `max_side` 2048;
      - stretched with the saved Plan stretch for that view, or auto;
      - converted with `tile_image`'s rules (no data is transparent), then `ImageTileSource::new(img, 256)`;
      - compositions mix the group overviews with the R/G/B tints, as `CompositeTileSource` does.
    - **NoPanels or Unknown:** no views, and the message is "No panels yet — resume to add them".
  - `pub fn thumbnail(view: &HeroView) -> Option<egui::ColorImage>`: fetch the top-level tile `(0, 0)` of the view's source; `Valid` or `Stale` gives its image.

- [ ] **Step 1: Write the failing tests** (synthetic sessions; for the merged case, analyse and blend both groups of `testutil::two_group_session` with `mmm_core::mosaic::{analyze_all, blend_group}` and write a summary with `summarize`):

```rust
#[test] fn merged_session_has_a_view_per_group()   // labels ["Ha · n panels", "OIII · m panels"]; start honours last_view "OIII" → 1
#[test] fn composition_view_is_added()             // ui compositions [HOO: R←Ha, G←OIII, B←OIII] → a third view "HOO"
#[test] fn planned_session_renders_previews()      // previews cached (run_previews or save_preview), no results → views per group
#[test] fn busy_session_has_no_live_views()        // lock held → views empty, message "Open in another window"
#[test] fn empty_session_says_so()                 // no panels → message "No panels yet — resume to add them"
#[test] fn thumbnail_is_the_top_tile()             // thumbnail(&views[0]) is Some with size ≤ 256×256
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-app hero_views`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mmm-app`, which also covers the Blend tab using `open`.
- [ ] **Step 5: Commit.** `git commit -m "feat(app): hero views and thumbnails for the start screen"`

---

### Task 12: The start screen

**Files:**
- Modify: `crates/mmm-app/src/start/mod.rs`, a rewrite of the screen's UI. Split the drawing into `start/layout.rs` if it passes about 600 lines.
- Test: `crates/mmm-app/src/start/tests.rs` (a `#[path]` child, moving the existing start tests there).

**Interfaces:**
- Consumes: `Library` (Task 5); `FactsLoader`, `CardFacts`, `save_notes_blocking` and notes (Task 6); `HeroStage` (Task 9); `session_card` (Task 10); `build_hero` and `thumbnail` (Task 11); `brand::lockup`; `reduced_motion` (Task 8); the plan 1 widgets.
- Produces: `StartScreen::ui(&mut self, ui) -> Option<StartAction>`, with the same `StartAction` as today.
- Hero:
  - The hero session is the first entry in `Library::by_recency()` whose facts show it `Available`. Until its facts arrive, it's the most recent entry.
  - A background `build_hero` runs for it as soon as its facts arrive.
  - The shelf's `FactsLoader` starts with `first` = the hero candidate.
  - Thumbnails for the other cards are built only after `HeroStage::first_image_drawn()`.
  - `reduced_motion()` runs once on a background thread, and the result is passed to the stage.
- Layout, per spec §5.2:
  - The `HeroStage` fills the window; the image area is the right two thirds, from 2 % above to 18 % below.
  - Compact lockup at the top left, `space::XXL` in.
  - The session block, at most 480 wide, from about a quarter of the height, contains in order:
    - the overline;
    - the name (`Type::Hero`);
    - "Opened {ago} · {path}" (`Type::Small`);
    - chips: "{n} panels" plus one "{group} · {count}" per group, tinted `GROUP_PALETTE[facts.group_colors[group]]`. A group without a saved colour falls back to `physical_color(group)`, else a neutral chip;
    - a status line mapped from `CardState`:
      - Merged: Success "Merged";
      - MergedExported: Success "Merged · exported";
      - ReMergeNeeded: Warning "Panels changed since the last merge — re-merge needed";
      - PlanReady: Info "Plan ready";
      - NoPanels: Neutral "No panels yet";
      - Busy: Neutral "Open in another window";
      - Missing: Problem "Missing";
      - Unreadable: Problem "Can't read this session";
    - a check summary: "{p} problems · {n} notes" in their tones, or nothing when unknown;
    - `notes_field`: read-only unless `Available`. It saves through `save_notes_blocking` on a background thread 1 s after the last edit, and on losing focus.
    - the buttons: **Resume session** (large primary; disabled unless `Available`, with the tooltip "Open in another window" when busy), **New mosaic** (ghost, `icons::PLUS`) and **Open…** (quiet). Next to Open… is a "More" icon button whose menu has "Type a path…". There is no cross-process window switching: the spec allows "otherwise it says where the session is open", and the busy status line and tooltip do that.
  - The shelf along the bottom has `overline("Recent sessions")` with a quiet "All sessions ›" button on the right. Its cards follow `Library::shelf_order()`, as many as fit the width (`CARD_WIDTH` plus `space::M` gaps).
    - Clicking a card selects it: the hero and block switch to it.
    - Double-click, or Resume, opens it.
    - A pin icon button on the selected card toggles its pin.
    - Missing cards are dimmed, and the block shows **Locate…** (folder picker, then `Library::relocate`) and **Remove from list**.
  - "All sessions" replaces the shelf with a scrolling wrapped grid of every entry's card, a `text_input` "Search sessions and notes" (case-insensitive over name and notes), and a quiet "‹ Back"; Esc goes back too.
  - First run (an empty library): a large lockup centred on the starfield, one line "Merge your stacked panels into one seamless mosaic.", **New mosaic** (large primary) and **Open…**. No shelf.

- [ ] **Step 1: Write the failing tests** (synthetic sessions in a temp dir, `StubPicker`, the `themed` guard; steps bounded by `step_until`):

```rust
#[test] fn draws_before_any_facts_arrive()   // first frame after build shows "Resume session" and the newest name
#[test] fn hero_is_the_newest_available()    // newest entry missing → hero is the next; overline "CONTINUE WHERE YOU LEFT OFF"
#[test] fn busy_session_is_not_the_hero()    // newest locked → hero is the next; its card status "Open in another window"
#[test] fn resume_opens_the_hero()           // click "Resume session" → StartAction::Open(hero path)
#[test] fn card_click_selects_and_double_click_opens() // click card B → block shows B's name, overline "SESSION";
                                                       // double-click → Open(B)
#[test] fn pinned_cards_lead()               // pin the oldest → it is the first card
#[test] fn notes_save_after_a_pause()        // type into the notes field → mosaic.json unchanged immediately;
                                             // after 1 s of frames → notes(ui) == typed text
#[test] fn missing_card_can_be_removed()     // select missing → "Remove from list" → card gone from the shelf
#[test] fn all_sessions_searches_names_and_notes() // "All sessions ›" → search "gradient" → only the session whose notes say so
#[test] fn first_run_offers_new_and_open()   // empty library → "New mosaic", "Open…", the first-run line; no "Recent sessions"
```

  Update the existing tests: `typed_path_opens_session` moves behind "More → Type a path…", and `recent_list_marks_missing` becomes a dimmed card with the status "Missing".

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-app start`.
- [ ] **Step 3: Implement the screen.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test -p mmm-app`.
- [ ] **Step 5: Smoke test.** Run `cargo run -p mmm-app --example make_selftest_session -- <dir>`, then `cargo run -p mmm-app -- --selftest <dir>/m.mmm-session`. Expected: it passes.
- [ ] **Step 6: Commit.** `git commit -m "feat(app): hero-stage start screen"`

---

### Task 13: New mosaic in one step, typed paths, Open another

**Files:**
- Modify:
  - `crates/mmm-app/src/start/mod.rs`, where the dialogs trait changes;
  - `crates/mmm-app/src/app.rs`: remove the floating "Open another session" window, inject a launcher, and land a new session on the Panels tab;
  - `crates/mmm-app/src/session.rs` (`open_in_new_process`).

**Interfaces:**
- Produces:
  - `pub trait SessionDialogs: Send + Sync { fn pick_folder(&self, title: &str) -> Option<PathBuf>; fn pick_new_session(&self) -> Option<PathBuf>; }`, replacing `FolderPicker`.
    - The `rfd` version of `pick_new_session` is a save dialog titled "New mosaic" with the default name "Untitled".
    - The start screen appends `.mmm-session` when the picked path lacks it, then `create_blocking`s it.
  - When any dialog returns `None`, the typed-path row is shown inline, with the hint "No dialog? Type a path".
  - `pub trait Launcher: Send + Sync { fn open_window(&self, session: Option<&Path>); }`, held by the Shell (a real one spawning `open_in_new_process`).
    - `open_in_new_process` takes `Option<&Path>`; `None` starts on the start screen.
    - In a session window, "Open another…" and "New mosaic…" both call `open_window(None)`.
  - A created session opens on `Tab::Panels`.

- [ ] **Step 1: Write the failing tests** (update `new_mosaic_creates_session`, `non_mosaic_folder_shows_error` and `relative_path_is_absolute_and_reopen_refused` to the new flows):

```rust
#[test] fn new_mosaic_creates_from_one_dialog()   // stub pick_new_session → <tmp>/Orion; click "New mosaic" →
                                                  // <tmp>/Orion.mmm-session exists, session open, tab Panels
#[test] fn cancelled_dialog_reveals_typed_path()  // stub returns None → a text input "No dialog? Type a path" appears
#[test] fn open_another_launches_a_window()       // in a session: click "Open another…" → stub Launcher saw open_window(None);
                                                  // no "Open another session" window exists
```

- [ ] **Step 2: Run them and confirm they fail.** Run `cargo test -p mmm-app new_mosaic cancelled_dialog open_another`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them and confirm they pass.** Run `cargo test --workspace`.
- [ ] **Step 5: Commit.** `git commit -m "feat(app): one-step new mosaic, typed-path fallback, Open another in a new window"`

---

### Task 14: Gallery, snapshots, docs

**Files:**
- Modify:
  - `crates/mega-ui/src/gallery.rs`, adding sections "Starfield", "Hero stage" (two `ImageTileSource` views made from generated gradients, paused so snapshots are stable) and "Session cards" (normal, selected, pinned, dimmed);
  - `crates/mega-ui/tests/gallery.rs`, with markers "STARFIELD", "HERO STAGE" and "SESSION CARDS";
  - the reference PNGs in `crates/mega-ui/tests/snapshots/`;
  - `docs/DESIGN.md`, a new section "Start screen (2026-11)": the summary file, the facts and hero loaders, the load order, `FileTileReader`, and the lock probe;
  - `CLAUDE.md`, if a command changes.

- [ ] **Step 1: Extend the gallery test with the new markers and confirm it fails.** Run `cargo test -p mega-ui --test gallery`.
- [ ] **Step 2: Add the gallery sections and confirm the test passes.**
- [ ] **Step 3: Refresh the snapshots and check them by eye.** Run `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored`, inspect the PNGs, then rerun without `UPDATE_SNAPSHOTS`. Expected: PASS.
- [ ] **Step 4: Run the full check.** Run `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo doc --no-deps -p mega-ui -p mmm-core && cargo test --workspace`. Expected: clean.
- [ ] **Step 5: Look at the real start screen.** Run the selftest, and open the app on a machine with a few synthetic sessions in its recent list. Check the hero's crossfade, the drift, hovering and focus on cards, and the All sessions grid.
- [ ] **Step 6: Commit.** `git commit -m "docs: start screen design notes; gallery and snapshots for the hero stage"`
