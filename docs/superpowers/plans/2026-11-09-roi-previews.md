# ROIs and Transient Previews Implementation Plan (project 3, plan 4 of 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ROIs become an ordered, session-level list (draw, rename, reorder, delete with Undo, go to) drawn over plan and mosaic alike. Each ROI shows a **transient preview**: its area merged with the current draft settings, made in memory by a dedicated preview runner, auto-merged while visible. A preview never touches a store, the provenance or `mosaic.json`.

**Architecture:**
- **Core:** gains `plan_preview`/`run_preview`. They drive `blend()` over the ROI's aligned rect into an in-memory sink, downsampled under a pixel cap.
- **The app:**
  - A `PreviewRunner` with one worker thread, separate from the `JobRunner`. It raises no job events and no toasts.
  - A preview cache keyed by what a preview was made from: rect, effective settings and the group's analysis stamp.
  - A per-frame scheduler that merges the dirty, visible previews of the groups in view. The nearest to the view centre goes first, and it cancels and restarts when the key moves.
- **mega-ui:**
  - Canvas overlays, patches and the draw-rectangle tool, shared by the tiled viewer and the plan canvas.
  - A reusable ordered-list component.
- Previews are converted with the stretch of the canvas beneath them and never feed it.

**Tech Stack:** Rust 2024, egui/eframe 0.36, egui_kittest, `mega-ui`, `mmm-core`.

**Spec:** `docs/superpowers/specs/2026-11-08-in-session-experience-design.md`. This plan covers:
- §8.1 (the list)
- §8.2 (previews)
- §7.3 (the ROIs toggle; previews drawn with the canvas stretch)
- §15 (Undo for deleting ROIs; the Draw ROI shortcut)

§8.3 (Compare A/B) was added as Task 7 after the user tried the mock-up (2026-11-09; https://claude.ai/artifact/CnMUSkLywwkqW6arKfJpUs). Code map: `docs/DESIGN.md` "Standalone app" sections.

## Global Constraints

- **Plans 1–3's constraints still hold:**
  - UI values come from `mega_ui::theme` tokens.
  - mega-ui never depends on mmm-core.
  - fmt, `clippy --all-targets -D warnings`, doc and tests are all clean.
  - Tests synthesise their inputs, and condition-based waits are bounded by `testutil::PATIENCE`.
  - The UI says **merge**.
  - Problems surface as toasts only.
  - No file I/O on the UI thread beyond the debounced `mosaic.json` saves and the documented core-mutator saves.
  - Every public mmm-core item has a doc comment.
- **Previews are transient.**
  - A preview never opens, creates, invalidates or writes a result store.
  - It never writes provenance or `mosaic.json`, and never raises a `JobEvent` or toast.
  - It lives in memory for the app's run only.
  - The CLI is unchanged.
- **Previews never change the display stretch.** They are converted with the stretch of the canvas beneath them (mosaic: the member's `stretch()`; plan: `PlanView::stretch_for_view()`), and their pixels never feed the stats.
- **Exact copy:**
  - New ROI names: "Region 1", "Region 2", … (first unused number). Existing names are kept.
  - Row states: "merging", "up to date", "out of date".
  - Canvas label while merging: "{name} · merging…".
  - Section button and its tooltip: **Draw ROI**, with the shortcut **D** in the tooltip.
  - Delete toast: "{name} deleted" with **Undo**.
- **Preview cap:** `PREVIEW_MAX_PIXELS = 4_194_304` (2048²) output pixels per patch. A larger aligned rect is blended with the smallest integer `downsample` that fits under the cap.
- **Preview cache cap:** `PREVIEW_CACHE_BYTES = 1 << 30`. Least recently drawn patches go first.
- **The plan-3 interim goes.** ROI merges no longer go through the `JobRunner` or write stores. "Merge ROI" is removed. `region_differs` and its wiring stay, because sessions touched by plan-3 builds may carry region entries.

## Review Focus

1. **A preview leaves the session untouched:** provenance, `mosaic.json` bytes and every result store's files are unchanged. Test: Task 5 `previews_leave_the_session_untouched`.
2. **Settings changed faster than previews finish:** no queue builds, and the preview shown ends up made from the final settings. Test: Task 5 `settings_changed_mid_preview_restarts_it`.
3. **An analysis starting (or a group needing one) while a preview runs:** the preview is cancelled, and none made from the old analysis is shown as up to date afterwards. Test: Task 5 `an_analysis_cancels_and_dirties_previews`.
4. **A whole-canvas-sized ROI** doesn't allocate gigabytes. Test: Task 1 `a_large_roi_is_downsampled_under_the_cap`.
5. **An ROI reprojected to a new rect after a frame change**, or deleted mid-preview, never shows a stale patch. Test: Task 5 `a_moved_or_deleted_roi_drops_its_preview`.

---

### Task 1: Core: in-memory previews

**Files:** `crates/mmm-core/src/mosaic/run.rs` (or a new `mosaic/preview.rs` re-exported from `mosaic`), `crates/mmm-core/tests/mosaic.rs`, `crates/mmm-core/src/lib.rs` (overview).

**Interfaces — Produces:**
```rust
pub const PREVIEW_MAX_PIXELS: u64 = 4_194_304;
pub struct PreviewPlan { pub group: String, pub session_dir: PathBuf, pub settings: BlendSettings,
                         pub rect: [u64; 4] /* aligned */, pub downsample: u32 }
pub struct PreviewPatch { pub rect: [u64; 4] /* aligned, level-0 px */, pub width: u32, pub height: u32,
                          pub channels: u32, pub downsample: u32, pub data: Vec<f32> /* planar */ }
pub fn plan_preview(m: &Mosaic, group: &str, rect: [u64; 4], settings: BlendSettings) -> Result<PreviewPlan>;
// refuses never-analysed and needs_analyze groups (like plan_blend_with); aligns with TileGeometry::align_rect;
// downsample = smallest integer d >= 1 with ceil(w/d)*ceil(h/d) <= PREVIEW_MAX_PIXELS
pub fn run_preview(plan: &PreviewPlan, ctl: &JobCtl) -> Result<PreviewPatch>;
// loads the analysis from plan.session_dir and drives blend::blend with
// BlendParams { downsample, extent: Canvas, roi: Some(rect), ..settings.to_params() } into an in-memory RowSink
// (wrapped in CtlSink for cancel + progress); opens no store, writes nothing
```

- [ ] **Step 1: Write the failing tests.**
  - `a_preview_matches_the_whole_merge_there`: at downsample 1, the patch equals the same region of a whole-canvas merge read back from the store.
  - `a_preview_writes_nothing`: the session dir's files and mtimes are unchanged.
  - `a_large_roi_is_downsampled_under_the_cap`: a canvas-sized rect gives `width*height <= PREVIEW_MAX_PIXELS` and `downsample > 1`.
  - `preview_refuses_unanalysed_groups`.
  - `a_cancelled_preview_errs_cancelled`.
- [ ] **Step 2:** Run `cargo test -p mmm-core --test mosaic`. It should FAIL.
- [ ] **Step 3:** Implement. If `blend` with both `downsample > 1` and `roi` isn't supported, make it supported in core. If it can't be, use the nearest honest alternative and say why in the report.
- [ ] **Step 4:** `cargo test -p mmm-core` passes.
- [ ] **Step 5:** Commit `feat(core): in-memory merge previews of a region`.

---

### Task 2: mega-ui: canvas overlays, patches and drawing on both canvases

**Files:** `crates/mega-ui/src/viewer/widget.rs`, `crates/mega-ui/src/plan.rs`, a new shared module `crates/mega-ui/src/canvas_layers.rs` (re-exported), tests, gallery.

**Interfaces — Produces:**
```rust
pub struct Overlay { pub rect: [f64; 4], pub color: Color32, pub label: String, pub dashed: bool }  // moved here from viewer
pub struct Patch { pub rect: [f64; 4] /* canvas px the texture covers */, pub clip: [f64; 4] /* draw only inside */,
                   pub texture: TextureHandle }
// TiledViewer: set_overlays (existing), set_patches(Vec<Patch>); painted after tiles, before overlays
// PlanCanvas: set_overlays, set_patches (painted after items, before outlines/labels), and the draw tool:
//   set_tool(Tool::DrawRect) / tool(); its show response carries drawn_rect like the viewer's (normalised,
//   clamped to the extent, Esc cancels, one rect then back to Navigate)
```
- **One painter for both canvases:** overlay and patch painting is one shared function used by both widgets (dashed stroke, label at top-left, clip by `clip`).

- [ ] **Step 1: Write the failing tests.**
  - `the_plan_draws_overlays_and_patches`: shapes appear at the mapped screen rects, and patches are clipped.
  - `the_plan_draw_tool_returns_a_rect`.
  - `viewer_patches_are_clipped`.
  - The existing viewer overlay and draw tests still pass.
  - The gallery shows a plan with an ROI overlay and a patch.
- [ ] **Step 2:** Run `cargo test -p mega-ui`. It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** It passes. Refresh the gallery snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored` and look at the diff.
- [ ] **Step 5:** Commit `feat(mega-ui): overlays, patches and drawing on the plan canvas too`.

---

### Task 3: mega-ui: the ordered list component

**Files:** `crates/mega-ui/src/widgets/ordered_list.rs` (+ tests), `widgets/mod.rs`, gallery.

**Interfaces — Produces:**
```rust
pub struct ListRow<'a> { pub id: u64, pub label: &'a str, pub detail: Option<&'a str>, pub selected: bool }
pub enum ListEvent { Select(u64), Activate(u64), Rename(u64, String), Move { id: u64, to: usize }, Delete(u64) }
pub struct ListState { /* editing id + text, drag state */ }
pub fn ordered_list(ui: &mut Ui, id_salt: impl Hash, rows: &[ListRow], state: &mut ListState) -> Vec<ListEvent>;
```
- **Click:** a click selects and also activates (the caller treats Activate as "go to").
- **Rename:** a double-click, or the row's edit action, renames inline. Enter commits, Esc cancels. An empty name is refused.
- **Reorder:** drag a row, with an insertion line shown while dragging, using `egui::DragAndDrop` as in `table.rs`. Each row also has **Move up** / **Move down** actions for keyboard users.
- **Delete:** a row action.
- **Row actions** show on hover or focus, as icon buttons with tooltips.
- **Details:** the selected row may show caller-drawn details below it. Expose a closure or return the selected row's rect so the caller can draw (this is for the ROI note).
- Theme tokens only.

- [ ] **Step 1: Write the failing tests:**
  - `click_selects_and_activates`
  - `double_click_renames_inline`
  - `move_up_down_emit_moves`
  - `drag_reorders`
  - `delete_emits`
  - `empty_rename_is_refused`
- [ ] **Step 2:** It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** It passes. Add the component to the gallery and refresh the snapshots as in Task 2.
- [ ] **Step 5:** Commit `feat(mega-ui): an ordered list for ROIs and composites`.

---

### Task 4: App: the ROI list, drawing on either canvas, Undo, the shortcut

**Files:** `crates/mmm-app/src/rois.rs`, `mosaic_view.rs`, `plan_view.rs`, `session_window/{mod.rs, sections.rs, shortcuts.rs, canvas.rs}`, and their tests.

**Interfaces — Consumes:** Tasks 2 and 3. **Produces:**
```rust
// Roi gains `#[serde(default)] pub id: u64` (0 on old entries: load_rois assigns fresh unique ids; saved back)
// RoiPanel selection is by id; unique_roi_name gives "Region n"
// Undo gains: Undo::RoiDelete { index: usize, roi: Roi }
pub(super) const DRAW_ROI: Key = Key::D;
```
- **The ROIs section** is built on `ordered_list`:
  - **Draw ROI** button.
  - Rows show the name, the size detail and the state text. The state reads "up to date" until Task 5 wires previews.
  - **Activate** goes to the ROI (the existing `frame`).
  - Rename, reorder and delete all work. Delete pushes "{name} deleted" with **Undo**, which reinserts the ROI at its index.
  - The note is edited in the selected row's details.
- **Draw ROI** (the button, or **D** when no text field has focus) arms the draw tool on the **shown** canvas, plan or mosaic. Nothing forces Mosaic any more.
- **Drawing:** a drawn rect becomes "Region n" at the end of the list, selected. Plan and mosaic both draw ROI outlines and labels when `look.rois` is on.
- **Removed:** "Merge ROI", `MosaicView::merge_roi`/`merge_roi_block_reason`, and the ROI path through `merge_groups`. `JobRunner::start_blend_with` goes if it is now unused.
- **Tests:** update the tests that encoded the old behaviour (e.g. `drawing_an_roi_shows_the_mosaic`, `merge_roi_starts_one_merge_per_view_group`, `an_roi_merge_uses_the_draft`, `a_region_with_other_settings_makes_the_group_out_of_date`, and the ROI-merge tests that drove `jobs.start_blend(.., Some(rect))` directly may stay if the core path stays). Keep `region_differs`'s core tests.

- [ ] **Step 1: Write the failing tests.**
  - `drawing_on_the_plan_adds_a_region`
  - `new_rois_are_named_region_n`
  - `old_rois_get_ids_and_keep_names`
  - `deleting_an_roi_offers_undo`
  - `reordering_and_renaming_persist`
  - `the_d_key_arms_the_draw_tool`
  - `the_plan_draws_roi_outlines`
  - `merge_roi_is_gone`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): the ROI list, drawn on either canvas, with undo`.

---

### Task 5: App: the preview runner, cache and scheduler

**Files:** a new `crates/mmm-app/src/previews.rs` (+ tests), `session_window/mod.rs`, `mosaic_view.rs`/`plan_view.rs` (as needed), `rois.rs`.

**Interfaces — Consumes:** Task 1. **Produces:**
```rust
pub const PREVIEW_CACHE_BYTES: usize = 1 << 30;
#[derive(Clone, PartialEq)] pub struct PreviewKey { pub rect: [u64; 4], pub settings: BlendSettings, pub analysis: usize }
// analysis = index of the newest "analyze" provenance entry listing the group (a cheap stamp: re-analysis moves it)
pub enum PreviewState { Merging, UpToDate, OutOfDate }
pub struct Previews { /* runner (one worker thread, its own cancel token), cache by (roi id, group), LRU */ }
impl Previews {
    pub fn schedule(&mut self, ctx: &egui::Context, session: &OpenSession, wanted: &[Wanted]); // per frame
    pub fn poll(&mut self);                                   // take a finished patch into the cache
    pub fn patch(&self, roi: u64, group: &str) -> Option<(&PreviewKey, Arc<PreviewPatch>)>;
    pub fn state(&self, roi: u64, groups: &[String], session: &OpenSession) -> PreviewState;
    pub fn forget_roi(&mut self, roi: u64);                   // delete: cancel and drop
}
pub struct Wanted { pub roi: u64, pub group: String, pub key: PreviewKey, pub priority: (u8 /* view group order */, u64 /* distance² to centre */) }
```
- **Each frame, the window builds `Wanted`:**
  - only while `look.rois` is on and the ROIs are `synced()`;
  - for every ROI that intersects the shown canvas's visible rect;
  - for each group in view (a group view: that group; a composite: its member groups in R, G, B order);
  - with `key.settings = draft_for(cfg, group)` and `key.analysis` from provenance;
  - skipping groups that need analysis, never-analysed groups, any group while an analysis job runs or a panel edit waits, and every group while `input_error` is set.
- **The scheduler:**
  - If the running preview's `(roi, group, key)` is no longer wanted, or its key is not the wanted key, it cancels the preview.
  - When the runner is idle, it starts the highest-priority wanted entry whose cached key differs.
  - It keeps no queue beyond that one.
- **Delivery:** results are delivered only if their key is still current. A late result for an old key is dropped.
- **An analysis starting** cancels the running preview. Its new stamp makes cached previews of that group out of date.
- **Row state:**
  - "merging" when that ROI has a preview running;
  - "out of date" when any in-view group's cached key differs from its wanted key, or none is cached;
  - "up to date" otherwise.
- **The canvas label** shows "{name} · merging…" while merging.

- [ ] **Step 1: Write the failing tests** (deterministic runner gate like `JobRunner`'s test gate).
  - `a_visible_roi_previews_by_itself`
  - `hidden_or_off_screen_rois_wait`
  - `nearest_the_centre_goes_first`
  - `settings_changed_mid_preview_restarts_it`
  - `an_analysis_cancels_and_dirties_previews`
  - `a_moved_or_deleted_roi_drops_its_preview`
  - `previews_leave_the_session_untouched`: provenance, `mosaic.json` bytes and store files are unchanged; no `JobEvent` and no toast.
  - `the_cache_stays_under_its_cap`
  - `row_states_follow_previews`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): transient previews merge themselves while in view`.

---

### Task 6: App: drawing previews over plan and mosaic

**Files:** `mosaic_view.rs`, `plan_view.rs`, `previews.rs`, `tile_source.rs` (reuse `planar_image`/`Mix`), and their tests.

**Interfaces — Consumes:** Tasks 2 and 5.

- **The mosaic:** it converts each visible ROI's cached patches to a texture with the shown member's current `stretch()`. A composite mixes its members' patches with the same channel tints as `CompositeTileSource`; a member without a patch adds nothing. The result is handed to `TiledViewer::set_patches`, clipped to the ROI's canvas rect.
- **The plan:** the same, with `PlanView::stretch_for_view()`, via `PlanCanvas::set_patches`.
- **Re-conversion:** a texture is converted again only when its patch, the stretch or the view changes, keyed by those. It never reads stats from the patch.
- **Hidden:** with `look.rois` off, no patch is drawn.
- **Not dimmed:** an out-of-date patch still shows (it shows what the settings it was made from gave), but its row says "out of date".

- [ ] **Step 1: Write the failing tests.**
  - `a_preview_draws_over_the_mosaic_with_its_stretch`: the patch's texture pixels use the canvas stretch, and toggling Strong re-converts it without changing the stretch.
  - `a_preview_draws_over_the_plan`
  - `a_composite_preview_mixes_its_members`
  - `the_rois_toggle_hides_previews`
  - `previews_never_change_the_stretch`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): previews draw over plan and mosaic with the canvas stretch`.

---

### Task 7: Compare A / B

**Files:** `crates/mmm-app/src/previews.rs`, `rois.rs`, `mosaic_view.rs`, `plan_view.rs`, `session_window/{mod.rs, shortcuts.rs, canvas.rs}`, mega-ui (if the label controls need a reusable piece: a canvas-label row with buttons), and their tests.

**Interfaces — Consumes:** Tasks 5 and 6. **Produces:**
```rust
pub struct Compare { pub roi: u64, pub a: BTreeMap<String /* group */, (PreviewKey, Arc<PreviewPatch>)>,
                     pub a_settings: BlendSettings, pub selected: Side, /* momentary sources held */ }
pub enum Side { A, B }
// Previews: start_compare(roi), end_compare(), use_a() -> BlendSettings, shown_side() -> Side
```
- **Entering compare:**
  - **Compare** sits on an ROI's canvas label (only for an ROI whose preview is up to date for the groups in view).
  - It freezes that ROI's current patches for the groups in view as **A**, with the draft settings they were made from. A is pinned in memory: it is not evicted by the LRU, and it never re-merges.
  - **B** is the live preview, exactly as in Tasks 5–6.
  - One ROI in compare at a time. Compare on another ROI ends the current compare first.
- **The A / B toggle:** it is on the label, and the selected side stays shown.
- **Momentary switches** show the other side only while held. From A they peek at B; from B, at A.
  - **Space** is held. It is skipped while a text field has focus or a dialog is open. Add it to shortcuts.rs's table.
  - Pressing and holding the **selected** A / B button peeks. Clicking the other button switches to it.
  - Pressing and holding the left mouse button on the compared ROI's preview peeks, on either canvas. A drag that moves past the drag threshold still pans and cancels the peek.
- **Use A** puts A's settings back into the draft through the normal draft edit, so B re-merges and matches A. It then leaves compare. **Done** leaves compare and keeps the draft.
- **Leaving compare:** compare also ends when the ROI is deleted or hidden, when the view's groups change, when an analysis starts, or when you leave the session.
- **The label row:** name, then the controls (Compare, or A | B, Use A and Done), then the state text. The state text reads "A · feather {n} px · flatten {f}" or "B · …", and "B · merging…" while B re-merges. The buttons never move as the text changes.
- **Accessibility:** the A and B buttons expose their pressed state, and a peek is announced through the label's description. A peek is shown by more than colour: an underline on the shown button.

- [ ] **Step 1: Write the failing tests:**
  - `compare_freezes_a_and_b_follows_the_draft`
  - `the_toggle_stays_and_space_peeks_the_other_side` (from A and from B)
  - `holding_the_selected_button_peeks_and_clicking_the_other_switches`
  - `holding_on_the_preview_peeks_and_a_drag_still_pans`
  - `use_a_restores_a_settings_and_ends_compare`
  - `compare_ends_when_its_roi_goes_or_an_analysis_starts`
  - `a_is_never_evicted_while_comparing`
  - `space_is_typing_in_a_text_field`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` (and `-p mega-ui` if touched) passes.
- [ ] **Step 5:** Commit `feat(app): compare an ROI preview with frozen settings`.

---

### Task 8: Docs, selftest, snapshots

- [ ] **Update DESIGN.md:**
  - the preview model (core entry, cap, runner, cache, keys, scheduler, priority, cancel and restart);
  - ROIs drawn and drawn-on on both canvases;
  - the ordered list;
  - Undo for ROI deletes;
  - the D shortcut;
  - the removal of "Merge ROI";
  - Compare A/B (the toggle, the momentary switches, Use A, Done, and when compare ends);
  - "Coming in project 3".
- [ ] **Selftest and gates:**
  - Run `make_selftest_session` then `--selftest`. Expected: exit 0.
  - Refresh the snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 …`, delete the diffs, and look at them.
  - fmt, clippy, doc and the workspace tests are clean.
- [ ] Commit `docs: ROIs and transient previews`.
