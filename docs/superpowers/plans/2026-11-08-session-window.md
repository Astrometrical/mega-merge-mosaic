# Session Window Implementation Plan (project 3, plan 1 of 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the session's Panels / Plan / Blend tabs with one session window: a top bar (‹, name, step strip, main button), one canvas that shows the plan or the mosaic with a shared camera, a view bar (Plan/Mosaic, group and composite chips, overlay and stretch toggles) and a foldable inspector (Problems, Panels, ROIs, Merge settings, Composites, Notes).

**Architecture:** The canvas parts of `PlanTab` and `BlendTab` are extracted into `PlanView` and `MosaicView` (canvas only, no toolbars or side panels); their side-panel pieces become methods the inspector calls. A new `session_window` module owns both views, the panel list (the existing `PanelsTab`, hosted until plan 2 rebuilds it), the inspector state and the view state, and replaces the tabs in `Shell`. Two generic components go into `mega-ui`: the step strip (plus a selectable chip) and the inspector. The display stretch becomes automatic with two toggles.

**Tech Stack:** Rust 2024, egui/eframe 0.36, egui_kittest, `mega-ui`, `mmm-core`.

**Spec:** `docs/superpowers/specs/2026-11-08-in-session-experience-design.md` (§4–§7, §15; §6 step strip; §7.3 stretch). Existing-code map: `docs/DESIGN.md` "Standalone app" sections.

## Roadmap (project 3)

Each plan ships working software and is merged to `main` before the next:

1. **Session window** (this plan): shell, canvas, view bar, inspector, step strip, stretch toggles, back to start, view state.
2. **Panel list and grouping** (spec §10): the sectioned panel table beside the plan, Group by, path keywords (WBPP rule), multi-select and Move to, roll-up, folders (top level only), two-phase outlines, remove with Undo.
3. **Draft merge settings and per-group staleness** (spec §11, §13, §10.2): settings as a draft until Merge, "Settings changed" vs "Out of date" from provenance, commit without invalidating other groups, per-group re-analysis when the frame doesn't move, merges queued behind exports.
4. **ROIs and transient previews** (spec §8): ROI list in the inspector, drawing on plan or mosaic, transient previews that never touch the tile store, auto-merge of visible dirty ROIs with priority and cancel/restart, the Compare A/B mock-up.
5. **Composites and export** (spec §12, §14): the **+** popover with presets, the composites list, the new Export dialog (N groups / M composites, pattern with live preview, 3-channel composite export, remembered choices).
6. **Checks, problems and seams** (spec §7.3–§7.4, §9): the new checks, the pre-aligned distortion pass, badges on panels/overlaps/chips, click-to-zoom, See all checks, the Seams overlay.

## Global Constraints

- UI code takes colours, sizes, radii and durations from `mega_ui::theme` tokens, never literals; reusable components go in `mega-ui`, which must not depend on `mmm-core` (CLAUDE.md, spec §3).
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` (CI's exact flags), `cargo doc` and `cargo test` stay clean after every task.
- Tests never read `test_data/`; sessions are synthesised (`testutil::two_group_session`, `jobs::tests::analyzed`, `synth_mosaic`). GUI runs only via `--selftest`.
- The UI says **merge**, never "blend": every label, tooltip, progress text, toast and error the user can see (spec §4). Engine identifiers (`blend_group`, `BlendSettings`, provenance action `"blend"`) are not renamed.
- Notifications are toasts (`mega_ui::widgets::Toasts`); nothing modal except the Export dialog.
- Exact UI copy: steps **Panels**, **Checks**, **Merge**, **Export**; main button **Add panels…** / **Merge** / **Export…**; inspector sections in this order **Problems**, **Panels**, **ROIs**, **Merge settings**, **Composites**, **Notes**; view bar **Plan** / **Mosaic**, toggles **Outlines**, **ROIs**, **Linked**, **Strong**; Problems ends with **See all checks ›**; ‹ has tooltip "Back to the start screen".
- Display stretch: automatic only; **Linked** off and **Strong** off by default; both per session (spec §7.3).
- No file I/O on the UI thread beyond what the tabs already do (the debounced `mosaic.json` saves).

## Review Focus

1. Switching Plan ↔ Mosaic, or the mosaic viewer being rebuilt (a merge starting, a store reopening), must never move the camera — Task 7 `camera_survives_the_mosaic_viewer_being_rebuilt`.
2. An empty session (no panels) renders every section, step and toggle without panics; Mosaic is disabled — Task 8 `empty_session_opens_on_the_panel_list`.
3. Keyboard shortcuts must not fire while typing (notes, ROI names, composition names, the typed path) or while the Export dialog is open — Task 11 `shortcuts_ignored_while_typing`.
4. Back to the start screen while previews or analysis run: jobs cancelled, lock released, no later writes into the session — Task 10 `back_cancels_running_work_and_releases_the_lock`.
5. A saved view naming a group or composition that no longer exists falls back to the first group, never panics — Task 11 `restored_view_that_no_longer_exists_falls_back`.

---

### Task 1: Move the shared helpers out of `blend_tab.rs`

**Files:**
- Create: `crates/mmm-app/src/util.rs`
- Modify: `crates/mmm-app/src/main.rs` (add `mod util;`), `blend_tab.rs`, `plan_tab.rs`, `panels_tab.rs`, `checks.rs`, `rois.rs`, `start/hero_views.rs` (imports only)

**Interfaces:**
- Produces: `crate::util::{Task, Poll, Debounce, SAVE_INTERVAL, group_names}` with the exact signatures they have today in `blend_tab.rs` (`Task::spawn(name: &str, ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Option<Task<T>>`, `Task::poll(&mut self) -> Poll<T>`, `enum Poll<T> { Pending, Done(T), Lost }`, `Debounce::{touch, pending, due, wait, saved}`, `group_names(&OpenSession) -> Vec<String>`).

- [ ] **Step 1:** Move the items verbatim (with any tests of them) into `util.rs`; replace every `blend_tab::{Task, Poll, Debounce, group_names, SAVE_INTERVAL}` import with `crate::util::…`. No behaviour change; `blend_tab.rs` keeps no re-exports.
- [ ] **Step 2:** Run `cargo test -p mmm-app` and `cargo clippy -p mmm-app --all-targets -- -D warnings`. Expected: everything passes as before.
- [ ] **Step 3:** Commit `refactor(app): shared task and debounce helpers live in util`.

---

### Task 2: `mega-ui` step strip and choice chip

**Files:**
- Create: `crates/mega-ui/src/widgets/step_strip.rs`, `crates/mega-ui/src/widgets/step_strip_tests.rs`
- Modify: `crates/mega-ui/src/widgets/chip.rs` (add `choice_chip`), `crates/mega-ui/src/widgets/mod.rs`, `crates/mega-ui/src/widgets/display_tests.rs`, `crates/mega-ui/src/gallery.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum StepState { Todo, Next, Done, Warn, Working { fraction: Option<f32> } }
  pub struct StripStep { pub label: String, pub state: StepState, pub cancellable: bool }
  pub enum StripEvent { Open(usize), Cancel(usize) }
  pub fn step_strip(ui: &mut egui::Ui, steps: &[StripStep]) -> Option<StripEvent>;
  pub struct ChoiceChip<'a> { pub label: &'a str, pub color: Option<egui::Color32>, pub selected: bool, pub dashed: bool }
  pub fn choice_chip(ui: &mut egui::Ui, chip: &ChoiceChip<'_>) -> egui::Response;
  ```
  Re-exported from `mega_ui::widgets`.

Look: each step is a pill a little shorter than `CONTROL_HEIGHT`, laid out left to right with `space::XS` gaps. `Todo`: muted text, `BORDER` outline. `Next`: `ACCENT_500` outline, strong text. `Done`: `Tone::Success` colour with the `icons::CHECK` glyph before the label. `Warn`: `Tone::Warning` colour with `icons::TRIANGLE_ALERT`. `Working`: the pill fills from the left to `fraction` of its width with the accent tint (`Tone::Info.tint()`), strong text; `None` fraction is a static half-strength fill (no animation, so reduced motion needs nothing). A cancellable working step shows an `icons::X` icon button, accessible label `"Cancel {label}"`, while the pill is hovered or has focus. Steps are buttons (accessible label = the label text). `choice_chip` looks like `chip` (same radius, padding, colour dot) but is a selectable toggle: selected = raised surface and strong text; `dashed` draws a dashed outline (composites); accessible as a toggle button with its selected state.

- [ ] **Step 1: Write the failing tests** in `step_strip_tests.rs` (harness with `crate::testutil::themed`, state holding the steps and the last event):
  ```rust
  #[test] fn steps_are_buttons_in_order()          // 4 steps → buttons "Panels","Checks","Merge","Export", left edges increasing
  #[test] fn clicking_a_step_opens_it()            // click "Merge" → Some(StripEvent::Open(2))
  #[test] fn working_step_fills_to_its_fraction()  // Working{Some(0.43)}: a filled RectShape inside the pill, width within 1 pt of 0.43 × pill width
  #[test] fn cancel_shows_while_hovered()          // cancellable working step: no "Cancel Merging" until hovered; hover → button; click → Cancel(2)
  #[test] fn done_and_warn_carry_icons()           // painted text contains icons::CHECK for Done and icons::TRIANGLE_ALERT for Warn
  ```
  and in `display_tests.rs`: `choice_chip_reports_selected_state` (accesskit node of a selected chip `is_selected()`/toggled true, unselected false; click returns `clicked()`).
- [ ] **Step 2:** `cargo test -p mega-ui --lib step_strip choice_chip` → FAIL (not defined).
- [ ] **Step 3:** Implement both widgets as above; add a "Step strip" gallery section (the five states, one working at 43 %) and selected/unselected/dashed chips to the Chips section.
- [ ] **Step 4:** Tests pass; `cargo clippy -p mega-ui --all-targets -- -D warnings` clean.
- [ ] **Step 5:** Commit `feat(mega-ui): step strip and choice chip`.

---

### Task 3: `mega-ui` inspector

**Files:**
- Create: `crates/mega-ui/src/inspector.rs`, `crates/mega-ui/src/inspector_tests.rs`
- Modify: `crates/mega-ui/src/lib.rs`, `crates/mega-ui/src/theme/tokens.rs` (add `INSPECTOR_WIDTH: f32 = 320.0`), `crates/mega-ui/src/icons.rs` (add `CHEVRON_LEFT: char = '\u{e06e}'`, Lucide `chevron-left`), `crates/mega-ui/src/gallery.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Debug, Default, PartialEq, Eq)]
  pub struct InspectorState { pub folded: bool, pub open: std::collections::BTreeSet<String>, /* private: pending reveal */ }
  impl InspectorState {
      pub fn is_open(&self, id: &str) -> bool;
      pub fn toggle(&mut self, id: &str);
      pub fn reveal(&mut self, id: &str); // unfold, open `id`, scroll its heading into view on the next draw
  }
  pub struct Badge { pub text: String, pub tone: crate::theme::Tone }
  pub struct Section<'a> { pub id: &'a str, pub title: &'a str, pub badge: Option<Badge> }
  pub fn inspector(ui: &mut egui::Ui, state: &mut InspectorState, sections: &[Section<'_>], body: impl FnMut(&mut egui::Ui, &str));
  pub fn inspector_width(state: &InspectorState) -> f32; // INSPECTOR_WIDTH, or the folded strip's width
  ```

Behaviour: drawn into the `ui` it is given (the caller places it as a right-hand column of `inspector_width`). Unfolded: an icon button "Hide inspector" (`CHEVRON_RIGHT`) at the top, then a vertical scroll area with every section heading in the given order. A heading is a full-width button: the title in the overline style, the badge (tone-coloured text) at the right, and a chevron (down when open, right when closed); clicking toggles the section; an open section draws `body(ui, id)` below its heading. Headings always show; only bodies fold. Folded: only an icon button "Show inspector" (`CHEVRON_LEFT`).

- [ ] **Step 1: Write the failing tests** (`inspector_tests.rs`, three sections `a`/`b`/`c` titled "Alpha"/"Beta"/"Gamma", bodies label "body a" etc.):
  ```rust
  #[test] fn headings_show_in_order_and_only_open_bodies_draw() // "ALPHA","BETA","GAMMA" tops increasing; only "body b" with open={b}
  #[test] fn clicking_a_heading_toggles_it()                    // click "ALPHA" → "body a" shows; click again → gone
  #[test] fn folding_leaves_only_the_show_button()              // folded → no "ALPHA"; "Show inspector" click → unfolded
  #[test] fn reveal_unfolds_and_opens()                         // folded, reveal("c") → after a frame: !folded, "body c" visible
  #[test] fn badge_sits_in_its_heading()                        // badge "2" label rect inside the "ALPHA" heading rect
  ```
- [ ] **Step 2:** `cargo test -p mega-ui --lib inspector` → FAIL.
- [ ] **Step 3:** Implement; add an "Inspector" gallery section (three sections, one open, a warning badge).
- [ ] **Step 4:** Tests pass; clippy clean.
- [ ] **Step 5:** Commit `feat(mega-ui): foldable inspector with sections`.

---

### Task 4: Stretch strength and the session's display stretch

**Files:**
- Modify: `crates/mmm-core/src/stretch.rs`
- Create: `crates/mmm-app/src/display_stretch.rs`; Modify `crates/mmm-app/src/main.rs`

**Interfaces:**
- Produces (core):
  ```rust
  pub enum Strength { Normal, Strong }
  pub const STRONG_SHADOWS_CLIP: f32 = -1.25;
  pub const STRONG_TARGET_BACKGROUND: f32 = 0.45;
  pub fn auto_stretch_with(stats: &[ChannelStats], linked: bool, strength: Strength) -> Vec<StretchParams>;
  ```
  `auto_stretch(stats, linked)` stays and equals `auto_stretch_with(stats, linked, Strength::Normal)`.
- Produces (app):
  ```rust
  pub const DISPLAY_STRETCH_KEY: &str = "display_stretch";
  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
  pub struct DisplayStretch { pub linked: bool, pub strong: bool }
  impl DisplayStretch {
      pub fn load(ui: &serde_json::Value) -> Self;      // absent or malformed → default (unlinked, normal)
      pub fn save(self, ui: &mut serde_json::Value);    // {"linked":bool,"strong":bool}
      pub fn strength(self) -> mmm_core::stretch::Strength;
      pub fn params(self, stats: &[ChannelStats]) -> Vec<StretchParams>;
  }
  ```

- [ ] **Step 1: Failing tests.** Core: `strong_lifts_the_background` (stats median 0.1, MAD 0.01: `apply(normal, 0.1) ≈ 0.25`, `apply(strong, 0.1) ≈ 0.45`, ±1e-4), `strong_clips_shadows_nearer_the_median` (strong `black` > normal `black`), `auto_stretch_is_normal`. App: `display_stretch_defaults_to_unlinked_normal`, `display_stretch_round_trips`, `params_follow_the_toggles` (linked → all channels equal; strong → median maps to 0.45).
- [ ] **Step 2:** `cargo test -p mmm-core --lib stretch` and `cargo test -p mmm-app --bin mega-merge-mosaic display_stretch` → FAIL.
- [ ] **Step 3:** Implement (`params_for` gains the clip and target as parameters).
- [ ] **Step 4:** Pass; doc comment on every new public `mmm-core` item.
- [ ] **Step 5:** Commit `feat: a strong automatic stretch and the session display stretch`.

---

### Task 5: `PlanView`: the plan canvas without its tab

**Files:**
- Create: `crates/mmm-app/src/plan_view.rs`, `crates/mmm-app/src/plan_view_tests.rs`, `crates/mmm-app/src/camera.rs`
- Modify: `crates/mmm-app/src/plan_tab.rs` (becomes a thin wrapper until Task 7), `plan_tab_tests.rs` (canvas tests move to `plan_view_tests.rs`), `crates/mega-ui/src/plan.rs` (add `pub fn view_mut(&mut self) -> &mut View`), `main.rs`

**Interfaces:**
- Consumes: `DisplayStretch` (Task 4), `crate::util::*` (Task 1).
- Produces:
  ```rust
  // camera.rs
  #[derive(Clone, Copy, Debug, PartialEq)]
  pub struct Camera { pub centre: (f64, f64), pub zoom: f64 }
  impl Camera { pub fn of(view: &mega_ui::viewer::View, now: f64) -> Self; pub fn apply(self, view: &mut mega_ui::viewer::View, now: f64); }
  // plan_view.rs
  pub struct CanvasLook { pub outlines: bool, pub numbers: bool, pub stretch: DisplayStretch }
  pub struct PlanView { /* textures, converter, layout, footprints, selection, auto stretch per view */ }
  impl PlanView {
      pub fn new() -> Self;
      pub fn show(&mut self, ui: &mut egui::Ui, session: &OpenSession, feed: &PlanFeed<'_>, look: &CanvasLook);
      pub fn select_view(&mut self, view: ViewKind);
      pub fn view(&self) -> Option<&ViewKind>;
      pub fn camera(&self) -> Option<Camera>;
      pub fn set_camera(&mut self, camera: Camera);
      pub fn selected(&self) -> Option<&Path>;
      pub fn select(&mut self, path: Option<PathBuf>);
      pub fn selection_ui(&mut self, ui: &mut egui::Ui, session: &OpenSession); // panel name, path, Show in folder, Copy path, facts, overlaps
      pub fn stretch_for_view(&self) -> Option<&[StretchParams]>;           // the auto stretch in use
      pub fn items(&self) -> &[ItemSnapshot]; pub fn is_settled(&self) -> bool;
      pub fn release_if_hidden(&mut self, now: Instant); pub fn frame_canvas(&self) -> Option<(u64, u64)>;
      pub fn view_key(&self) -> Option<&str>;
  }
  ```
  `PlanFeed` moves to `plan_view.rs` unchanged.

The stretch is always automatic: `look.stretch.params(stats)` of the view's preview statistics (the existing `view_auto_stretch`, now with strength and linking), recomputed when the view, the look or the previews generation changes. Manual stretch, the per-view `plan_stretch` saves and the Plan stretch panel go; the `plan_stretch` key in old sessions is ignored, not deleted. Composition views keep their additive layers. `PlanTab` keeps its toolbar (with **Linked**/**Strong** checkboxes replacing the stretch panel) and side panel by calling `PlanView`, so the Plan tab still works until Task 7.

- [ ] **Step 1: Failing tests** in `plan_view_tests.rs`: `camera_round_trips` (set_camera → camera() equal within 1e-9 after a frame), `strong_stretch_brightens_the_plan` (mean of the uploaded preview texels higher with `strong`), `old_manual_plan_stretch_is_ignored` (a session with a saved `plan_stretch` renders with the automatic stretch). Move the existing canvas tests (`plan_draws_partial_previews`, `composition_view_uses_additive_layers`, `preview_image_stretches_and_keeps_no_data_clear`, `plan_places_solved_panels_before_previews`, `linked_auto_stretch_ignores_empty_channels`, `converter_drops_superseded_epochs`, `copy_path_puts_path_on_clipboard`, `show_in_folder_uses_the_platform_opener`) to drive `PlanView`.
- [ ] **Step 2:** `cargo test -p mmm-app --bin mega-merge-mosaic plan_view` → FAIL.
- [ ] **Step 3:** Extract; delete `stretch_panel.rs` uses from Plan.
- [ ] **Step 4:** `cargo test -p mmm-app` all pass (Plan tab tests that remain test the wrapper).
- [ ] **Step 5:** Commit `refactor(app): the plan canvas is a view of its own`.

---

### Task 6: `MosaicView`: the merged canvas without its tab

**Files:**
- Create: `crates/mmm-app/src/mosaic_view.rs`, `crates/mmm-app/src/mosaic_view_tests.rs`
- Modify: `crates/mmm-app/src/blend_tab.rs` (thin wrapper until Task 7), `blend_tab_tests.rs` (canvas/merge tests move), `tile_source.rs` (auto stretch with strength), `main.rs`

**Interfaces:**
- Consumes: `Camera`, `CanvasLook` (Task 5), `DisplayStretch` (Task 4).
- Produces:
  ```rust
  pub struct MosaicView { /* results, viewer, rois: RoiPanel, export: ExportDialog, settings: SettingsPanel, health */ }
  impl MosaicView {
      pub fn new() -> Self;
      pub fn show(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, jobs: &mut JobRunner, look: &CanvasLook, plan_stretch: Option<&[StretchParams]>);
      pub fn select_view(&mut self, view: ViewKind); pub fn view(&self) -> Option<&ViewKind>; pub fn group(&self) -> Option<&str>;
      pub fn camera(&self) -> Option<Camera>; pub fn set_camera(&mut self, camera: Camera);
      pub fn merge_block_reason(&self, session: &OpenSession, jobs: &JobRunner) -> Option<String>; // None = Merge allowed
      pub fn merge_all(&mut self, session: &mut OpenSession, jobs: &mut JobRunner);              // was "Blend whole mosaic"
      pub fn merge_roi(&mut self, session: &mut OpenSession, jobs: &mut JobRunner);              // was "Blend ROI"
      pub fn open_export(&mut self, session: &OpenSession);
      pub fn export_ui(&mut self, ctx: &egui::Context, session: &OpenSession, jobs: &mut JobRunner);
      pub fn rois_ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession);
      pub fn settings_ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, jobs: &mut JobRunner);
      pub fn health_ui(&mut self, ui: &mut egui::Ui);
      // unchanged hooks the shell drives every frame:
      pub fn on_event(&mut self, event: &JobEvent); pub fn track_analysis(&mut self, id: u64);
      pub fn settings_editing(&self, session: &OpenSession) -> bool;
      pub fn poll_settings(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner);
      pub fn flush_save(&mut self, session: &OpenSession); pub fn view_key(&self, ui: &serde_json::Value) -> Option<String>;
      // test accessors kept: source, absent, health, rois, shown_text, is_settled
  }
  ```
  `MosaicTileSource::auto(linked: bool)` becomes `auto(stretch: DisplayStretch)`.

Stretch: automatic from the shown members' coarse statistics via `DisplayStretch::params`; manual stretch, `blend_stretch` saves and the Blend stretch panel go (old keys ignored). **No pumping**: while a merge of a shown group runs, the stretch in use is held (the previous result's, or `plan_stretch` when the group has no result yet); it is recomputed when the merge ends, when the view changes, or when the look changes. The manual **Analyze** button goes (analysis is automatic; plan 1 Task 8 offers resume in the Checks step). `BlendTab` keeps its toolbar and side panel by calling `MosaicView` until Task 7.

- [ ] **Step 1: Failing tests** in `mosaic_view_tests.rs`: `merge_all_starts_one_merge_per_group` (two-group analysed session → two merge jobs), `merge_block_reason_explains_missing_analysis` (unanalysed → `Some(text)` containing "analys"), `stretch_holds_while_merging` (stretch identical across progress frames of a running merge; recomputed after `Finished`), `first_merge_uses_the_plan_stretch` (no prior result → the `plan_stretch` passed in), `camera_round_trips`. Move the existing tests that exercise the canvas, merging, ROIs, health and export to `MosaicView`.
- [ ] **Step 2:** `cargo test -p mmm-app --bin mega-merge-mosaic mosaic_view` → FAIL.
- [ ] **Step 3:** Extract.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `refactor(app): the mosaic canvas is a view of its own`.

---

### Task 7: The session window: canvas, view bar and inspector

**Files:**
- Create: `crates/mmm-app/src/session_window/mod.rs`, `session_window/canvas.rs` (view bar, mode, shared camera), `session_window/sections.rs` (inspector bodies), `session_window/tests.rs`
- Modify: `crates/mmm-app/src/app.rs` (Plan and Blend tabs replaced by the session window; the Panels tab stays until Task 8), `compositions.rs`, `session.rs` (`OpenSession.merged`), `main.rs`

**Interfaces:**
- Consumes: `PlanView`, `MosaicView`, `CanvasLook`, `Camera` (Tasks 5–6); `inspector`, `InspectorState`, `Section`, `Badge`, `choice_chip`, `segmented` (Tasks 2–3); `DisplayStretch` (Task 4).
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum CanvasMode { Plan, Mosaic }
  pub struct SessionWindow { /* plan, mosaic, inspector: InspectorState, mode, view: Option<ViewKind>, look, … */ }
  impl SessionWindow {
      pub fn new() -> Self;
      pub fn ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, jobs: &mut JobRunner, pipeline: &mut Pipeline, checks: &ChecksEngine) -> SessionOutput;
      pub fn mode(&self) -> CanvasMode; pub fn set_mode(&mut self, mode: CanvasMode);
      pub fn view(&self) -> Option<&ViewKind>; pub fn select_view(&mut self, view: ViewKind);
      pub fn merge(&mut self, session: &mut OpenSession, jobs: &mut JobRunner); // merge_all + switch to Mosaic
      pub fn inspector(&self) -> &InspectorState; pub fn inspector_mut(&mut self) -> &mut InspectorState;
      pub fn plan(&self) -> &PlanView; pub fn mosaic(&self) -> &MosaicView; pub fn mosaic_mut(&mut self) -> &mut MosaicView;
  }
  pub struct SessionOutput { pub accept: Vec<Check> } // the shell calls checks.accept for each
  // session.rs
  pub struct OpenSession { /* … */ pub merged: bool } // any processing group's result store exists; set in open_blocking, set true when a merge job finishes
  ```
  Section ids: `"problems"`, `"panels"`, `"rois"`, `"merge-settings"`, `"composites"`, `"notes"`.

Layout: the canvas area takes the width left of the inspector (`inspector_width`). The view bar runs across the top of the canvas: `segmented` **Plan** / **Mosaic** (Mosaic disabled with tooltip "Nothing merged yet" until `session.merged`); one `choice_chip` per group (group colour, label `"{group} · {n}"`) then each composition (`dashed`) then a **+** chip (tooltip "New composite") — chips only when the session has two or more groups; at the right, `choice_chip` toggles **Outlines**, **ROIs**, **Linked**, **Strong** (Linked disabled with tooltip "One channel: nothing to link" when the view is mono). The mode starts as Mosaic when `session.merged`, else Plan. Switching mode or view copies the camera (`Camera::of` the shown view → `set_camera` on the other) so nothing moves. Mosaic is drawn by `MosaicView::show` with `plan_stretch = plan.stretch_for_view()`; plan by `PlanView::show`. **+** creates a composition (`unique_composition_name`), selects it and `inspector.reveal("composites")`.

Inspector bodies (`sections.rs`): **Problems** — `checks_list` of problems with Accept, then a Quiet button **See all checks ›** that toggles showing notes and passed checks; badge = unaccepted problem count (`Tone::Warning`), none when zero. **Panels** — a summary line "{n} panels · {g} groups · {W × H} px" (canvas via `format_canvas`), then `plan.selection_ui` when a panel is selected. **ROIs** — `mosaic.rois_ui` plus a Secondary button **Merge ROI** (`merge_roi`, disabled with the block reason) ; ROI drawing needs Mosaic mode until plan 4 (the draw button switches the canvas to Mosaic). **Merge settings** — `mosaic.settings_ui`. **Composites** — each composition's `composition_editor` and a Danger **Delete composite**. **Notes** — `notes_view` on `session::notes` / `set_notes`, saved debounced (`Debounce`) like the start screen. `health_ui` sits at the end of Merge settings.

- [ ] **Step 1: Failing tests** in `session_window/tests.rs` (harness over a `SessionWindow` with session/jobs/pipeline/checks state; analysed and merged sessions from `jobs::tests`):
  ```rust
  #[test] fn plan_shows_until_something_is_merged()            // unmerged → mode Plan, "Mosaic" disabled
  #[test] fn mosaic_shows_by_default_once_merged()              // merged → mode Mosaic
  #[test] fn merge_switches_to_the_mosaic()                     // Plan mode, w.merge(..) → Mosaic, merge jobs started
  #[test] fn plan_and_mosaic_share_the_camera()                 // zoom/pan the plan, switch → mosaic camera equal (1e-6)
  #[test] fn camera_survives_the_mosaic_viewer_being_rebuilt()  // start a merge of the shown group → camera unchanged
  #[test] fn chips_pick_the_view_in_both_modes()                // click "OIII · 4" → plan and mosaic views Group("OIII")
  #[test] fn no_chips_for_a_single_group()                      // one-group session → no "+" chip
  #[test] fn plus_adds_a_composite_and_opens_composites()       // click "+" → a new composition, view selects it, inspector open has "composites"
  #[test] fn linked_is_disabled_for_a_mono_view()               // mono synthetic session → "Linked" disabled
  #[test] fn inspector_sections_are_in_order()                  // PROBLEMS, PANELS, ROIS, MERGE SETTINGS, COMPOSITES, NOTES tops increasing
  #[test] fn problems_accept_a_check()                          // Accept → SessionOutput.accept has it
  #[test] fn notes_section_saves_notes()                        // type in Notes → mosaic.json notes saved after the debounce
  ```
- [ ] **Step 2:** `cargo test -p mmm-app --bin mega-merge-mosaic session_window` → FAIL.
- [ ] **Step 3:** Implement; in `Shell` the tab enum becomes `{ Panels, Session }` (labels "Panels", "Mosaic") until Task 8; `poll_jobs` drives `SessionWindow`'s plan/mosaic hooks instead of the tabs'. Delete `plan_tab.rs`/`blend_tab.rs` wrappers once nothing uses them, moving any remaining tests.
- [ ] **Step 4:** `cargo test -p mmm-app` passes; clippy clean.
- [ ] **Step 5:** Commit `feat(app): one session window with a shared canvas and an inspector`.

---

### Task 8: Top bar, step strip, main button and the panel list

**Files:**
- Create: `crates/mmm-app/src/session_window/top_bar.rs`, `session_window/steps.rs`
- Modify: `session_window/mod.rs`, `session_window/tests.rs`, `app.rs` (tabs removed entirely; the session bar's Auto-analyze checkbox and "Open another…"/"New mosaic…" buttons go), `panels_tab.rs` (draws into a given `Ui`; `poll` unchanged)

**Interfaces:**
- Consumes: `step_strip`, `StripStep`, `StepState`, `StripEvent` (Task 2); `SessionWindow` (Task 7); `JobRunner::views()`, `Pipeline::{step, previews, previews_complete, request_previews}`, `ChecksEngine::counts`, `MosaicView::merge_block_reason`.
- Produces: `pub fn steps(session: &OpenSession, jobs: &JobRunner, pipeline: &Pipeline, checks: &ChecksEngine) -> [StripStep; 4]` (pure; unit-testable) and `SessionWindow::panel_list_open: bool` (pub(crate)).

Top bar (one row, `space` tokens): an icon button `icons::ARROW_LEFT` (tooltip "Back to the start screen"; wired in Task 10), the session name (`Type::Heading`), the step strip, and at the right the main button. Step states (spec §6):

| Step | State |
|---|---|
| Panels | no panels → `Todo` "Add panels"; previews job running → `Working` "Reading previews {done} / {n}" (fraction = done/n), cancellable; previews incomplete and paused (cancelled or failed) → `Next` "Read previews"; else `Done` "{n} panels" |
| Checks | analysis running → `Working` "Analysing…" (job fraction), cancellable; checks running → `Working` "Checking…"; analysis needed and not running → `Next` "Analyse"; problems > 0 → `Warn` "{p} problem(s)"; else `Done` "Checks passed" |
| Merge | merge running → `Working` "Merging {group} · {i} of {n} · {pct} %", cancellable; any group without a result or with stale tiles (today's notion) → `Next` "Merge"; else `Done` "Merged" |
| Export | export running → `Working` "Exporting {i} of {n}", cancellable; else `Todo` "Export" |

Clicks: Panels toggles the panel list (and in the "Read previews" state calls `pipeline.request_previews()`); Checks → `inspector.reveal("problems")`, or starts the analysis when the state is `Next` "Analyse"; Merge → `SessionWindow::merge` when `merge_block_reason` is `None`; Export → `mosaic.open_export`. Cancel cancels the matching jobs. Main button: no panels → Primary **Add panels…** (opens the panel list and its file picker); a merge is needed → Primary **Merge**, followed by a Quiet small button with the settings summary "{Mode} · feather {n} px" that reveals `"merge-settings"`; otherwise Primary **Export…**. The panel list: when open, the existing `PanelsTab` UI replaces the canvas area (plan 2 puts it beside the plan); a session with no panels opens with it open; dropped files are added whatever is showing (Panels' `poll` already reads them).

- [ ] **Step 1: Failing tests:**
  ```rust
  #[test] fn steps_follow_the_session()                 // pure `steps`: empty / previews running / analysed with 2 problems / merged
  #[test] fn checks_step_reveals_the_problems()         // click "2 problems" → inspector open has "problems", !folded
  #[test] fn main_button_follows_the_work()             // empty "Add panels…" → analysed "Merge" → merged "Export…"
  #[test] fn settings_summary_reveals_merge_settings()  // click "Pyramid · feather …" → "merge-settings" open
  #[test] fn cancelling_a_working_step_cancels_its_job()// merging; hover Merge step; click "Cancel Merging …" → no merge job left
  #[test] fn empty_session_opens_on_the_panel_list()    // created session → panel list open, every step drawn, no panic, "Mosaic" disabled
  #[test] fn panels_step_toggles_the_panel_list()
  ```
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. **Step 4:** `cargo test -p mmm-app` passes. **Step 5:** Commit `feat(app): top bar with the step strip and main button`.

---

### Task 9: Toasts, no status bar, "merge" wording, window title

**Files:**
- Modify: `crates/mmm-app/src/app.rs`, `jobs.rs` (label `format!("Merge {group}")`), `status.rs` (keep `format_canvas`/`format_bytes`; remove the bar, `SysMonitor`, `job_items`, `check_items`, `right_items` and their tests), `export.rs`, `tile_source.rs`, `rois.rs`, `blend_settings.rs`, `panels_tab.rs`, `summary_writer.rs` tests, `session_window/*`

**Interfaces:**
- Produces: `Shell` shows job outcomes as `mega_ui::widgets::Toasts` ("Merge Ha finished", "{label} cancelled", failures with `Tone::Problem`); no `Notice`, no status bar.

Wording: every user-visible "blend" becomes "merge" — find them with `grep -rn -i blend crates/mmm-app/src` (string literals in labels, tooltips, headings, notices, errors and job labels; the test below is the authority) — e.g. "Merge settings", "MERGE · re-merge only", "Existing results go stale; merge again to see the change…", "Changed since the last merge", "Export the merged result", "Not merged yet.", "The merged result is not up to date.", "Merge whole mosaic first", job labels "Merge {group}". The window title is `"{session name} — Mega Merge Mosaic"` while a session is open (via `ViewportCommand::Title`) and "Mega Merge Mosaic" otherwise.

- [ ] **Step 1: Failing tests:** `job_outcomes_show_as_toasts` (a finished merge → a toast "Merge Ha finished"), `no_status_bar`, `no_user_visible_blend_wording` (session window in Plan and Mosaic, every inspector section open, Export dialog open: no painted text and no accesskit label contains "blend", case-insensitive), `title_names_the_session` (the viewport command output carries the title).
- [ ] **Step 2:** FAIL. **Step 3:** implement; update tests that asserted the old labels. **Step 4:** pass. **Step 5:** Commit `feat(app): toasts, merge wording and the window title`.

---

### Task 10: Back to the start screen

**Files:**
- Modify: `crates/mmm-app/src/app.rs`, `session_window/top_bar.rs`, `start/mod.rs` (refresh the session's card), `session_window/tests.rs` or `app.rs` tests

**Interfaces:**
- Produces: `Shell::close_session(&mut self, ctx: &egui::Context) -> Result<(), String>` (`Err` while an export runs).

Order on ‹: refuse while an export runs (the button is disabled with tooltip "Exporting: cancel it or wait to go back"); cancel every job and wait for them to stop (bounded, as `flush_notes` waits); flush pending saves (debounces, notes, view state from Task 11); write the summary synchronously; drop the `OpenSession` (releases the lock); reset the decoded cache dir; the start screen re-reads that session's facts; window title back to "Mega Merge Mosaic".

- [ ] **Step 1: Failing tests:** `back_returns_to_the_start_screen` (click ‹ → start screen's "Resume session" visible), `back_cancels_running_work_and_releases_the_lock` (analysis running, ‹ → no jobs, `lock_session(dir)` succeeds, `mosaic.json` unchanged afterwards), `back_is_disabled_while_exporting`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat(app): back to the start screen`.

---

### Task 11: View state and keyboard shortcuts

**Files:**
- Create: `crates/mmm-app/src/session_window/view_state.rs`
- Modify: `session_window/mod.rs`, `canvas.rs`, `tests.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const VIEW_STATE_KEY: &str = "session_view";
  pub struct ViewState { pub mode: CanvasMode, pub view: Option<String> /* group or "comp:<name>" */, pub camera: Option<Camera>,
                         pub inspector_folded: bool, pub inspector_open: Vec<String>, pub panel_list_open: bool,
                         pub outlines: bool, pub rois: bool }
  impl ViewState { pub fn load(ui: &serde_json::Value) -> Option<Self>; pub fn save(&self, ui: &mut serde_json::Value); }
  ```
  `DisplayStretch` keeps its own key (Task 4).

Saved debounced (1 s, `Debounce`) whenever any of it changes, flushed by `Shell::flush_pending_saves` and on back. On open: restored after the first frame lays out the canvas (the camera needs the screen size); a view key naming a missing group or composition falls back to the first group; a saved Mosaic mode with nothing merged falls back to Plan. Default inspector: unfolded, Problems open. Shortcuts (ignored when `ctx.wants_keyboard_input()` or the Export dialog is open), shown in their controls' tooltips: `I` fold the inspector, `M` Plan/Mosaic, `[` / `]` previous/next group or composite, `O` Outlines, `R` ROIs, `L` Linked, `S` Strong, `Ctrl+Enter` Merge. `summary.last_view` follows the canvas's view key.

- [ ] **Step 1: Failing tests:** `view_state_survives_reopening` (change mode, view, camera, fold, toggles; flush; reopen the session → all restored), `restored_view_that_no_longer_exists_falls_back`, `shortcuts_toggle_the_canvas` (`O` → outlines off; `M` → mode flips; `]` → next view), `shortcuts_ignored_while_typing` (focus Notes, type "o" → outlines unchanged, notes text "o").
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat(app): the session window remembers its view, with shortcuts`.

---

### Task 12: Selftest, docs, gallery snapshots and cleanup

**Files:**
- Modify: `crates/mmm-app/src/app.rs` (selftest phases), `docs/DESIGN.md`, `CLAUDE.md` (layout line if a module name changed), `crates/mega-ui/tests/snapshots/*.png`, remove dead code (`stretch_panel.rs`, any leftover tab code)

- [ ] **Step 1:** Re-point `--selftest`: open the session → Mosaic of the first group (settled) → the composition → Plan (settled) → Done; log lines name "merge"/"Mosaic"/"Plan", not tabs. Run `cargo run -p mmm-app --example make_selftest_session -- <scratch dir>` then `cargo run -p mmm-app -- --selftest <dir>/m.mmm-session`. Expected: exit 0, "self-test passed".
- [ ] **Step 2:** Update `docs/DESIGN.md`: the Standalone app sections describe the session window (top bar, step strip, canvas, view bar, inspector, view state, shortcuts, back to start, toasts), the automatic stretch with Linked/Strong, and drop the tab descriptions and the manual stretch; note what plans 2–6 will change.
- [ ] **Step 3:** Refresh gallery snapshots: `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored`; delete `*.diff.png`/`*.old.png`; look at the new images.
- [ ] **Step 4:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo doc --no-deps`, `cargo test` all clean.
- [ ] **Step 5:** Commit `docs: the session window` (and the cleanup).
