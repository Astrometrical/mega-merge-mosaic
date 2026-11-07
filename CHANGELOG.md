# Changelog

## Unreleased

Groundwork for the standalone application; no change to any algorithm or
to the output of existing commands, apart from the XISF decode notice and
`mmm info` wording described under Changed.

### Fixed

- **Display stretch slider lag.** Dragging a stretch slider in the Blend
  view queued every intermediate stretch's tiles for upload (more finished
  per frame than the 32 uploaded), so the final stretch appeared only after
  that backlog drained. Superseded tiles are now dropped instead, and the
  view settles on the final stretch within one load cycle.
- **Zoomed-out views of an ROI re-blend.** After a settings change, blending
  an ROI updated only full-resolution tiles: parent (zoomed-out) tiles on
  the ROI's edge kept their old pixels and stayed invalid. Parents are now
  always rebuilt from their children's current data, valid or stale, and
  shown dimmed until every child is current.

### Added (app)

- **Blend settings in the app.** A Blend settings panel in the Blend tab
  edits the mosaic-wide options (mode, feather, defect veto, flatten as
  blend-local; input, surface, gain as global). Edits are staged with Apply
  and Revert, state their cost first ("N of M tiles" go stale, or analysis
  reruns), wait for running jobs to stop, and mark options changed since the
  last blend. Groups with CLI overrides are named.

### Added (app phase 3)

- **Panels, Plan and export in the app.** Panels tab (add files, optional
  grouping with stable physical group colours), Plan tab (cached previews
  placed in the shared frame, compositions, selection with Show in folder,
  checks with Accept), automatic previews then analysis after edits, Blend
  ROIs anchored to the sky, composition views in Blend, and export to
  32-bit float FITS and TIFF with provenance and a stale/missing warning.
- **`mmm export -s DIR --group G -o FILE [--format fits|tiff]`.**
- **Cross-process session lock** (`session.lock`); `init`, `set`, `run` and
  `export` refuse a session open elsewhere.
- **Core:** `mosaic::set_inputs`, the `preview` and `export` modules.
- Not yet: XISF export (needs an XISF writer in mpp-core); TIFF export uses a
  temporary scratch file about the size of the image.

### Added

- **Mosaic sessions.** `mmm init` creates a mosaic directory
  (`mosaic.json`, a shared reference frame, one analysis session per group)
  from a panel list, optionally split into groups with
  `--group-by header:KEY|token:KEYWORD`. `mmm set` edits settings
  mosaic-wide or for one group (`--group`) and reports which cached results
  the change makes stale. `mmm check` lists the groups and every input,
  file and analysis check, by severity. `mmm run` analyzes every group onto
  the shared frame and writes one FITS (and optional PNG) per group; each
  run re-analyses and re-blends every group. `init` and `set` refuse a
  `--feather` that is not a finite number greater than 0, and `check`
  reports a group whose cached analysis cannot be read instead of stopping.
  Existing single-group session directories keep working with
  `analyze`, `blend` and `report`; those commands refuse a mosaic directory
  and point at `run` / `check`.
- **Tiled result store** (`result.mmmt`) in `mmm-core`: a memory-mapped
  multi-level tile pyramid with per-tile validity and torn-read protection,
  so a viewer can read a blend while it is still being written (stale tiles
  included); a cancelled blend keeps the tiles it finished.
- **Job control** in `mmm-core`: cancellation tokens, progress reporting and
  cancellable analyze and blend.
- **Serialisable settings** with per-group overrides and change-impact
  reporting, display-stretch statistics, structured checks and mosaic
  health metrics as data, for the app to consume.
- **Desktop app preview** (`mega-merge-mosaic`, crate `mmm-app`) and the
  generic `mega-ui` egui crate: start screen, sessions, background jobs, and
  a Blend tab with a tiled zoomable viewer and automatic display stretch.
  `--selftest` renders a few frames and exits. Under WSL the app selects the
  Mesa `d3d12` GL driver unless `GALLIUM_DRIVER` is set.
- **Concurrency hardening in `mmm-core`:** one writer per tile store
  (`Error::Busy`), busy-aware settings edits, `plan_analyze_with`,
  `invalidate_group`.

### Changed

- **The decode cache keeps each panel's own sample format.** A compressed
  UInt16 panel is cached at its uncompressed size, half the former f32
  copy, and converted to 32-bit float on read. An uncompressed panel of any
  sample format (UInt8/16/32, Float64, or Float32 whose `bounds` aren't
  `0:1`) is read in place with no cache, where 1.7.0 decoded it. Pixel
  values are unchanged.
- A session from 1.7.0 decodes its compressed panels once more: the cache
  files 1.7.0 wrote are never read and can be deleted from
  `<session>/decoded/`.
- The decode notice names the sample format and the reason, for example
  `Decoding Float32 XISF panel … (zstd+sh compressed) to …`, and `mmm info`
  says how each XISF panel's pixels are read.

## 1.7.0 — 2026-10-29

CLI, PixInsight module and worker. Requires PixInsight ≥ 1.9.5, as 1.6.0 did.

### Added

- **Compressed XISF input.** XISF 1.0 (spec revision 1.02) compressed images
  and properties are read: zlib, LZ4, LZ4HC and Zstandard, each with or
  without byte shuffling, in one block or in subblocks. This covers the
  compressed masters that WeightedBatchPreprocessing now writes; previously
  they were refused.
- **XISF checksums** (SHA-1, SHA-256, SHA-512, SHA3-256, SHA3-512) are
  verified, and a mismatch is an error.
- **More XISF sample formats:** UInt8/16/32 and Float64 alongside Float32,
  plus big-endian and planar/normal pixel storage and multi-image files (the
  first image is used).
- **Decode cache.** A compressed panel is decompressed once into
  `<session>/decoded/` (the uncompressed size: about 2 GB per full-canvas
  colour panel) and reused by later runs. A notice on stderr (CLI) or in the
  process console (PixInsight) says when and how much. Without a session,
  for example `mmm info --stats`, panels decode into memory instead.

### Changed

- **FITS output headers** follow the FITS standard more strictly: exponents
  use an uppercase `E`, `COMMENT`/`HISTORY` cards have no `= `, and long
  string values pass through as `CONTINUE` cards. Pixel data is unchanged.
- **Non-gnomonic linear-only astrometric solutions** are now refused for
  solved-mode alignment, with a reason. They were previously treated as TAN.
- **Float XISF panels** whose `bounds` aren't `0:1`, or that carry an
  `offset`, are rescaled to [0, 1] on read.
- XISF, FITS and astrometric-solution decoding now come from the
  [Mega Pixel Processor](https://github.com/Astrometrical/mega-pixel-processor)
  `mpp-core` library. On real data the analyze and blend results are
  byte-identical to 1.6.0.

### Fixed

- Hostile or corrupt XISF headers that declare huge block sizes are an error,
  not an out-of-memory abort.
- A full disk while caching a decoded panel is reported as an error, not a
  crash.

### Build

- The PixInsight module is compiled against the pinned PCL 2.10.8 headers,
  not the installed core's. PixInsight 1.9.5 build 1706 ships PCL 2.11.0
  headers, and a module built against them failed to load with
  `undefined symbol: _ZN3pcl…`. On Linux the build now refuses to produce
  such a module.

## 1.6.0 — 2026-10-12

See the [v1.6.0 release](https://github.com/Astrometrical/mega-merge-mosaic/releases/tag/v1.6.0).
