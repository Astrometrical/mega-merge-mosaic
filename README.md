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

**Phase 1 POC working.** On a 12-panel mosaic (9255×18310×3 canvas, 24 GB of
panels): analyze 3.5 s, full-res blend to FITS 6.1 s, downsampled preview
0.6 s (64-thread Threadripper, warm cache). Photometric matching + feather
blend produce seam-free results on real data, validated against a synthetic
ground truth. Phase 2 (star-avoiding seams, multiband, residual gradient
surfaces, WCS from XISF properties) is next.

```
mmm analyze test_data/orion_mosaic/*.xisf --session orion.mmm-session
mmm report  --session orion.mmm-session
mmm blend   --session orion.mmm-session -o mosaic.fits [--downsample 8 --png prev.png]
```

## Build

```sh
cargo build --release
target/release/mmm --help
```
