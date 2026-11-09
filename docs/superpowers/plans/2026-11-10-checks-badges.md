# Checks, Badges and Seams Implementation Plan (project 3, plan 6 of 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Problems stay in view and lead to their place.
- **Checks gain targets.** A check names a panel, an overlap, a seam or a group, so a problem can be badged where it is (on the plan, the mosaic and its group's chip) and clicked to zoom there.
- **The check set grows** to the spec's new checks: duplicates, coverage holes, thin overlaps, in-panel gradient, noise out of line, poor level matching, ragged edges and empty areas, and the residual seam after merging.
- **Pre-aligned panels** pass the distortion check.
- **A Seams overlay** draws where the merge changes panel, with flagged seams in the warning colour.
- **"See all checks"** drills down by severity, with un-accept.
- **The selected panel's details** follow the UI standard.

**Architecture:**
- **Core:**
  - `Check` gains a `target`, and `CheckKey` can name a pair (serde-defaulted, so accepted keys persist).
  - `input_checks` learns the input mode.
  - Cheap checks are computed from analysis outputs that are already loaded.
  - New per-panel statistics (noise, gradient, edge raggedness) are computed at analysis time into `PanelMeta`.
  - A `seam_lines` function turns the owner map into canvas polylines.
  - Seam health feeds level-matching and residual-seam checks.
- **App:**
  - The checks worker loads photometry and runs health for every group (with the feather of the group's last merge, else the draft's).
  - It returns the seam lines, re-runs after merges and draft feather changes, and supports un-accept.
  - The window resolves check targets to canvas rects for zooming and badges.
- **mega-ui:** canvas badges (icon and count, clickable), polyline overlays on both canvases, a chip problem count, and clickable, grouped checks lists with un-accept.

**Tech Stack:** Rust 2024, egui/eframe 0.36, egui_kittest, `mega-ui`, `mmm-core`.

**Spec:** `docs/superpowers/specs/2026-11-08-in-session-experience-design.md`. This plan covers:
- §7.3 (Outlines and Seams overlays over plan and mosaic)
- §7.4 (pointing at things: badges, click to zoom and select, panel details)
- §9 (problems and checks)

Recon map: the controller's `plan6-map.md`.

## Global Constraints

- **Plans 1–5's constraints hold:**
  - UI values come from `mega_ui::theme` tokens; mega-ui never depends on mmm-core.
  - fmt, `clippy --all-targets -D warnings`, doc and tests are clean. Tests synthesise their inputs, and condition waits are bounded by `testutil::PATIENCE`.
  - The UI says **merge**.
  - Problems surface in place, never in a pop-up; system errors are toasts.
  - No file I/O on the UI thread beyond the documented saves. Checks run on the checks worker.
  - Never process densely over the canvas: work from L8 summaries, overlap bands and sparse structures.
  - Every public mmm-core item has a doc comment.
  - Never signal with colour alone: every severity colour comes with its icon.
- **Persisted compatibility:**
  - `CheckCode` serialised names never change; new codes are added only.
  - Old `accepted_checks` entries keep matching.
  - Old sessions without the new `PanelMeta` statistics get no noise, gradient or edge checks (no false Passed) until they are analysed again.
  - The CLI's `mmm check` lists the new checks through the same `Check` messages.
- **Thresholds** are named, documented constants in core:
  - `THIN_OVERLAP_PX = 128` (the overlap's short side, canvas px).
  - `DUPLICATE_IOU = 0.9`.
  - `EMPTY_AREA_FRACTION = 0.25` (uncovered share of the panel's bbox).
  - `NOISE_FACTOR = 2.0` (vs the group median).
  - `GRADIENT_FACTOR = 5.0` (gradient amplitude vs the background MAD).
  - `RAGGED_FRACTION = 0.3` (share of edge cells partly covered).
  - Seams are flagged by the existing `flag_steps` rule.
- **Severities:**
  - **Problems:** duplicate panels, holes in coverage, thin overlaps, and the residual seam after merging.
  - **Notes:** strong gradient, noise out of line, ragged edges or empty areas, and poor level matching (before a merge: "a seam is likely").
  - **Passed:** pre-aligned "No distortion model: accepted for pre-aligned panels".
- **Exact copy:**
  - The check messages are fixed in Tasks 1–2 (they must pass `no_check_says_blend`).
  - The view bar toggle is **Seams**, toggled with **Shift+S** (S is Strong).
  - The drill-down sections are **Problems**, **Notes** and **Passed**.
  - The actions are **Accept** and **Un-accept**.
  - The badge's accessible description is "{n} problem(s) here".
  - The chip's problem count reads "{n}" with the warning icon.

## Review Focus

1. **Old sessions:** accepted checks stay accepted (old keys match), and checks needing new statistics stay silent until re-analysis. Test: Task 1 `old_accepted_keys_still_match` and Task 2 `old_sessions_get_no_stat_checks`.
2. **A clean synthetic session gets no false problems** from the new checks; they read Passed or stay silent. Tests: Task 1 `a_clean_session_raises_no_new_problems` and Task 2 the same for its checks.
3. **The checks worker never blocks the UI.** Health for every group runs on the worker. Draft feather edits re-run it debounced, not per keystroke. Test: Task 3 `feather_typing_runs_checks_once`.
4. **Clicking a problem whose target is gone** (panel removed, group renamed) does nothing harmful: no zoom, no panic, and the list updates on the next run. Test: Task 6 `a_stale_problem_target_is_harmless`.
5. **Badges** carry an icon and a count, read correctly to a screen reader, and sit at the same canvas place on the plan and the mosaic. Test: Task 5 `badges_match_on_plan_and_mosaic`.

---

### Task 1: Core: check targets, aligned distortion, cheap new checks

**Files:** `crates/mmm-core/src/checks.rs` (+ tests), `crates/mmm/src/mosaic_cmd.rs` (if `mmm check` needs the new parameter).

**Interfaces — Produces:**
```rust
pub enum CheckTarget { Session, Group(String), Panel(PathBuf), Overlap(PathBuf, PathBuf), Seam { group: String, a: PathBuf, b: PathBuf } }
pub struct Check { /* existing */ pub target: CheckTarget }
pub struct CheckKey { pub code: CheckCode, pub panel: Option<PathBuf>, #[serde(default)] pub other: Option<PathBuf> }
pub fn input_checks(cfg, facts, groups, aligned: bool) -> Vec<Check>;   // aligned: Passed "No distortion model: accepted for pre-aligned panels"
pub fn layout_checks(group: &str, session: &Session, graph: &OverlapGraph) -> Vec<Check>;
// new CheckCodes: DuplicatePanels, CoverageHole, ThinOverlap, EmptyArea
```
**The cheap checks:**
- **DuplicatePanels** (Problem, a pair target): the same canonical path twice, or two panels' bboxes with IoU at or above `DUPLICATE_IOU`.
- **CoverageHole** (Problem, a group target):
  - Rasterise the group's panel bboxes onto a coarse grid (L8 cells).
  - A hole is an uncovered region fully enclosed by covered cells and at least one panel's footprint in area.
  - It is reported with its rect.
  - `CheckTarget::Group` plus the hole rect: add `pub rect: Option<[u64; 4]>` to `Check` for area targets.
- **ThinOverlap** (Problem, an overlap target): an `OverlapEdge` whose short side (`bbox8` × 8) is below `THIN_OVERLAP_PX`.
- **EmptyArea** (Note, a panel target): `1 − nonzero_frac × canvas_area / bbox_area > EMPTY_AREA_FRACTION`.

Existing checks gain their targets: per-panel checks get `Panel`, `NoOverlap` gets `Panel`, `SkyLevelMismatch` gets `Panel`, and group checks get `Group`.

- [ ] **Step 1: Write the failing tests.**
  - One test per new check, firing and not firing.
  - `aligned_mode_passes_missing_distortion`.
  - `old_accepted_keys_still_match`: an old key without `other` matches.
  - `pair_keys_accept_one_overlap`.
  - `a_clean_session_raises_no_new_problems`.
  - Update `every_check`, `keys_are_stable_json` and `no_check_says_blend`.
- [ ] **Step 2:** `cargo test -p mmm-core checks` should FAIL.
- [ ] **Step 3:** Implement, with the constants documented.
- [ ] **Step 4:** `cargo test -p mmm-core` and `cargo test -p mmm` pass.
- [ ] **Step 5:** Commit `feat(core): checks name their place; pre-aligned panels pass; layout checks`.

---

### Task 2: Core: panel statistics at analysis, statistic checks, seam lines and seam checks

**Files:** `crates/mmm-core/src/{analyze.rs, session.rs, summary.rs, seam.rs, diag.rs, health.rs, checks.rs}` and their tests.

**Interfaces — Consumes:** Task 1. **Produces:**
```rust
// PanelMeta gains (all #[serde(default)]):
pub noise: Option<Vec<f64>>,     // per channel: robust MAD of L8 `detail` over the panel's background cells
pub gradient: Option<f64>,       // planar fit of L8 `mean` over covered cells: (max−min over the bbox) / background MAD
pub ragged: Option<f64>,         // share of the bbox's edge cells partly covered (0.05 < coverage < 0.95)
pub fn stat_checks(group: &str, session: &Session) -> Vec<Check>;   // NoiseOutOfLine, PanelGradient, RaggedEdges (Notes; skipped when the stats are None)
pub struct SeamLine { pub a: u16, pub b: u16, pub points: Vec<[f64; 2]> /* canvas px polyline(s) */, pub flagged: bool }
pub fn seam_lines(owner: &OwnerMap, health: &Health) -> Vec<SeamLine>;
pub fn health_with_lines(...) -> Result<(Health, Vec<SeamLine>)>;   // one owner-map pass for both
pub fn seam_checks(group: &str, session: &Session, health: &Health, merged: bool) -> Vec<Check>;
// flagged seams → LevelMatching (Note: "{a} and {b} still disagree after matching: a seam is likely") before a merge,
// ResidualSeam (Problem: "A seam shows between {a} and {b}") once the group is merged; target Seam { group, a, b }
```
- **Statistics** come from the panel's L8 summary at analysis time, which is already loaded there. There is no dense pass.
- **The noise check** compares each panel with its group's median noise per channel, using `NOISE_FACTOR`.
- **Seam lines** are the boundaries between owner cells, merged into polylines and scaled to canvas px. A seam line carries its flag from the matching `SeamStep`.

- [ ] **Step 1: Write the failing tests.**
  - `analysis_records_noise_gradient_and_raggedness`: synthetic panels with a known gradient, noise and ragged edge.
  - `stat_checks_fire_and_stay_quiet`.
  - `old_sessions_get_no_stat_checks`.
  - `seam_lines_follow_the_owner_boundary`.
  - `seam_checks_switch_from_note_to_problem_after_a_merge`.
  - `a_clean_session_raises_no_new_problems`, the same for these checks.
- [ ] **Step 2:** It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-core` passes.
- [ ] **Step 5:** Commit `feat(core): panel statistics, seam lines and seam checks`.

---

### Task 3: App: the checks engine runs everything

**Files:** `crates/mmm-app/src/checks.rs` (+ tests), `app.rs`, `session_window/mod.rs`.

**Interfaces — Consumes:** Tasks 1–2. **Produces:**
```rust
pub struct ChecksReport { pub checks: Vec<Check>, pub errors: Vec<String>, pub seams: BTreeMap<String /* group */, Vec<SeamLine>> }
impl ChecksEngine { pub fn unaccept(&self, session: &mut OpenSession, check: &Check) -> Result<(), String>; }
// SessionOutput gains `unaccept: Vec<Check>`; app.rs handles it like `accept`
```
- **`run_checks`** takes `aligned: bool` (from `session.frame`: `ReferenceFrame::Aligned`). For each analysed group it adds `layout_checks`, `stat_checks`, Photometry and `health_with_lines`, then `seam_checks`.
  - The feather is that of the group's last whole merge (`last_merge_settings`) when merged, else the draft's.
  - The seam lines go into the report.
- **Re-runs:** after analysis or previews (as today), after a whole merge finishes, and after a draft feather change, debounced by `CHECKS_DEBOUNCE = 0.75 s` after the last change.
- **Un-accept** removes the check's keys from `accepted_checks` and saves, mirroring `accept`.

- [ ] **Step 1: Write the failing tests.**
  - `checks_cover_every_group_with_seams`.
  - `a_merge_reruns_checks`.
  - `feather_typing_runs_checks_once`.
  - `unaccept_restores_a_problem`.
  - `aligned_sessions_pass_the_distortion_check`.
  - Update `checks_engine_counts_and_accept_persists`.
- [ ] **Step 2:** It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): checks run for every group, with seams, re-run after merges`.

---

### Task 4: mega-ui: canvas badges, polylines, chip counts, checks lists that lead somewhere

**Files:** `crates/mega-ui/src/canvas_layers.rs`, `plan.rs`, `viewer/widget.rs`, `widgets/chip.rs`, `checks.rs`, their tests, and the gallery and snapshots.

**Interfaces — Produces:**
```rust
pub struct CanvasBadge { pub id: u64, pub at: [f64; 2] /* canvas px */, pub tone: Tone, pub count: u32, pub description: String }
pub struct Polyline { pub points: Vec<[f64; 2]>, pub color: Color32, pub width: f32 /* token */, pub dashed: bool }
// TiledViewer & PlanCanvas: set_badges(Vec<CanvasBadge>), set_lines(Vec<Polyline>); responses report `badge_clicked: Option<u64>`
// ChoiceChip gains `problems: Option<u32>` (warning icon + count, accessible "{n} problem(s)")
pub enum CheckAction { Accept(usize), Unaccept(usize), Open(usize) }
pub fn checks_list(ui, heading, items, mode: ListMode) -> Vec<CheckAction>;   // ListMode::{Problems, All} — All = grouped Problems / Notes / Passed sections
// CheckItem gains `goes_somewhere: bool` (row clickable, keyboard-activatable)
```
- **Badges:**
  - A badge is a pill with the tone's icon and the count, sized from tokens, drawn at a canvas point after the overlays, at a constant screen size.
  - It is clickable and keyboard-reachable through its accessible node, which carries the description.
- **Lines** are painted with the overlays.
- **The plan** replaces its old "!" marker with a badge.
- **Lists:**
  - Rows that go somewhere are buttons: a click or Enter gives `Open`.
  - Accepted problems show **Accepted** and **Un-accept** in `All`.

- [ ] **Step 1: Write the failing tests:**
  - `a_badge_shows_icon_and_count_and_clicks`
  - `badges_keep_their_screen_size_when_zooming`
  - `polylines_paint_on_both_canvases`
  - `a_chip_shows_its_problem_count`
  - `rows_that_go_somewhere_open`
  - `all_mode_groups_by_severity_with_unaccept`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement. Refresh the gallery and snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored` and look at them; keep each page under 4096 pt.
- [ ] **Step 4:** `cargo test -p mega-ui` and `cargo test -p mmm-app` pass.
- [ ] **Step 5:** Commit `feat(mega-ui): canvas badges and lines; chip problem counts; checks lists that go somewhere`.

---

### Task 5: App: badges, outlines and seams on both canvases; chip counts

**Files:** `crates/mmm-app/src/{plan_view.rs, mosaic_view.rs}`, `session_window/{canvas.rs, mod.rs, shortcuts.rs, view_state.rs}`, and their tests.

**Interfaces — Consumes:** Tasks 3–4. **Produces:**
```rust
pub fn target_rect(session: &OpenSession, layout: &Layout, target: &CheckTarget) -> Option<[u64; 4]>;   // panel bbox, overlap intersection, seam bbox, check.rect
// CanvasLook gains `seams: bool` (#[serde(default)] in ViewState); SEAMS shortcut = Shift+S
```
- **Badges:**
  - One badge per place, for the unaccepted problems of the view's groups, with the count of problems there.
  - A panel badge sits at its outline's top-right; an overlap or seam badge at its rect's centre.
  - Badges show on the plan and the mosaic alike, behind the ROIs toggle's sibling rules: badges always show, and outlines follow **Outlines**.
- **Outlines over the mosaic:** **Outlines** now also draws the panel outlines over the mosaic, from the layout.
- **The Seams overlay:**
  - It draws the shown group's seam lines in a neutral token colour. Flagged seams use the warning colour and a thicker width, so colour isn't the only signal, and the flagged seam also has its badge.
  - On the plan it follows `frame_matches`.
- **Chips:** a group chip shows its unaccepted problem count. A composite chip shows the sum over its members.
- **Clicking a canvas badge** acts as clicking its problem (Task 6).

- [ ] **Step 1: Write the failing tests:**
  - `badges_match_on_plan_and_mosaic`
  - `outlines_show_over_the_mosaic`
  - `the_seams_toggle_draws_flagged_seams_in_warning`
  - `shift_s_toggles_seams`
  - `chips_show_problem_counts`
  - `the_seams_toggle_is_remembered`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): problems are badged where they are; seams and outlines on both canvases`.

---

### Task 6: App: going to a problem, See all checks, un-accept, panel details

**Files:** `session_window/{sections.rs, mod.rs, canvas.rs}`, `plan_view.rs` (`selection_ui`), `panel_list.rs`, and their tests.

**Interfaces — Consumes:** Tasks 3–5. **Produces:**
```rust
pub enum Selected { Panel(PathBuf), Overlap(PathBuf, PathBuf), Seam { group: String, a: PathBuf, b: PathBuf } }   // window-level selection
```
- **Opening a problem** (a click or Enter on a row in Problems, See all, a panel's checks, or a canvas badge):
  - It frames `target_rect` on the shown canvas, through `go_to_roi`'s frame rules (on the plan with a mismatched frame, a toast and nothing more).
  - It selects the target: a panel through the plan's selection; an overlap or seam as a highlighted rect on the canvas, with an "Overlap {a} – {b}" or "Seam {a} – {b}" heading in the Panels section listing its checks.
  - A target that no longer resolves does nothing.
- **See all checks ›** switches the Problems section to the grouped `All` mode, with un-accept. **‹ Problems** returns.
- **Panel details:**
  - Restyled with `MegaButton` and theme tokens.
  - Facts gain Filter, Exposure, Frames and Integration (from `PanelHeader`).
  - "Overlaps" become rows that select the neighbour and frame the overlap.
  - The panel's checks are listed with Accept, Un-accept and Open.

- [ ] **Step 1: Write the failing tests:**
  - `opening_a_problem_frames_and_selects_it` (panel, overlap, seam)
  - `a_badge_click_opens_its_problem`
  - `a_stale_problem_target_is_harmless`
  - `see_all_groups_and_unaccepts`
  - `panel_details_show_filter_and_integration`
  - `overlap_rows_go_to_the_neighbour`
  - `keyboard_opens_a_problem`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement. Update `problems_accept_a_check`, `a_selected_panel_shows_its_details_and_checks` and `rows_show_their_problems` as needed.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): problems lead to their place; see all checks; panel details`.

---

### Task 7: Docs, selftest, snapshots

- [ ] **DESIGN.md:**
  - check targets and keys; the full check set with thresholds and severities;
  - the analysis statistics, seam lines and seam checks;
  - the checks engine's runs and debounce;
  - badges, outlines and seams on both canvases, and chip counts;
  - going to a problem and the selection;
  - See all checks and un-accept;
  - panel details;
  - "Coming in project 3", which now closes project 3.
- [ ] **Selftest and gates:**
  - Run `make_selftest_session` then `--selftest`; it must exit 0.
  - Refresh the snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 …`, look at them, and delete the diffs.
  - fmt, clippy, doc and the workspace tests (twice) are clean.
- [ ] Commit `docs: checks, badges and seams`.
