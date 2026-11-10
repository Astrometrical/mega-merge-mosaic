# Project 3 — the in-session experience: rulings and deferred minors

The record of project 3's execution (2026-11-08 to 2026-11-10). It lists
every judgment call made while implementing the six plans where the spec or
plan was silent, ambiguous or wrong ("rulings"), and every minor review
finding that was parked or deferred rather than fixed. Each ruling ends with
what it costs if it was wrong. Read it before follow-up work, so a ruling is
changed knowingly rather than by accident.

- Spec: `docs/superpowers/specs/2026-11-08-in-session-experience-design.md`.
- Plans, in `docs/superpowers/plans/`:
  1. `2026-11-08-session-window.md`
  2. `2026-11-09-panel-list.md`
  3. `2026-11-09-draft-settings.md`
  4. `2026-11-09-roi-previews.md`
  5. `2026-11-09-composites-export.md`
  6. `2026-11-10-checks-badges.md`
- The design as built is in `docs/DESIGN.md`. Where this record and
  DESIGN.md disagree, DESIGN.md (and the code) is current. Later rulings and
  final fix passes supersede earlier ones within a plan.

## What shipped (all on main)

| Plan | Merge | What it added |
|---|---|---|
| 1 | 07995bb | One session window: step strip, view bar, inspector, toasts |
| 2 | 152ce1d | The panel list and grouping (rules, Undo, add/drop folders, column sort) |
| 3 | cf80629 | Draft merge settings and per-group staleness |
| 4 | 7e607ac | ROIs, transient previews and Compare A/B |
| 5 | f3fdb6c | Composites and the Export dialog |
| 6 | ae3e71e | Checks, badges and seams (closes project 3) |

CI fixes merged between plans: cc9d546 (session lock retry), d52d435 (title
test), 18ace0f (busy probe retry), 523050b (previews test).

## Open follow-ups when project 3 closed

1. **Real-data smoke run on `test_data/orion_mosaic`** (the machine that
   built project 3 had no test data, so none of these were measured):
   - **Plan 6 thresholds.** Count the Problems on a clean, merged session:
     - ResidualSeam (relative rule, with no absolute floor);
     - ThinOverlap on diagonal neighbours' corner overlaps;
     - PanelGradient on the M42 panels;
     - NoiseOutOfLine.
   - **Composite badges.** That an SHO composite shows one badge per
     pointing (`SAME_POINTING_IOU = 0.5`).
   - **Time and memory.** One checks run per group, and preview time and
     memory (about 0.9 GB of L8 summaries per 12-panel group).
2. **Known issues from before project 3**, not fixed:
   - Quitting mid-export leaves `.part`/`.scratch` files with no prompt.
   - Those sibling names could clobber a same-named user file.
   - The rfd file pickers run on worker threads (macOS prefers the main
     thread).
3. **Housekeeping.** `actions/checkout@v4` targets Node 20, which is
   deprecated; consider moving to `checkout@v5`.

CI on main after plan 6 (ae3e71e): ubuntu, windows and macOS all green on
the first run; Module workflow green.

---

# The full record, plan by plan

Ruling IDs (R1, R-P2, R6-P3 …) are local to their plan. "Task N" is the
plan's task; "Final" is the whole-branch review and its fix pass. "Parked"
minors were noted at a task review; "deferred" ones at the final review.

## Plan 1 — session window

### Rulings
- Ruling R1: Ruling: Task 10 flushes the saves that exist when it runs (debounces, notes, summary); Task 11 adds the view-state flush to the same close/flush paths (its text already says "flushed by Shell::flush_pending_saves and on back") — the plan's T10 mention of Task 11 is forward-looking, not a T10 requirement — cost if wrong: one extra line in T11.
- Task 1: Ruling: plan's `Task::poll(&mut self)` is a plan typo; the verbatim move keeps `&self`; blend_settings.rs and export.rs imports also updated (unlisted users of the helpers) — cost if wrong: none (signature unchanged from today)
- Task 2: Ruling: reviewer's 'gallery snapshots stale' (Important) — the plan refreshes all image snapshots in Task 12 (they are ignored GPU tests), so they stay stale until then — cost if wrong: none; T12 regenerates them.
- Task 3: Ruling: stale gallery snapshots (Important) — same as Task 2: Task 12 regenerates — cost if wrong: none.
- Task 6: Ruling: export_ui takes &mut OpenSession (plan said &OpenSession) because the dialog's merge-first path starts a merge — cost if wrong: none (callers have &mut)
- Task 7: Ruling: temporary Merge / Export… / Cancel buttons in the old session bar keep the app usable between Task 7 and Task 8 — Task 8 removes them with the bar — cost if wrong: none
- Task 7: Ruling: the Mosaic segment is enabled once a merge has started (session.merged OR a merge job queued/running), not only after the first completes — spec §7.1 says pressing Merge switches to the mosaic so the user watches it fill in; the disabled-but-selected state lets a user who clicks Plan mid-first-merge never get back — cost if wrong: Mosaic briefly enabled showing an empty canvas before the first tiles land
- Task 8: Ruling: (plan-mandated finding) only one step is highlighted Next — the first step that isn't done — and Checks shows Next "Analyse" only when the pipeline will not start the analysis itself (stalled after a cancel/failure); otherwise Todo "Checks" / Working; Merge stays Todo while analysis is needed or running — spec §6 ('The step that is the natural next action'; automatic work 'needs no button') overrides the plan's table — cost if wrong: a user who cancelled analysis sees the Analyse prompt only after the stall, as intended
- Task 8: Ruling: the pipeline ignores a stored auto_analyze=false (analysis is always automatic now the checkbox is gone; the key stays in mosaic.json) — spec §6; otherwise such sessions stay manual with no way back — cost if wrong: a CLI-era user who disabled auto-analysis gets automatic analysis
- Task 8: Ruling: clicking a Done 'Merged' Merge step shows the mosaic (switches the canvas to Mosaic) instead of re-merging every group — spec §6 'Clicking Merge merges (as the main button does)' and the main button reads Export… when merged; one click on a status pill must not start an hours-long re-merge — cost if wrong: none
- Task 8: Ruling: 'the first step that isn't done' means the first step waiting on the user — a Working step (previews loading, checks re-running) doesn't take the Next highlight — matches spec §6's intent (highlight the natural next action, not background work) — cost if wrong: one-line change
- Task 9: Ruling: format_bytes and OpenSession::group_count deleted (plan said keep format_bytes) — both became dead under -D warnings — cost if wrong: re-add when needed
- Final: Ruling: re-graded by effect into the single fix pass: minors #4 (empty plan copy says 'Panels tab'), #5 (double fold in Merge settings), #6 (demoted Analyse opens Problems), #7 (silent no-op clicks / missing disabled reasons), #8 (analyse/analyze spelling: app UI uses British 'analyse'), #9 (three DESIGN.md inaccuracies), #10 (composite member note shifts the image on Plan↔Mosaic), #11 (single-group 'Merging mosaic · 1 of 1'), #14 (toast test flake risk on CI), T11 Tab-focus disables shortcuts (use text_edit_focused) — each cheap and user-visible or CI-relevant — cost if wrong: a slightly larger fix pass
- Final: Ruling: deviate from 'no second fix wave' for this one residual — resume the fix implementer for a one-line guard (&& !jobs.analyzing() / not Working) plus a test, and the 8 pt item-spacing one-liner; controller verifies with the focused tests rather than another review seat — shipping a confirmed regression to main is worse than a one-line extra round — cost if wrong: one small extra round

### Deferred minors
- Task 1: minor (deferred): Debounce's test stays in blend_tab_tests.rs (part of the stretch-save test, which Task 6 rewrites) rather than util.rs
- Task 2: minor (deferred): pill accent border flickers off while the pointer is over its cancel button (use rect_contains_pointer)
- Task 2: minor (deferred): local size literals in step_strip.rs/chip.rs (icon font 12, HEIGHT-6) rather than tokens
- Task 2: minor (deferred): step state (done/warn/progress) not exposed to AccessKit, only the label
- Task 2: minor (deferred): selected choice chip fills with BORDER (no SURFACE_600 token)
- Task 2: minor (deferred): None-fraction (half-strength) working fill's label contrast not asserted
- Task 3: minor (deferred): chevron glyph size literal 14.0 and stroke 1.0 in inspector.rs (name consts / tokens)
- Task 3: minor (deferred): heading lacks pointing-hand cursor and expanded/collapsed AccessKit state; long titles not single-line
- Task 3: minor (deferred): no tests for badge tone colour, Hide inspector button, reveal's scroll
- Task 4: minor (deferred): strong-stretch midtones ordering and flat-data (mad==0) strong path not asserted
- Task 5: minor (deferred): plan_tab toolbar reads view before PlanView::show validates it (empty View combo first frame) — wrapper goes in Task 7
- Task 5: minor (deferred): 'not placed yet' label negative check dropped; selection-dropped-when-panel-removed rule untested
- Task 5: minor (deferred): composition_stretch_key doc still mentions PLAN_STRETCH_KEY; start/hero_views.rs:262 still renders through saved plan_stretch (Task 12 / start-screen follow-up)
- Task 5: minor (deferred): #[cfg(test)] uploaded_images hook in the production struct
- Task 5: minor (deferred): a pan during the provisional-extent window is undone when the frame's extent arrives
- Task 6: minor (deferred): camera kept on a real canvas-geometry change (re-analysis changed canvas) where it used to re-home — check vs plan view in final review
- Task 6: minor (deferred): JobRunner::analyzing() defined by exclusion of other kinds
- Task 6: minor (deferred): can_open_export checks only tracked analyze_job, not jobs.analyzing()
- Task 6: minor (deferred): m.stretch survives a result becoming Absent (next first merge holds the old stretch, not plan_stretch)
- Task 6: minor (deferred): duplicated debounced save in BlendTab::save_if_due and MosaicView::update (transitional until Task 7)
- Task 6: minor (deferred): no tests that a look change / view change recomputes an open source's stretch; app.rs plan_stretch-only-when-views-match untested
- Task 7: minor (deferred): a change made in ui() is saved on the next poll() frame (no repaint request) — close-time flush covers it
- Task 7: minor (deferred): no test that switching chips keeps the camera
- Task 7: minor (deferred): merged never resets to false after a regroup leaves no group with a result
- Task 7: minor (deferred): See-all button reads 'Problems only' while showing all (copy constraint names only the off state)
- Task 7: minor (deferred): gallery doesn't show a disabled segment
- Task 7: minor (deferred): view bar draws wrapped on the very first frame then one row (one-frame layout shift); nothing asserts the app settles after merges in jobs_show_in_status_bar_and_notices (run_steps(2))
- Task 8: minor (deferred): Export step is a silent no-op when !can_open_export(); main Export… has no disabled-hover reason
- Task 8: minor (deferred): click paths untested: Panels Read previews, Merge step → merge, Export step → open_export, main Merge disabled while merging
- Task 8: minor (deferred): test harness pipeline 'editing' omits panels_editing() unlike the shell
- Task 8: minor (deferred): steps.rs imports sections::counted (move to util)
- Task 8: minor (deferred): settings summary is a normal-size Quiet button (no small size in mega-ui)
- Task 8: minor (deferred): top bar's ‹/name/strip side does not wrap on a narrow window (strip may run under the main button) — check in final review
- Task 8: minor (deferred): clicking a demoted (Todo) stalled 'Analyse' reveals Problems instead of starting analysis while Panels is Next; untested interaction
- Task 8: minor (deferred): Mosaic segment's 'a merge has started' uses jobs.blending() (includes ROI merges) — deliberate
- Task 9: minor (deferred): wording test misses hover-only/disabled-hover text, the start screen and the rfd dialog title (swept by grep)
- Task 9: minor (deferred): title test polls with real sleeps (30 s deadline); toasts.show return value discarded (no toast actions today); repeated ctx time reads
- Task 10: minor (deferred): UI frozen up to 3 s with no feedback while jobs stop (note in DESIGN.md, Task 12)
- Task 10: minor (deferred): final summary may record a just-applied panel edit's count with the old groups (self-corrects on next open; DESIGN.md note)
- Task 10: minor (deferred): reaper-thread spawn failure releases the lock at once (worker may still write; summary lost) — close_session doc overclaims; safer: keep handles/lock in Shell and poll is_finished()
- Task 10: minor (deferred): test doesn't prove the reaper's final summary write happens before release
- Task 7: minor (deferred): panel-Accept test now clicks via accesskit (no longer proves scroll/pointer reachability); title test observes 3 steps instead of a settled run
- Task 11: minor (deferred): Tab-focusing a chip/button disables all shortcuts (wants_keyboard_input counts any focus; clicks don't focus) — consider text_edit_focused()
- Task 11: minor (deferred): one bad field drops the whole saved view (strict deserialize)
- Task 11: minor (deferred): SessionWindow::view_key duplicates MosaicView::view_key; can_merge duplicates the main button's branch conditions
- Task 11: minor (deferred): restored camera not checked against the current canvas; [/] unreachable on AltGr layouts; no idle-frames-don't-save test
- Task 12: minor (deferred): DESIGN.md :1026/:1065 still say 'Blend ROI(s)'/'composition views' in phase-3 prose; some re-edited lines not re-wrapped; inspector body not indented in the gallery demo
- Final: minor (deferred): #12 [StripStep;4] + index constants will need a refactor for the §16 processing step; #13 stretch_panel.rs misleading name; T10 3 s UI block could shrink (~300 ms) given the reaper; T10 core: no cancel check after DECODE_LOCK + a 'Closing…' card state
- Final: minor (deferred): plan_blend's 'analyze first' messages (run.rs:389,:394) American and not naming the command, reachable only via a race as a merge failure toast; mixed '…re-run `mmm analyze`. Analyse again.' checks error predates the branch; shortcut letters act while a ComboBox popup is open

CI for plan 1 merge (07995bb): CI success (ubuntu/windows/macos) 8m44s; Module success.

## Plan 2 — panel list and grouping

### Rulings
- Ruling R1: Ruling: "New group…" creates the group from the current selection rather than an empty group — groups exist only through their members' `manual_group` in the data model; persisting empty groups would need a new config field for no user gain (you can't merge an empty group) — cost if wrong: add an empty-groups list later.
- Ruling R2: Ruling: the copy 'Move to ▾' is drawn as 'Move to' + icons::CHEVRON_DOWN (trailing icon in the icon font) — '▾' doesn't render in Inter (box glyph) — cost if wrong: none; carry to Tasks 6/7
- Task 4: Ruling: component-internal geometry as named local constants (ICON_SIZE, DOT, TAG_HEIGHT, MIN_FIRST_WIDTH, HIGHLIGHT_STRENGTH) is accepted — matches existing mega-ui widgets (chip DOT, session_card PIN_SIZE, step_strip); colours, spacing and radii come from theme tokens; promoting icon-size tokens is a later theme pass — cost if wrong: a small refactor across widgets
- Ruling R3: Ruling: Undo restores only what its action changed — Undo of Remove re-inserts exactly the removed PanelEntry values (with their manual groups, at their former positions when possible) into the CURRENT edit; Undo of automatic grouping restores only the previous Grouping — instead of re-submitting a full PanelsEdit snapshot (which would also revert later, unrelated edits) — carry to Tasks 7 and 9 (Undo variants e.g. Readd(Vec<(usize, PanelEntry)>) / Grouping(Grouping)) — cost if wrong: a snapshot undo is simpler but surprising
- Task 6: Ruling: fold minors #3 (stable row ids from the path, not indices — Task 7's selection/focus depend on it), #4 (max width so the canvas can't be squeezed to nothing), #5 ('—' while reading: global constraint), #1 (test for checks-report problem rows incl. a problem row staying visible in a rolled-up section) into fix round 1 with the Important — each cheap and load-bearing for Task 7 or a constraint — cost if wrong: a slightly larger round
- Task 8: Ruling: dropping/adding a folder that yields no new panels (none at its top level — the per-filter-subfolders case — or all duplicates, or unreadable) gets a toast explaining why (e.g. 'No panels at the top level of {folder}' / 'Those panels are already in the session' / 'Couldn't read {folder}'), folded into Task 9 — a reasonable person dropping a parent folder otherwise sees nothing happen — cost if wrong: one more toast
- Task 9: Ruling: auto-grouping is skipped for a session opened with analysis results (group_dirs non-empty at first poll) while its panels are still those it was opened with; once panels are added/removed (which invalidates everything anyway) it may fire; an accepted FiltersDifferUngrouped note or a disabled non-default rule counts as a prior choice — spec §10.3's 'one click undoes it' must hold, and Undo can't restore invalidated results — cost if wrong: an old ungrouped multi-filter session isn't grouped automatically (the user picks FILTER header)
- Task 9: Ruling: re-picking the shown Group by choice changes nothing (legacy FilenameToken stays until the keyword is edited); fold in minors: a stale auto-grouping Undo is a no-op unless the grouping is still the automatic Header{FILTER}; the core FiltersDifferUngrouped remedy copy becomes 'Group by FILTER header to merge each filter separately.' (update its test) — cost if wrong: none
- Final: Ruling: build column sorting (click a column heading sorts rows within each section by that column, numeric columns numerically; again reverses; sort persists in TableState) rather than defer — spec §10.2 is binding and this plan's scope — cost if wrong: a small mega-ui addition
- Final: Ruling: the single fix pass includes the reviewer's five 'fix before merge' minors (T3 footprints cleared on frame change; T3 stale docs; T5 dismiss undo toasts on session close/open via Toasts::dismiss; T7 folding a section deselects its rows; T9 toast for batches of only non-panel/missing files) plus cheap hardening/UX: catch_unwind around header reads (Unreadable row instead of a hot retry loop); a plain row click on a panel outside the shown view switches the canvas to its group; empty-state copy mentions folders; conflicting path-keyword reason; keep the previous placement while a rule change resolves (no 150-row No-group flash); case-insensitive key compare for Header keyword re-pick — each user-visible and small — cost if wrong: a larger pass

### Deferred minors
- Task 1: minor (deferred): a later keyword occurrence as the last token returns None even when an earlier occurrence had a value (FILTER_Ha/m_FILTER.xisf) — undocumented/untested
- Task 1: minor (deferred): no regression tests for multi-dot names, dotted folders, UNC, no-parent, non-UTF-8 paths; keywords containing a separator can never match (undocumented)
- Task 2: minor (deferred): quoted numeric EXPTIME ('300') reads as unknown; huge NCOMBINE saturates; EXPOSURE fallback / FITS source untested
- Task 3: minor (deferred): footprints not cleared on a frame change (old outlines in wrong coords until the worker finishes after a solved↔aligned switch); stale module/field docs say 'solved frames'; cancellation untested; compressed-panel path untested
- Task 4: minor (deferred): narrow widths clip trailing columns and the actions button (no truncation/hiding); only the first cell is exposed to AT, headings lack expanded state; Hover still emitted during a drag; per-frame width layout O(rows×cols); 'Panel actions' hard-coded in a generic component; problem rows hidden when a section is folded (heading warning covers it)
- Task 5: minor (deferred): clearing undos on close leaves the toast visible with a dead Undo button; queued-undo/Dismissed/clear paths untested
- Task 6: minor (deferred): header cache untested; Add path row can't be dismissed; Add folder disabled without tooltip; PathKeyword field doesn't remember token_keyword; infos never pruned; second open for non-FILTER keys; MixedChannels problem un-rolls whole groups
- Task 6: minor (deferred): first column clipped rather than ellipsis-elided when narrow; CANVAS_MIN_WIDTH token lives in generic mega-ui tokens
- Task 7: minor (deferred): plain click on a row whose add is still queued clears the selection (plan drops non-session panel); selection can include folded-away rows (Delete/Move act on them); plan/list selection can drift (Ctrl-deselect, empty-canvas click); Readd positions after out-of-order undos; test gaps (readd skip, queued undo, Esc closing a menu keeps selection); highlight invisible on a selected plan item
- Task 8: minor (deferred): picker-returned files with a non-panel extension aren't filtered (predates); case-sensitive sort on Unix; unreadable-folder/symlink cases untested
- Task 9: minor (deferred): a batch of only non-panel/missing files stays silent (needs new copy); 'Couldn't read' only unit-tested; filters lives apart from resolved
- Task 9: minor (deferred): add-then-remove back to the opened set keeps auto-grouping blocked (harmless); no explicit add-Ha/analyse/add-OIII test in one window
- Task 10: minor (deferred): DESIGN.md panel-list bullet lists Add files only while a later paragraph adds Add folder/drops (overlap); 'exposure times frames' wording
- Final: minor (deferred): Shift-range O(n²) id→path lookups (fine at 150); smoke-test INTEGRATION/SCALE on real MBC panels (test_data not present on this machine)
- Final: minor (deferred): DESIGN.md/plan_view Drop docs overclaim (an in-flight compressed decode still finishes); panel_list module doc misses the ONLY_PANELS reason; a dropped missing .xisf gets the 'Only XISF and FITS' toast; Outline::Preview entries not fingerprint-rechecked; UNKNOWN duplicates mega_ui::table::UNKNOWN_CELL; hidden selection via drag onto a folded heading / rule change into a folded section


## CI fix (between plans 2 and 3)

- Ruling: macOS CI flake (`edited_notes_leave_room_for_the_scroll_bar` got Busy from save_notes) fixed by lock_session retrying contention for up to 250 ms (LOCK_CONTENTION_WAIT) — a session_busy shared probe from the start screen's card loader can briefly hold the lock on macOS — cost if wrong: a genuinely busy session reports Busy 250 ms later; only worker threads call lock_session.


## Plan 3 — draft merge settings

### Rulings

- Ruling R-P1: guard tests that already pass before the change are allowed and must be named as such in the report — they pin behaviour the plan promises to keep — cost if wrong: none beyond a test that never went red.
- Ruling R-P2: apply_blend drops its "settings changed during the merge" refusal (the CLI never calls it; provenance now records the settings that made the tiles); merge jobs capture settings at enqueue; whole merges with new settings invalidate first (`fresh`) — this replaces the earlier envisaged "restart other groups' in-flight merges" ruling, because spec §11 says a running merge keeps the settings it started with — cost if wrong: an in-flight merge's group shows "Settings changed" instead of being re-merged with the new commit; the user presses Merge again.
- Ruling R-P3: panel edits still cancel every non-export job (unaffected groups' merges included) — the scoped analysis that follows would cancel them anyway (start_analyze cancels running non-previews) — cost if wrong: an unaffected group's merge must be pressed again.
- Ruling R-P4: the job runner stays single-slot, so every merge (not only a merge of a group being exported, spec §14) waits behind a running export; the strip says so — cost if wrong: merges wait longer than the spec allows during exports; plan 5 (export dialog) may revisit.
- Ruling R-P5: plan 3 ROI merges still write into the real stores with committed settings (plan 4 makes them transient previews of the draft) — an ROI merge after "Merge this group" committed new settings can mix settings inside another group's store — cost if wrong: a group shows "Settings changed" over a store with some new-settings tiles until plan 4.
- Task 1: Ruling: last_merge_settings falls back to an older entry when the newest whole entry's settings fail to parse — carried into Task 2's dispatch (stop at the newest whole entry; None if unparseable) since Task 2's `fresh` reads it — cost if wrong: a false "Merged"/"Settings changed" after a schema change.
- Task 5: Ruling: the analysis cost line shows once, as a muted note under the analysis fields (spec §11 says "beside it") — one line beats the same sentence repeated in a 4th column of a narrow inspector — cost if wrong: a layout tweak.
- Task 5: Ruling: Undo of an analysis change raises its own "Analysis settings changed" toast (replacing the previous), whose Undo redoes the change — every applied analysis change gets an Undo, spec §15 — cost if wrong: one extra toast after Undo.
- Task 6: Ruling: tag reason order replaced — needs_analyze+settings differ → analysis settings changed; needs_analyze → panels changed; no whole merge → Not merged yet; an "inputs" entry newer than the last whole merge touching the group → panels changed; an "analyze" entry newer than the last whole merge with different settings → analysis settings changed; else merge didn't finish — the brief's order was a plan defect — cost if wrong: a tag names a slightly wrong cause.
- Task 6: Ruling: commit_blend_settings saves mosaic.json on the UI thread at Merge — one small JSON write, the same accepted exception as job apply saves — cost if wrong: a tiny hitch on a slow disk at Merge.
- Task 6: Ruling: Merge this group is disabled ("Merging {group}…") while that group's whole merge runs/queues, matching the main Merge's block — cost if wrong: the user waits instead of restarting with a changed draft.
- Final: Ruling: R-P5 replaced — ROI merges use the draft; a group with an ROI entry newer than its last whole merge with other settings is out of date ("Out of date — region merged with other settings"); start screen agrees — restores spec §11's try-interactively loop before plan 4 and stops silent mixed exports — cost if wrong: an interim mechanism plan 4 replaces with transient previews.
- Final: Ruling: re-graded Minor 3 (quiet tag TEXT_MUTED on transparent bg can fail AA over bright image) to Important — user's standing "accessibility trumps looks" and UI standard AA — cost if wrong: a slightly less quiet tag.
- Final: Ruling (declined-to-judge lines): ReMergeNeeded copy covers more causes than "panels changed" — kept per plan; a never-merged new group makes the step read "Out of date" not "Merge" — plan's order; reveal-on-first-analysis repeats on reopen of a never-merged session — spec allows; others pre-existing — cost if wrong: copy tweaks.

### Deferred and parked minors

- Task 1: minor (parked): commit_does_not_invalidate has a no-op `let (_t, mut m)` wrapper; settings_changed_respects_overrides lacks a non-pinning-override case; no test of a base made invalid only through an override.
- Task 2: minor (parked): last_merge_settings doc has one over-long line; `a_same_settings_plan_is_not_fresh` also asserts never-merged and different-settings cases (name undersells); a never-merged group's first merge runs an unnecessary invalidate_all on an empty store.
- Task 3: minor (parked): plan_analyze_scoped turns a corrupt/unreadable frame file into None (Needed run skips the moved-frame guard) — app uses Needed only when session.frame is Some; no test for Some(affected) naming a dir-less group, nor for two marked groups with one completing; processing_groups() built three times in plan_analyze_scoped.
- Task 4: minor (parked): restart's mark_needs_analyze untested (All marks anyway); poll_until_applied / apply_edit test helpers near-duplicated; apply_waiting take-then-put-back style.
- Task 5: minor (parked): lost assertions on the old waiting label and on cancelled merges leaving no provenance; Busy retry untested (before and after).
- Task 5: minor (parked): a settings failure during apply_waiting on leave is logged but its toast is likely never drawn.
- Task 6: minor (parked): per-frame repeated draft_changed/compositions_of work.
- Task 7: minor (parked): no test of ROI-merge/repeat-call exclusions for cancel_for_panel_edit; edit_stopped set never pruned (ids unique, tiny growth); a flagged merge finishing before seeing the cancel still gets "Merge stopped".
- Task 8: minor (parked): no hero test for the SettingsChanged state (covered by the match only).
- Final: minor (deferred): plan_analyze_scoped(Needed) reads reference.mmm-frame.json on the UI thread after each scoped edit.
- Final: minor (deferred): Merge isn't blocked while a panel edit is pending/scoping (a merge pressed in that window gets stopped with "Merge stopped").
- Final: minor (deferred): Undo of an analysis change pressed while a newer analysis edit waits drops the newer edit.
- Final: minor (deferred): after safety-net mark-all paths (FrameMoved restart, check_frame), untouched groups' tags read "merge didn't finish" (no inputs entry).
- Final: minor (deferred): "Merging {group}…" block reason / MERGING tooltip show while a merge is only queued behind an export.
- Final: minor (deferred): test gaps — window-level Undo during analysis; window old-session test keeps whole:true entries; no test that a partly-run fresh merge leaves unfinished rows invalid.

Plan 3 notes (from the session record):
- R-P2: apply_blend drops its settings-changed refusal; merge jobs capture settings at enqueue; whole merges with new settings invalidate first ("fresh") — replaces the envisaged "restart other groups' in-flight merges" — cost if wrong: an in-flight merge's group shows Settings changed, user presses Merge again.
- R-P3: panel edits still cancel every non-export job — the scoped analysis would cancel them anyway — cost: an unaffected group's merge must be pressed again.
- R-P4: single-slot job runner, so every merge waits behind a running export (spec: only a merge of the exported group) — cost: longer waits during exports; plan 5 may revisit.
- R-P5: plan 3 ROI merges still write real stores with committed settings until plan 4 — cost: possible mixed-settings tiles under "Settings changed" until plan 4.
- Plan 3 copy calls: canvas tags "Out of date — merge didn't finish" and "Not merged yet" (spec gave only examples); invalid feather blocks Merge ("Feather width must be a number above 0").
- CI after the lock fix (cc9d546): ubuntu, windows, macOS all green. Annotation: actions/checkout@v4 targets Node 20 (deprecated, forced onto Node 24) — worth bumping to checkout@v5 sometime.
- Plan 3 T5: analysis cost line "Re-runs the analysis; the mosaic goes out of date" shown once under the analysis fields, not beside each — cost: layout tweak.
- Plan 3 T5: Undo of an analysis change raises its own toast (acts as redo), replacing the previous — cost: one extra toast.
- Plan 3 T6: canvas tag "why" order reads provenance (inputs/analyze entries newer than the last whole merge) because the automatic re-analysis clears needs_analyze — cost: a slightly wrong cause named.
- Plan 3 T6: Merge commit saves mosaic.json on the UI thread (small write, accepted exception) — cost: tiny hitch on slow disks.
- Plan 3 T6: Merge this group disabled while that group already merges — cost: wait instead of restart.
- Plan 3 final: R-P5 replaced — ROI merges use the draft; an ROI merged with other settings than the group's last whole merge makes the group Out of date ("region merged with other settings") — cost: interim until plan 4's transient previews.
- Plan 3 final: quiet "Settings changed" tag gets a translucent panel for AA contrast (re-graded to Important) — cost: slightly less quiet.
- Plan 3 final: chip marks get tone icons + hover text (colour alone broke the UI standard).
- Plan 3 deferred minors: UI-thread frame-file read after scoped edits; Merge not blocked during edit scoping; Undo can drop a newer waiting analysis edit; safety-net mark-all leaves "merge didn't finish" tags; "Merging…" tooltip while queued behind export; a few test gaps. Earlier parked: see ledger.

## Plan 4 — ROIs, transient previews and Compare A/B

### Rulings

- Ruling R4-P1: Compare A/B (spec §8.3) excluded from plan 4 — the spec says the user decides on a mock-up first; the controller brings a mock-up to the user rather than block the run — cost if wrong: one more small plan later.
- Ruling R4-P2: previews run on a dedicated single-thread PreviewRunner with one preview in flight (no queue): the scheduler re-picks each frame — spec §8.2 "no queue builds up", cancel/restart — cost if wrong: one preview at a time is slower with many visible ROIs.
- Ruling R4-P3: preview output capped at PREVIEW_MAX_PIXELS (2048²) by integer downsample; cache capped at 1 GiB LRU — a canvas-sized ROI would otherwise need ~2 GB per patch — cost if wrong: large ROIs preview blurry when zoomed in.
- Ruling R4-P4: the analysis stamp of a preview key is the index of the newest "analyze" provenance entry listing the group — cheap, moves on every re-analysis (panel and analysis-setting changes go through one) — cost if wrong: a spurious re-preview after an unrelated re-analysis.
- Ruling R4-P5: region_differs and its wiring stay though the app stops writing region entries — sessions touched by plan-3 builds may have them — cost if wrong: dead-ish code until removed.
- Ruling R4-P6: ROI names "Region n" for new ROIs (spec §8.1); old "ROI n" names kept.
- Task 1: Ruling: preview downsample is 1 or 8 only (blend supports no other factors) — an ROI over PREVIEW_MAX_PIXELS previews at 1/8 — typical seam ROIs fit at full res — cost if wrong: mid-size ROIs (4–268 MP) look soft until intermediate factors exist.
- Task 1: Ruling: a preview may fill the decoded-panel cache (<session>/decoded, a derived cache any panel read fills) — docs say so; Task 5's untouched-session test checks provenance, mosaic.json and stores, not that cache — cost if wrong: a cache file appears after a preview.
- Task 2: Ruling: the gallery snapshot (fixed 1200×4000) cuts off its bottom sections (pre-existing, worsened by the new Plan canvas section) — grow the snapshot to fit the whole gallery — cost if wrong: bigger PNGs.
- Task 3: Ruling: the gallery snapshot becomes paged (several snapshot images, each ≤4096 pt tall) so new sections never overflow wgpu's texture cap — done in Task 3, which adds the ordered list to the gallery — cost if wrong: more snapshot files.
- Task 4: Ruling: while the plan's frame ≠ the analysed frame, the plan shows no ROI overlays (and later no patches) and Draw ROI can't arm on the plan ("Available once the analysis catches up with the panels") — one predicate reused by Task 6; the mosaic stays consistent with the analysed frame — cost if wrong: no plan drawing during a re-analysis window.
- Task 5: Ruling: the ROI panel's store-tile freshness machinery (fresh, fresh_task, want_fresh, fresh_text, the "Merged tiles" details line) is replaced by preview state — it described store tiles, which previews no longer touch — cost if wrong: a lost tile-coverage readout in the ROI details.
- Ruling: the label layout in the app is "buttons first" (the user's first suggestion); the second-row alternative stays in the mock-up — cost if wrong: a small layout swap.
- Task 7: Ruling: Use A shows A's pinned patches at once (they match the restored draft) instead of re-merging B — accepted, faster and equivalent — cost if wrong: none visible.
- Task 7: Rulings for the fix round (from the implementer's concerns): Space peeks only when no widget has keyboard focus (a focused button keeps Space as activation — accessibility); Compare shows only on the selected ROI's label and the compared ROI (less canvas clutter); a click inside the compared preview on the plan doesn't select the panel underneath; the canvas's own overlay buttons never trigger the hold peek; B out of date but not merging reads "B · out of date".
- Final: Ruling: partial analyses record an "analyze" entry for their completed groups; an analysed group without a stamp previews (sentinel) — cost if wrong: one extra provenance entry per stopped analysis.
- Final: Ruling: replaces "downsample 1 or 8 only" — ROIs up to 32 MP blend at full resolution into a decimating sink (box-average, no-data aware) under PREVIEW_MAX_PIXELS; above, the L8 coarse path with the row note "coarse preview: merge mode not shown" — cost if wrong: mid-size previews take longer (full-res blend of up to 32 MP).
- Final: Ruling: re-graded to the fix pass — empty-view "up to date" + dead Compare; plan frame gate gaps (quiet rect, go-to) and patches drawn before the ROI sync; invisible armed draw tool (crosshair + D reveals the ROIs section) and the keyboard-peek tooltip.
- Final: Ruling (declined-to-judge lines): fixed per-preview blend working set (~0.9 GB of L8 summaries for 12 panels) predates the branch — real-data smoke run recommended; composite priority order per the plan; Use A doesn't restore overrides (not the draft); drawing with ROIs hidden allowed; "up to date" while an analysis runs (stamp semantics); on_exit doesn't join previews (like jobs); LRU thrash needs ≥21 max patches; no keyboard rectangle drawing; go-to via Activate; duplicate names → duplicate accessible names — cost if wrong: small UX gaps.

### Deferred and parked minors

- Task 1: minor (parked): exactness test ROI covers nearly the whole canvas (no interior-only rect); cancel test hits only the pre-check, not CtlSink mid-blend.
- Task 2: minor (parked): LABEL_OFFSET/OVERLAY_STROKE/OVERLAY_DASH local consts moved, not theme tokens (pre-existing literals).
- Task 3: minor (parked): no focus-ring assertion / Tab-order test across actions; details drawn for every selected:true row (documented).
- Task 5: minor (parked): a failed preview only logs and the row keeps reading "out of date" (plan 6 problems could say why); PreviewKey::wanted re-parses draft/provenance per ROI×group per frame; DESIGN.md stale on draft_for and fresh-tile fractions → Task 8.
- Task 6: minor (parked): textures dropped for off-screen ROIs → 63–110 ms re-conversion gap when panning back; a mosaic composite member without an open source contributes nothing (accepted).
- Task 7: minor (parked): a canvas narrower than the compare control row clips Use A/Done (CANVAS_MIN_WIDTH makes it unlikely); wheel now zooms over the canvas's own overlay buttons (trivial).
- Final: minor (deferred): drawing/restoring an ROI cancels another ROI's in-flight preview until the sync lands; a finished, still-valid preview is dropped if its ROI is briefly off-screen.

Plan 4 notes (from the session record):
- CI after plan 3 + title fix (d52d435): ubuntu, windows, macOS all green. The plan 3 merge's first CI run (cf80629) failed on macOS in app::tests::title_names_the_session — a test that skipped the step after Enter; fixed (test-only).
- Plan 4 T4: while the plan's frame differs from the analysed frame (after a canvas-changing edit, until re-analysis), the plan hides ROI outlines/patches and can't draw ROIs — cost: no plan drawing in that window.
- User decision 2026-11-09: Compare A/B ships (plan 4 Task 7) — label controls, buttons first; A/B toggle; momentary peeks via Space, holding the selected button, holding on the preview; one ROI at a time.
- User decision round 2: all three peeks permanent; settings text on a second label row only in compare.
- Plan 4 T7 rulings: Space peeks only with no widget focused; Compare only on the selected/compared ROI; plan click in a compared preview doesn't select the panel; canvas overlay buttons never peek; 'B · out of date' when not merging; Use A shows A's pinned patches at once; label anchors at the ROI's visible top-left.
- Plan 4 final: partial analyses record their completed groups in provenance; analysed-but-unstamped groups preview — fixes stale "up to date" after a cancelled re-analysis.
- Plan 4 final: ROIs up to 32 MP preview at full resolution (decimated to ≤4 MP) honouring the merge mode; larger ones use the coarse 1/8 path with "coarse preview: merge mode not shown" — cost: slower mid-size previews.
- Plan 4 deferred minors: drawing an ROI restarts another's running preview; a finished preview of a briefly off-screen ROI is discarded. Declined-to-judge: per-preview L8 working set (~0.9 GB / 12 panels) — needs a real-data smoke run.
- Plan 4 merged (7e607ac). CI: ubuntu + windows green; macOS failed edited_notes_leave_room_for_the_scroll_bar (no notes text box after Edit) — second macOS failure of that test, both lock-race shaped. Fix 18ace0f: session_busy retries brief contention (250 ms, worker threads only); the test waits for Edit enabled + the text box with a diagnostic message.
- CI after the busy-probe fix (18ace0f): macOS failed a different test, pipeline::tests::user_cancelled_previews_stay_cancelled (test bookkeeping race); fixed test-only.

## Plan 5 — composites and the Export dialog

### Rulings

- Ruling R5-P1: an OSC member of a composite contributes the mean of its channels, in export and view alike (the plan's composite slot already does) — cost if wrong: an OSC member's colour is lost in a composite.
- Ruling R5-P2: a finished composite export stamps exported_at for each member group (the summary is per group) — cost if wrong: "Exported" may read true when only composites, not groups, were exported.
- Ruling R5-P3: Ctrl+E opens the Export dialog (spec §15 lists shortcuts "at least"; Export is a main action) — cost if wrong: one shortcut to remove.
- Ruling R5-P4: presets are offered only when every band resolves to distinct groups; a band claimed by two groups drops those presets (no guessing) — cost if wrong: a preset missing in an ambiguous session; Custom still works.
- Ruling R5-P5: exports get their own lane (one export at a time); a merge of a group being exported (or a member of an exported composite) waits; merges of other groups run — this replaces R-P4's single-slot interim — cost if wrong: CPU/disk contention between an export and a merge.
- Task 2: Ruling (for Task 3): composite ids must never be reused — persist a monotonic counter in ui (e.g. "composition_next_id") so a new composite never inherits a deleted one's id (stale saved stretch/view keys, Undo collisions); Undo restore of an id that's taken gets a fresh id — cost if wrong: an extra ui key.
- Task 3: Ruling: group_bands recognises group names only — default FILTER grouping names groups by their filter values, and no UI-thread I/O is allowed — cost if wrong: manually named groups ("Night 2") get no presets; Custom still works.
- Task 5: Ruling: an AUTOMATIC analysis (the pipeline's) waits for the export lane to drain instead of cancelling exports — spec §14 "settings changes during an export … never change what is being written", a user expects an export to finish — a user-started analysis may still cancel; cost if wrong: re-analysis after a panel/settings edit is delayed until exports end.
- Task 6: Ruling: no writability probe (no file created in the user's folder, not even a hidden temp) — check exists + is_dir + not read-only via metadata; an unwritable folder surfaces as the export's failure toast — cost if wrong: a failure discovered at export time instead of in the dialog.
- Task 6: Ruling: keep the previous destination check's marks and overwrite line visible until the new check answers (Export stays disabled meanwhile) — no layout jump — cost if wrong: one frame of stale marks.
- Task 6: Ruling: the collision message when the pattern already has {name} (a group and a composite sharing a name) reads "Two exports would have the same file name: rename one of them" — cost if wrong: copy.
- Task 6: Ruling (amended R1): read-only folder check on Unix only — Windows marks Documents etc. read-only and ignores it for directories; failures surface as the job's toast.
- Final: Ruling: every analysis (user or automatic) waits for the export lane and never cancels exports; Checks step "Analysis queued after export" while waiting — replaces "a user analysis may still cancel" — re-graded from Minor (a highlighted Analyse could kill a long export) — cost if wrong: analysis delayed until exports end.
- Final: Ruling: re-graded into the fix pass — composite weights normalised to 1 (view ≠ export otherwise); composite FITS drops FILTER (an HOO file claimed FILTER='Ha'); remembered export choices store unticked rows so newly ready rows start ticked (spec §14); Dialog Role::Dialog + title; + chip named "New composite" with expanded state; quoted pasted folder paths trimmed; DESIGN name; test temp dir.
- Final: Ruling (declined-to-judge lines): quitting the app mid-export leaves .part/.scratch with no prompt (pre-existing, §15 covers only ‹); .part/.scratch sibling names can clobber same-named user files (pre-existing core); rfd pickers on worker threads vs macOS main-thread dialogs (pre-existing pattern); hero numeric legacy names (deliberate, tested); toast Undo under the modal (untested, harmless); per-frame load_compositions (pre-existing); composite export in the CLI (out of scope) — cost if wrong: these remain as known issues for a later plan.

### Deferred and parked minors

- Task 1: minor (parked): stale member gets "different canvases" before "merge it again" (dialog only offers ready composites, so unreachable from the app); composite tests don't exercise summed stale/missing counts, no-data in an OSC mean, TIFF cancel keeping the destination; "different canvases" names no groups.
- Task 2: minor (parked): migration builds name moves for entries that already had ids (hand-edited mixed lists only); hero clones ui twice for migration.
- Task 3: minor (parked): bracketed/long filter forms ("[OIII]", "Oxygen") unrecognised (accepted); a group named "B"/"G" reads as Blue/Green (matters only with R,G,B all present); Composition::new now cfg(test).
- Task 4: minor (parked): Stroke::new(1.0) literal (no stroke-width token in the crate yet).
- Task 5: minor (parked): same-second stamps can read "Exported" after a quick merge (→ Task 7 docs note); a ready group without a blended_at stamp never reads Exported (CLI/legacy sessions, consistent with the start screen); transient wrong "Merge queued after export" label with ROI/previews running; ExportTarget also identifies blends (name).
- Task 6: minor (parked): export_dialog imports session_window helpers (module coupling).
- Task 6: minor (parked): the broken-picker fake panics (captured by libtest; resume_unwind would be quieter); a stray .part if the process dies after linking; no-hard-link filesystems keep a tiny check-then-rename window (documented).
- Task 7: minor (parked): a failing selftest export surfaces as the 60 s watchdog, not the export's error.
- Final: minor (deferred): merge_guard says "Waiting for the analysis to finish." while an analysis only waits for exports (same wording issue for merges); focus return can override a click/Tab in the single frame after a keyboard close.

Plan 5 notes (from the session record):
- CI on main after plan 4 + busy-probe + previews-test fixes (523050b): ubuntu, windows, macOS all green.
- Plan 5 T6 rulings: no writability probe in the user's folder (metadata only); previous destination marks stay until the new check answers; collision copy when {name} is already in the pattern.
- Plan 5 T6: exports never replace a file you didn't confirm — core no-replace (opt-in; CLI keeps replacing) checked before writing and just before the rename; the dialog counts running/queued exports' destinations as existing.
- Plan 5 final: every analysis waits for exports (none cancel them); composite weights normalised to 1; composite FITS drops FILTER; export dialog remembers unticked rows (new ready rows start ticked); Dialog role/name; + chip "New composite" + expanded; dialog returns focus on keyboard close.
- Plan 5 known issues (declined to judge, pre-existing): quitting mid-export leaves .part/.scratch with no prompt; .part/.scratch sibling names could clobber same-named user files; rfd pickers on worker threads (macOS prefers main thread).
- Plan 5 merged (f3fdb6c): CI ubuntu, windows, macOS all green on the first run.

## Plan 6 — checks, badges and seams

### Rulings

- Ruling R6-P1: area targets (coverage holes) carry `Check.rect` beside `target` (a hole isn't a panel or a pair) — cost if wrong: one extra optional field.
- Ruling R6-P2: the Seams overlay toggles with Shift+S (S is the Strong stretch) — cost if wrong: a shortcut swap.
- Ruling R6-P3: the seam checks use the feather of the group's last whole merge when merged, else the draft's — the residual seam describes the mosaic that exists; the level-matching note previews the draft — cost if wrong: a seam note computed with a slightly different feather.
- Ruling R6-P4: severities per the plan's Global Constraints (layout faults that make merging unreliable are Problems; per-panel quality cues are Notes; a residual seam after merging is a Problem) — cost if wrong: a severity flip.
- Ruling R6-P5: thresholds as named core constants with the plan's starting values (spec: "a starting point to refine over time") — cost if wrong: tuning on real data.
- Task 1: Ruling: HOLE_MIN_PANEL_FRACTION = 0.25 of the smallest panel's bbox accepted (a missing panel leaves a gap smaller than itself) — cost if wrong: tuning.
- Task 1: Ruling (for the fix round): ThinOverlap fires when the overlap's short side is below THIN_OVERLAP_PX AND below THIN_OVERLAP_FRACTION = 0.05 of the smaller panel's short side — proportionate for binned/small images, quiet on tiny synthetic test panels — cost if wrong: thin overlaps on small images reported later.
- Task 1: Ruling (for the fix round): new area/group-scoped checks (CoverageHole) key by group too (CheckKey gains #[serde(default, skip_serializing_if)] group) so accepting one hole doesn't accept every hole; existing checks keep their keys — cost if wrong: one more optional key field.
- Task 1: Ruling: the CLI passes Some(is_aligned(frame)) (no frame → solved, keeping check_lists_missing_distortion); only the app skips the distortion verdict while session.frame is None — cost if wrong: the CLI reports distortion before analysis as it always has.
- Task 2: Ruling: seam checks stay out of the CLI's `mmm check` (they need photometry + a full health pass + the feather policy) — cost if wrong: the CLI lists no seam checks.
- Task 2: Ruling (for the fix round): PanelGradient fits the plane on background cells only and fires when amplitude > GRADIENT_FACTOR × noise AND amplitude > GRADIENT_FRACTION (0.1) × |background level| (the second test drops out for a near-zero background, e.g. after gradient removal) — noise alone flags ordinary gradients in deep low-noise stacks and nebulosity — cost if wrong: thresholds need a real-data smoke run to tune.
- Task 2: Ruling: NoiseOutOfLine compares each panel with the median of the OTHER panels in its group (needs ≥1 other) — matches "noisier than the rest of {group}" — cost if wrong: a noisier pair partner flags.
- Task 5: Ruling: a seam line is warning-coloured + thicker exactly when it has an unaccepted Problem (after-merge ResidualSeam) — warning line ⇔ badge; accepted goes neutral like every accepted problem (spec §7.3 "flagged after a merge") — cost if wrong: an accepted seam drawn neutral.
- Task 5: Ruling: on the plan, analysed-frame overlays (seam lines, overlap/seam/area badges) follow frame_matches; panel badges from plan item rects always show — cost if wrong: overlap badges hidden while the plan's frame is behind.
- Task 5: Ruling: Session/Group-targeted problems get no canvas badge (they count on chips and in the Problems list) — cost if wrong: a group-wide problem lacks a canvas mark.
- Task 5: Ruling: panel outlines turn the problem colour only for Panel-targeted problems; DuplicatePanels / ThinOverlap (Overlap targets) badge at the overlap instead of outlining both panels (spec §7.4) — cost if wrong: duplicate panels lose their red outline (they keep a badge).
- Task 5: Ruling: overlap and seam problems of the same pair share ONE badge at the overlap centre (spec §7.4 "a badge at the overlap"; separate badges stacked on each other); target_rect still frames a seam by its lines — cost if wrong: one badge listing two kinds of problem.
- Task 6: Ruling: an empty click clears the selection on the plan only; on the mosaic the selection goes with Esc / another selection (the mosaic has no click-selection at all; adding one needs a mega-ui ViewerResponse change and must not fight compare's peek) — cost if wrong: one extra key press on the mosaic.
- Task 6: Ruling: opening a problem from the Problems / See all / panel lists selects and frames WITHOUT revealing the Panels section (keeps the reader's place in the list; Panels updates in place if open); a canvas badge click reveals Panels (the user asked "what is this?") — cost if wrong: one click to open Panels after a list open.
- Task 6: Ruling: a selected overlap or seam lists ALL its pair's checks (Place::badged() equal — overlap and seam together, matching the pair badge's count); the heading names what was selected — cost if wrong: a seam heading also lists the overlap's checks.
- Final: Ruling: the reviewer's "declined to judge" items stand as ruled: relative flag_steps rule and PanelGradient on nebulae are threshold tuning (R6-P5); Outlines over the mosaic for existing sessions is spec §7.3; panel badges on a frame-mismatched plan toast on click (ruling 3); no canvas badge for Session / rect-less Group problems; no seam checks in `mmm check`; no mosaic click-selection; notes have no Accept (pre-existing); the seam verdict is a prediction from analysis data (spec: "from seam health") — cost if wrong: each is a later refinement, none blocks a user.
- Final: Ruling: deviate from "one fix pass" for one residual — coincident composite badges merged within COINCIDENT_PX = 8 px, too tight for real registered panels of different filters (tens of px apart) → merge by same-pointing IoU ≥ 0.5 instead (resumed the fix implementer; a scoped re-review covers both) — shipping a fix that fails on real data is worse than one small extra round — cost if wrong: one small extra round.
- Final: Ruling: ROI go-to keeps edge-to-edge framing (only problem opens and Overlaps rows get the margin) — ROIs are places the user drew to fill the view — cost if wrong: one call-site change.
- Final: Ruling: a badge's accessible name names its place(s); its description/hover is the problem titles (replaces "{n} problem(s) here") — accessibility: identical announcements told the user nothing — cost if wrong: wording.

### Deferred and parked minors

- Task 1: minor (parked): 4-connectivity reports a gap open only through a corner touch as enclosed; GroupCountMismatch/Ungrouped/FiltersDifferUngrouped target Session (defensible).
- Task 3: minor (parked): dead unwrap_or fallback in run_checks; no direct merged-feather test; shared "Analyse again" wording for all seam-measure failures.
- Task 4: minor (parked): an entirely off-screen badge isn't keyboard-reachable (list rows are the keyboard route); pointer over a badge doesn't hover the canvas (untested).
- Task 5: minor (parked): the window copies its small marks list to the plan each frame (kept for badge clicks); plan paints panel outlines over ROIs while the mosaic paints them under (pre-existing plan order).
- Task 6: minor (parked): Un-accept's refresh of counts/badges in the real shell is covered only at the engine level (the harness records out.unaccept without applying it).
- Task 6: minor (parked): the Overlaps-row no-reveal path isn't asserted directly (Panels is already open there); Places refactor / muted "No other panel" / single file_name have no fail-if-undone tests (refactor/cosmetic); the ring's view check resolves group members each frame (cheap at this size).
- Final: minor (deferred): the Merge settings Health grid uses the committed feather while checks use the draft's — the two can disagree while tuning an unmerged group.
- Final: minor (deferred): ThinOverlap fires on incidental corner overlaps between diagonal neighbours (judged per graph edge) — confirm on real data; consider essential links only.
- Final: minor (deferred): the seam cache keys on the group's analysis stamp — files changed outside the app without a new analysis aren't noticed until the next analysis; errors / no-photometry results are retried every run (not cached).
- Final: minor (deferred): a merged badge keeps its first place's rect — if that rect fails to resolve while a coinciding one's does, the badge isn't drawn (needs a seam problem on non-overlapping panels with no seam lines; practically unreachable).
- Final: minor (deferred): problem_marks runs per frame with O(places × marks) small clones in coincide (fine at realistic counts).
- Final: minor (deferred): hole keys accepted on this branch before the per-hole key no longer match (no released session affected — CoverageHole is new in plan 6).

Plan 6 final fix pass — what superseded earlier rulings:
- A coverage hole's check key includes the hole's rect rounded outward to L8
  cells (`CheckKey.rect`), so accepting one hole accepts only that hole
  (supersedes Task 1's group-only key).
- Session-targeted problems, and Group-targeted ones without a rect, get no
  canvas badge; a coverage hole (Group target with a rect) badges as an area
  (refines Task 5's "Session/Group-targeted problems get no canvas badge").
- Badges of the same kind at one pointing merge across groups: panels whose
  analysed bboxes have IoU ≥ `SAME_POINTING_IOU` (0.5); pairs when both
  members match; areas by rect IoU.
- Opening a problem frames it with the plan's zoom margin; a selected panel
  is ringed on the mosaic; a stale area target does nothing.
- The chip's problem count uses the problem icon (not the warning triangle
  of "Out of date").
- Checks during Merge all run once after the last merge; each group's seam
  result is cached by analysis stamp, members, feather and merged state.
