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

### Phase 2 (in progress)

**A. Residual surface correction.** After the global gain solve, per-panel
low-order 2-D polynomial corrections `s_i(x,y)` (order ≤ 2 over normalized
canvas coords) fitted per channel to the remaining overlap differences on L8,
solved globally (6N×6N normal equations) so corrections agree around loops.
Guard rails (see user validation above): fit uses **background cells only**
(exclude cells brighter than panel median + k·MAD — signal regions must not
steer the fit), sigma-clipped, small ridge toward zero correction, reference
panel's constant term gauged to 0, and reported max |s| per panel so runaway
corrections are visible in `mmm report`. Applied during blend as
`v' = g·v + o + s_i(x,y)`.

**B. Two-band blend with star-avoiding seams.** Split each corrected panel
into base (bilinear-upsampled L8 mean — smooth, star-free) + detail
(full-res minus base). Base blends with the existing wide feather (hides any
residual low-frequency mismatch); detail comes from exactly **one** panel per
pixel via an owner/label map — stars are never averaged, so sub-pixel
misregistration cannot pinch or double them. Owner map computed on the L8
grid: start from argmax feather weight (Voronoi-like mid-overlap boundary),
then per-edge seam optimization (DP path over the overlap band) with a cost
of |corrected difference| + penalty on high-detail cells (star/structure
avoidance, using an L8 detail-energy plane added to the analyze scan). Detail
transitions ramp over ~16 px in background but snap hard near stars. Seam is
shared across channels (single owner map) to avoid colour fringing.

**C. WCS passthrough.** PixInsight stores plate solutions as XISF
`<Property>` elements (not FITS keywords). Parse properties, extract the
linear part of the astrometric solution, emit standard WCS cards
(CTYPE/CRVAL/CRPIX/CD) shifted by the crop origin. Spline solutions are
approximated by their linear part (documented limitation).

**D. ROI blending.** `--roi x,y,w,h` restricts the output canvas (fast
problem-area iteration at full res).

Deferred to phase 3: FITS *input*, compressed XISF ingest, full Laplacian
pyramid (if two-band proves insufficient), wgpu GPU path, GUI.

### Phase 4 (in progress): pyramid base

Generalize the two-band blend to a full multiband scheme **without touching
the star guarantees**. The split: detail (<8 px + all star flux, since the
base is star-free by construction) keeps the phase-3 treatment verbatim —
hard per-pixel ownership, ±16 px ramp, star-lock, defect veto. The star-free
*base* replaces its single wide feather with a **Laplacian pyramid on the L8
cell grid**: levels at 8, 16, 32, … px scale (up to the feather scale), each
level blended with an ownership-mask pyramid whose transition width is
proportional to the level's scale. Mid-frequency structure (8 px–feather)
thus gets seam-switched over distances matched to its wavelength instead of
being averaged across the whole feather — the classic Burt–Adelson result —
while everything runs on the small L8 grid (e.g. 1157×2289 for the Orion
set), so cost is negligible and out-of-core pyramid plumbing is avoided
entirely. `BlendMode::Pyramid` becomes the default; TwoBand and Feather stay
for comparison. The benefit case: panels with differing PSF (seeing/focus)
or few-pixel misregistration, where the feathered base would soften or
double mid-scale structure.

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
- **User validation (PixInsight, unlinked screen stretch): "best mosaic merge
  I've seen on that mosaic."** No star issues in seams, no bad gradients, no
  odd colours. Notably: other tools "dig holes" in the region between M42 and
  the Running Man (fighting gradient mismatches in signal-dominated overlap);
  mmm does not — because it fits levels globally and never lets the blender
  reconcile signal. **Design guard rail derived from this:** the phase-2
  residual surface correction must be prevented from absorbing signal
  differences (fit background cells only, robust clipping, low order, capped
  magnitude) or we will reintroduce exactly this failure mode.
- ⚠ finding: PixInsight stores plate solutions as XISF `<Property>` elements,
  NOT FITS keywords — the registered panels carry no CRPIX/CRVAL/CTYPE
  keywords. WCS passthrough requires parsing `<Property>` elements (todo,
  phase 2); `keywords_for_output`'s CRPIX shifting is implemented and tested
  but currently has nothing to shift on real data.

## Phase 2 results (2026-07-24)

All four phase-2 tasks landed (75 tests, clippy clean). Real-data numbers,
12-panel Orion set, warm cache:

- analyze (now incl. detail planes, surfaces): **5.2 s**; full-res TwoBand
  blend + WCS: **6.1 s**; ROI 2000×2000: **1.3 s**.
- **Surfaces**: inter-panel overlap residual RMS improved 1.9× globally
  (3.3× on worst edges); max|s| ~1e-4–3e-4, all under the 5×MAD guard.
  Honest finding: much of the visible large-scale colour cast is *common to
  all panels* (real sky gradient/IFN) — out of scope by design; the guard
  rail worked (star regions moved the fit ≤1.3% in tests).
- **Two-band + seams**: anti-pinching e2e proves merged stars match ONE
  panel (≤0.0006 vs noise threshold 0.024) under 0.6 px synthetic
  misregistration, while feather mode fails the same check (0.08–0.13).
  Implementation deviation (kept within design intent): the base band
  excludes starry + rim cells (onion-peel background fill) — raw cell means
  near stars otherwise imprint colour blobs; rim cells leak garbage.
  Trade-off to watch: single-panel defects (cosmic rays, satellite trails)
  in overlaps are no longer averaged down — the owner shows them at full
  strength.
- **WCS**: PixInsight 1.9.4 stores the solution in `PCL:AstrometricSolution:*`
  properties (inline base64 f64 vectors/matrices, not attachments); pure
  linear Gnomonic → exact TAN cards, center error 0.0″, crop-shift invariant.
  Downsampled previews get no WCS (CD would need rescaling — refusing to lie).

## Phase 3 results (2026-07-25)

Quality-only phase (user directive: no GPU). 97 tests, clippy clean.

- **Connected star masks** (spike-kink fix): flood-fill masks (seed 3×,
  grow 1.5× median detail) follow diffraction-spike arms out from cores;
  used by seam DP cost (+100× median on masked cells), star-lock, and base
  exclusion. Spike-integrity test: merged arms match ONE panel under 0.8 px
  shift + 0.02 rad spike rotation (fails with masks off). Real-data diff vs
  phase 2: background identical; seam-adjacent stars now owned whole.
- **Defect veto**: in overlaps, detail outliers vs the partner panel
  (>6× the cleaner panel's cell RMS, star-mask-clear both sides) take the
  smaller detail — cosmic residue/satellite trails are suppressed instead of
  shown at full strength. Plan's owner-RMS threshold was self-defeating (a
  trail inflates its own cell RMS); min-of-two used instead. Real data:
  ~1.9k sub-visible single-pixel fixes, star cores byte-identical.
- **Diagnostics**: `mmm report --seam-png` renders the owner map (tinted
  regions, boundaries, panel ids) over the preview; per-edge `seam Δ` (mean
  |corrected_a−corrected_b| over boundary cells, ⚠ >3× median). Orion: all
  seams ≤7e-5 linear; the only ⚠ edge borders PANEL-8's bright background;
  seams visibly detour around stars and thread the M42/Running Man corridor.
- **Opt-in `--flatten 1|2`**: global background poly fitted to the merged
  L8 background (star-mask + median+3×MAD excluded), folded equally into all
  panels' corrections — cross-panel math (seams, veto) provably unaffected,
  so nebula hole-digging is structurally impossible (verified). Halves the
  varying colour cast; remaining cast is genuine IFN. Not a GraXpert
  replacement; refuses when <20% background. Caveat: the background
  thresholds are relative, so a >80%-structure mosaic can evade the refusal.

## Performance notes

- Full-plane scan measured at ~0.9 GB/s single-threaded; analyze is I/O-bound
  with rayon. Target: 12-panel analyze in ~30 s cold, blend full-res in tens of
  seconds.
- FITS output is big-endian: byte-swap on write. Write ROWORDER='TOP-DOWN'.
- WSL2 caps RAM at 50% of host by default (`.wslconfig` to raise); mmap +
  streaming keeps us indifferent.
```
