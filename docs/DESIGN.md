# Mega Merge Mosaic — Design

Standalone, fast merging/blending of pre-aligned astro mosaic panels
(MosaicByCoordinates output: full-canvas FITS/XISF frames, linear data, hard
zeros outside coverage). CLI first (`mmm`), engine in `mmm-core` so a GUI can
follow. Cross-platform; CPU-parallel via rayon now, wgpu compute later.

## Facts about the input (verified on real data)

- Every panel frame has the **full canvas geometry** (e.g. 9255×18310×3ch
  Float32 for the Orion test set; 12 frames × 2 GB).
- Zero is a reliable no-data sentinel: real data floor ≈ 0.001–0.006. A pixel
  is *covered* iff **all channels are nonzero**.
- Each panel covers only ~9% of the canvas → all processing must be
  sparse/overlap-band based, never dense over the canvas.
- Registered frames carry the **canvas-level WCS** (identical across panels —
  it is the mosaic projection, not stale pointing). Output WCS = copy from any
  panel, adjust CRPIX1/2 by the crop origin.
- PixInsight XISF: monolithic, uncompressed attachment at 4096-aligned offset,
  planar, little-endian → **directly mmap-able**. No cache copy of pixel data
  is needed for uncompressed inputs; the source file *is* the random-access
  store. (Compressed inputs, later, will decompress into a tile cache once.)
- Data may be raw stacks: no gradient removal, no color calibration. Panel
  background levels genuinely differ (2× between adjacent panels observed).

## Pipeline

```
analyze ─────────────► photometric solve ───► blend ───► output
  │ per-panel scan:        │ per-edge robust      │ feather (POC) /
  │ L8 summary, bbox,      │ linear fit on L8,    │ seam+multiband (phase 2)
  │ stats                  │ global gain solve    │ streamed row bands
  └── overlap graph from L8 coverage
```

All stages persist artifacts in a **session directory** and are individually
re-runnable; `blend` never re-analyzes.

### Analyze

One streaming pass per panel (rayon across panels, I/O-bound):
- **L8 summary**: canvas at 1/8 per 8×8 block, per panel: per-channel mean of
  covered pixels + coverage fraction (0..1). ~2.6 MP per panel for the test
  set. This one artifact powers overlap detection, photometric fitting,
  distance/feather maps, and downsampled previews.
- Content bbox, per-channel min/max/mean/nonzero stats.
- Persisted: `panels/<id>/summary.bin` (+ meta in `session.json`).

### Overlap graph

For each panel pair: AND of *full-coverage* blocks (fraction == 1.0) in the L8
grid → pixel count + bbox. Edges with count above threshold form the graph.
Distance maps: two-pass chamfer transform on each panel's L8 coverage (distance
to nearest uncovered block, in coarse units).

### Photometric solve

Per edge (i,j), per channel, on L8 means over blocks fully covered by *both*
panels: robust linear fit `y ≈ a·x + b` via 3 rounds of least squares with
2.5σ residual clipping. (Binning is linear, so gains fit on binned data are
unbiased.) Keep sufficient statistics per edge: `n, Σx, Σy, Σxx, Σyy, Σxy`
(post-clip).

Global adjustment: find per-panel `(g_i, o_i)` minimizing
`Σ_e Σ_p ((g_i·x + o_i) − (g_j·y + o_j))²` — expands exactly in the per-edge
sufficient statistics → sparse 2N×2N normal equations, solved dense (N ≤ a few
hundred) with LU. Gauge: fix `g=1, o=0` for the largest panel in each connected
component. Per channel independently.

### Blend (POC: feather)

Output canvas = union of content bboxes. Stream in row bands (~256 rows),
rayon across bands. Per band, per intersecting panel: read rows from the source
mmap, apply `g,o`, accumulate `w·v` and `w`; output `Σwv/Σw`.

Weight per pixel: `w = max(clamp(d/feather, 0, 1), ε)` where `d` = bilinear
sample of the panel's coarse distance map × 8 (px), `feather` default 300 px,
`ε = 1e-4` so single-coverage rim pixels survive normalization but lose to any
overlapping partner. Uncovered pixels (any channel zero) get `w = 0` outright.
Panel-edge interpolation garbage is thereby suppressed in overlaps
automatically (distance ≈ 0 ⇒ tiny weight).

`--downsample 8` blends from L8 summaries instead (seconds, for previews);
optional PNG output with an autostretch (median/MAD midtone transfer) for
quick visual checks.

### Phase 2 (not in POC)

Star-avoiding seam paths (DP/graph-cut on overlap bands, star penalty), shared
across channels; multiband blend confined to seam bands; residual low-order
surface correction after the gain fit. Never average stars — each star comes
from exactly one panel.

## Session directory

```
<name>.mmm-session/
  session.json        # canvas geometry, panel list+paths, stage stamps, params
  panels/<id>/summary.bin   # L8 planes: mean[ch] + coverage, f32
  analysis/overlap_graph.json
  analysis/photometry.json  # per-edge fits + global gains/offsets per channel
```

## CLI surface (POC)

```
mmm info <files…> [--stats]
mmm analyze <panels…> --session S
mmm report --session S            # graph + fit table, warnings on poor fits
mmm blend --session S -o out.fits [--downsample N] [--feather PX] [--png P]
```

## Testing

- Unit tests per module (synthetic minimal XISF files).
- **Synthetic ground-truth harness**: generate a known sky (gradient background
  + Gaussian stars + noise), cut into overlapping zero-padded canvas frames,
  apply per-panel gain/offset perturbations, write XISF; run the full pipeline;
  assert recovered gains and RMSE of (merged − truth) in interior regions.
- Real-data smoke: `test_data/orion_mosaic/` (12 panels; 3,4,7,8 cover M42).

## POC results (2026-07-24, Phase 1 complete)

Real data: 12-panel Orion mosaic, canvas 9255×18310×3 Float32 (24 GB input),
Threadripper 64T under WSL2, page cache warm:

- `analyze` (L8 summaries + overlap graph + photometric solve): **3.5 s**
- `blend` full-res → 1.94 GB FITS: **6.1 s**; `--downsample 8` + PNG: **0.6 s**
- Overlap graph: 29 edges (17 grid-neighbour + 12 diagonal corner), one
  connected component. Global gains 0.85–1.23; PANEL-8's 2.3× brighter
  background correctly absorbed into offsets.
- Visual: no visible seams or brightness steps at 1/8 or at full res,
  including the seam bands crossing M42/Running Man (signal-dominated
  overlap). Stars round through all seams — no pinching/doubling.
- Remaining artifacts (expected, phase 2 targets): large-scale per-panel
  colour-tint gradients that global gain+offset cannot remove (residual
  surface correction); cosmetic staircase where rotated panel rims meet the
  canvas edge.
- ⚠ finding: PixInsight stores plate solutions as XISF `<Property>` elements,
  NOT FITS keywords — the registered panels carry no CRPIX/CRVAL/CTYPE
  keywords. WCS passthrough requires parsing `<Property>` elements (todo,
  phase 2); `keywords_for_output`'s CRPIX shifting is implemented and tested
  but currently has nothing to shift on real data.

## Performance notes

- Full-plane scan measured at ~0.9 GB/s single-threaded; analyze is I/O-bound
  with rayon. Target: 12-panel analyze in ~30 s cold, blend full-res in tens of
  seconds.
- FITS output is big-endian: byte-swap on write. Write ROWORDER='TOP-DOWN'.
- WSL2 caps RAM at 50% of host by default (`.wslconfig` to raise); mmap +
  streaming keeps us indifferent.
```
