# Composites and the Export Dialog Implementation Plan (project 3, plan 5 of 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Composites become first-class:
- the **+** chip opens a popover with presets recognised from the groups (RGB, SHO, HOO, HSO) and a Custom form;
- composites have stable ids and are managed in the inspector with the ordered list (order, rename, edit channels, delete with Undo).

The Export dialog is rebuilt as a session-wide modal:
- it lists the groups and composites with their readiness;
- you choose a folder, a format and a file-name pattern, with a live preview and an explicit Overwrite;
- the session remembers your choices.

Exports run in their own lane, so only a merge of a group being exported waits. A composite exports as one three-channel colour file.

**Architecture:**
- **Core** gains `export_composite`. It streams one tile row of each distinct member's level-0 data into the R, G and B planes, the same way a group export streams, with WCS from a member and provenance from every member. It writes FITS directly and TIFF through the staging scratch file.
- **The app:**
  - `Composition.id`, with views, saved stretches and saved views keyed by id, and old name keys migrated.
  - A pure `recognise()` for filter bands.
  - The **+** popover, and the composites section on `ordered_list`.
  - A second `JobRunner` lane for exports, with merges of exported groups gated.
  - A session-level `ExportDialog` in a mega-ui modal frame, with choices remembered in `ui`.

**Tech Stack:** Rust 2024, egui/eframe 0.36 (`egui::Modal`), egui_kittest, `mega-ui`, `mmm-core`, mpp-core (FITS and TIFF writers; there is no XISF writer).

**Spec:** `docs/superpowers/specs/2026-11-08-in-session-experience-design.md`. This plan covers:
- §12 (composites)
- §14 (export)
- §13 (a merge pressed during an export waits only for the groups being exported)
- §6 (the "✓ Exported" step state)

Recon map (paths, signatures, hazards): the controller's `plan5-map.md`.

## Global Constraints

- **Plans 1–4's constraints hold:**
  - UI values come from `mega_ui::theme` tokens; mega-ui never depends on mmm-core.
  - fmt, `clippy --all-targets -D warnings`, doc and tests are clean. Tests synthesise their inputs, and condition waits are bounded by `testutil::PATIENCE`.
  - The UI says **merge**.
  - Problems are reported in toasts only. The Export dialog is the one modal, used for choosing, never for reporting.
  - No file I/O on the UI thread beyond the debounced `mosaic.json` saves and the documented core-mutator saves. Checking which destination files exist runs on a worker.
  - Every public mmm-core item has a doc comment.
- **The CLI's `mmm export` is unchanged:** `export_group`, `export_check` and the stale/missing warnings stay.
- **Exact copy:**
  - **Presets:** **RGB**, **SHO**, **HOO**, **HSO**.
  - **Custom form:** **Custom**, with **Name**, **R**, **G**, **B** (each a group or "None") and **Create**.
  - **Composite delete toast:** "{name} deleted" with **Undo**.
  - **Dialog title:** "Export". The counts line reads "{n} groups · {m} composites" (singular forms for 1).
  - **Row reasons:** "out of date", "not merged", "maps no groups", "missing group {g}".
  - **Format labels:** "FITS", "TIFF".
  - **Pattern:** the default is `{session}_{name}`; the tokens are `{session}` and `{name}`.
  - **Collision message:** "Two exports would have the same file name: add {name} to the pattern".
  - **Overwrite line:** "{n} file(s) already exist and will be replaced".
  - **Primary button:** **Export**, which becomes **Overwrite and export** (Danger) when any target exists. Also **Cancel**.
  - **Export step:** **Export**, "Exporting {i} of {n}", "✓ Exported".
- **Readiness:**
  - A group is ready when its result is Complete and it is not out of date (`steps::out_of_date` false). "Settings changed" never blocks.
  - "not merged" means there is no result. "out of date" means a result exists but `out_of_date` is true.
  - A composite is ready when it maps at least one group, every mapped group exists, and every mapped group is ready.
- **An OSC member in a composite:** both the view and the export use the mean of its channels (as the plan's composite slot already does).
- **Weights** stay 1 and are not shown. The stored `f32` stays.

## Review Focus

1. **A merge waits only for an export of its own group.** A merge of a group being exported, or a member of a composite being exported, waits. Merges of other groups run during the export. Test: Task 5 `only_merges_of_exported_groups_wait`.
2. **Nothing is overwritten by accident.** No existing file is overwritten without the explicit **Overwrite and export**. A failed or cancelled export leaves existing files untouched. Test: Task 6 `existing_files_need_overwrite`; core already writes to `.part` and renames.
3. **Two exports never share a file name.** A pattern that gives two exports the same name is flagged in the preview, and Export is disabled. Test: Task 6 `colliding_names_block_export`.
4. **A composite whose member group was renamed or removed** shows "missing group {g}" in the dialog and is not exportable. It shows the existing unmerged-member note in the view, and nothing panics. Tests: Task 3 `a_composite_with_a_missing_group_says_so` and Task 6.
5. **Old sessions' composites** (no ids, saved view and plan stretch keyed `comp:{name}`) load with ids and keep their saved view and stretch. Test: Task 2 `old_composites_get_ids_and_keep_their_view`.

---

### Task 1: Core: composite export

**Files:** `crates/mmm-core/src/export.rs`, `crates/mmm-core/tests/export.rs`.

**Interfaces — Produces:**
```rust
pub struct CompositeChannels<'a> { pub name: &'a str, pub r: Option<&'a str>, pub g: Option<&'a str>, pub b: Option<&'a str> }
pub fn export_composite(session: &Path, channels: &CompositeChannels, out: &Path, format: ExportFormat,
                        ctl: &JobCtl) -> Result<ExportReport>;
```
- **Readers:** it opens one `TileReader` per distinct mapped group; a group mapped to two channels, as in HOO, is read once.
- **Checks:** it requires the same width, height and tile size across members, each equal to its analysed canvas. Otherwise it errors with "the composite's groups have different canvases".
- **Writing:** per tile row it builds a 3-plane band. A mono member gives its channel 0. An OSC member gives the mean of its channels. An unmapped channel is zeros. It streams the band exactly like `export_group`: FITS through `FitsSink` (`NAXIS3 = 3`), TIFF through the staging scratch file and `write_tiff_native_with_progress`. It writes `.part` and renames on success, and leaves an existing destination untouched on failure or cancel.
- **WCS and pass-through cards** come from the first mapped member's group session.
- **HISTORY** reads "Exported by Mega Merge Mosaic (mmm {v}), composite '{name}'". Then one line per channel ("R: group 'SII'"), then each member's latest "blend" provenance. Generalise the private `history_lines`.
- **`ExportReport`** sums the members' stale and missing counts.
- **Progress** goes through `EXPORT_STAGE`, as for groups.

- [ ] **Step 1: Write the failing tests.**
  - `composite_export_maps_groups_to_planes`: synthesised stores for two groups, an HOO mapping. The FITS has 3 planes: R equals Ha, G and B equal OIII, read back with mpp-core.
  - `composite_export_averages_an_osc_member`.
  - `composite_export_refuses_different_canvases`.
  - `composite_export_cancels_and_leaves_the_destination`.
  - `composite_tiff_export_is_three_channel`.
- [ ] **Step 2:** `cargo test -p mmm-core --test export` should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-core` passes.
- [ ] **Step 5:** Commit `feat(core): export a composite as one three-channel file`.

---

### Task 2: App: stable composite ids

**Files:** `crates/mmm-app/src/compositions.rs`, every `ViewKind::Composition` site (the recon lists about 25: session_window/{canvas, mod, sections, shortcuts}.rs, mosaic_view.rs, plan_view.rs, app.rs selftest, start/hero_views.rs), `examples/make_selftest_session.rs`, and their tests.

**Interfaces — Produces:**
```rust
pub struct Composition { pub id: u64 /* #[serde(default)] */, pub name: String, pub channels: [Option<(String, f32)>; 3] }
pub enum ViewKind { Group(String), Composition(u64) }   // by id
pub fn composition_stretch_key(id: u64) -> String;      // "comp:{id}"
```
- **Loading:** `load_compositions` assigns fresh unique ids to entries without one. Ids are never reused: keep max + 1, as ROIs do.
- **Migration:** on load, a saved view key or plan-stretch key `comp:{name}` that matches a composite's name moves to `comp:{id}`. It is saved back with the next save.
- **Index fix-ups go:** `composite_deleted` and the shortcut cycle work by id.

- [ ] **Step 1: Write the failing tests:**
  - `old_composites_get_ids_and_keep_their_view`: an old-format session whose saved view was a composite by name, with a plan stretch under `comp:{name}`. It opens on that composite with that stretch.
  - `views_follow_a_composite_by_id_across_reorder_and_delete`.
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement. Update the existing composite tests to the id API.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `refactor(app): composites have stable ids`.

---

### Task 3: App: recognised presets, the + popover, the composites list

**Files:** a new `crates/mmm-app/src/bands.rs` (+ tests), `compositions.rs`, `session_window/{canvas.rs, sections.rs, mod.rs}`, and their tests.

**Interfaces — Consumes:** Task 2. **Produces:**
```rust
pub enum Band { R, G, B, Ha, Sii, Oiii }
pub fn recognise(text: &str) -> Option<Band>;
// case-insensitive; ignores spaces, hyphens, underscores and bandwidth suffixes:
// "Ha" "H-alpha" "Halpha" "Ha 7nm" → Ha; "OIII" "O3" "O-III" "OIII 3nm" → Oiii;
// "SII" "S2" "S-II" → Sii; "R" "Red" → R; "G" "Green" → G; "B" "Blue" → B
pub fn group_bands(session: &OpenSession) -> BTreeMap<Band, String /* group */>;
// a band claimed by two groups is left out (ambiguous)
pub struct Preset { pub label: &'static str, pub channels: [Band; 3] }
// RGB=[R,G,B] SHO=[Sii,Ha,Oiii] HOO=[Ha,Oiii,Oiii] HSO=[Ha,Sii,Oiii]
pub fn presets_for(bands: &BTreeMap<Band, String>) -> Vec<Preset>;   // offered only when all its bands resolve
// Undo gains: Undo::CompositeDelete { index: usize, composite: Composition }
```
- **Recognition order:** `group_bands` tries each group's name first. It then tries the FILTER values of the group's panels, where the panel list's header facts know them. An unknown value is skipped.
- **The + chip** opens a popover (`egui::Popup`, as the group chips' menu does).
  - **Presets come first.** One click creates the composite (named by its label, made unique: "SHO 2"), selects it and reveals the Composites section.
  - **Custom** follows: Name, then R, G and B (each a group or "None"), then **Create**. Create is disabled while the name is empty or taken, or nothing is mapped.
  - The **+** is shown only with two or more groups, as today.
- **The Composites section** is built on `ordered_list`.
  - Rows show the name. The detail is "R {g} · G {g} · B {g}", or "missing group {g}".
  - Activate selects the composite's view.
  - Rename, reorder and delete with Undo all work.
  - The selected row's details hold the channel editor: R, G and B combo boxes over the groups and "None". There is no weight slider.
- **A composite whose mapped group no longer exists** keeps it in its channels, so it comes back if the group does, and shows "missing group {g}".

- [ ] **Step 1: Write the failing tests:**
  - `recognise_filter_spellings` (a table of inputs and outputs, including unknowns)
  - `presets_follow_the_recognised_groups` (SHO only with all three; HOO with Ha and OIII; an ambiguous band drops its presets)
  - `the_plus_popover_creates_a_preset`
  - `the_custom_form_creates_a_composite`
  - `composites_rename_reorder_and_delete_with_undo`
  - `a_composite_with_a_missing_group_says_so`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement. Replace `plus_adds_a_composite_and_opens_composites`.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): composite presets, the + popover, and the composites list`.

---

### Task 4: mega-ui: the modal dialog frame

**Files:** a new `crates/mega-ui/src/widgets/dialog.rs` (+ tests), `widgets/mod.rs`, the gallery (a page with room) and its snapshots.

**Interfaces — Produces:**
```rust
pub struct Dialog<'a> { pub title: &'a str, pub width: f32 /* a theme token */ }
pub struct DialogResponse<R> { pub inner: R, pub close: bool /* Esc or backdrop click */ }
impl Dialog<'_> { pub fn show<R>(self, ctx: &egui::Context, id: egui::Id, body: impl FnOnce(&mut Ui) -> R) -> DialogResponse<R>; }
```
- **The frame:**
  - It is built on `egui::Modal`, with a backdrop and input behind blocked.
  - The title uses the theme's title type. Spacing, radius and colours come from tokens.
  - Esc and a backdrop click report `close`. The caller decides whether to close.
  - Focus starts on the first focusable control in the body, and Tab stays inside the dialog.
- **The footer** is a helper for right-aligned buttons: primary, secondary and danger kinds from `MegaButton`.

- [ ] **Step 1: Write the failing tests:**
  - `esc_and_backdrop_report_close`
  - `input_behind_is_blocked`
  - `tab_stays_inside`
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement. Add the dialog to the gallery, and refresh and look at the snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 cargo test -p mega-ui --test snapshots -- --ignored`.
- [ ] **Step 4:** `cargo test -p mega-ui` passes.
- [ ] **Step 5:** Commit `feat(mega-ui): a modal dialog frame`.

---

### Task 5: App: an export lane, composite export jobs, "Exported"

**Files:** `crates/mmm-app/src/jobs.rs` (+ tests), `summary_writer.rs`, `session_window/{steps.rs, top_bar.rs}`, `app.rs` (leaving and the reaper), and their tests.

**Interfaces — Consumes:** Task 1. **Produces:**
```rust
pub enum ExportTarget { Group(String), Composite { id: u64, name: String, channels: [Option<String>; 3] } }
impl JobRunner {
    pub fn start_export(&mut self, session: &OpenSession, target: ExportTarget, out: PathBuf, format: ExportFormat) -> u64;
    pub fn exporting_groups(&self) -> BTreeSet<String>;   // running + queued exports' groups and composite members
}
```
- **A second lane.** Exports get their own `Option<Running>` and a FIFO queue, run one at a time, and are not chained to the main lane's previous worker.
- **The main lane** keeps all its rules, with one change: a whole-canvas or ROI merge of a group in `exporting_groups()` waits in the queue until no export uses that group. A merge of any other group runs at once.
- **Analysis** still cancels running exports and drops queued ones. A panel edit spares exports.
- **Everything that observes the lanes sees both:** `stop_all`, leaving the session (both workers end, or are reaped while the lock is held), `busy`, `busy_except_exports`, `views`, `run_of(JobTarget::Export)`, the test gate and stepper, and `merge_waits_for_export` (true only for a queued merge of an exported group).
- **Labels:** "Export {group}", or "Export {composite name}" ("Export" alone in a one-group session, as today).
- **Stamps:** a finished composite export stamps `exported_at` for each member group.
- **Export step:**
  - While exports run: "Exporting {i} of {n}" (or "Exporting"), cancellable.
  - Else **✓ Exported** (Done) when at least one group is ready and every ready group's `exported_at >= blended_at`.
  - Else **Export**.

- [ ] **Step 1: Write the failing tests.**
  - `only_merges_of_exported_groups_wait`: hold a running export of Ha; a merge of OIII runs and finishes; a merge of Ha queues until the export ends. Then the same with a composite export holding both.
  - `analysis_cancels_exports_on_their_lane`.
  - `leaving_waits_for_both_lanes`.
  - `a_composite_export_stamps_its_members`.
  - `the_export_step_says_exported`.
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement. Update the existing export job tests to `ExportTarget`.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): exports run in their own lane; only merges of exported groups wait`.

---

### Task 6: App: the Export dialog

**Files:** a new `crates/mmm-app/src/export_dialog.rs` (+ tests), which replaces `export.rs`'s dialog and keeps any helpers still used, plus `session_window/{mod.rs, top_bar.rs, shortcuts.rs}` and `mosaic_view.rs` (remove the old dialog ownership).

**Interfaces — Consumes:** Tasks 3, 4 and 5. **Produces:**
```rust
pub const EXPORT_KEY: &str = "export";   // ui: { folder, format: "fits"|"tiff", pattern, selection: [ {group: name} | {composite: id} ] }
pub enum Readiness { Ready, NotMerged, OutOfDate, MapsNoGroups, MissingGroup(String) }
pub fn readiness_of(session: &OpenSession, target: &ExportTarget) -> Readiness;
pub fn file_names(pattern: &str, session: &str, names: &[&str], ext: &str) -> Vec<String>;   // sanitised
```
- **Ownership and opening:**
  - The dialog is owned by `SessionWindow`.
  - It opens from the top bar's **Export…**, the Export step, and Ctrl+E. Add Ctrl+E to shortcuts.rs's table and its tooltip.
  - It is disabled only while analysis runs ("Waiting for the analysis to finish.") or nothing is merged ("Nothing is merged yet.").
- **The body, in this order:**
  1. The counts line.
  2. A rows table: groups, then composites. Each row has a checkbox (enabled only when Ready), the name, and the reason greyed when not ready. Ready rows start ticked; with a remembered selection, the remembered ones that are ready start ticked.
  3. **Folder:** a text field and **Browse…**, which opens a native folder picker on a worker behind a trait for tests.
  4. **Format:** a FITS | TIFF segmented control, with the existing one-line format notes.
  5. **File name pattern:** a text field.
  6. **The preview:** the exact file names, one per ticked row. An existing file is marked "exists" (checked on a worker, re-checked when the folder, pattern, format or selection change). Collisions are flagged with the collision message.
  7. The overwrite line, when it applies.
  8. The footer: **Cancel** and **Export**, or **Overwrite and export**.
- **Export is disabled** with nothing ticked, an empty or missing folder, or a collision.
- **On Export:**
  - Remember folder, format, pattern and selection in `ui` and touch the save.
  - Start one export job per ticked row through Task 5.
  - Close the dialog.
- **Sanitising:** file names drop path separators and characters Windows refuses, avoid device names, and trim. Reuse or expose the grouping module's `dir_base`/reserved-name logic.
- **Shortcuts** stay gated while the dialog is open. Esc closes the dialog only.
- **Removed:** the old view-scoped dialog, `ExportAction::BlendFirst` and the stale "Export anyway" flow. Rewrite their tests against the new dialog. The CLI path is untouched.

- [ ] **Step 1: Write the failing tests:**
  - `rows_show_readiness_and_ready_ones_are_ticked`
  - `settings_changed_does_not_block`
  - `the_preview_lists_exact_names`
  - `colliding_names_block_export`
  - `existing_files_need_overwrite`
  - `choices_are_remembered`
  - `a_composite_exports_one_file`
  - `a_composite_with_a_missing_group_is_not_exportable`
  - `esc_closes_only_the_dialog`
  - `file_names_are_sanitised` (unit)
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): the Export dialog`.

---

### Task 7: Docs, selftest, snapshots

- [ ] **DESIGN.md:** composite ids and keys; the presets and recognition; the + popover; the composites list; composite export in core; the export lane and merge gating; the Export dialog (readiness, pattern, preview, overwrite, remembered choices); the Exported step state; the dialog frame; "Coming in project 3". Update the old "Export." bullet.
- [ ] **Selftest:** extend it to export one group and one composite into a temp folder, if it fits the selftest's phases. Otherwise leave it. Run `make_selftest_session` then `--selftest`; it must exit 0.
- [ ] **Snapshots and gates:** refresh the snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 …` and look at them. fmt, clippy, doc and the workspace tests (twice) are clean.
- [ ] Commit `docs: composites and the Export dialog`.
