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
panels: 3 rounds of ordinary least squares with 2.5σ residual clipping reject
outliers, then the reported line comes from a **detrended symmetric fit**
(reworked 2026-08-19, the gain-collapse fix — see below): each side's own
best-fit plane over the overlap is subtracted, so the slope is measured on
non-planar shared structure (stars, nebulosity texture) and smooth gradients
— shared or per-panel — cannot alias into it; the slope estimator is Deming
(equal noise variances), immune to the errors-in-variables attenuation that
biases y-on-x OLS toward 0 when signal variance ≈ noise variance. An
**identifiability guard** (detrended correlation r ≥ 0.5) catches overlaps
that cannot support a gain measurement — pure noise and one-sided structure
both measure r ≈ 0 — and reverts them to `gain = 1` + mean-level match,
flagged `gain_identifiable: false` (report shows `-`).

Global adjustment, per channel, decoupled: **gains** are node potentials in
log space (`λ_a − λ_b = ln gain` per identifiable edge, weighted by relative
cell count), solved to an **L1 objective via IRLS** — on a potential problem
L1 concentrates loop inconsistency onto the fewest edges, so a slope
contaminated by structure only one panel sees (residual vignetting, a
reflection) is outvoted by the loop-consistent majority instead of dragging
its neighbourhood; a weak ridge pulls unconstrained gains to 1, and the
largest panel of each component is gauged `g=1, o=0`. **Offsets** then follow
linearly from the corrected mean-level constraints (graph Laplacian). Never
chain raw second moments through the global solve — their noise terms
re-introduce the attenuation bias.

`--gain fit|unity` (analyze): `unity` pins every gain at 1 and solves offsets
only, for mosaics known photometrically homogeneous; recorded in
`session.json` and shown by `report`, which also warns when solved gains
leave [0.5, 2].

History (2026-08-19, RickJay 25-panel Ha Barnard's Loop set): the original
solve — per-edge OLS chained as raw moments — collapsed gains to 0.009–0.16
because the faint-background overlaps are noise-dominated (EIV attenuation
per edge, compounded multiplicatively across the 5×5 grid from one gauge
panel), crushing every non-gauge panel's signal. The rework recovers gains
0.87–1.5 on that set, and star-flux checks confirmed the surviving spread is
*real* per-panel transparency variation (up to ~2.4×), correctly compensated.
Orion aligned set: gains and output visually unchanged.

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
                      # + input kind and the mosaic frame for solved input
  panels/<id>/summary.bin   # L8 planes: mean[ch] + coverage, f32
  panels/<id>/aligned.bin   # solved input only: reprojection cache (planar f32)
  analysis/overlap_graph.json
  analysis/photometry.json  # per-edge fits + global gains/offsets per channel
```

## CLI surface

```
mmm info <files…> [--stats]
mmm analyze <panels…> --session S [--surface off|0|1|2] [--input auto|aligned|solved]
            [--gain fit|unity]
mmm report --session S [--seam-png P]   # graph + fit/seam tables, ⚠ on outliers
mmm blend --session S -o out.fits [--downsample 1|8] [--feather PX]
          [--mode pyramid|twoband|feather] [--png P] [--roi x,y,w,h]
          [--defect-veto on|off] [--flatten off|1|2] [--wcs-frame topdown|flipped]
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

### WCS north–south mirror: the full saga (2026-07-25, RESOLVED)

Three rounds, preserved here so nobody re-litigates it. The final state:
**top-down pixel rows + ROWORDER='TOP-DOWN' + WCS cards verbatim in the PI
top-down convention** — the original phase-2/3 format.

Round 1: a "seems mirrored" report led to reflecting the cards into the
bottom-up frame (CRPIX2 → H+1−CRPIX2, negate the CD dy column) while keeping
top-down storage. The verifying agent's own checker contained the same flip
it applied, so it "confirmed" a fix that actually inverted a correct
solution. Round 2 (user annotation screenshot = ground truth): still
mirrored; independent re-verification proved the verbatim cards had been
data-index-correct all along. We then stored rows bottom-up with the
reflected (standard-frame) cards — self-consistent for Astropy — but a
second annotation test STILL mirrored. The two screenshots together pin
PixInsight's actual behaviour: it normalizes pixel rows via ROWORDER but
interprets WCS cards in **display (top-down) space**. With top-down storage,
display space and data-index space coincide, so verbatim cards are correct
for PixInsight AND Astropy/DS9 simultaneously (catalog-star verified:
Alnitak/Alnilam/Mintaka/Trapezium at predicted indices). Round 1's report
appears in hindsight to have been a misdiagnosis. Lessons recorded: verify
fixes with tooling independent of the fix, and treat a user screenshot as
the ground truth over any model of a third-party reader. No flip property
exists in the XISF — orientation is entirely in the matrix convention.

## Phase 4 results (2026-07-25)

`BlendMode::Pyramid` (new default) landed: star-free base blended as an
L8-grid Laplacian pyramid with scale-proportional seam transitions; detail/
star path untouched. 107 tests, clippy clean.

- Mid-frequency ghost metric (24 px blobs displaced 3 px between panels):
  **82% RMS reduction** vs the feathered TwoBand base (bound was 30%);
  ratios 0.02–0.18 across seeds. All phase-3 guarantees re-proven in
  Pyramid mode; Feather/TwoBand outputs hash-identical to before.
- Real data: full-res blend 8.2 s; diff vs TwoBand sub-noise everywhere
  (RMS 1.8e-5, p99 < 1e-4 vs data floor 1e-3+), concentrated in seam
  transition zones; seam map byte-identical (ownership unchanged). On this
  well-registered set the pyramid is insurance, not a visible improvement —
  its benefit case is differing PSF or few-px misregistration between panels.
- WCS N–S mirror: see "§WCS north–south mirror: the full saga" above for the
  final resolution (verbatim top-down cards + top-down rows).
- Seam-through-structure fix (user-reported staircase in M42's bright edge):
  the connected star mask floods across extended nebulosity, which used to
  hard-snap the detail transition along the 8-px cell boundary exactly where
  panel mismatch is largest. Masks are now split by component morphology —
  compact components (stars + spike arms; area ≤ 40 cells, bbox dim ≤ 12, or
  thin cross shapes) keep the snap; extended structure ramps with a widened
  32 px half-width. Base exclusion stays FULL-mask (compact ∪ structure) —
  a compact-only attempt caused a dark-moat regression around haloed bright
  stars (reflection halos flood components past compactness → halo entered
  the pyramid base → asymmetric Laplacian mixing near boundaries dug a wide
  dark annulus; user-reported, reproduced synthetically red/green). Ramped
  detail contributions also fade over each panel's last 32 px of coverage
  (rim fade) so transitions forced onto coverage rims complete inside
  coverage. Verified: staircase gone (metric 0.0015 vs 0.028 pre-fix),
  moats gone, stars pixel-identical, all guarantees green.
- Dark-streak fix (user-reported, 2026-07-25): broad dark bands (~1e-4 deep,
  hundreds of px wide) in panels' single-coverage zones next to seam bands.
  Cause: `mask_pyramid` smoothed ownership masks with no validity limit, so
  at coarse levels a partner's mask support extended ~700 px past its
  geometric coverage — where its data pyramid is the normalized-convolution
  0.0 sentinel or a baseless extrapolation; blending those dragged the base
  down. Fix: mask chain + stored levels are clamped to zero where the
  identically-downsampled validity (the same plane `build_masked` uses)
  falls below `MASK_SUPPORT_MIN` — a panel's blend weight is now strictly
  zero beyond its coverage dilated by ~2 cells of each level's grid, and
  wherever its mask is nonzero its data is a genuine normalized convolution.
  Deep-single-coverage identity is enforced by e2e test (panel influence
  measured zero beyond 454 px at feather 256 vs ~1000 px pre-fix; deliberate
  0.13 background mismatch). Real data: detached streaks gone (0 blocks
  < −5e-5 beyond 384 px of any overlap, was 1108), residual differences
  confined to the seams' transition widths at less than half the depth;
  ghost metric unchanged (ratio 0.181); M42 core byte-level unchanged;
  blend 8.1 s. Pyramid hash recaptured; Feather/TwoBand byte-identical.

## Phase 5 results (2026-07-25): unaligned solved-panel input

The MosaicByCoordinates prerequisite is gone: `analyze` accepts raw panels
carrying PixInsight astrometric solutions (`--input auto|aligned|solved`,
default auto). 132 tests, clippy clean.

- **Full WCS model** (`astrometry::WcsModel`): PI's precomputed
  `PointGridInterpolation` spline grids (both directions) are interpolated
  bicubically — no thin-plate-spline evaluation. Cross-frame star chain on
  real data validated the whole model at 0.034 px median (all verified layout
  facts live in the module docs).
- **Frame + reprojection** (`align.rs`): fresh TAN frame (spherical mean
  center, median scale, union footprint + 16 px margin) whose rotation is
  the axial mean of the tiles' own rotations (doubled-angle mean, so
  meridian-flipped panels agree) — a camera rotated away from north gets a
  canvas hugging the tiles instead of an axis-aligned box with black
  corners; axis-aligned tiles compute rotation exactly 0 = the historical
  north-up frame, bit-identical. Lanczos-3 reprojection with a hard
  full-support-or-zero rim into mmap-able session caches; `PanelReader`
  gives the scan and blender storage-agnostic row access. Aligned-input
  artifacts and blends stayed byte-identical through the refactor (hash
  regression guard).
- **Auto-detect** (binding rule): same geometry AND ≥ 2 panels AND every
  panel's covered fraction < 50% → aligned; otherwise solved, where every
  input must yield a model (per-file error naming the missing properties).
  Geometry is checked from headers before any scan; the coverage rule is
  applied after the aligned scan and re-dispatches to solved when violated.
  `--input` overrides in both directions (needed for the rare undetectable
  case of same-geometry raw panels with < 50% coverage, and for ≥ 50%-overlap
  2-panel aligned mosaics).
- **Output metadata**: solved sessions persist the fresh frame + input kind
  in `session.json`; `blend` emits the frame's WCS cards and filters the
  panel-0 geometry/pointing cards (RA/DEC/CRVAL…/CD…) from FITS-keyword
  passthrough — non-geometric metadata (EXPTIME, INSTRUME, …) still passes.
- **Synthetic e2e**: analytic sky cut into four offset-geometry panels with
  linear solutions including 0–3° rotations → solved pipeline → RMSE
  1.7–1.8e-3 vs the noiseless truth (phase-1 bound 2σ = 4e-3).
- **Real acceptance** (12 RAW Orion panels vs the registered-input phase-4
  output): auto-detected solved; chosen frame 9286×18341 @ 1.5974″/px
  (registered canvas: 9255×18310 @ 1.597″/px, center 0.7″ apart). 25 bright
  stars detected in both outputs by local centroid and matched through the
  two WCS solutions: **median residual 0.030 px, max 0.224 px** (bound 1 px).
  Value difference over 17.6k sky-mapped samples: median 9.0e-6, p90 2.6e-5,
  p99 1.7e-4 vs a ≥ 1e-3 data floor — sub-noise everywhere; mean
  raw/registered ratio 0.9997 (the two sessions share the same gauge panel).
  Timings, warm cache: align (12 reprojections) 8.1 s, analyze total 10.6 s,
  full-res blend 6.9 s. Cold not directly measurable in this environment
  (no root to drop caches); first partially-cold run measured align 11.0 s,
  and the 24 GB of source reads bound a fully cold align well under the 60 s
  target at the measured ≥ 0.9 GB/s scan rate.

## Performance notes

- Full-plane scan measured at ~0.9 GB/s single-threaded; analyze is I/O-bound
  with rayon. Target: 12-panel analyze in ~30 s cold, blend full-res in tens of
  seconds.
- FITS output is big-endian: byte-swap on write. Write ROWORDER='TOP-DOWN'.
- WSL2 caps RAM at 50% of host by default (`.wslconfig` to raise); mmap +
  streaming keeps us indifferent.

## IPC transport (PixInsight)

A native C++ PCL module (Plan 2) can drive mmm without writing panels to
disk first by spawning `mmm-ipc-worker` — a Rust process that links
`mmm-core` and does the actual analyze/blend — and talking to it over a
control channel (the worker's stdin/stdout, length-prefixed frames) plus a
named shared-memory segment for bulk pixels. Panel bands are pulled by the
worker on demand (request → host fills a shm slot → reply), and blended
output bands stream back the same way in reverse, so neither side ever
needs a second full-canvas-resident copy of the mosaic; a worker crash or
panic is isolated to its own process and cannot take down the host. Three
job modes cover the input side: `Aligned` (already-registered full-canvas
panels, read as-is), `Solved` (raw panels carrying a PixInsight plate
solution, reprojected by mmm's phase-5 path before blending), and `Files`
(plain file paths, bypassing the shm band-pull entirely on input).

The Rust-side transport (`crates/mmm-core/src/ipc/`) and `mmm-ipc-worker`
are implemented and tested; the C++ host module itself is Plan 2, not yet
built. See the
[design spec](superpowers/specs/2026-07-27-pixinsight-integration-design.md)
for the full design rationale and the
[wire protocol reference](../integration/pixinsight/PROTOCOL.md) for the
exact frame format, every tag, binary field layouts, JSON schemas, and shm
slot layout — the latter is the authoritative reference for the Plan-2 C++
implementer.
