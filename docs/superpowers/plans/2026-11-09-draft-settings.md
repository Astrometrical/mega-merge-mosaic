# Draft Merge Settings and Per-Group Staleness Implementation Plan (project 3, plan 3 of 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merge settings become a draft that only Merge commits. "Settings changed" (still exportable) is told apart from "Out of date" (must re-merge). A commit never invalidates a finished mosaic. Adding or removing panels re-analyses and re-merges only the groups it touches, unless the reference frame moves. Merges queue visibly behind exports, and a chip menu merges one group.

**Architecture:**
- **Provenance records what made the tiles.** Each "blend" provenance entry records the settings that made the tiles and whether the merge was whole-canvas, so "the settings this group was merged with" is a provenance lookup.
- **Merge jobs carry their own settings.**
  - A merge job captures its settings when it is queued.
  - Its apply no longer refuses settings that changed meanwhile. The provenance says what they were.
  - A whole-canvas merge with different settings from the group's last merge invalidates the store as it starts, so a stopped merge never leaves old and new tiles both marked valid.
- **Commit without invalidating.** Core gains a non-invalidating `commit_blend_settings`.
- **Per-group panel edits.** Core gains per-group input invalidation (`set_inputs_affecting`) and a scoped analysis (`AnalyzeScope::Needed`, which fails with `FrameMoved` as a safety net). The app scopes each panel edit on a worker: it resolves the new groups and checks the frame.
- **The draft.** The app keeps the draft in `mosaic.json`'s `ui` and applies analysis settings at once, with an Undo toast.
- **One set of predicates.** The step strip, main button, chips, canvas tag and the start screen all derive their Merge state from one shared set of predicates.

**Tech Stack:** Rust 2024, egui/eframe 0.36, egui_kittest, `mega-ui`, `mmm-core`.

**Spec:** `docs/superpowers/specs/2026-11-08-in-session-experience-design.md`. This plan covers:
- §11 (merge settings)
- §13 (merging)
- §10.2 (per-group staleness on add and remove)
- §7.1–§7.2 (the out-of-date tag, chip state, Merge this group)
- §6 (Merge step states)
- §14 ("Settings changed" doesn't block export)
- §15 (Undo for analysis-setting changes)

Code map: `docs/DESIGN.md` "Standalone app" sections.

## Global Constraints

- **Plans 1–2's constraints still hold:**
  - UI values come from theme tokens.
  - mega-ui never depends on mmm-core.
  - fmt, `clippy --all-targets -D warnings`, doc and tests are all clean.
  - Tests synthesise their inputs.
  - Condition-based waits are bounded by `testutil::PATIENCE`.
  - The UI says **merge**.
  - Problems surface as toasts only.
  - No file I/O on the UI thread beyond the debounced `mosaic.json` saves.
- **The CLI's behaviour stays as it is:**
  - `mmm set` (`set_settings`/`set_group_override`) still invalidates.
  - `mmm run` is unchanged.
  - The CLI never calls `plan_blend`/`apply_blend`/`blend_group`.
  - New core functions are additive.
- **Existing sessions load unchanged:**
  - With no `ui.merge_draft`, the draft is the saved `config.blend`.
  - "blend" provenance entries without `whole` count as whole-canvas.
- **Exact copy:**
  - Merge step labels: **Merge**, **Merged**, **Out of date**, **Settings changed**, **Merge queued after export**.
  - Canvas tags: "Out of date — panels changed", "Out of date — analysis settings changed", "Out of date — merge didn't finish", "Not merged yet", and the quieter "Settings changed since this merge".
  - Chip menu item: **Merge this group**.
  - Toast for a merge cancelled by a panel edit: "Merge stopped: panels changed".
  - Undo toast: "Analysis settings changed" with **Undo**.
  - Cost line beside each analysis field once a mosaic exists: "Re-runs the analysis; the mosaic goes out of date".
  - Merge block reason for an invalid feather: "Feather width must be a number above 0".
- **Definitions:**
  - "Out of date": a group's result isn't Complete, or the group needs analysis.
  - "Settings changed": the group isn't out of date, but its last whole-canvas merge used different effective merge settings from the draft (the draft with the group's override applied).
  - "Settings changed" never blocks export.

## Review Focus

1. **A commit while another group's merge runs.** That merge finishes with the settings it started with, its tiles stay valid, and its group then shows "Settings changed". Test: Task 6 `a_running_merge_keeps_its_settings_across_a_commit`.
2. **A whole merge stopped part-way after the settings changed.** It must not leave old- and new-settings tiles both valid. The group reads Out of date. Test: Task 2 `a_stopped_fresh_merge_leaves_the_group_out_of_date`.
3. **An old session (no draft key, provenance without `whole`).** It shows no false "Settings changed" and no false "Out of date". Tests: Task 1 `old_blend_entries_count_as_whole` and Task 5 `an_old_session_shows_merged`.
4. **An add that moves the reference frame** (a solved panel growing the canvas). It re-analyses every group, so none is left analysed against the old frame. Test: Task 4 `a_frame_moving_add_reanalyses_everything`.
5. **Undoing an analysis-setting change while analysis runs.** The undo is queued like any edit and loses nothing. Test: Task 5 `undo_of_an_analysis_change_waits_for_the_running_analysis`.

---

### Task 1: Core: commit without invalidating, and per-group provenance lookups

**Files:** `crates/mmm-core/src/mosaic/run.rs`, `mosaic/mod.rs`, `crates/mmm-core/src/lib.rs` (overview), `crates/mmm-core/tests/mosaic.rs`.

**Interfaces — Produces:**
```rust
pub fn commit_blend_settings(m: &mut Mosaic, blend: BlendSettings) -> Result<()>;
// validates the base and every override's effective settings; sets config.blend; ONE save;
// takes no store locks, invalidates nothing, leaves needs_analyze alone
pub fn last_merge_settings(cfg: &MosaicConfig, group: &str) -> Option<BlendSettings>;
// newest "blend" entry for `group` whose detail.whole != Some(false); parses detail.settings
pub fn last_analyze_settings(cfg: &MosaicConfig, group: &str) -> Option<AnalyzeSettings>;
// newest "analyze" entry whose detail.groups has `group`
pub fn settings_changed(cfg: &MosaicConfig, group: &str, draft: &BlendSettings) -> bool;
// last_merge_settings(..).is_some_and(|s| s != draft.with(&<group's override>.blend))
```
`Mosaic::apply_blend` adds `"whole": plan.rect.is_none()` to the "blend" provenance detail.

- [ ] **Step 1: Write the failing tests.**
  - `commit_does_not_invalidate`: every level-0 tile of the merged store stays valid, `needs_analyze` is unchanged, and `config.blend` is updated on disk.
  - `commit_refuses_invalid_settings`: a feather of 0 gives Err and nothing is saved.
  - `last_merge_settings_is_per_group_and_whole_canvas`: an ROI blend after a whole blend doesn't count, and group B's entry doesn't answer for A.
  - `last_analyze_settings_is_per_group`.
  - `settings_changed_respects_overrides`: an override that pins the changed field means no change.
  - `old_blend_entries_count_as_whole`: an entry with no `whole` key.
  - `set_settings_still_invalidates`: the CLI path is unchanged.
- [ ] **Step 2:** Run `cargo test -p mmm-core --test mosaic`. It should FAIL: the functions are not defined yet.
- [ ] **Step 3:** Implement, with doc comments, re-exports and a line in lib.rs's overview.
- [ ] **Step 4:** `cargo test -p mmm-core` passes.
- [ ] **Step 5:** Commit `feat(core): commit merge settings without invalidating; per-group provenance lookups`.

---

### Task 2: Merge jobs carry their settings; fresh merges

**Files:** `crates/mmm-core/src/mosaic/run.rs`, `crates/mmm-core/tests/mosaic.rs`, `crates/mmm-app/src/jobs.rs` (+ its tests).

**Interfaces — Consumes:** Task 1. **Produces:**
```rust
pub fn plan_blend_with(m: &Mosaic, group: &str, rect: Option<[u64; 4]>, settings: BlendSettings) -> Result<BlendPlan>;
// plan_blend(m, g, rect) = plan_blend_with(m, g, rect, m.config.settings_for(g).1)
pub struct BlendPlan { /* existing fields */ pub fresh: bool }
// fresh = rect.is_none() && last_merge_settings(cfg, group) != Some(settings)
// app: Kind::Blend { group, rect, settings: BlendSettings }, captured by start_blend from the
// committed config (settings_for(group).1) when queued
```
- **`run_blend`:** for a `fresh` plan, it invalidates the whole store (`invalidate_all` + `flush_state`) after taking the writer lock and before writing.
- **`Mosaic::apply_blend`:** drops the "settings changed during the merge" refusal and keeps the `needs_analyze` refusal. The provenance records `plan.settings`.
- **`jobs::discard_blend`:** invalidates only when the group needs analysis.
- **`start_blend`'s replace rules:** unchanged. A newer merge of the same group cancels or replaces the older one.

- [ ] **Step 1: Write the failing tests.**
  - Core:
    - `a_blend_records_the_settings_it_ran_with`. This replaces `settings_edit_between_plan_and_run_blend_is_caught`: `set_settings` lands between the plan and the run; the apply is Ok; `last_merge_settings` equals the plan's settings; `settings_changed` against the new config is true.
    - `a_fresh_plan_invalidates_before_writing`: a run cancelled at once leaves no level-0 tile valid.
    - `a_same_settings_plan_is_not_fresh`.
    - `needs_analyze_still_refuses_the_apply`.
  - App (jobs):
    - `a_queued_merge_keeps_the_settings_it_was_queued_with`: queue behind an export, commit new settings, and the finished merge's provenance shows the old ones.
    - `a_stopped_fresh_merge_leaves_the_group_out_of_date`: `result_state` is not Complete.
- [ ] **Step 2:** Run `cargo test -p mmm-core --test mosaic`, then `cargo test -p mmm-app jobs::`. Both should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-core` and `cargo test -p mmm-app` pass.
- [ ] **Step 5:** Commit `feat: merge jobs carry their settings; a merge with new settings starts fresh`.

---

### Task 3: Core: per-group input invalidation and scoped analysis

**Files:** `crates/mmm-core/src/mosaic/run.rs`, `mosaic/grouping.rs`, `crates/mmm-core/src/error.rs`, `crates/mmm-core/tests/mosaic.rs`.

**Interfaces — Produces:**
```rust
pub fn affected_groups(old: &Groups, new: &Groups) -> BTreeSet<String>;
// processing groups whose member set differs, including added and vanished groups
pub fn set_inputs_affecting(m: &mut Mosaic, panels: Vec<PanelEntry>, grouping: Grouping,
                            affected: Option<&BTreeSet<String>>) -> Result<()>;
// None = every group (what set_inputs does); Some = locks/invalidates/marks only those groups
pub enum AnalyzeScope { All, Needed }
// Needed = groups in needs_analyze, plus resolved groups without a dir
pub fn plan_analyze_scoped(m: &mut Mosaic, resolved: Groups, scope: AnalyzeScope) -> Result<AnalyzePlan>;
impl Error { pub fn is_frame_moved(&self) -> bool }   // new error kind FrameMoved
```
- **Defaults:** `set_inputs` becomes `set_inputs_affecting(.., None)` and `plan_analyze_with` becomes `plan_analyze_scoped(.., All)`. Their behaviour is unchanged.
- **A `Needed` plan:**
  - locks, invalidates and marks only its groups;
  - drops the marks of vanished groups, as `All` does;
  - stores the saved analysed frame (if any) in a new `AnalyzePlan.expected_frame: Option<ReferenceFrame>`.
- **`run_analyze`:** derives the frame first, as today. When `expected_frame` is Some and differs, it returns `FrameMoved` before writing anything (the frame file included).
- **"analyze" provenance:** written once the plan's groups all finish. It lists the groups that plan analysed.

- [ ] **Step 1: Write the failing tests.**
  - `affected_groups_finds_changed_members`: a move, an add, a remove, a new group, a vanished group, and an unchanged rule switch.
  - `inputs_affecting_one_group_spares_the_others`: the OIII store stays valid and unmarked.
  - `needed_scope_analyses_only_marked_groups`: OIII's analysis files are untouched, and the provenance lists only Ha.
  - `a_moved_frame_fails_the_scoped_run`: `is_frame_moved()`, the frame file unchanged.
  - `set_inputs_still_marks_everything`.
- [ ] **Step 2:** Run `cargo test -p mmm-core --test mosaic`. It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** It passes.
- [ ] **Step 5:** Commit `feat(core): invalidate and analyse only the groups an edit touches`.

---

### Task 4: App: panel edits and analysis use the scope

**Files:** `crates/mmm-app/src/panel_edits.rs`, `jobs.rs`, `pipeline.rs`, and their tests.

**Interfaces — Consumes:** Task 3. **Produces:**
```rust
fn edit_scope(m: Mosaic /* session's mosaic with the edit's panels and grouping */,
              old: Option<Groups>, frame: Option<ReferenceFrame>) -> Option<BTreeSet<String>>;
// worker: None if old or frame is None, if reference::derive(new panel paths, analyze.input)
// fails or differs from `frame`; else Some(affected_groups(&old, &m.groups()))
```
- **Scoping an edit:**
  - `PanelEdits` scopes each edit on a worker (`Task`) before it applies. Jobs are cancelled at submit, as today.
  - A newer edit replaces one being scoped.
  - Once the scope is known and the runner is idle, the edit applies with `set_inputs_affecting(.., scope.as_ref())`.
  - `has_pending()` is true while an edit is being scoped.
- **The automatic analysis:** it starts with `AnalyzeScope::Needed` when `session.frame` is Some and some resolved group is analysed and unmarked; otherwise it uses `All`. The scope rides on `Kind::Analyze`.
- **A `FrameMoved` failure:** it marks every group (`mark_needs_analyze`) and restarts with `All`, with no failure toast.
- **`Pipeline::check_frame`:** unchanged.

- [ ] **Step 1: Write the failing tests.**
  - `adding_a_panel_to_one_group_keeps_the_others_merged`: a merged two-group session gets an Ha panel. The OIII result stays Complete and is neither re-analysed nor re-merged; Ha is re-analysed.
  - `a_frame_moving_add_reanalyses_everything`: a solved synthetic session gets a panel that grows the canvas, and every group is re-analysed.
  - `an_unresolvable_edit_falls_back_to_every_group`.
  - `a_newer_edit_replaces_one_being_scoped`.
- [ ] **Step 2:** It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): panel edits re-analyse only the groups they touch`.

---

### Task 5: App: the settings draft and immediate analysis settings

**Files:** `crates/mmm-app/src/blend_settings.rs` (+ `blend_settings_tests.rs`), `mosaic_view.rs`, `session_window/{mod.rs, sections.rs, top_bar.rs}`.

**Interfaces — Consumes:** Task 1. **Produces:**
```rust
pub const MERGE_DRAFT_KEY: &str = "merge_draft";
pub fn load_draft(cfg: &MosaicConfig) -> BlendSettings;     // ui.merge_draft if it parses, else cfg.blend
pub fn save_draft(ui: &mut serde_json::Value, draft: &BlendSettings);
impl SettingsPanel { pub fn input_error(&self) -> Option<&'static str>;          // invalid feather text
                     pub fn take_analysis_undo(&mut self) -> Option<AnalyzeSettings>; } // previous values
// Undo gains: Undo::Analyze(AnalyzeSettings)
```
- **Merge fields** (mode, feather, defect veto, flatten):
  - They edit the draft at once. There is no Apply or Revert.
  - The draft is saved with the window's debounced save.
  - Nothing is invalidated and no job is cancelled.
  - Invalid feather text keeps the last valid draft and shows its error. While it shows, Merge is blocked with "Feather width must be a number above 0".
- **Analysis fields** (input, surface, gain):
  - They apply at once through the existing pending-edit path (`set_analyze_settings`, cancelling what's in the way except exports).
  - Once a mosaic exists, each shows the cost line.
  - Each applied change makes the window push a toast "Analysis settings changed" with **Undo** (`Undo::Analyze(previous)`). The toast replaces the previous analysis toast.
  - Undo applies as a normal analysis edit and is queued like any.
- **The accent dot:**
  - A merge field gets it when its draft value (with the shown group's override) differs from that group's `last_merge_settings`.
  - An analysis field gets it when its value differs from `last_analyze_settings`.
  - For a composite, any member group counts.
  - There is no dot without a last merge or analysis.
- **The main button's settings summary** reads the draft.
- **Removed:** the Apply/Revert UI, the tile counting (`count_tiles`, `cost_text`, the count task) and the app's `last_blend`/`changed_fields`, which give way to the core helpers. Their tests are replaced.

- [ ] **Step 1: Write the failing tests.**
  - `merge_fields_edit_the_draft`: a feather edit saves `ui.merge_draft`; `config.blend` is unchanged, stores are untouched, and no job is cancelled.
  - `the_draft_survives_reopening`.
  - `an_invalid_feather_blocks_merge`.
  - `analysis_fields_apply_at_once_with_undo`: a gain change sets needs_analyze and shows the toast; Undo restores the previous gain.
  - `undo_of_an_analysis_change_waits_for_the_running_analysis`.
  - `the_dot_follows_the_last_merge`.
  - `an_old_session_shows_merged`: no draft key, merged session; the step reads Merged and no dot shows.
- [ ] **Step 2:** It should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p mmm-app` passes.
- [ ] **Step 5:** Commit `feat(app): merge settings are a draft; analysis settings apply at once with undo`.

---

### Task 6: App: Merge commits the draft; states, chips and the canvas tag

**Files:** `session_window/{mod.rs, steps.rs, top_bar.rs, canvas.rs}`, `mosaic_view.rs`, `crates/mega-ui/src/widgets/chip.rs` (+ tests, gallery, snapshots).

**Interfaces — Consumes:** Tasks 1, 2, 5. **Produces:**
```rust
pub fn out_of_date(session: &OpenSession, group: &str) -> bool;    // results != Complete || needs_analyze
pub fn draft_changed(session: &OpenSession, group: &str) -> bool;  // !out_of_date && settings_changed(cfg, g, &load_draft(cfg))
pub fn merge_needed(session: &OpenSession) -> bool;                // any group out_of_date || draft_changed
// mega-ui: ChoiceChip gains `mark: Option<ChipMark>`; pub enum ChipMark { Attention, Changed }
```
- **Merge** (the button, the step and Ctrl+Enter):
  - commits the draft (`commit_blend_settings`, when the draft differs from `config.blend`);
  - merges every group that is `out_of_date` or `draft_changed`;
  - switches to Mosaic.
- **Merge this group**, on a group chip's context menu (`egui::Popup::menu`, as `panel_list.rs` `row_menu` does), commits and merges that group only. Other running or queued merges keep the settings they were queued with.
- **Merge step:**
  - A merge running: Working "Merging …" (as today).
  - Nothing ever merged (`!session.merged`): Next "Merge".
  - Any group out of date: Next "Out of date".
  - Else any group `draft_changed`: Next "Settings changed".
  - Else: Done "Merged".
- **Main button:** **Merge** whenever `merge_needed`, else **Export…**. Export stays allowed when only "Settings changed".
- **Chips:**
  - `Attention` for an out-of-date group, `Changed` for settings changed.
  - A composite chip takes its members' worst mark.
  - The mark is a dot in tone colours with an accessible description ("out of date", "settings changed").
- **Canvas tag** (Mosaic mode, top-left of the viewer). An out-of-date shown group gets a tag over the dimmed image, chosen in this order:
  1. needs analysis and its analysis settings differ from `last_analyze_settings`: "Out of date — analysis settings changed";
  2. needs analysis otherwise: "Out of date — panels changed";
  3. no whole merge recorded: "Not merged yet";
  4. else: "Out of date — merge didn't finish".

  A `draft_changed` group gets "Settings changed since this merge". Either tag, clicked, reveals Merge settings. The tooltip says Merge re-merges it.
- **Findability:** Merge settings opens by itself the first time an analysis finishes in a session never merged (spec §11).

- [ ] **Step 1: Write the failing tests.**
  - `merge_commits_the_draft_and_merges_what_needs_it`: a changed draft feather on a merged two-group session. Merge commits; only the groups needing it merge; the whole entries record the draft.
  - `settings_changed_is_not_out_of_date`: the step reads "Settings changed", the main button Merge, and export is allowed.
  - `out_of_date_wins`.
  - `merge_this_group_merges_one`.
  - `a_running_merge_keeps_its_settings_across_a_commit`.
  - `chips_mark_their_state`.
  - `the_canvas_tags_say_why`.
  - `merge_settings_open_when_analysis_first_finishes`.
  - mega-ui: `choice_chip_draws_its_mark`.
- [ ] **Step 2:** It should FAIL.
- [ ] **Step 3:** Implement, updating the module doc table in `steps.rs`.
- [ ] **Step 4:** `cargo test -p mmm-app` and `cargo test -p mega-ui` pass.
- [ ] **Step 5:** Commit `feat(app): merge commits the draft; out of date versus settings changed`.

---

### Task 7: App: queued behind exports, and edits that stop merges

**Files:** `session_window/steps.rs`, `panel_edits.rs`, `session_window/mod.rs`, `app.rs`, and their tests.

- **Queued behind an export:** a whole-canvas merge queued while an export runs shows the Merge step as Working (no fraction) "Merge queued after export". It is cancellable from the step.
- **A panel edit that stops merges:** when an edit cancels running or queued whole-canvas merges, the window pushes one toast, "Merge stopped: panels changed". The shell's generic per-job "cancelled" toasts are suppressed for those merges.

- [ ] **Step 1: Write the failing tests.**
  - `a_merge_waits_behind_an_export`.
  - `a_panel_edit_stops_the_merge_with_one_toast`.
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** They pass.
- [ ] **Step 5:** Commit `feat(app): merges queue visibly behind exports; edits say why they stopped a merge`.

---

### Task 8: Summary and start screen

**Files:** `crates/mmm-core/src/session_summary.rs`, `crates/mmm-app/src/summary_writer.rs`, `start/{facts.rs, mod.rs, hero_views.rs}`, and their tests.

- **Core:** `GroupSummary` gains `settings_changed: bool` (`#[serde(default)]`).
- **`summarize`** sets it for Complete groups that aren't marked `needs_analyze`, from `settings_changed(cfg, g, &load_draft(cfg))`. The writer's dirty inputs include the draft and the provenance length.
- **The start screen** gains `CardState::SettingsChanged`:
  - It applies when every group is complete and some group has `settings_changed`.
  - Status: `Tone::Info`, "Merge settings changed since the last merge".
  - Card label: "Settings changed".
  - The hero treats it as merged.
- **`ReMergeNeeded`** keeps "Panels changed since the last merge — re-merge needed" for stale or partial results.

- [ ] **Step 1: Write the failing tests.**
  - `summary_records_settings_changed`.
  - `a_settings_changed_card_reads_so`.
  - `old_summaries_still_load`.
- [ ] **Step 2:** They should FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** They pass.
- [ ] **Step 5:** Commit `feat: the start screen tells settings changed from out of date`.

---

### Task 9: Docs, selftest, snapshots

- [ ] **Update DESIGN.md:**
  - the draft model and commit-at-merge;
  - merge jobs carrying their settings, and fresh merges;
  - the Merge states and predicates;
  - chip marks and canvas tags;
  - analysis settings with Undo;
  - per-group input invalidation and scoped analysis, with the FrameMoved fallback;
  - merges queued behind exports;
  - the start screen's new state;
  - the "Coming in project 3" list.
- [ ] **Selftest and gates:**
  - Run `make_selftest_session` then `--selftest`. Expected: exit 0.
  - Refresh snapshots with `WGPU_BACKEND=vulkan UPDATE_SNAPSHOTS=1 …`, delete the diffs, and look at them.
  - fmt, clippy, doc and the workspace tests are clean.
- [ ] Commit `docs: draft merge settings and per-group staleness`.
