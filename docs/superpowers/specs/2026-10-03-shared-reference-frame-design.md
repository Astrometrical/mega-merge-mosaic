# Shared reference frame across mosaic groups (aligned multi-filter output)

Status: approved design 2026-10-03, not yet implemented. Stage 1 covers
`mmm-core` and the `mmm` CLI; the PixInsight module and IPC protocol follow
in a later stage (§8).

## Problem

A mono imager merges each filter's panel set with mmm separately (L, R, G,
B, or broadband plus narrowband). The resulting mosaics are not on a common
pixel grid, so they cannot be combined directly (LRGBCombination,
PixelMath). Two independent mechanisms cause the misalignment:

1. **Frame derivation (solved input).** `align::choose_frame` derives the
   centre, scale, rotation and size from the panels it is given. Four filter
   sets with slightly different pointings or solutions yield four frames
   that differ by fractions of a pixel in centre and scale, and often by
   whole pixels in size.
2. **Output crop (both input kinds).** `blend` writes only the union of the
   panels' content bboxes (`blend::union_bbox`). Even on an identical
   canvas, each filter's coverage differs a little, so the crop origin
   shifts between channels. This affects aligned MosaicByCoordinates input
   as well as solved input.

## Goal

Merge several groups of panels so that every group's output has identical
pixel dimensions and an identical WCS, ready for direct channel
combination. The engine stays single-group: the shared state is one
explicit *reference frame* that every group's session adopts, plus an
output extent that does not depend on a group's coverage.

Non-goals (this stage): reprojecting already-registered canvases onto a
different frame; clipping panels that fall outside an imposed frame;
enforcing one channel count across groups (an OSC RGB mosaic and a mono Ha
mosaic may legitimately share a frame); PixInsight module / wire changes.

## Design

### 1. The reference frame artifact

A small JSON file, conventionally `<name>.mmm-frame.json`, serialized from a
new `mmm-core` type:

```rust
/// A reference frame shared by several sessions so their outputs land on
/// one pixel grid. Persisted as `<name>.mmm-frame.json`.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReferenceFrame {
    /// Solved input: the mosaic frame every group reprojects onto.
    Solved { frame: MosaicFrame },
    /// Aligned input: the shared canvas geometry and, when the panels
    /// carry one, the canvas WCS the groups must agree with.
    Aligned { width: u64, height: u64, wcs: Option<LinearWcs> },
}
```

The file is written through a wrapper `ReferenceFrameFile { version: u32,
#[serde(flatten)] frame: ReferenceFrame }` so the JSON carries a top-level
`"version": 1` beside `"kind"`; `load` rejects any other version with a
message naming the file and the version it found. No channel count is
recorded (see non-goals). `MosaicFrame` and
`LinearWcs` are the existing serializable types; `rotation_deg` keeps its
`serde(default)`.

Module placement: `mmm_core::reference` (new file `reference.rs`) holding
the type, `load`/`save`, and the derivation function of §2. Public items
are documented (`missing_docs`).

### 2. `mmm frame <panels…> -o F [--input auto|aligned|solved]`

Header-only; never scans pixels. Opens every panel with `InputPanel::open`
(errors aggregated per file, as `analyze_solved` does today), then applies
the cheap half of the existing auto-detect rule:

- `aligned` when `--input aligned`, or `auto` with ≥ 2 panels whose
  `(width, height)` all agree. Result: `Aligned { width, height, wcs }` with
  `wcs` = `InputPanel::linear_wcs()` of the first panel.
- otherwise `solved`: every panel must yield a `WcsModel`
  (`InputPanel::wcs_model`), reusing the aggregated "solved input requires
  an astrometric solution in every panel" message; result:
  `Solved { frame: choose_frame(&models) }`.

Channel counts may differ across the input (groups are checked
individually by analyze). The coverage half of the auto rule (≥ 50 %
covered re-dispatches to solved) needs a scan and is deliberately *not*
applied here; a same-geometry raw-panel set must pass `--input solved`
exactly as analyze requires it today.

Engine entry point: `reference::derive(paths: &[PathBuf], input:
InputSelect) -> Result<ReferenceFrame>`. The CLI prints the frame summary
(size, scale, centre, rotation) in the same format analyze prints a fresh
frame.

### 3. `mmm analyze … --frame F`

`analyze_full` (and the thin wrappers that need it) gain an
`Option<&ReferenceFrame>` parameter. Behaviour per path:

**Solved path** (`analyze_solved`): with `Some(Solved { frame })`, the
loaded frame replaces the `choose_frame` call. Before any reprojection, a
*footprint check* runs: for each panel, the boundary samples used by
`choose_frame` (`boundary_samples(width, height)` → `pixel_to_sky` →
`frame.linear_wcs().sky_to_pixel`) must lie within `[0, width) × [0,
height)` of the frame. Any violation is a hard error naming the panel and
the overshoot in pixels on each side, e.g.

```
panel P3_Ha.xisf extends 212 px beyond the right edge of the reference
frame (9286 px wide): re-derive the frame over the full set with `mmm
frame`, or exclude the panel
```

All violating panels are listed in one message. No clipping (explicit
decision; revisit if users ask).

**Aligned path** (`analyze_aligned`): with `Some(Aligned { width, height,
wcs })`, after the scan's existing geometry check, the canvas `(w, h)` must
equal `(width, height)`. When both the frame and the first panel carry a
`LinearWcs`, the two are compared by mapping the canvas corners and centre
through the panel's WCS and back through the frame's inverse; the maximum
pixel displacement must be ≤ `ALIGNED_WCS_TOLERANCE_PX = 0.05`. Either
failure is a hard error with the hint:

```
canvas 9255x18310 of this group does not match the reference frame
(9240x18300): align every group to one common reference, or process the
groups separately
```

(The WCS variant of the message quotes the displacement.) A panel set
without a WCS and a frame without one pass on geometry alone.

**Kind mismatch**: `Solved` frame but the set dispatches to aligned, or
`Aligned` frame but the set dispatches to solved, is an error naming both
kinds and suggesting `--input`. Auto-detect still runs first, so a `Solved`
frame with same-geometry raw panels (≥ 50 % coverage) still works through
the re-dispatch.

**Session record**: `Session` gains `#[serde(default)] pub frame_imposed:
bool`. Solved sessions keep storing the (now imposed) `MosaicFrame` in
`frame` exactly as before, so `blend`'s WCS emission is unchanged. Aligned
sessions keep `frame: None`.

### 4. Blend extent

`BlendParams` gains

```rust
pub enum Extent { Union, Canvas }
pub extent: Option<Extent>,   // None = session default
```

`blend::output_bbox` resolves the extent first: `Canvas` →
`[0, 0, canvas.w, canvas.h]`; `Union` → `union_bbox(session)` as today.
`None` resolves to `Canvas` when `session.frame_imposed`, else `Union`, so
every existing session blends byte-identically (hash guard). `--roi`
intersects whichever extent is active; an empty intersection is the
existing error.

For a solved session, `Canvas` is the frame including its
`FRAME_MARGIN_PX` margin; for an aligned session it is the whole input
canvas. Output rows outside every panel's bbox are zero (no-data
sentinel). Implementation must verify the band loop, the pyramid/two-band
machinery and the PNG preview tolerate bands with no intersecting panel
(the union bbox can already contain fully empty bands, so this is expected
to hold; a test asserts it).

CLI: `mmm blend … [--extent union|canvas]`, default per the session rule.
WCS cards are unchanged: they already derive from `session.frame` (solved)
or panel-0 passthrough (aligned) with the output origin applied, so
identical frames and extents give identical cards. The IPC blend path
gets the same default behaviour for free through `output_bbox`; the wire
field arrives in §8.

### 5. `mmm batch`, the grouped front-end

```
mmm batch --group L L/*.xisf --group R R/*.xisf … -s DIR -o OUTDIR
          [analyze options] [blend options]
```

Clap derive: `#[arg(long, num_args = 2.., action = Append)] group:
Vec<Vec<String>>` keeps each occurrence's values together; the first value
is the group name, the rest are panel paths (the shell expands globs).
Validation: ≥ 1 group; names unique, non-empty and filesystem-safe
(`[A-Za-z0-9._-]+`); every group has ≥ 1 panel.

Steps, all in the CLI crate (no new engine concept):

1. `reference::derive` over the union of all groups' panels →
   `DIR/reference.mmm-frame.json`.
2. For each group, in order: analyze into `DIR/<group>.mmm-session` with
   the frame imposed, printing the same per-session summary `mmm analyze`
   prints.
3. For each group: blend to `OUTDIR/<group>.fits` (and
   `OUTDIR/<group>.png` when `--png` is given) with the session default
   extent (= canvas). `--roi` is accepted and applied identically to every
   group.
4. Final summary: per group, output path and dimensions; one line
   confirming all outputs share the frame.

The analyze options (`--surface`, `--input`, `--gain`) and blend options
(`--downsample`, `--feather`, `--mode`, `--png`, `--roi`, `--defect-veto`,
`--flatten`, `--wcs-frame`) move into two clap structs (`AnalyzeOpts`,
`BlendOpts`) flattened into `Analyze`, `Blend` and `Batch`, so each option
has exactly one definition and default. A failure in any group stops the
run with the group named; completed sub-sessions stay on disk and are
reusable with the single commands (`mmm report -s DIR/L.mmm-session`).

### 6. Testing (synthetic only; `test_data/` never required)

- **Aligned outputs, solved path**: synth sky cut into two groups whose
  footprints differ deliberately (second group offset by tens of pixels
  and missing one panel), so independently derived frames would differ.
  `frame` → `analyze --frame` ×2 → `blend` ×2. Assert equal output
  dimensions, identical WCS cards, and a planted star centroid within
  0.1 px of the same output pixel in both.
- **Aligned outputs, aligned path**: two groups of same-canvas aligned
  frames with different content bboxes → equal dimensions (full canvas).
- **Footprint failure**: a panel displaced outside the frame → error names
  the panel and side.
- **Aligned mismatch**: differing canvas size → error with the
  common-reference hint; same size but WCS shifted by 2 px → error
  quoting the displacement.
- **Kind mismatch** both directions.
- **Extent default**: an un-imposed session blends with `Union` and stays
  byte-identical (existing hash regression guard); `--extent canvas`
  widens it and zero-fills.
- **`mmm frame` detection**: mixed geometries → `Solved`; equal geometries
  → `Aligned` carrying panel-0 WCS; `--input` overrides both ways; a
  solved set with one unsolved file → aggregated error.
- **`batch`**: end-to-end over two groups into a temp dir; asserts the
  file layout and that both outputs match the single-command results.
- Round-trip of `ReferenceFrame` JSON for both kinds, and rejection of an
  unknown version.

### 7. Documentation

`docs/DESIGN.md`: new section describing the reference frame, the extent
rule and `batch`; CLI surface and session-directory listings updated
(`frame_imposed`, `*.mmm-frame.json`). User-facing note for the aligned
case: filters registered separately get separate canvases; either align
all against one common reference or feed the raw solved panels.

### 8. Later stage: PixInsight

The IPC `InitJob` gains an optional `frame: Option<ReferenceFrame>` and an
optional `extent`; the worker passes them into the same `analyze_full` /
`BlendParams` fields, so no engine change is needed. The module gains a
reference-frame parameter (import a `.mmm-frame.json`, or derive one from
a chosen set of views) and an export action. The frame-probe
(`--probe-frame`) reply can carry the derived frame so the host can save
it. Wire details belong to that stage's spec and `PROTOCOL.md`.

## Rationale for the shape

Groups stay a CLI convenience over a shared frame rather than a session
concept, because (a) the PixInsight host cannot easily submit several
groups of views in one job but can attach a frame to each of several jobs,
(b) a user can align new data (another filter, a later season) to an
existing frame without re-running earlier groups, and (c) report, blend
internals, the band cache and the IPC worker stay untouched.
