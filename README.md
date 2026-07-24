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

Early development. Phase 1 (ingest, overlap graph, photometric solve, feather
blend) in progress.

## Build

```sh
cargo build --release
target/release/mmm --help
```
