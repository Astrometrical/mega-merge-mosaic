# Panel List and Grouping Implementation Plan (project 3, plan 2 of 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the old card-based panel list with a sectioned table beside the plan canvas: a Group by control (Don't group / FILTER header / header keyword / path keyword / by hand), sections per group with roll-up, multi-select and Move to, Remove with Undo, hover linking with the plan, Add folder and dropped folders, and early outlines for aligned panels.

**Architecture:** Core gains two grouping rules (`PathKeyword`, `Manual`), a one-open panel header reader (with integration time) and an exposed aligned-panel bounding box. `mega-ui` gains a generic sectioned table. The app's `PanelsTab` is split: its edit model stays (moved to `panel_edits.rs`), its UI is replaced by `panel_list.rs`, drawn as a left column beside the canvas. Undo toasts flow from the window to the shell through `SessionOutput`.

**Tech Stack:** Rust 2024, egui/eframe 0.36 (no `egui_extras`), egui_kittest, `mega-ui`, `mmm-core`.

**Spec:** `docs/superpowers/specs/2026-11-08-in-session-experience-design.md` §10 (panels and grouping), §7.4 (pointing at things), §15 (Undo toasts). Code map of what exists: `docs/DESIGN.md` "Standalone app" sections.

## Global Constraints

- Everything in plan 1's Global Constraints holds: UI values from `mega_ui::theme` tokens; reusable components in `mega-ui`, which never depends on `mmm-core`; `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo doc --no-deps`, `cargo test` clean after every task; tests never read `test_data/`; GUI only via `--selftest`; the UI says **merge**, never "blend"; notifications are toasts; no file I/O on the UI thread beyond the existing debounced `mosaic.json` saves (opening headers, reading folders, `is_dir` checks and bbox scans run on workers).
- Tests wait on background work with condition-based loops bounded by `crate::testutil::PATIENCE` (robust on loaded CI runners).
- Exact UI copy: **Add files…**, **Add folder…**, **Group by**, choices **Don't group**, **FILTER header**, **Header keyword…**, **Path keyword…**, **By hand**; section heading "{group} · {n}" (with " · {integration}" when known); "No group" section first; roll-up row "… {n} more"; row menu **Move to ▾** with the groups, **New group…**, **No group**, **Let the rule decide**, then **Remove**; toast "Removed {n} panel(s)" with action **Undo**; auto-grouping toast "Grouped by FILTER: {k} filters" with **Undo**; drop overlay "Drop panel files or folders to add them".
- Columns: **PANEL**, **FILTER**, **INTEGRATION**, **SCALE**, **SIZE** (overline headings); numbers in the mono face; "—" when unknown.
- Persisted shapes stay backward compatible: existing `mosaic.json` files (incl. `GroupRule::FilenameToken`) load and resolve exactly as before.
- Per-group staleness on add/remove is plan 3; this plan keeps today's invalidation.

## Review Focus

1. A session with 150 panels: the list stays responsive (rolled-up sections bound the drawn rows; nothing per-row reads files on the UI thread) — Task 6 `a_large_session_draws_a_bounded_list`.
2. A panel file that can't be opened shows a row reading "Unreadable" with its problem and never panics; grouping still works for the rest — Task 6 `an_unreadable_panel_shows_a_row`.
3. Adding a folder that holds already-added files, or dropping the same file twice, adds no duplicates — Task 8 `folders_add_only_new_top_level_panels`.
4. Removing the panel selected in the plan (or every panel) clears the plan/inspector selection and the hover, never panics; Undo restores it — Task 7 `removing_the_selected_panel_clears_the_selection`.
5. A rule that places nothing (keyword in no path) puts every panel in "No group" with the reason; switching back restores the groups — Task 9 `a_rule_that_places_nothing_explains_why`.

---

### Task 1: Core grouping rules: path keyword and by hand

**Files:** Modify `crates/mmm-core/src/mosaic/grouping.rs`, `crates/mmm-core/src/mosaic/mod.rs` (the `GroupRule` enum).

**Interfaces — Produces:**
```rust
pub enum GroupRule { Header { key: String }, FilenameToken { keyword: String }, PathKeyword { keyword: String }, Manual }
// serde: {"kind":"path_keyword","keyword":"FILTER"}, {"kind":"manual"}; Header stays the default
pub fn path_keyword_value(path: &Path, keyword: &str) -> Option<String>;
```
`path_keyword_value` (WBPP's rule, hardened): split the whole path — every folder and the file stem, without the extension — into tokens on `_`, `-`, space, `.`, `/` and `\`, dropping empty tokens; the keyword matches a whole token, case-insensitively; the value is the next token, in its original case. No match, or the keyword as the last token → `None`. The keyword found more than once with values that differ (case-insensitively) → `None`; the same value repeated → that value (first occurrence's case). `Manual` places no panel by rule: only `manual_group` places panels; the rest are ungrouped. `FilenameToken` is unchanged (legacy sessions).

- [ ] **Step 1: Failing tests** in `grouping.rs`:
  ```rust
  #[test] fn path_keyword_reads_folders_and_names() // "C:/data/Orion/FILTER_Ha/master.xisf"→"Ha"; "/d/Orion_FILTER-OIII_300s.fits"→"OIII"; "/d/FILTER Ha/x.xisf"→"Ha"; keyword "filter" matches "FILTER"
  #[test] fn path_keyword_needs_a_whole_token()     // "/d/filtered_Ha.xisf", "filter" → None
  #[test] fn path_keyword_refuses_conflicts()       // "/d/FILTER_Ha/FILTER_OIII.xisf" → None; "/d/FILTER_Ha/m_FILTER_ha.xisf" → Some("Ha")
  #[test] fn path_keyword_without_a_value()         // "/d/Orion_FILTER.xisf" → None (extension is not a token)
  #[test] fn manual_rule_places_only_manual_groups()// resolve_groups with Manual: manual panels grouped, others ungrouped
  #[test] fn rules_round_trip_through_json()        // PathKeyword/Manual serde shapes above; an old FilenameToken config loads unchanged
  ```
- [ ] **Step 2:** `cargo test -p mmm-core --lib grouping` → FAIL. **Step 3:** implement (`resolve_groups` handles both new rules; doc comments on every public item). **Step 4:** pass; `cargo test -p mmm-core` passes. **Step 5:** Commit `feat(core): path keyword and by-hand grouping rules`.

---

### Task 2: Core panel header facts

**Files:** Create `crates/mmm-core/src/panel_header.rs`; modify `crates/mmm-core/src/lib.rs`, `crates/mmm-core/src/checks.rs` (`panel_facts` built from it).

**Interfaces — Produces:**
```rust
#[derive(Clone, Debug, PartialEq)]
pub struct PanelHeader { pub width: u64, pub height: u64, pub channels: u64, pub filter: Option<String>,
                         pub exposure_s: Option<f64>, pub frames: Option<u64>,
                         pub solution: SolutionKind, pub scale_arcsec: Option<f64> }
impl PanelHeader { pub fn integration_s(&self) -> Option<f64>; } // exposure_s × frames when both are known, else None
pub fn read_panel_header(path: &Path) -> Result<PanelHeader>;    // ONE InputPanel::open, header only
```
`exposure_s` from `EXPTIME` (else `EXPOSURE`), `frames` from `NCOMBINE`; numeric cards parsed with `formats::card_number`; non-finite or ≤ 0 values → `None`. `filter` as `InputPanel::filter_name()`. `panel_facts(path)` becomes a projection of `read_panel_header` (one open instead of two).

- [ ] **Step 1: Failing tests** (synthesise XISF with FITS keywords as `synth_mosaic`/`mmm_core::synth::write_xisf_with_header_xml` do): `header_reads_size_filter_and_integration` (EXPTIME 300, NCOMBINE 24 → 7200 s), `integration_needs_both_cards` (EXPTIME only → `None`), `bad_cards_are_unknown` (EXPTIME "abc"/-1 → `None`), `panel_facts_match_the_header`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat(core): one-open panel header facts with integration time`.

---

### Task 3: Early outlines for aligned panels

**Files:** Modify `crates/mmm-core/src/preview.rs`, `crates/mmm-app/src/plan_view.rs` (+ `plan_view_tests.rs`).

**Interfaces — Produces:**
```rust
pub fn aligned_bbox(path: &Path, ctl: &JobCtl) -> Result<Option<[f64; 4]>>; // mmm_core::preview; canvas px of the covered area
```
Wraps the existing private `covered_bbox` (strided channel-0 scan, exact refinement) with `PanelReader::open_file`; `None` for a panel with no covered pixels. In the app, the plan's footprint worker (`start_footprints`/`load_footprints`) handles `ReferenceFrame::Aligned` by calling `aligned_bbox` per panel (on the worker; cancelled when the previews generation or the frame changes), so aligned panels get their dashed outline before their preview, as solved panels already do.

- [ ] **Step 1: Failing tests:** core `aligned_bbox_finds_the_covered_area` (synthetic full-canvas panel, data only inside a known rect → that rect, ±1 px); app `aligned_panels_get_outlines_before_previews` (aligned synthetic session, a feed with no previews → every panel placed with a dashed outline; `unplaced() == 0`).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat: aligned panels get their outline before their preview`.

---

### Task 4: `mega-ui` sectioned table

**Files:** Create `crates/mega-ui/src/table.rs`, `crates/mega-ui/src/table_tests.rs`; modify `lib.rs`, `gallery.rs`.

**Interfaces — Produces:**
```rust
pub const ROLL_UP_ROWS: usize = 3;
pub struct Column<'a> { pub title: &'a str, pub numeric: bool }
pub struct Row<'a> { pub id: u64, pub cells: Vec<String>, pub problem: Option<&'a str>, pub tag: Option<&'a str>,
                     pub selected: bool, pub highlighted: bool }
pub struct Heading<'a> { pub id: &'a str, pub title: String, pub color: Option<egui::Color32>,
                         pub warning: Option<String>, pub rolls_up: bool, pub drop_target: bool }
pub struct Section<'a> { pub heading: Option<Heading<'a>>, pub rows: Vec<Row<'a>> } // heading None: a flat list
#[derive(Default)] pub struct TableState { /* folded and expanded (un-rolled) section ids */ }
pub enum TableEvent { Click { id: u64, modifiers: egui::Modifiers }, Hover(u64), Drop { ids: Vec<u64>, section: String } }
pub fn sectioned_table(ui: &mut egui::Ui, state: &mut TableState, columns: &[Column<'_>], sections: &[Section<'_>],
                       row_menu: impl FnMut(&mut egui::Ui, u64)) -> Vec<TableEvent>;
```
Look (spec §10.2 "a table, not a spreadsheet"): no grid lines; roomy rows (`CONTROL_HEIGHT`); column headings in the overline style; `numeric` columns right-aligned in the mono face; a `problem` row shows `icons::TRIANGLE_ALERT` (warning tone) after its first cell, with the problem as its tooltip; a `tag` shows as a small muted pill after the first cell; selected rows use the selection fill, highlighted rows a lighter accent fill. A heading row: a fold chevron, the colour dot, the title, the warning in the warning tone; clicking it folds/unfolds. A section with `rolls_up` shows its first `ROLL_UP_ROWS` rows plus every row with a `problem`, then a clickable "… {n} more" row that expands it. The row menu: a Quiet **Move to ▾**-style menu button (labelled by the caller through the closure's menu contents; the table draws a `MegaButton` "⋯" with tooltip "Panel actions") shown while the row is hovered or focused, opening `row_menu(ui, id)`. Rows are drag sources (payload: the selected ids if the dragged row is selected, else that row) and headings with `drop_target` are drop zones (egui 0.36 `dnd_*`), producing `Drop`. Accessible: rows are selectable list items labelled by their first cell; headings are buttons labelled by their title.

- [ ] **Step 1: Failing tests** (`table_tests.rs`): `headings_and_rows_draw_in_order`, `numeric_columns_align_right`, `rolled_up_sections_show_their_first_rows_and_problems` (10 rows, one problem at index 7 → rows 0–2, row 7, and "… 6 more"), `the_more_row_expands_the_section`, `a_heading_folds_its_rows`, `clicks_report_their_modifiers` (plain, Shift, Ctrl), `hovering_a_row_reports_it`, `the_row_menu_opens_from_its_button`, `dragging_rows_onto_a_heading_drops_them` (if kittest can't drive egui's dnd, test the payload/drop resolution as a function and say so).
- [ ] **Step 2:** FAIL. **Step 3:** implement; gallery section "Table" (two sections, one rolled up, a problem row, a tag). **Step 4:** pass; clippy clean. **Step 5:** Commit `feat(mega-ui): sectioned table`.

---

### Task 5: Undo toasts from the session window

**Files:** Modify `crates/mmm-app/src/session_window/mod.rs` (`SessionOutput`), `crates/mmm-app/src/app.rs`.

**Interfaces — Produces:**
```rust
pub enum Undo { Panels(PanelsEdit) /* restore this edit */, }
pub struct WindowToast { pub toast: mega_ui::widgets::Toast, pub undo: Option<Undo> }
pub struct SessionOutput { /* existing */ pub toasts: Vec<WindowToast> }
impl SessionWindow { pub fn undo(&mut self, undo: Undo, session: &mut OpenSession, jobs: &mut JobRunner); }
```
The shell pushes each `WindowToast`, keeps `ToastId → Undo` for toasts with an action, and on `ToastEvent::Action(id)` calls `window.undo(..)`; `Dismissed` drops the entry; leaving the session drops them all. `Undo::Panels` re-submits the stored `PanelsEdit` through the existing edit path (queued behind jobs like any edit).

- [ ] **Step 1: Failing test:** `an_undo_toast_restores_the_edit` (window emits a toast with `Undo::Panels(previous)`; the harness clicks its **Undo** → the panels are back in `mosaic.json`).
- [ ] **Step 2:** FAIL. **Step 3:** implement (`Undo` is an enum so plans 4–5 can add variants). **Step 4:** pass. **Step 5:** Commit `feat(app): undo toasts from the session window`.

---

### Task 6: The panel list beside the plan

**Files:** Create `crates/mmm-app/src/panel_edits.rs` (the edit model moved out of `panels_tab.rs`: `PanelsEdit`, `Applied`, submit/queue/retry, `has_pending`, `apply_waiting`, `add_paths`), `crates/mmm-app/src/panel_list.rs`, `crates/mmm-app/src/panel_list_tests.rs`; modify `session_window/mod.rs` (left column), `main.rs`; delete `panels_tab.rs` and its tests once moved (keep every still-meaningful test, re-pointed).

**Interfaces:**
- Consumes: `sectioned_table` (Task 4), `read_panel_header`/`PanelHeader` (Task 2), `resolve_groups` + rules (Task 1), the checks report (per-panel unaccepted problems, as `plan_view` computes them).
- Produces: `pub struct PanelList` with `new(picker)`, `poll(ctx, session, jobs)` (every frame: drops, picker, header worker, edits), `ui(ui, session, jobs, report: Option<&ChecksReport>) -> PanelListOutput`, `has_pending()`, `apply_waiting(..)`, `pick_files(ctx)`; `pub struct PanelListOutput { pub hovered: Option<PathBuf>, pub clicked: Option<PathBuf>, pub toasts: Vec<WindowToast> }`.

Layout: while the list is open it is an `egui::Panel::left("panel-list")` (resizable, default width about half the window, min ~420 pt) beside the canvas — view bar and plan stay visible. Toolbar: **Add files…**, **Add folder…** (wired in Task 8), and the **Group by** control (Task 9 adds its rules; this task keeps today's on/off + header key behaviour behind the new control's look). Table: columns per the Global Constraints; one row per panel (`PANEL` = file name; `FILTER` = header filter; `INTEGRATION` like "3h 05m" from `integration_s`; `SCALE` "1.21″"; `SIZE` "9255 × 18310"); a file that can't be opened shows "Unreadable" in `FILTER`, "—" elsewhere, and a problem tooltip with the error. Sections when grouping is on: "No group" first (never rolls up, heading warning = the reason, e.g. "FILTER missing"), then the groups in name order, each heading "{group} · {n}" + " · {total integration}" when every member's is known, colour from `group_colors`, warning "{d} fewer than {largest group}" when its count is below the largest group's; sections roll up (`rolls_up: true`). Grouping off: one flat section. The header worker reads `read_panel_header` once per panel (cached by path + fingerprint), never on the UI thread.

- [ ] **Step 1: Failing tests** (`panel_list_tests.rs`, harness over `SessionWindow` with the list open): `the_list_sits_beside_the_plan` (list open → "Plan" segment and the plan canvas still drawn, list rect left of the canvas), `rows_show_header_facts` (synthetic panels with EXPTIME/NCOMBINE → "INTEGRATION" cell "2h 00m" etc.), `sections_head_each_group` (two groups → headings "Ha · 4", "OIII · 4"; a group with one fewer member shows "1 fewer than Ha"), `no_group_comes_first_with_its_reason`, `groups_roll_up`, `an_unreadable_panel_shows_a_row`, `a_large_session_draws_a_bounded_list` (150 panels in 3 groups → at most ~3×(ROLL_UP_ROWS+1)+headings rows drawn; frame completes), plus the re-pointed edit-model tests from `panels_tab_tests.rs` (`edit_while_job_running_is_queued_then_applied`, `a_waiting_edit_applies_when_the_session_is_left`, `queued_edit_cancels_jobs_started_later`, `panels_edit_spares_exports`, `group_colors_are_stable_and_physical`, `add_files_dedupes` minus the typed-path part if Task 8 moves it).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** full `cargo test -p mmm-app` passes. **Step 5:** Commit `feat(app): the panel list is a sectioned table beside the plan`.

---

### Task 7: Selecting, moving, removing, and the plan link

**Files:** Modify `panel_list.rs`, `panel_list_tests.rs`, `session_window/mod.rs`, `session_window/shortcuts.rs`, `plan_view.rs` (+ tests), `crates/mega-ui/src/plan.rs` (`PlacedItem.highlighted: bool`, drawn as an accent outline under the selection style).

**Interfaces — Produces:** `PlanView::hovered() -> Option<&Path>` (the panel under the pointer, from `PlanResponse.hovered`), `PlanView::set_highlight(&mut self, path: Option<PathBuf>)`, `PlanView::select` made public (not test-only).

Behaviour: click / Shift-click / Ctrl(Cmd)-click select rows (the existing selection model); a plain click on a row also selects that panel in the plan (so the inspector's Panels section shows its details, spec §7.4) and a plan click selects its row. Hovering a row highlights its outline on the plan; hovering a panel on the plan highlights its row (`Row.highlighted`). The row menu: **Move to ▾** with each group, **New group…** (an inline name field in the menu; creates the group from the selection — groups exist through their members, no empty groups), **No group**, **Let the rule decide**, then **Remove**. Move to and Remove act on the selection when the row is selected, else on that row. Dragging rows onto a group heading moves them (manual group). A moved panel's row shows the tag "by hand" (its `FILTER` cell keeps the header value). **Delete** (or Backspace on macOS) removes the selection when the list is open and no text field is focused (the plan-1 shortcut guard). Remove pushes a `WindowToast` "Removed {n} panel(s)" with **Undo** = `Undo::Panels(the edit before)`. Esc clears the selection.

- [ ] **Step 1: Failing tests:** `shift_and_ctrl_extend_the_selection`, `move_to_puts_rows_in_a_group_by_hand` (tag "by hand" shown, FILTER cell unchanged, `manual_group` saved), `new_group_takes_the_selection`, `dropping_rows_on_a_heading_moves_them` (or the drop-resolution test from Task 4 applied here), `remove_offers_undo` (Remove → rows gone, toast "Removed 2 panels"; Undo → back), `delete_removes_the_selection`, `a_row_click_selects_the_panel_in_the_plan` (and the inspector Panels section shows it), `hover_links_rows_and_outlines` (hover row → `PlacedItem.highlighted` for that panel; hover plan item → that row highlighted), `removing_the_selected_panel_clears_the_selection` (incl. removing every panel; Undo restores).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat(app): select, move and remove panels, linked with the plan`.

---

### Task 8: Add folder and dropped folders

**Files:** Modify `panel_list.rs`, `panel_edits.rs`, the `FilesPicker` trait (+ `RfdFilesPicker`, every stub in tests), `panel_list_tests.rs`.

**Interfaces — Produces:** `FilesPicker::pick_folders(&self) -> Option<Vec<PathBuf>>` (rfd `pick_folders`); `fn panel_files_in(paths: Vec<PathBuf>) -> Vec<PathBuf>` (worker-side: each path that is a directory contributes its top-level files with a panel extension, sorted; each file with a panel extension is kept; others dropped; all normalised).

**Add folder…** opens the folder picker (on a worker) and adds `panel_files_in(folders)`. Dropped paths go through the same worker, so dropping folders works (top level only, spec §10.1). Duplicates (by normalised path, against the session and within the batch) are ignored. The typed-path fallback moves behind a "⋯" menu next to **Add folder…** ("Type a path…", as on the start screen), unchanged in behaviour. The drop overlay reads "Drop panel files or folders to add them".

- [ ] **Step 1: Failing tests:** `folders_add_only_new_top_level_panels` (a folder with 3 panels + a subfolder with 2 + a `.txt` + one already-added panel → 2 new panels; dropping the same folder again adds none), `dropped_files_and_folders_mix`, `typed_path_still_works`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat(app): add folders of panels by picker or drop`.

---

### Task 9: Group by and automatic FILTER grouping

**Files:** Modify `panel_list.rs`, `panel_list_tests.rs`.

**Interfaces — Consumes:** `GroupRule::{Header, PathKeyword, Manual, FilenameToken}` (Task 1), `WindowToast`/`Undo` (Task 5).

The **Group by** control is a dropdown: **Don't group** (`enabled: false`), **FILTER header** (`Header { key: "FILTER" }`), **Header keyword…** (`Header { key }` with a key field, committed on Enter or focus loss), **Path keyword…** (`PathKeyword { keyword }` with a keyword field, default "FILTER"), **By hand** (`Manual`). A legacy `FilenameToken { keyword }` shows as **Path keyword…** with its keyword and keeps resolving as before until the user edits it, which stores `PathKeyword`. Any choice made by the user sets `ui.grouping_chosen = true` in the session. **Automatic grouping:** when grouping is off, `ui.grouping_chosen` is not set, and the resolved headers carry two or more distinct FILTER values, the list switches to **FILTER header** by itself (one edit) and pushes "Grouped by FILTER: {k} filters" with **Undo** (`Undo::Panels` back to the previous grouping; undoing also sets `grouping_chosen`). The "No group" heading's reason follows the rule: "FILTER missing" / "{key} missing" / "no {keyword} in the path" / "not placed by hand".

- [ ] **Step 1: Failing tests:** `group_by_switches_rules` (each choice → the matching `Grouping` saved; path keyword groups a synthetic set laid out in `FILTER_Ha/`/`FILTER_OIII/` folders), `a_legacy_filename_token_session_keeps_its_groups`, `filters_group_themselves_once` (two FILTER values, grouping off → grouped by FILTER, toast with Undo; Undo → ungrouped and not re-grouped on the next resolve), `a_rule_that_places_nothing_explains_why` (path keyword absent → all in "No group" with "no FILTER in the path"; switch to FILTER header → groups back).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** pass. **Step 5:** Commit `feat(app): group by header, path keyword or by hand, and automatic FILTER grouping`.

---

### Task 10: Docs, selftest, snapshots

**Files:** `docs/DESIGN.md`, `crates/mega-ui/tests/snapshots/*.png`, selftest if it touches the panel list.

- [ ] **Step 1:** Update `docs/DESIGN.md`: the panel list (table, sections, roll-up, Group by incl. the path keyword rule and auto FILTER, selection/Move to/Remove with Undo, hover link, Add folder/drops), core rules and `read_panel_header`, aligned outlines, the sectioned table in the Mega UI component list; remove the old card/columns description; update "Coming in project 3".
- [ ] **Step 2:** `cargo run -p mmm-app --example make_selftest_session -- <scratch dir>` then `cargo run -p mmm-app -- --selftest <dir>/m.mmm-session` → exit 0, "self-test passed".
- [ ] **Step 3:** `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored` (and the start screen's); delete `*.diff.png`/`*.old.png`; look at the images.
- [ ] **Step 4:** fmt, clippy `-D warnings`, doc, workspace tests clean. **Step 5:** Commit `docs: the panel list`.
