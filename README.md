# Mega Merge Mosaic (mmm)

Fast, standalone merging/blending of pre-aligned astrophotography mosaic panels.

Takes the aligned, zero-padded panel frames produced by PixInsight's
**MosaicByCoordinates** (FITS/XISF, linear data) and produces a seamless merged
mosaic (FITS/TIFF) — automatically, quickly, and without pinched stars.

## Why

Existing options each fall short: GradientMergeMosaic distorts stars in overlap
regions, PhotometricMosaic needs heavy manual tuning and is slow, and newer
tools are immature. MMM exploits the structure of the problem — panels are
pixel-aligned on a common projection with hard zeros outside coverage — so all
expensive work happens only in overlap bands, never across the whole canvas.

## Approach

1. **Ingest** — stream panels into a sparse tiled cache (only non-empty tiles),
   with a downsample pyramid. Handles canvases of 70k×30k+ and dozens of panels
   without holding frames in RAM.
2. **Coverage & overlap graph** — per-panel coverage masks (zeros = no data),
   pairwise overlap regions as graph edges.
3. **Photometric solve** — robust per-channel linear fits per overlap, plus a
   global least-squares gain/offset adjustment across the panel graph.
4. **Seam & blend** — star-avoiding seam paths through overlaps, multiband
   blending across seams. Stars are never averaged: each comes from exactly one
   panel.
5. **Output** — streamed 32-bit float FITS/TIFF with WCS carried through.

CPU-parallel (rayon) first; GPU acceleration (wgpu) planned. Core is a library
(`mmm-core`) with the CLI (`mmm`) as a thin frontend, so a GUI can follow.

## Status

**Phase 2 complete.** On a 12-panel mosaic (9255×18310×3 canvas, 24 GB of
panels, 64-thread Threadripper): analyze 5 s, full-res blend 6 s, ROI crop
1.3 s, preview under a second. The pipeline is photometric matching (global
gain/offset solve) → residual surface correction (background-only, signal-
protected) → star-avoiding seams with two-band blending (stars always come
from exactly one panel — misregistration cannot pinch or double them) →
streamed FITS with real WCS from the XISF astrometric solution. Validated
against synthetic ground truth (75 tests) and user-validated in PixInsight
against GradientMergeMosaic/PhotometricMosaic on real data ("best merge
I've seen on that mosaic").

```
mmm analyze test_data/orion_mosaic/*.xisf --session orion.mmm-session
mmm report  --session orion.mmm-session
mmm blend   --session orion.mmm-session -o mosaic.fits
mmm blend   --session orion.mmm-session -o crop.fits --roi 5000,3500,2000,2000
mmm blend   --session orion.mmm-session -o prev.fits --downsample 8 --png prev.png
```

Phase 3 (quality) added: connected star masks so seams can never kink a
diffraction spike, a cross-panel defect veto (cosmic residue and satellite
trails in overlaps are suppressed, not shown at full strength), seam-map
diagnostics (`mmm report --seam-png map.png` + per-edge seam Δ), and an
opt-in `--flatten 1|2` global background flatten that provably cannot dig
holes in nebulosity. 97 tests.

Phase 4 added the full multiband blend: the star-free base is now blended
as a Laplacian pyramid with seam transitions proportional to each scale
(default mode; `--mode twoband|feather` retained), closing the last gap —
mid-frequency structure under differing seeing or slight misregistration.
Output WCS verified against catalog star positions. 107 tests.

Next: tidy-up for external testers (PixInsight/XISF workflows, CLI), then
FITS/compressed-XISF input, wgpu GPU path, GUI.

## Build

```sh
cargo build --release
target/release/mmm --help
```
