# The in-session experience (project 3) — design

Date: 2026-11-08. Follows `2026-11-07-ui-standard-and-start-screen-design.md`,
whose §7 ("Context for project 3") this design takes up. It replaces the
session window's Panels, Plan, Blend and Export tabs with one session window.

## 1. Intent

mmm's engine works, and the start screen and the `mega-ui` standard have given
the app its look. What happens inside a session has never been designed: it
grew as four tabs that mirror the engine's stages (Panels → Plan → Blend →
Export). This design starts again from what a mosaic imager does, aiming for
**clarity and simplicity**: the happy path is clean and quick, and the detail
(every check, every setting, every panel's facts) is always within reach but
never in the way.

The user's own path, which the design must make effortless:

1. Create a session (on the start screen).
2. Load the panels: usually one folder, often already open in Explorer, so
   dragging files or the folder onto the window must work.
3. Check the plan: every panel in its place, previews filling in as they're
   read. Missing or duplicate panels are obvious from the layout, and so is
   "the full horror" of mismatched gradients and sky levels.
4. If the plan looks right and nothing worth fixing has been flagged, merge.
   On a big mosaic, first merge just a region to test the settings; someone
   reworking a familiar mosaic skips this.
5. Merge the whole mosaic and inspect it: seam artefacts (janky stars or star
   spikes, hard edges in nebulosity, abrupt changes in colour or noise across
   a seam) and gradients (residual gradients in panels, how well gradients
   match across seams). Seams must be easy to show and hide. If something is
   wrong, try other settings on a region over it until it's fixed, then merge
   the whole mosaic again.
6. Export, ready for downstream processing.

A one-shot-colour (OSC) mosaic (e.g. the Orion test data: 12 colour panels,
no grouping) is the simple and very common case. A narrowband mosaic adds:
grouping panels by filter as they're loaded, checking the plan per group,
**composites**, merging per group, and exporting groups and/or composites.

**Composites are unique to mmm** and users will love them: a colour view (SHO,
HOO, RGB…) of the mosaic long before channel combination, which is both
instant gratification and an early warning of colour casts, channel
misalignment and gradients that would hurt combination downstream.

## 2. Goals, non-goals, anti-goals

### Goals

- One session window, one canvas: the plan and the mosaic are two views of
  the same canvas, not two places.
- The happy path needs no decisions beyond dropping files, glancing at the
  plan, pressing **Merge** and pressing **Export…**.
- Data-quality problems are first-class and in line: as visible as the
  panels, ROIs and settings, never in a pop-up.
- Trying merge settings is a tight loop: change a setting, see the regions
  you're looking at re-merge in a second or two.
- Groups and composites are obvious and one click apart whenever there are
  groups, and invisible when there aren't.
- Results appear in seconds: outlines first, then previews, then the mosaic
  filling in as it merges.
- The app's language says **merge**, never "blend" (brand consistency).

### Non-goals

- The pre-merge processing stage (gradient removal, colour correction): it is
  needed to fix colour biases and residual gradients across the mosaic, but
  it is a later project. This design only leaves its slot (§16).
- Export format options (bit depth, compression, …). XISF export waits for an
  XISF writer in mpp-core, as before.
- ROI thumbnails (they would come from merges that aren't kept up to date).
- Changes to the start screen, the CLI's behaviour or the PixInsight module.
- Account login.

### Anti-goals (failure even if it technically works)

- A wizard: steps you must do in order, or can't go back to. You can add
  panels after merging; the session works out what is stale.
- Plan and merge as separate places, so that "where do I draw a test region?"
  has no good answer.
- Problems shown in modals, or tucked away where they're found late.
- A panel list that looks like a spreadsheet, or one built from tiles you
  have to read across (the current Panels tab's tiles-in-group-columns was
  hard to use).
- Groups hidden behind a mode switch the user has to discover.
- ROI test merges that touch, invalidate or overwrite the mosaic.
- Settings that are hard to find (the user's flagged concern; see §11).
- Advertising composites. They must be easy to discover, not pushed.

## 3. Constraints

- Everything in `CLAUDE.md` holds: linear data end to end, zero is the no-data
  sentinel, never dense over the canvas, per-channel maths for colour, session
  directories hold all cached analysis, tests synthesise their inputs (never
  `test_data/`), GUI runs only through `--selftest`.
- The `mega-ui` standard: every colour, size, radius and duration comes from
  `mega_ui::theme` tokens; components that aren't mmm-specific belong in
  `mega-ui`, which must not depend on `mmm-core`; egui/eframe 0.36; AA text
  contrast; focus rings; reduced motion respected; the gallery and snapshot
  tests cover new components. Notifications are toasts only.
- Engine facts the design relies on (`docs/DESIGN.md`):
  - Analysis (overlap graph and photometric solve) must precede any merge. It
    takes seconds to tens of seconds for 12 large panels. A merge never
    re-analyses.
  - An ROI merge of about 2000 × 2000 px takes about 1–2 s. A full-resolution
    merge takes seconds to tens of seconds.
  - Results live in a per-group tile store that the viewer reads while a merge
    writes it, which is what lets the mosaic fill in on screen.
  - Previews (at most 512 px per panel) and solved panels' outlines from
    headers already exist (`mmm_core::preview`). An aligned panel has no
    header-only outline today.
  - The checks engine (`mmm_core::checks`, `crates/mmm-app/src/checks.rs`) and
    "accepted" checks in `mosaic.json`; seam health after a merge
    (`mmm_core::health`: seam steps, flagged when well above the median).
  - Settings today: merge (mode Pyramid/TwoBand/Feather, feather width, defect
    veto, flatten) and analysis (input Auto/Aligned/Solved, surface, gain).
  - Compositions exist (`crates/mmm-app/src/compositions.rs`: a name and three
    channels, each an optional group with a weight), drawn as additive layers
    tinted R, G and B.
  - Export writes linear 32-bit float FITS (WCS and provenance) or TIFF.
  - ROIs are already saved in the session (`ui.rois`: name, note, sky
    corners, canvas rect).

## 4. Vocabulary (what the UI says)

- **Panel**: one input file (a stacked master).
- **Plan**: the panels shown in place on the canvas, as loaded, before
  merging (mismatched levels and all).
- **Merge** (verb and noun): combining panels into the mosaic. Never "blend"
  anywhere the user can see it (labels, tooltips, progress, errors, the
  selftest's visible strings). Code identifiers may keep `blend`.
- **Mosaic**: the saved result of a full merge (per group). **Out of date**
  when its panels or the analysis have changed since it was merged: it must
  be merged again before it can be exported. **Settings changed** when the
  merge settings now showing differ from those it was merged with: it is
  still a complete mosaic and can be exported (§11).
- **Group**: the panels of one filter (or whatever the grouping rule
  produced). Each group is merged into its own mosaic.
- **Composite**: a named colour view built from up to three groups in the R,
  G and B channels. It is a way of looking at mosaics and an export
  convenience. A composite is never merged itself; its groups are.
- **ROI** (region of interest): a named rectangle on the sky, as on the
  Astrometrical website. Used to jump to an area and to preview how the
  current settings merge it.
- **Check**: one data-quality test. A **problem** is a check that found
  something likely to hurt the result; a **note** is worth knowing; a passed
  check found nothing. Problems can be **accepted** (they then stop counting).
  System errors (an unreadable file, a full disk) are not checks: they're
  toasts.

## 5. The session window

```
┌──────────────────────────────────────────────────────────────────────────────────┐
│ ‹  Orion SHO   (✓ 36 panels)(⚠ 2 problems)(Merge)(Export)      [Merge · Pyramid] │ top bar
├───────────────────────────────────────────────────────────────┬──────────────────┤
│ [Plan|Mosaic] (Hα·12)(OIII·12)(SII·12)(SHO)(+)                 │ PROBLEMS       2 │
│                          Outlines  Seams  ROIs  Stretch ▾  ◧   │  ⚠ Hα 3: gradient│
│                                                               │  ⚠ OIII 1–2: thin│
│                                                               │  See all checks ›│
│                         canvas                                │ PANELS        36 │
│                                                               │ ROIS           3 │
│                                                               │ MERGE SETTINGS   │
│                                                               │ COMPOSITES     1 │
│                                                               │ NOTES            │
└───────────────────────────────────────────────────────────────┴──────────────────┘
```

- **Top bar**: **‹** (back to the start screen, §15), the session name, the
  **step strip** (§6) and the **main button**: **Add panels…** while the
  session is empty, **Merge** while any group's mosaic is missing or out of
  date or the merge settings have changed since it was merged, otherwise
  **Export…** (Export is always reachable from its step). While the main button reads **Merge** it carries a one-line
  summary of the merge settings ("Merge · Pyramid · feather 64 px"); the
  summary is a separate small target that opens Merge settings (§11).
- **Canvas** (§7), with its view bar: Plan / Mosaic, the group and composite
  chips ending in **+**, and the overlay toggles.
- **Inspector**: a column on the right with sections in a fixed order:
  Problems, Panels, ROIs, Merge settings, Composites, Notes. Section headings
  always show (with counts where they apply). Only their contents fold, so
  nothing is ever more than one click away. The whole inspector **folds away**
  (a button on its edge and a keyboard shortcut) so the canvas gets the full
  width. Folded, the step strip still shows the problem count, so nothing
  important disappears.
- **Panel list** (§10): opens as a wide column on the left of the canvas
  (list | canvas | inspector) while adding and grouping panels.
- A new, empty session opens with the panel list open as one big drop target
  (with **Add files…** and **Add folder…**). Files or folders dropped anywhere
  on the window are added, whatever is showing.

## 6. The step strip: where you are, and work in progress

A row of steps in the top bar: **Panels · Checks · Merge · Export**. It is not
a wizard: every step is always clickable and nothing forces an order. It
does three jobs.

1. **Status.** Each step shows its state:
   - Panels: "Add panels" (none), "✓ 36 panels".
   - Checks: "Checks" (not yet run), "⚠ 2 problems", "✓ Checks passed".
   - Merge: "Merge" (next), "✓ Merged", "Out of date", "Settings changed".
   - Export: "Export", "✓ Exported".
   The step that is the natural next action is highlighted (accent border).
2. **Progress.** The step doing background work fills like a progress bar and
   says what it's doing: "Reading previews 21 / 36", "Analysing…", "Merging
   OIII · 2 of 3 · 43 %", "Exporting 4 of 5". Hovering a working step offers
   **Cancel** (an ✕). Automatic work (reading outlines and previews, analysis,
   checks) needs no button and starts by itself, as the pipeline does today.
   ROI merges are *not* shown here; each ROI shows its own progress on its
   label (§8).
3. **Navigation.** Clicking Panels opens or closes the panel list. Clicking
   Checks opens the inspector at Problems. Clicking Merge merges (as the main
   button does). Clicking Export opens the Export dialog.

## 7. The canvas

### 7.1 Plan or mosaic

The canvas shows either the **plan** or the **mosaic** of the current group
or composite.

- With no mosaic yet, it shows the plan.
- Once a mosaic exists, it shows the mosaic by default.
- **Plan / Mosaic** (a segmented control) switches between them whenever
  both exist. Mosaic is disabled (with a tooltip) while there is none.
- **Pressing Merge switches the canvas to the mosaic** so the user sees where
  their merge went, and watches it fill in tile by tile as it is written
  (results in seconds).
- An out-of-date mosaic still shows, marked as out of date (dimmed, with an
  "Out of date" tag in the canvas corner that names why: panels added,
  analysis settings changed…). Merge re-merges it. A mosaic whose merge
  settings have changed since it was merged shows normally with a quieter
  "Settings changed since this merge" tag that opens Merge settings.
- The camera (zoom and position) is shared by plan and mosaic and by all
  groups and composites: switching any of them never moves the view.

### 7.2 Groups and composites

- When the session has groups, the view bar shows one chip per group
  (coloured with its group colour, with its panel count), then the composites
  (marked as composites, e.g. a dashed outline), then **+** (§12). The chips
  pick what the canvas shows. With one group (an OSC session) no chips show.
- A chip shows the group's or composite's state at a glance: a problem badge
  when that group has problems, and an out-of-date mark.
- A group chip's context menu offers **Merge this group**.
- Previous / next group has a keyboard shortcut.

### 7.3 Overlays and display

Toggles in the view bar, each available over both plan and mosaic:

- **Outlines**: the panels' outlines (with numbers, as now).
- **Seams**: where the merge transitions from one panel to the next. A seam
  the seam-health check flagged after a merge is drawn in the warning colour.
- **ROIs**: one toggle shows or hides all ROIs (outlines, labels and their
  merged previews).
- **Display stretch** (display only; the data stays linear). Always
  automatic, simplified for this version to two toggles, with no manual
  modes and no per-group settings:
  - **Linked / Unlinked** (default **Unlinked**): whether colour channels
    share one stretch or each get their own. It matters only for colour views
    (OSC groups and composites); for a mono group it shows greyed out with a
    tooltip, so the controls never shift. Unlinked hides colour casts, so
    Linked is the one to use when checking a composite's colour.
  - **Normal / Strong** (default **Normal**): Strong exaggerates faint
    gradients, so mismatched gradients and seams stand out.
  - The stretch is worked out again when what's shown changes (group,
    composite, plan or mosaic) and when the data settles (previews done, a
    merge completed). While a mosaic fills in during a merge, the plan's
    stretch holds, so the image doesn't pump; it updates when the merge
    completes.
  - Its statistics come from low-resolution data (previews, pyramid levels),
    never a dense pass over the canvas, and ignore zero (no data).
  - ROI previews are drawn with the stretch of the canvas beneath them and
    never change it, so a patch compares fairly with its surroundings.
  - Both toggles belong to the session, not to a group, and are restored on
    reopen. Per-group stretches saved by older versions are ignored.

### 7.4 Pointing at things

- Hovering a panel on the canvas highlights its row in the panel list (if
  open), and the reverse.
- Clicking a panel selects it. Its details show in the inspector's Panels
  section: path, **Show in folder**, **Copy path**, header facts, the panels
  it overlaps and its checks (with **Accept**).
- A problem tied to a panel puts a warning badge on that panel's outline. A
  problem tied to an overlap or a seam puts a badge at the overlap. Badges
  carry an icon and a count, not colour alone.
- Clicking a problem in the inspector zooms the canvas to it and selects it.

## 8. ROIs

ROIs do two jobs: **places to go** (as on the Astrometrical website) and
**previews** of how the current merge settings merge that area. Both outlive
a run of mmm, so ROIs are saved in the session (the existing `ui.rois`, with
its sky anchoring).

### 8.1 The list

- The inspector's ROIs section is an **ordered list**. Each row shows the
  label ("Region 1", "Region 2"… until renamed) and its state (merging, up to
  date, out of date). No thumbnails.
- Row actions: **Go to** (zooms the canvas to it; also a click on the row),
  rename (double-click or an edit action), reorder (drag, plus move up / down
  for keyboard users), delete (with Undo in a toast). The existing ROI note
  stays, edited in the row's details.
- **Draw ROI**: a button in the section and a keyboard shortcut. You drag a
  rectangle on the canvas (plan or mosaic). The new ROI is added to the end
  of the list and selected.
- ROIs belong to the session, not to a group: the same rectangles show over
  every group and composite.
- The list's look and behaviour is shared with the composites list (§12): one
  reusable ordered-list component in `mega-ui`.

### 8.2 Previews

- An ROI's **preview** is its area merged with the **current settings**,
  drawn over the canvas in place, over plan or mosaic alike.
- Previews are **transient**. They never touch, invalidate or overwrite the
  mosaic or its tile store. They live only as long as the app runs (in
  memory or session scratch space) and are rebuilt when needed. This changes
  today's behaviour, where an ROI merge writes into the group's result and
  invalidates the merge.
- A preview is **dirty** when the settings, panels or analysis it was merged
  with have changed, or when it has never been merged for the group in view.
- **Auto-merge of visible ROIs.** While ROIs are shown, a dirty ROI that is
  in view on the canvas is merged by itself: a newly drawn ROI, every visible
  ROI after a settings change, an ROI panned or zoomed into view. ROIs out of
  view, or hidden by the ROI toggle, are not merged until they come into view.
- **Priority.** The group in view merges first. For a composite, each member
  group's patch merges, the channel in view first, so the preview updates as
  fast as possible. Among several visible ROIs, the one nearest the centre of
  the view goes first.
- **Cancel and restart.** If settings change while a preview merges, that
  merge is cancelled and restarted with the new settings: the user can change
  settings faster than merges finish without a queue building up.
- An ROI shows its own progress on its label ("Trapezium · merging…").
- A preview of an ROI over an out-of-date mosaic shows what the current
  settings would give, which is the point: it's how a fix is tried before
  the whole mosaic is merged again.

### 8.3 Compare A / B

Comparing a preview with the previous settings is useful, but "before" is
ambiguous when previews re-merge as settings change. The user tried a
mock-up (2026-11-09) and decided it ships in this version:

- An ROI's canvas label has **Compare**. Entering compare freezes the
  current preview as **A** with the settings it was merged with. **B** keeps
  updating live as settings change.
- **A / B** on the label is a **toggle**: the selected side stays shown.
- A **momentary switch** shows the *other* side only while held (from A it
  peeks at B, from B at A). Three, all built so the user can keep the ones
  that feel right on real panels:
  - holding **Space**;
  - pressing and holding the **selected** A / B button (clicking the other
    button switches to it);
  - pressing and holding the left mouse button on the preview (a drag still
    pans).
- **Use A** restores A's settings into the draft. **Done** leaves compare and
  keeps the draft.
- The label's controls keep a stable position: the buttons come before the
  settings text (the user's first suggestion; the other, the settings text
  on a second row, can be tried in the mock-up and swapped in if preferred).
- One ROI in compare at a time.

## 9. Problems and checks

- **Three places, never a pop-up.** The problem count in the step strip; a
  badge where the problem is on the canvas (§7.4) and on the group's chip;
  and the inspector's Problems section, listing each problem with its text
  and, where there is one, its remedy and **Accept**. Clicking one goes to it.
- **See all checks ›** at the end of the Problems section drills down to
  every check, passed ones included, grouped by severity (problems, notes,
  passed) and showing which are accepted. Accepted problems can be
  un-accepted there.
- Problems are advisory. They never block Merge or Export; only the engine's
  own needs do (no analysis yet, a store held by a running job).
- **The check set** is a starting point to refine over time. Existing checks:
  missing file, file changed since added, no astrometric solution,
  unsupported distortion model, pixel scales differ, colour and mono mixed,
  filters differ but not grouped, panels in no group, groups with different
  panel counts, a panel overlapping no other, sky levels differing strongly.
  New checks:
  - **Duplicate panels**: the same file twice, or two panels covering nearly
    the same sky.
  - **Holes in coverage**: a gap surrounded by panels (a missing panel).
  - **Thin overlaps**: too narrow to match the panels reliably.
  - **Strong gradient inside a panel** (it may need gradient removal before
    merging).
  - **Noise out of line**: a panel much noisier than its group (shorter
    integration, a bad night).
  - **Poor level matching**: after analysis, overlaps still disagree, so a
    seam is likely.
  - **Ragged stacking edges or large empty areas** inside a panel.
  - **Residual seam after merging** (from seam health).
- **Pre-aligned panels have no distortion model by design.** Panels
  registered by, e.g., PixInsight's MosaicByCoordinates were undistorted
  during alignment, so in aligned input mode a missing distortion model is a
  **passed** check reading "No distortion model: accepted for pre-aligned
  panels", not a problem.
- System errors are toasts (`mega_ui::widgets::Toasts`), not checks.

## 10. Panels and grouping

### 10.1 Adding panels

- **Add files…**, **Add folder…**, and drag and drop of files or folders
  anywhere on the window. Duplicates (the same path) are ignored, as now.
- A folder (added or dropped) contributes the supported image files at its
  **top level only**, never its subfolders: a WBPP output folder also holds
  hundreds of calibrated and registered frames that aren't panels. For
  per-filter subfolders, the user selects those folders and drops them
  together.
- **Two phases, outlines first.** Phase 1 reads only what's needed to place
  each panel (its outline) and draws all the outlines on the plan as soon as
  possible. Phase 2 reads the image data and makes previews, which fill in
  panel by panel as they're ready. Analysis and checks follow by themselves.
  An aligned panel (no WCS of its own) gets its outline as early as a cheap
  read allows.

### 10.2 The panel list

Opened from the Panels step (and open by itself in an empty session), it
sits on the left of the canvas with the plan beside it, so hovering a row
lights up that panel's outline and the reverse. That's how duplicates and
missing panels show.

```
 [Add files…] [Add folder…]  or drop them anywhere        Group by [FILTER header ▾]
 PANEL                     INTEGRATION   SCALE   FILTER
 ▾ No group · 1 — FILTER missing
   Orion_P7_300s.xisf          3h 10m    1.21″   —         Move to ▾
 ▾ ● Hα · 12 · 38h 20m
   Orion_Ha_P1.xisf            3h 05m    1.21″   Ha
   Orion_Ha_P3.xisf ⚠          1h 15m    1.21″   Ha
   … 10 more
 ▸ ● OIII · 12 · 40h 00m
 ▸ ● SII · 11 · one fewer than Hα
```

- **A section per group.** Each group is a heading row: its colour, name,
  panel count, total integration, and any mismatch with the other groups
  ("one fewer than Hα"). Sections fold.
- **Rolled up by default.** A section shows its first few rows, then "… N
  more", which opens the whole group. Panels with a problem always show, even
  in a rolled-up section. "No group" sits first, never rolls up, and goes
  away when empty.
- **A table, not a spreadsheet.** Columns line up for scanning (panel name,
  integration, pixel scale, filter, size, channels, solution; the builder
  picks the final set from what headers reliably give, showing "—" when
  unknown). No grid lines, roomy rows, quiet column headings in the overline
  style, numbers in the mono face, a ⚠ on rows with problems (hover for the
  text). Row actions appear on hover or focus. Sortable within sections.
- **Fixing groups.** **Move to ▾** on a row, or drag rows onto another
  group's heading. **Multi-select** (click, Shift-click, Ctrl-click) moves
  several at once. If multi-select makes the list convoluted it can wait,
  but plain standard selection shouldn't. A panel moved by hand carries a
  "by hand" tag, and its FILTER value stays visible, so a header that says
  Hα on a panel in OIII explains itself. Manual assignments survive rule
  changes, as now. **New group** creates an empty group to move panels into.
- **Removing panels**: a row action and Delete on a selection, with Undo in
  a toast. Adding or removing panels after a merge re-runs the analysis by
  itself and marks **only the affected groups'** mosaics out of date, so long
  as the shared reference frame doesn't move (always the case with
  pre-aligned panels). If the change moves the frame (a solved panel that
  grows the canvas), every group is re-analysed and goes out of date, as
  today. This is new engine work (per-group invalidation, detecting whether
  the frame moved, analysing only the affected groups) and is planned as its
  own step.
- An OSC or single-filter session shows the same list with no sections.

### 10.3 Group by

There is no "group mode". A **Group by** control above the list chooses the
rule:

- **Don't group**: one mosaic.
- **FILTER header** (or another header keyword).
- **Path keyword…**: a keyword found in the file path, whose value is the
  next token in the path. Tokens are separated by spaces, underscores,
  dashes and path separators. Match what PixInsight's WBPP does for its
  grouping keywords, hardened (case, folder vs file name, a keyword with no
  value).
- **By hand**: groups created and filled manually.

When the panels carry more than one FILTER value, mmm sets **Group by FILTER
header** by itself (one click undoes it). Panels the rule can't place go to
"No group", which raises the existing "in no group" problem.

## 11. Merge settings

- **All settings show**, merge and analysis together, because there are only
  seven. An "Advanced" fold comes only if the list grows.
- **Merge settings are the ones tried interactively**: mode, feather width
  and flatten (defect veto rarely). They come first.
- **Analysis settings** (input, surface, gain) matter before the first full
  merge and rarely after. Each says beside it what changing it costs once a
  mosaic exists ("Re-runs the analysis; the mosaic goes out of date").
- **Merge settings are a draft until Merge.** There is no Apply button: a
  change takes effect in the ROI previews at once (§8.2), but the settings
  saved in the session (`mosaic.json`) change only when **Merge** is
  pressed, and the mosaics are never invalidated by a settings edit.
  - A setting changed back by hand, or by **Undo**, leaves nothing to
    re-merge: "Settings changed" is worked out by comparing the draft with
    the settings each group's mosaic was merged with (its provenance), not
    from tile state.
  - A stray click never forces a re-merge, and never blocks export.
  - A merge already running keeps the settings it started with; a draft
    changed meanwhile shows as "Settings changed" once it finishes.
  - The draft is kept in the app state (`ui`), so closing and reopening
    without merging keeps it.
  - Merging commits the draft. Committing it (including for **Merge this
    group**) must not invalidate other groups' finished mosaics: they show
    "Settings changed" and stay exportable. The CLI's behaviour (`mmm set`,
    `mmm run`) stays as it is.
  - The summary file the start screen reads takes "re-merge needed" from the
    same comparison.
- **Analysis settings take effect at once**, as today, because previews
  can't use them without re-running the analysis: the analysis re-runs
  (debounced, as the pipeline does for input edits), ROI previews wait for
  it, and the mosaics go out of date. **Undo** in a toast restores the
  previous values (which re-runs the analysis again).
- The accent dot (a field differing from what the last merge used) stays.
- **Findability** (the user's flagged concern): the Merge settings heading
  always shows in the inspector; the section opens by itself when analysis
  first finishes (the next decision is merging); the main button's settings
  summary opens it; the canvas tooltip on an out-of-date tag links to it.
  Mock it up early and check it with the user.

## 12. Composites

- **Make one**: the **+** at the end of the group chips (shown only once
  there are two or more groups). It opens a small popover:
  - **Presets first**, when the groups are recognised as R, G, B, Hα, SII or
    OIII (from their names or filters): RGB, SHO, HOO, HSO, and any others
    the recognised groups allow. One click creates the composite.
  - **Custom**: a name, then a group (or none) for each of **R**, **G** and
    **B**.
- From the user's point of view a composite then behaves like a group: a chip
  that picks the canvas view (plan or mosaic), ROI previews (each member
  group merged), and an Export entry. Underneath, nothing is merged for a
  composite.
- **Managing composites**: the inspector's Composites section lists them
  with the same component and behaviour as ROIs (order, rename, edit
  channels, delete with Undo).
- A composite's mosaic view is ready when all its member groups' mosaics
  are. Until then, a member not merged yet adds nothing and is named above
  the image, as now.
- Composites must be easy to discover but not advertised: the **+** chip is
  the whole invitation (no banners, no prompts).
- The existing per-channel weight stays at 1 and isn't shown.

## 13. Merging

- **Merge** (main button or the Merge step) commits the draft merge
  settings (§11) and merges every group whose mosaic is missing, out of
  date, or merged with other settings, one after another. **Merge this
  group** is on a group chip's menu.
- Adding or removing panels while a merge runs cancels it (its result would
  be out of date anyway), and a toast says so.
- The canvas switches to the mosaic (§7.1). Progress is in the step strip;
  cancel there.
- Merging never waits for problems to be fixed or accepted.
- A merge pressed while an export runs is **queued** and starts when the
  export finishes; the strip says so ("Merge queued after export").

## 14. Export

- **Export…** opens the **Export dialog**, the one place a modal is right.
- It lists the session's **N groups and M composites**, laid out so their
  numbers are plain. Each one with a complete mosaic that isn't out of date
  (for a composite: all its member groups) can be ticked; "Settings changed"
  doesn't stop it. The others show greyed out with the reason ("out of
  date", "not merged"). Everything ready starts
  ticked, so the common case is one click. (User feedback may refine this
  later; users may prefer all groups or all composites, rarely both.)
- Choices:
  1. **Folder.**
  2. **Format**: FITS or TIFF (linear 32-bit float, with no format options).
     XISF joins when mpp-core can write it.
  3. **File name pattern** with tokens, at least the group or composite name
     (needed when exporting several) and the session name. Default
     `{session}_{name}`. A **live preview** lists the exact files that will be
     written.
- **Overwrite warnings are critical**: files that already exist are marked in
  the preview, and exporting over them takes an explicit **Overwrite**.
- A **composite exports as one three-channel colour file** built from its
  groups' mosaics, with the same WCS and provenance as a group export.
- The session **remembers** the folder, format, pattern and selection.
- Exports run **in the background** with progress in the step strip and
  **Cancel**. They lock out as little as possible: only a merge of a group
  being exported waits (queued, §13). ROI previews, settings, browsing, ROIs
  and notes stay live. Settings changes during an export only mark the mosaic
  out of date; they never change what is being written.

## 15. Around the session

- **‹** returns this window to the start screen in place (it releases the
  session's lock). This was deferred to project 3 by the start-screen
  design. While an export runs, ‹ is disabled, its tooltip says why, and the
  export's Cancel is in the step strip. A running merge, analysis or ROI
  preview is simply cancelled on leaving: all of them can be run again.
- **Notes**: an inspector section using `mega_ui::widgets::notes_view`, the
  same notes the start screen shows.
- **Reopening restores the view**: plan or mosaic, the group or composite,
  the camera, the inspector's fold state and open sections, whether the panel
  list is open, the overlay toggles and the stretch toggles. Saved in `mosaic.json`'s
  `ui` with the rest of the app state.
- **Undo in a toast** for removing panels, deleting ROIs and composites, and
  analysis-setting changes. (Merge-setting changes need none: they're a
  draft, §11.) No "are you sure?" dialogs.
- Keyboard: shortcuts exist at least for folding the inspector, toggling each
  overlay, Plan/Mosaic, previous/next group, Draw ROI and Merge, and appear in
  tooltips.

## 16. Room for later: processing

A pre-merge processing stage (gradient removal, colour correction) will sit
between grouping and merging. In this design it would become a step between
Panels and Merge in the step strip, and an inspector section. Nothing in
this design should make that hard (e.g. the strip is a list of steps, not
four hard-coded ones).

## 17. Decided by the user

- One canvas for plan and mosaic (option A of the mockups), not separate
  Plan and Merge tabs, nor side-by-side views.
- Plan shows by default until a mosaic exists, then the mosaic. Switchable
  when both exist. Pressing Merge switches to the mosaic.
- ROIs: session-level, saved, an ordered list without thumbnails, one
  show/hide toggle, transient previews merged with the current settings,
  auto-merge of dirty ROIs in view, priority to the group or composite in
  view, cancel and restart on fast settings changes.
- Compare A/B ships (mock-up tried 2026-11-09): controls on the canvas
  label, A/B a toggle, momentary peeks at the other side (Space, holding the
  selected button, holding on the preview), one ROI at a time (§8.3).
- The inspector layout (option A), foldable. Problems sit alongside panels,
  ROIs and settings. "See all checks ›" ends the problems list. Settings
  findability is a focus area.
- The **+** after the composite chips.
- Merge settings are the interactive ones (mode, feather width, flatten).
  Analysis settings matter before the first merge. All settings show.
- The check list in §9 as a starting point. Pre-aligned panels with no
  distortion model pass that check.
- Composites: name, R/G/B channels, presets, managed like ROIs, a
  visualisation and export convenience only, exported as 3-channel files.
- Export: a modal dialog; ready items ticked by default; folder, format,
  pattern; live file name preview; overwrite warnings; remembered choices;
  background with progress and cancel.
- Panel list: sections per group (option A) beside the plan, rolled up by
  default with "… N more", multi-select (low priority if convoluted).
- Grouping: by FILTER header, path keyword (WBPP-like) or by hand; mmm may
  group by FILTER itself.
- Progress in the step strip; ROI progress on the ROI.
- Merge settings are a draft until Merge; undoing a change leaves nothing to
  re-merge; a settings difference never blocks export; analysis settings
  take effect at once.
- Adding or removing panels re-analyses and re-merges only the affected
  groups when the shared frame doesn't move.
- Add folder and dropped folders take top-level image files only.
- "Merge", never "blend", in the UI.
- The display stretch is automatic, with only Linked/Unlinked (default
  Unlinked) and Normal/Strong (default Normal), updating when the view or
  its data changes.

## 18. Calls made by Claude, for review

- No "group mode": a **Group by** control with **Don't group** as one choice
  (answering the user's request for a better name than "group mode").
- The step strip doubles as navigation (Panels opens the list, Checks the
  problems, Merge merges, Export opens the dialog).
- Main button: Add panels… → Merge → Export…, with a settings summary on
  Merge.
- Inspector section order: Problems, Panels, ROIs, Merge settings,
  Composites, Notes. Headings always visible.
- ‹ is disabled while an export runs; leaving cancels a running merge,
  analysis or ROI preview.
- Adding or removing panels during a merge cancels the merge, with a toast.
- The main button reads Merge whenever the draft settings differ from a
  group's merged ones (the user changed them on purpose), otherwise
  Export….
- Panel details live in the inspector's Panels section, whichever way the
  panel was selected.
- Problems never block merging or exporting.
- Problem badges on overlaps as well as panels; flagged seams tinted in the
  seams overlay; clicking a problem zooms to it.
- Stretch details: Linked greyed out for mono views; recomputed when the
  data settles, held while a mosaic fills in; low-resolution statistics;
  ROI previews use the canvas's stretch; old per-group stretches ignored.
- A shared camera across plan, mosaic, groups and composites.
- ROI previews prioritise the ROI nearest the view's centre. An ROI drawn
  while ROIs are shown merges at once.
- Merge merges every missing or out-of-date group; a chip menu merges one.
- A merge pressed during an export queues.
- The file name pattern's default `{session}_{name}`.
- Undo toasts instead of confirmation dialogs.
- Keyboard shortcuts for the listed actions.

## 19. Left to the builder

- Splitting this into implementation plans and their order (it is several
  plans' worth: e.g. the window shell, canvas and step strip; the panel list
  and grouping; ROI previews and their engine work; composites and export;
  the new checks).
- How transient ROI previews are stored and scheduled (memory, scratch tile
  stores, a preview job queue alongside `JobRunner`), and how a large ROI
  stays responsive, so long as previews never touch the group's tile store,
  dirty tracking is exact, and cancellation is prompt.
- How merge settings are committed at Merge without invalidating other
  groups' finished mosaics, while the CLI's `mmm set` keeps its behaviour
  (e.g. an app-only core path that records the settings and merges with
  them explicitly).
- How seam geometry is derived for the Seams overlay (from the overlap graph,
  the merge weights, or the seam-health steps).
- How an aligned panel gets an early outline.
- The detection methods and thresholds of the new checks.
- WBPP's exact keyword rule (look it up and match it), and the panel list's
  final columns (what headers give reliably: e.g. integration from EXPTIME
  and NCOMBINE).
- The automatic stretch's parameters for Normal and Strong, and what a
  linked stretch is computed from for a composite. Whether the start
  screen's hero switches to the automatic stretch too (it uses a session's
  saved stretch when there is one), which would be simpler.
- The exact keyboard shortcuts, roll-up threshold, file name tokens beyond
  `{session}` and `{name}`, and how "out of date" is shown on the mosaic.
- Which new components go into `mega-ui` (likely: the step strip, the
  inspector with folding sections, the ordered-list manager for ROIs and
  composites, the panel table with sections, the drop target) and their
  gallery entries and snapshots.
- How the session window replaces the tabs in `crates/mmm-app` (reusing the
  viewer, plan rendering, stretch, checks engine, pipeline, jobs and export
  code), and the matching updates to `docs/DESIGN.md` and the `--selftest`
  run.

## 20. Later

- Compare A/B, if it doesn't make the cut (§8.3).
- Export defaults refined from user feedback (groups vs composites).
- Export format options; XISF export.
- The processing stage (§16).
- More checks, refined thresholds.

## 21. References

- `docs/superpowers/specs/2026-11-07-ui-standard-and-start-screen-design.md`
  (§7, §10).
- `docs/DESIGN.md`: Standalone app sections (pipeline, previews, checks,
  ROIs, compositions, export, settings panel), Mega UI standard, Start screen.
- Astrometrical's ROIs: `../astrometrical/apps/web/src/components/RoiEditor.tsx`
  (ordered list, labels "Region N", go to, reorder, delete).
- The brainstorming mockups (wireframes, not committed): the canvas options,
  the session layouts, the panel list layouts and the step strip states.
