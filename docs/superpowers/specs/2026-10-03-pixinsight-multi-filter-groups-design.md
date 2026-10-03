# PixInsight multi-filter groups (shared reference frame, stage 2)

Status: approved design 2026-10-03, not yet implemented. Stage 1 (Rust core
and CLI) is in
[the shared reference frame spec](2026-10-03-shared-reference-frame-design.md)
and merged. This stage brings the same capability to the PixInsight module
and the IPC worker. Module version 1.6.0.

## Problem

A mono imager stacks one panel set per filter and wants one mosaic per
filter, all on one pixel grid, to combine afterwards (LRGBCombination,
PixelMath). The module today runs exactly one worker job per execution over
one flat panel list, and every run derives its own frame and crops to its
own content, so per-filter outputs do not align. Stage 1 fixed this for the
CLI with an explicit reference frame and `mmm batch`; PixInsight users need
the equivalent without ever seeing a session directory or a frame file.

## Goals

- A user adds every panel of every filter to the module's list, assigns
  groups in a few clicks, runs once, and gets one output window per group,
  all with identical dimensions and WCS.
- A run with a single group stays byte-identical to today's behaviour.
- Process icons keep working: old icons load as one group.
- The process console shows where each group starts and ends.

Non-goals (this stage): importing or exporting `.mmm-frame.json` from the
module (PixInsight users are not exposed to sessions; revisit if asked);
mixing views and files in one run; nested or hierarchical groups; a wire
`extent` field (the imposed frame already selects the canvas extent).

## Design

### 1. Data model (process parameters)

Both panel tables, `inputViews` and `filePaths`, gain a second `MetaString`
column named `group` (`MmmViewGroupParameter`, `MmmFileGroupParameter`),
registered after the existing id/path column of each table. Instance fields
`Array<String> p_viewGroups` and `Array<String> p_fileGroups` run parallel
to `p_viewIds` / `p_filePaths`; `AllocateParameter` for a table resizes both
arrays of that table, `LockParameter` serves the group column, `Assign`
copies it, `ResetInstance`/`ImportProcess` carry it.

Semantics of the group string:

- Empty = the **default group**. An icon saved by an earlier version has no
  column, so every panel lands in the default group and the run is a
  single-group run (identical to today).
- Names are free text in the UI, compared exactly (case-sensitive, after
  trimming surrounding whitespace at assignment time).
- Run order = order of first appearance in the list.

Window ids: a group's output window is `MegaMergeMosaic_<sanitised>` where
`sanitised` maps every character outside `[A-Za-z0-9_]` to `_`; the default
group keeps `MegaMergeMosaic`. Seam maps follow the same rule
(`seam_map_<sanitised>` / `seam_map`). Two groups that sanitise to the same
id are refused before any work ("groups 'R-1' and 'R_1' both map to window
id MegaMergeMosaic_R_1"). PixInsight's own collision suffixing still applies
to pre-existing windows.

Validation at run start (before any worker is spawned): no mixed views and
files (unchanged), at least two panels in total (unchanged), every group
non-empty by construction. A group may contain a single panel: that is the
"align one Ha frame to the LRGB mosaic" case.

### 2. UI (Target Frames section)

- The `Views_TreeBox` / `Files_TreeBox` gain a second column, **Group**, and
  `EnableMultipleSelections()`.
- A new control row under the list:
  - **Filter** `Edit` — a wildcard pattern (`*` and `?`, matched
    case-insensitively against the view id or the file name without
    directory). Non-empty → only matching rows are shown; the tree is
    rebuilt from the instance arrays on every edit (PCL `TreeBox` nodes
    cannot be hidden individually). PCL `Edit` has no native placeholder, so
    one is emulated: while the field is empty and unfocused it shows
    `e.g. *_Ha*` in a muted colour via `SetStyleSheet`; `OnGetFocus` clears
    the hint and restores the normal colour, `OnLoseFocus` with empty text
    restores the hint. The hint text is never treated as a pattern.
  - **Group** `Edit` + **Set group** `PushButton` — applies the trimmed
    group text to the **selected rows if any are selected, otherwise to
    every row currently displayed** (i.e. the filtered subset, or the whole
    list when no filter is active). This supports the loop: type a filter,
    set a group, type the next filter, set the next group.
  - **Group by FILTER** `PushButton` — fills the group of every row
    (selected rows only if there is a selection, else all displayed rows)
    from the panel's FILTER value. Views: `View::Window().Keywords()`
    `FILTER` card, quotes and whitespace stripped. Files: the worker's
    `--probe-panels` reply (see §5), run through the existing pumped
    `run_probe_process` so the GUI stays responsive. Panels without a FILTER
    value are left unchanged and counted in a console warning
    ("3 panels carry no FILTER keyword; their group was not changed").
  - **Clear groups** `PushButton` — empties the group of the selected rows,
    or of every displayed row when nothing is selected.
- Rows added via Add views / Add files start in the default group. Remove
  works on the multi-selection.
- Tooltips on every new control. Section heights re-measured in
  `e_ToggleSection`; the Control-before-SectionBar declaration order is kept
  (enforced by `host/test/test_sectionbar_order.cpp`).

### 3. Execution (`MmmExecution.cpp`)

`run_blend` partitions the instance's panels into groups (order of first
appearance) and then:

**Single group** → exactly today's path: `RunViews`/`RunFiles` once, no
reference, window `MegaMergeMosaic`. Byte-identical output; the existing
golden tests cover it and must not change.

**Several groups**:

1. **Derive the reference** once over every panel of every group. Views:
   build the `PanelDesc` list for *all* panels (with astrometric properties
   as Solved mode does today) and call the new worker probe
   `--probe-reference` (§5), honouring the instance's `inputSelect` the same
   way `reference::derive` does (Auto → aligned iff ≥ 2 panels of one
   geometry, else solved). Files: `--probe-panels` already runs; its reply
   now carries the reference. Print `Reference frame: <describe()>` and the
   panel/group counts.
2. **Resolve the registration mode once** from the reference kind (aligned
   → `JobMode::Aligned`, solved → `JobMode::Solved`; Files mode passes
   `input_select` and the reference and lets the worker check). A group that
   cannot fit the reference (footprint, canvas or WCS mismatch) fails in its
   own job with the stage-1 error text, naming the group in the console.
3. **Per group, in order**: console header (§4), a fresh temp session dir
   (one `AutoSessionDirGuard` per group), `RunViews`/`RunFiles` with
   `reference` set in the `InitJob`, the `ImageWindowCollector` constructed
   with the group's window id, seam map with the group's id, then the
   existing `PrintSummary` under the header. Slot sizing for the solved
   path uses the reference frame's width (not `probe_frame`); aligned keeps
   the canvas. The host's `Begin` check (output ≤ Init canvas when the
   canvas is non-zero) is satisfied: imposed aligned frames equal the panel
   canvas, and solved jobs keep sending a 0×0 canvas.
4. **Failure or cancel in group k**: the current group's hidden window is
   closed as today; windows of groups 1..k−1 are **kept** (they are complete
   and valid); the error message names the group
   (`"group Ha: <reason>"`), and the remaining groups are not run.
5. **Final summary** (§4).

### 4. Console output

Written only between stages (never while a `ConsoleProgress` line is live;
after `CommitLine`). Style follows the existing summary block.

```
<end><cbr><br><b>Reference frame</b>  solved frame 9286x18341 px, 1.597"/px, center RA 83.8 Dec -5.4, rotation 0.00°
  groups          4 (L, R, G, B)   panels 48

<end><cbr><br><b>== Group L (12 panels)</b>
  … existing reproject/analyze/blend progress lines …
  … existing Mosaic summary block …

<end><cbr><br><b>== Group R (12 panels)</b>
  …

<end><cbr><br><b>Groups complete</b>  4 outputs on the shared 9286x18341 grid
  L               MegaMergeMosaic_L
  R               MegaMergeMosaic_R
  G               MegaMergeMosaic_G
  B               MegaMergeMosaic_B
```

The default group is shown as `(default)` in headers and the summary. A
failed group ends with the existing `**` warning style naming the group.

### 5. Wire protocol and worker

All additions are optional fields with `#[serde(default)]`; protocol
version stays 3 (precedent: `seam_map`, `gain`). The worker-version
handshake (1.6.0 on both sides) pins module and worker together.

- `InitJob.reference: Option<ReferenceFrame>` — the stage-1 type, serialised
  exactly as `.mmm-frame.json` (internally tagged `kind`). Honoured by all
  three job modes: the worker passes it to `analyze_ipc_aligned`,
  `analyze_ipc_solved` (both gain a `reference: Option<&ReferenceFrame>`
  parameter mirroring `analyze_full`, including the kind-mismatch,
  footprint and aligned checks and `frame_imposed = true`) and to
  `analyze_full` in Files mode. With `frame_imposed` set, the blend's
  default extent is the canvas, so outputs share the frame's geometry.
- `PanelProbeGeom.filter: Option<String>` — the panel's FILTER value: FITS
  `FILTER` card, or XISF FITS-keyword `FILTER`, else the XISF property
  `Instrument:Filter:Name`; `None` when absent.
- `PanelProbeReply.reference: Option<ReferenceFrame>` — `reference::derive`
  over the probed paths with the request's `input_select`; `None` when it
  cannot be derived (the existing `frame` field keeps its semantics; the
  stage-1 "no solution in …" error still surfaces as before when the
  caller forces solved).
- New worker flag `--probe-reference`: reads a bare `InitJob` (Views-style
  `PanelDesc`s with properties) on stdin, replies with one line of JSON, the
  `ReferenceFrame`, or exits non-zero with the derive error on stderr. Core
  gains `reference::derive_from_descs(&[PanelDesc], InputSelect) ->
  Result<ReferenceFrame>` sharing the kind rule and the solved path with
  `derive` (both route through one internal function over per-panel
  `(label, width, height, Option<LinearWcs>, Result<WcsModel>)` items, so
  files and descriptors cannot drift).
- `PROTOCOL.md`: §6 (`Init`), §11 (probes), §10 (history line for 1.6.0),
  §13 unchanged in rule but note the imposed-frame case.

Host (`mmm_host`): `probe_reference()` mirroring `probe_frame()` but parsing
JSON; `probe_panels()` parses the new reply fields; `InitJob` builder takes
the reference JSON verbatim (the host never interprets it beyond width and
height for slot sizing, read from the JSON).

### 6. Testing

Rust (`mmm-core`, `mmm-ipc-worker`):
- `derive_from_descs` over descriptors built from the stage-1 synthetic
  panels equals `derive` over the same files (both kinds).
- `analyze_ipc_aligned` / `analyze_ipc_solved` with an imposed reference
  through the in-process test host: `frame_imposed`, frame adopted, blended
  output geometry = reference geometry; kind mismatch and footprint errors
  surface as worker faults with the stage-1 text.
- Worker end-to-end: two Init jobs with the same reference → identical
  output geometry; `--probe-reference` round-trips; `--probe-panels` reply
  carries `filter` (from a synthetic FITS with a FILTER card and an XISF with
  a FILTER keyword) and `reference`.
- Serde: `InitJob` without `reference` still parses (old hosts).
- `version_sync.rs` passes at 1.6.0.

Host / module (C++ tests under `integration/pixinsight/host/test`):
- Group partitioning and window-id sanitisation unit tests, including the
  collision refusal.
- Golden harness: a two-group job sequence over the synthetic fixtures; both
  outputs carry the reference geometry; the single-group golden run is
  unchanged.
- `test_sectionbar_order.cpp` extended for the new controls.

Manual smoke (Daniel): real LRGB panel set in Views and Files mode,
Group by FILTER, console headers, icon round-trip.

### 7. Documentation and version

- Module HTML doc: new section "Merging several filters onto one grid"
  (groups column, Filter/Set group loop, Group by FILTER, output naming,
  the aligned-input caveat from stage 1 in tool-neutral wording).
- `docs/DESIGN.md`: stage-2 paragraph under the shared-reference-frame
  section.
- Version 1.6.0 in `Cargo.toml`, `MmmVersion.h`, `mmm_protocol.h`
  (`kExpectedWorkerVersion`) and the HTML doc version line.

## Rationale

The flat Group column keeps the data model one string per panel, which is
all PCL's non-nesting tables can persist and all the engine needs; "Group
by FILTER" plus the filter-then-set loop covers the common cases without a
tree UI. Deriving the reference automatically over the whole list, and only
when there is more than one group, gives multi-filter users the alignment
guarantee while leaving every existing single-group workflow and icon
untouched. Keeping the frame out of the UI (no import/export) matches how
PixInsight users experience the module: windows in, windows out.
