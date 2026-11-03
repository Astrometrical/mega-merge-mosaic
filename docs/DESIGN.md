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
  store. **Compressed inputs** (WBPP writes compressed masters, 2026) are
  decoded once into the session's decode cache and mapped from there — see
  "Formats come from mpp-core" below.
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

**Input precondition: one channel count for the whole set.** Everything
downstream assumes it — the canvas carries a single `channels`, the IPC host
sizes its shm slots from the first panel's, and the photometric solve is per
channel across panels. A mono/colour mix is therefore refused up front, in
every entry point (`analyze_full`, `probe_panels`, `analyze_ipc_aligned`,
`solved_frame`), with a message naming the first input of each channel count;
without that check the mix surfaced far from its cause — `--input auto` reads
the differing geometries as unregistered panels and the run dies complaining
about a missing plate solution.

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
leave [0.5, 2]. On the PixInsight wire it travels as `BlendParamsWire.gain`
(`"fit"`/`"unity"`, serde-default `"fit"` so pre-field hosts keep working);
the module exposes it as the **Gain mode** control (PROTOCOL.md §6 is the
authoritative reference).

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

Output is clamped to [0, 1] (2026-08-20): saturated input pixels (clipped at
1.0) owned by a gain > 1 panel scale above 1.0 under the photometric
correction — their true flux is unknowable, and out-of-range floats make
PixInsight's auto-STF clip the region to black (user-reported on the RickJay
M42 core; 1.5k such pixels mosaic-wide, every saturated star core owned by a
g > 1 panel, values clustering exactly at the per-panel gains). Lanczos
undershoot beside saturation cliffs and offset-corrected noise tails dip
marginally below 0. Both ends clamp in the shared band sanitize step (all
modes, full-res + L8 + IPC); the count is logged at info level.

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

Deferred to phase 3: compressed XISF ingest (landed 2026-10-29, see
"Formats come from mpp-core"), full Laplacian pyramid (if
two-band proves insufficient), wgpu GPU path, GUI (FITS input landed
2026-09-26, below).

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

### Masked-base detail reference (2026-08-19)

RickJay real data (25-panel Ha, ~450 px overlap bands) exposed a pyramid/
twoband failure at M42: the saturated core sits on a 4-panel corner whose
overlap is entirely bright structure, ownership boundaries are forced through
the masked complex, and the output printed giant over/undershoot blocks with
detail stripped. Root causes and fix — three mechanisms in `blend.rs`, spec in
[the design doc](superpowers/specs/2026-08-19-masked-base-detail-reference-design.md):
the onion fill is two-phase (local passes any brightness, long-range passes
background-level only — a detail-flat saturated plateau evades the star mask
and used to flood one panel's base with ~0.95 over tens of thousands of
cells); the detail band references the *blended* base instead of each
panel's own fill inside deep masked complexes plus a feather-scaled halo
(shared switch plane `ref_t`; base sins cancel exactly there, output =
ramp-mix of corrected panels, i.e. feather behaviour exactly where base
content is fiction); star/blob-sized masks never trigger the switch, so all
phase-3/4 guarantees and the pyramid's mid-frequency benefit are preserved
(192 tests, ghost ratio unchanged, Feather byte-identical).

## Session directory

```
<name>.mmm-session/
  session.json        # canvas geometry, panel list+paths, stage stamps, params
                      # + input kind and the mosaic frame for solved input
                      # + frame_imposed when a shared reference frame was adopted
  panels/<id>/summary.bin   # L8 planes: mean[ch] + coverage, f32
  panels/<id>/aligned.bin   # solved input only: reprojection cache (planar f32)
  decoded/<key>.f32         # decode cache: non-mappable (e.g. compressed) XISF
                            # panels decoded once, planar f32; deletable
  analysis/overlap_graph.json
  analysis/photometry.json  # per-edge fits + global gains/offsets per channel
<name>.mmm-frame.json # shared reference frame (mmm frame / batch), outside any session
```

A **mosaic session** (`mmm init`, 2026-11) wraps one such session per group:

```
<name>.mmm-session/
  mosaic.json               # panel list (+ file fingerprints), grouping rule,
                            # analyze/blend settings + per-group overrides,
                            # group_dirs (group name -> directory, persisted),
                            # accepted check keys
  reference.mmm-frame.json  # the shared frame all groups adopt
  groups/<dir>/             # an ordinary session in the layout above
  groups/<dir>/result.mmmt  # tiled result store (see "Standalone app groundwork");
                            # written by blend_group (the app), not `mmm run`
```

`<dir>` is a filesystem-safe, case-insensitively unique form of the group
name, chosen once and kept in `mosaic.json` so renaming rules never orphans a
cache. A directory holding `session.json` and no `mosaic.json` is a legacy
single-group session; `analyze`/`blend`/`report` still work on it and refuse
a mosaic directory.

## CLI surface

```
mmm info <files…> [--stats]
mmm frame <all panels…> -o F.mmm-frame.json [--input auto|aligned|solved]
mmm analyze <panels…> --session S [--frame F] [--surface off|0|1|2]
            [--input auto|aligned|solved] [--gain fit|unity]
mmm report --session S [--seam-png P]   # graph + fit/seam tables, ⚠ on outliers
mmm blend --session S -o out.fits [--downsample 1|8] [--feather PX]
          [--mode pyramid|twoband|feather] [--png P] [--roi x,y,w,h]
          [--defect-veto on|off] [--flatten off|1|2] [--wcs-frame topdown|flipped]
          [--extent auto|union|canvas]
mmm batch --group NAME <panels…> [--group …] -s DIR -o OUTDIR [--png]
          [analyze options] [blend options]
mmm init <panels…> -s DIR [--group-by header:KEY|token:KEYWORD] [--force]
         [--surface] [--input] [--gain] [--mode] [--feather]
         [--defect-veto] [--flatten]       # mosaic session (no --session default)
mmm set -s DIR [--group NAME] [same settings flags]  # prints the change impact
mmm check -s DIR                  # groups + problems / notes / passed count
mmm run -s DIR -o OUTDIR [--png] [--wcs-frame topdown|flipped]
                                  # analyze all groups on one frame, blend each
```

`init`/`set`/`check`/`run` operate on mosaic sessions and require `--session`.
`run` is the batch counterpart of what the app does: `analyze_all`, then one
blend per group through the streaming `blend` path to
`OUTDIR/<group dir>.fits`. It never writes `result.mmmt` (the app's
`blend_group` does) and it re-analyses and re-blends every group on each
run; the staleness `set` reports is tracked for the app, not used by `run`.

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
- **WCS**: PixInsight ≥ 1.9.5 stores the solution as the XISF rev 1
  `AstrometricSolution:*` block (layers: linear, projective 3×3, RBF spline
  distortion), which `astrometry/standard.rs` decodes and samples onto lookup
  grids; ≤ 1.9.4 stored `PCL:AstrometricSolution:*` with precomputed grids
  (`astrometry/legacy.rs`, inline base64 f64 vectors/matrices). Pure linear
  Gnomonic → exact TAN cards, center error 0.0″, crop-shift invariant.
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
- **Auto-detect** (binding rule): same geometry AND ≥ 2 panels AND canvas
  solutions agree AND every panel's covered fraction < 50% → aligned;
  otherwise solved, where every input must yield a model (per-file error
  naming the missing properties). Geometry and the solution tie-breaker
  (`reference::same_geometry_reads_aligned`, amended 2026-10-04: registered
  canvases carry one identical canvas WCS, raw panels from one camera each
  carry their own; no WCS at all keeps the geometry rule) are checked from
  headers before any scan, so same-size raw panels go straight to the solved
  path; the coverage rule is applied after the aligned scan and re-dispatches
  to solved when violated (registered canvases without a WCS). `--input`
  overrides in both directions (needed for ≥ 50%-overlap 2-panel aligned
  mosaics, and for same-size raw panels that carry no solution). The
  PixInsight module's Views path classifies same-size views through the
  worker's `--probe-reference` with the same rule.
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

## PixInsight 1.9.5 / XISF revision 1 (2026-09-19)

PixInsight 1.9.5 (PCL 2.10.8, API 0x0188) adopts XISF 1.0 revision 1, whose
standard `AstrometricSolution:*` block replaces the legacy
`PCL:AstrometricSolution:*` properties (the core deletes the legacy ids when
it regenerates a solution). Spec:
[2026-09-19-pixinsight-1-9-5-xisf-rev1-design.md](superpowers/specs/2026-09-19-pixinsight-1-9-5-xisf-rev1-design.md).

- **Decoder** (`astrometry/standard.rs`, `astrometry/spline.rs`): layer 1
  (projection), layer 2 (projective 3×3 both ways), layer 3 (RBF surface
  splines: ThinPlateSpline / VariableOrder / Gaussian / Multiquadric /
  InverseMultiquadric / InverseQuadratic, Global + Local + Fallback terms
  with Wendland C2 weights), evaluated per the spec and PCL's reference
  implementation, then sampled onto the existing `Grid2D` lookup grids (16 px
  spacing, lazily per direction on first use) so `WcsModel` and the align
  stage are unchanged. Unavailable
  layers fall back per the spec (with a warning). PixInsight's private
  `PCL:AstrometricSolution:{Grid,Generation}:*` extras are ignored.
- **Verified** on a 1.9.5-regenerated Orion raw panel (Global TPS, order 2,
  ~3.1k nodes per component, separate X/Y node sets): our sampled grid vs
  PixInsight's own grid cache agrees to < 0.001″ over the whole field;
  standard vs the 1.9.4 legacy solution of the same panel agrees to ≤ 0.07″
  (all 12 panels). Cost: at the original 8 px spacing, eager sampling of both
  grids took ~1 s per 4898×3230 panel and was repeated by every worker pass
  (panel probe, frame probe, analyze) — 12 panels ≈ 15 s before the frame.
  Now 16 px (worst deviation from PixInsight's 8 px cache 0.0023″) and lazy
  per-direction sampling: decode-to-frame ≈ 2.4 s for 12 panels, inverse
  grid ≈ 0.4 s per panel paid once in the align stage (11.5 s vs 6.8 s),
  whole solved run 15.8 s vs 23.2 s. The lazy `OnceLock` init must never
  run a nested rayon sampling from inside a rayon worker (work stealing
  deadlock, seen live): `reproject_core` calls `WcsModel::ensure_grids()`
  first, and an in-worker first use samples sequentially.
- **Legacy files stay readable** (`astrometry/legacy.rs`); a file carrying a
  standard block is read through it exclusively.
- **Build**: one PCL pin (2.10.8) for every arch; module requires PixInsight
  ≥ 1.9.5 by design (v1.4.2 stays available for 1.9.0–1.9.4). The in-repo
  update-repository packaging scripts were removed (the tools website owns
  distribution).

## FITS input (2026-09-26)

FITS panels are a peer input format everywhere (CLI and the PixInsight
Files path); spec:
[2026-09-26-fits-input-sip-design.md](superpowers/specs/2026-09-26-fits-input-sip-design.md).

- **Reader** (`formats/fits.rs`): primary HDU, NAXIS 2/3 planar, BITPIX
  8/16/32/−32/−64; rows decoded on demand into the per-thread band cache
  (`band_cache.rs`, shared with the IPC backing). Integer data normalized to
  [0, 1] by the type range after BZERO/BSCALE; NaN/Inf/BLANK → 0; negative
  integer samples → 0; float samples keep their sign.
  Extension HDUs, fpack, BITPIX 64 refused by name.
- **Orientation**: `ROWORDER = 'TOP-DOWN'` files read verbatim; everything
  else (standard FITS) is flipped on read and the WCS reflected into the
  top-down frame (`LinearWcs::reflect_rows`). PixInsight-exported FITS
  carries no astrometric solution at all (PixInsight dropped WCS keywords
  from its FITS output some time ago; XISF is its only solved format), so a
  PixInsight FITS can only be *aligned* input, never solved input — the
  display-space card question from the mirror saga never arises for it.
- **WCS** (`astrometry/fits_wcs.rs`): TAN / TAN-SIP only; CD, PC+CDELT, or
  CDELT+CROTA2. TPV/PV refused. **SIP** (`astrometry/sip.rs`): forward
  polynomials; AP/BP only seed a Newton inverse (exact either way);
  validated like the standard solution; sampled onto the same 16 px grids.
  Verified against astropy on four synthetic headers
  (`tests/fixtures/sip_oracle.json`): < 1e-9° / 1e-4 px on direct
  evaluation, 2e-7° / 5e-3 px through the sampled grids.
- **Refusals**: FK4 `RADESYS`, `EQUINOX` ≠ 2000, `LONPOLE` ≠ 180, non-`deg`
  `CUNIT`, and a present-but-unparseable `*_ORDER` are errors, never
  approximated.
- **Output**: SIP cards never pass through; aligned FITS sessions emit
  fresh linear cards from the reflected WCS (input SIP is not propagated —
  the output carries only the linear WCS). BLANK, DATAMIN/DATAMAX and
  CHECKSUM/DATASUM are dropped from the passthrough.
- **Memory**: a solved-mode FITS panel is materialized once in anonymous
  memory by `reproject_from_reader` (same footprint as the XISF mmap path,
  ~195 MB for a 4944×3284×3 Orion raw panel). In aligned mode the band cache
  allocates cells lazily: `cells_touched × 16 × width × channels × 4` bytes
  per open panel — ≤ 16 × 1.78 MB ≈ 28 MB for a 9255-wide RGB Orion panel
  at 16 threads (was ~121 MB eagerly at 64 rows).
- **Real data** (2026-09-27, `scripts/nova_solve_panels.py` +
  `compare_mosaics.py`): raw Orion panels 3/4/7/8 solved privately by
  nova.astrometry.net (tweak_order 3, all first try; `A_ORDER` 3, SIP corner
  terms 2–9 px except panel 7's 21 px) → bottom-up TAN-SIP FITS, vs the
  PixInsight-solved XISF originals. Analyze 5.1 / 5.1 s, blend 8.1 / 6.2 s
  wall (FITS / XISF). Graded star offsets vs catalogue, FITS / XISF: θ² Ori A
  1.71 / 1.28 px, ι Ori 0.74 / 1.39, 42 Ori 0.97 / 0.75 (brief's 42 Ori
  coordinate was ~140 px off; θ¹ Ori C ungraded — clipped core merges θ¹ A/B/D).
  3.3k-star run-to-run median 0.44 px, no constant shift. Channel 0 raw RMS
  21.0 % (5 of 13,963 lattice points on star wings = 92 % of squared diff,
  shifted 1–2 px between the two solutions), trimmed RMS 0.58 %, median
  0.99 %. Caveat: nova's order-3 SIP is less accurate than PixInsight's
  spline solution (worst in panel 7's corners, run-to-run p90 2–4 px).
  PNGs: same orientation, clean seams. **PASS** (trimmed gate).
- Follow-up (fixed 2026-10-29, mpp-core's FITS writer): the output FITS
  writer passed XISF keyword values with lowercase exponents (`2.0496e+03`)
  and wrote `COMMENT =` cards; astropy flagged both as non-standard. Real
  exponents are now uppercased, commentary cards carry no `= `, and
  `CONTINUE` long strings pass through verbatim.

## Formats come from mpp-core (2026-10-29)

All XISF and FITS handling lives in the sibling Mega Pixel Processor repo
(`mpp-core`, built `default-features = false`): XISF 1.0 baseline-decoder
conformance at spec revision 1.02 — every codec (`zlib`/`lz4`/`lz4hc`/`zstd`,
`+sh`), subblocks, SHA-1/2/3 checksums, both byte orders and storage models,
UInt8/16/32 and Float32/64, multiple images, compressed property blocks —
and the FITS reader/writer that used to live here (mpp `docs/DESIGN.md`,
"XISF conformance"). mmm keeps the adapters: `InputPanel`, `PanelReader`,
`FitsSink` (a `RowSink` over mpp's `FitsWriter`) and the astrometry glue.
The value types crossing the PixInsight IPC (`FitsKeyword`, `XisfProperty`,
`PropertyValue`) are mpp's, serde-identical to the former mmm ones
(`tests/wire_golden.rs` pins the form in both repos).

- **Decode cache.** An uncompressed full-canvas Float32 panel is still read
  from its own mmap. Any other XISF panel is decoded once (`crate::decoded`)
  into `<session>/decoded/<key>.f32` — written under a temporary name and
  renamed, keyed by canonical path, size, mtime, image id and block checksum
  — and served through `PanelReader`'s existing `Cache` backing, so analyze,
  reprojection and blending are unchanged. Cost: time on first use and as
  much disk as the decoded panel (a 2 GB panel → 2 GB cache).
- **Notice.** Every decode is announced: the CLI prints a boxed notice on
  stderr; the PixInsight worker sends it as a completed `Progress` frame,
  whose stage text the module prints verbatim on the console — no protocol
  or module change.
- **Smoke test (2026-10-29), M42 panels 3/4/7/8** re-saved by PixInsight as
  zstd+sh + SHA-256 (2.03 GB → ~122 MB each):
  - The blended output is byte-identical (header and 708 MB of data) to the
    uncompressed set's.
  - Analyze took 14.8 s on first use (four parallel 2 GB decodes, peak RSS
    16 GB) against 1.3 s for the uncompressed set, and 1.4 s on a re-run,
    which reused the cache silently.
  - Follow-up: parallel decodes each hold a full-size scratch buffer for
    `+sh` codecs. Bound the decode concurrency (or stream the unshuffle) if
    memory matters on smaller machines.
- **Astrometric solutions** (2026-10-29) are decoded by mpp-core too:
  - PixInsight's standard block and the legacy block, and FITS TAN /
    TAN-SIP (`mpp_core::astrometry`).
  - mmm keeps `WcsModel`: grid sampling (`astrometry/sampling.rs`), its own
    `Grid2D` evaluation, and `validate_grids` on legacy grids.
  - The linear WCS for output headers comes from mpp's lenient
    `linear_layer`, so a broken higher layer still yields it.
  - Parity on real data: analyze + blend of `orion_mosaic_raw_panels`
    (legacy solutions) and `orion_mosaic_raw_panels_195` (standard
    solutions, full distortion) give byte-identical sessions and blend
    output, header included, to the pre-switch binary.
- **Dependency on mpp-core.** A git dependency on the public
  mega-pixel-processor repo, pinned by `rev` in the workspace `Cargo.toml`
  (CI checks out only this repo). Bump the rev deliberately, after the mpp
  commit is pushed. For co-development against the sibling checkout, put
  this in a gitignored `.cargo/config.toml` (paths are relative to the
  directory holding `.cargo/`):

  ```toml
  [patch."https://github.com/Astrometrical/mega-pixel-processor"]
  mpp-core = { path = "../mega-pixel-processor/crates/mpp-core" }
  ```

  Cargo then builds the local checkout; `cargo tree -i mpp-core` shows which
  one is in use. While patched, cargo drops mpp-core's `source = "git+…"`
  line from `Cargo.lock`; don't commit that hunk (`git checkout Cargo.lock`)
  — the committed lock must keep the git source CI resolves.

## Performance notes

- Full-plane scan measured at ~0.9 GB/s single-threaded; analyze is I/O-bound
  with rayon. Target: 12-panel analyze in ~30 s cold, blend full-res in tens of
  seconds.
- FITS output is big-endian: byte-swap on write. Write ROWORDER='TOP-DOWN'.
- WSL2 caps RAM at 50% of host by default (`.wslconfig` to raise); mmap +
  streaming keeps us indifferent.

## Shared reference frame across groups (2026-10-03)

Mono imagers merge one mosaic per filter and combine afterwards, which needs
every output on one pixel grid. Two mechanisms broke that: `choose_frame`
derives the frame from the panels it is given (per-filter pointings differ by
fractions of a pixel), and the blend crops to the union of content bboxes
(per-filter coverage differs). Spec:
[shared reference frame design](superpowers/specs/2026-10-03-shared-reference-frame-design.md).

- **`reference::ReferenceFrame`** (`*.mmm-frame.json`, version 1): `solved`
  wraps a `MosaicFrame`; `aligned` wraps the canvas geometry plus the first
  panel's canvas WCS. No channel count — OSC and mono groups may share one.
  `reference::derive` is header-only over *every* panel of *every* group
  (the header-level auto-detect rule, including the canvas-WCS tie-breaker
  for same-size panels).
- **`analyze --frame F`** adopts the frame instead of choosing one. Solved
  input: footprint check — every panel's boundary samples must land inside
  the frame, else a hard error naming the panel, side and overshoot
  (`check_footprints`; no clipping by design). Aligned input: the canvas must
  equal the frame's and, when both carry a WCS, corners and centre must
  agree within `ALIGNED_WCS_TOLERANCE_PX = 0.05` px (`check_aligned`); the
  hint is tool-neutral ("align every group to one common reference, or
  process the groups separately"). Kind mismatch (solved frame, aligned
  set or vice versa) is an error. The session records `frame_imposed`.
- **Blend extent** (`blend::Extent`): `Union` (historical crop) or `Canvas`
  (whole canvas, zero outside coverage). Default: `Canvas` when
  `frame_imposed`, else `Union` — pre-existing sessions are byte-identical
  (regression guard unchanged). `--roi` intersects the active extent.
- **`mmm batch`** is CLI-only orchestration: derive once over all groups
  into `DIR/reference.mmm-frame.json`, analyze each group into
  `DIR/<name>.mmm-session`, blend each to `OUTDIR/<name>.fits`. Groups are
  `--group NAME panels…` occurrences (recovered through
  `ArgMatches::get_occurrences`; clap's grouped `Vec<Vec<T>>` derive is
  unstable).
- **Aligned-input caveat for users**: filters registered separately get
  separate canvases; either align all against one common reference or feed
  the raw solved panels.
- **PixInsight (stage 2, module 1.6.0)**: a flat `group` column on both
  panel tables; a multi-group run derives one reference via
  `--probe-reference` (views) or the `--probe-panels` reply (files), runs
  one worker job per group with `InitJob.reference` (honoured by all three
  job modes — the shm analyze paths gained the same reference parameter as
  `analyze_full`), names windows `MegaMergeMosaic_<group>`, and prints group
  headers; a single-group run is unchanged. Group helpers (partition,
  sanitised ids, wildcard filter) are PCL-free in `MmmGroups.h` and tested
  by the host CTest suite. Spec:
  [PixInsight multi-filter groups](superpowers/specs/2026-10-03-pixinsight-multi-filter-groups-design.md).

## IPC transport (PixInsight)

The PixInsight module (`integration/pixinsight/module/`, a C++ PCL module
built on the PCL-free host library in `integration/pixinsight/host/`)
drives mmm without writing panels to disk first. It spawns
`mmm-ipc-worker` (`crates/mmm-ipc-worker`), a Rust process that links
`mmm-core` and does the actual analyze/blend, and talks to it over a
control channel (the worker's stdin/stdout, length-prefixed frames) plus a
named shared-memory segment for bulk pixels. Panel bands are pulled by the
worker on demand (request → host fills a shm slot → reply), and blended
output bands stream back the same way in reverse, so neither side ever
needs a second full-canvas-resident copy of the mosaic; a worker crash or
panic is isolated to its own process and cannot take down the host. Three
job modes cover the input side: `Aligned` (already-registered full-canvas
panels, read as-is), `Solved` (raw panels carrying a PixInsight plate
solution, reprojected by mmm's phase-5 path before blending), and `Files`
(plain file paths, bypassing the shm band-pull entirely on input). Probe
modes (`--probe-panels`, `--probe-reference`, `--probe-frame`) answer the module's
pre-run questions, and multi-filter groups run one worker job per group on
a shared reference frame (see "Shared reference frame across groups").

All of it shipped: the module was released for Linux, Windows and macOS
through v1.7.0, and its development is now eased off (next section). The
Rust-side transport lives in `crates/mmm-core/src/ipc/`. See the
[design spec](superpowers/specs/2026-07-27-pixinsight-integration-design.md)
for the design rationale and the
[wire protocol reference](../integration/pixinsight/PROTOCOL.md) for the
exact frame format, every tag, binary field layouts, JSON schemas, shm
slot layout, and the host-side validation rules; the protocol reference is
authoritative for any host implementation.

The worker is not PixInsight-specific: it is the ready-made out-of-process
path should a standalone frontend ever want job isolation (an
out-of-memory failure on 2 GB frames would otherwise take the UI down
with it).

## PixInsight module status (2026-11-02): development eased off

The module (released through v1.7.0) is **expected to be shelved**; the
formal call is deferred, but no further feature work goes into
`integration/pixinsight/` until it is made. Fixes for users of the released
module are still fair game. Two reasons:

- **CPD rules 1.0.1** (published 2026-10-27, effective 2026-12-12). Rule
  5.8 forbids signed code from being "a front end, launcher or bridge for
  external programs", and rule 2.3 counts the developer's own worker as an
  external program. The module drives `mmm-ipc-worker`, so it is
  that front end. Mega Image Integrator shelved its module on these
  grounds on 2026-10-26; its ROADMAP §5 ruling "The PixInsight module is
  shelved" has the full reading of the rules and the open letter on
  "internal competitors", and it applies to mmm unchanged.
- **PixInsight 1.9.6 ships core mosaic features** that do much of what mmm
  does, so mmm also falls under the letter's second kind of internal
  competitor: projects that replace standard tools.

If it is shelved, the same terms as mii apply: the code stays in the tree
out of the release path (no signing, no repository publication), the module
CI job goes, and PixInsight consumes mmm's output as files. The standalone
application becomes the primary frontend; its direction is being worked out
separately.

## Standalone app groundwork (2026-11)

Phase 1 of the standalone app (spec:
`docs/superpowers/specs/2026-11-02-standalone-app-design.md`) gave `mmm-core`
what a desktop frontend needs, without changing any algorithm: every item
below is UI-agnostic and the CLI is its first consumer.

- **Job control** (`job`). `CancelToken` is a shared atomic flag;
  `JobCtl` bundles a token with a progress callback; `CtlSink` wraps any
  `RowSink`, checks the token per row and reports progress. Cancellation
  surfaces as `Error::Cancelled` (not a failure to log). `analyze_ctl` is the
  cancellable analyze; `analyze` is a wrapper with no-op control.
- **Settings** (`settings`). `AnalyzeSettings` / `BlendSettings` are the
  serialisable forms of the CLI options; `GroupOverride` holds per-group
  deltas (an explicit `null` is distinct from absent). Editing returns a
  `ChangeImpact` (`None` / `Reblend` / `Reanalyze`): analyze changes rerun
  analysis and stale every result tile; blend changes keep the analysis and
  stale only result tiles, so the UI can show exactly what needs recomputing.
- **Tile store** (`tiles`, file `result.mmmt`). The blended mosaic as a
  memory-mapped pyramid so a viewer reads it while a blend writes it.
  Layout: 4096-byte header (magic `MMMTILE\0`, version 1, tile size 256,
  width, height, channels, levels; all little endian); a per-tile `u32`
  state table (little endian; bit 31 = valid, bits 0-30 = sequence number,
  odd while being written; zero = never written);
  then f32 planar tile data per level, edge tiles stored full size and
  zero-padded. Level `l` is `ceil(w/2^l) x ceil(h/2^l)`, added until one tile
  holds it. Each state entry is a **seqlock** with a single writer: a reader
  copies a tile only if the entry is valid and even before the copy and
  unchanged after it, so a copy straddling any rewrite or invalidation,
  including a full rewrite, is rejected. A crash mid-write leaves the entry
  odd = invalid, and the next write recovers it. Samples are accessed as
  relaxed atomics so racing copies are never UB. `state()` reports a tile's
  `TileState` (`Missing` / `Stale` / `Valid`) from its entry alone, and
  `read_tile_any` copies a stale tile too under the same protocol, so the
  viewer can show stale tiles dimmed; `read_tile` accepts valid tiles only.
  `TileSink` is a `RowSink` that fills level 0 and, after each level-0 tile
  row, runs `build_parents` over that row, so the upper levels fill in
  during the blend (a parent is valid only when all its in-image children
  are; the rebuild checks cancellation once per parent tile row), and
  `blend_tiles` blends a canvas rect (an ROI re-blend invalidates only the
  tiles it touches) cancellably. A cancelled or failed blend keeps its
  finished work (spec §8): level-0 tiles it fully wrote stay valid, the
  rest of the rect stays stale, and only the parents over rows the sink
  wrote but had not yet rebuilt are rebuilt (the rest are already current
  or stay invalid), so a late cancel re-reads no finished tiles. The
  rebuild checks every in-image child's state before copying any. A store
  whose geometry no longer matches the analysis is dropped and recreated,
  as is a missing or unparseable one (`Error::Format`); any other open
  error is returned, never answered by truncating the file. Blending
  against stale analysis is refused.
- **Mosaic orchestration** (`mosaic`). `mosaic.json` config, grouping rules
  (`header:KEY`, `token:KEYWORD`; panels no rule places go to a "No group"
  processing group), persisted `group_dirs`, `needs_analyze`, `Mosaic` with
  `analyze_all` (every group onto one shared reference frame, the existing
  shared-frame machinery) and `blend_group` (a group into its tile store),
  and `set_settings` (analyze + blend in one validation, invalidation pass
  and save) / `set_analyze_settings` / `set_blend_settings` /
  `set_group_override`, which invalidate exactly the affected stores. Jobs
  are split from the configuration: `plan_analyze` / `plan_blend` take a
  self-contained `Send` plan from the `Mosaic`, `run_analyze` /
  `run_blend` run it with no `Mosaic`, and `Mosaic::apply_analyze` /
  `apply_blend` record the outcome; `analyze_all` and `blend_group` compose
  the three. `plan_analyze` marks every group in `needs_analyze` (saved) and
  invalidates its store; `apply_analyze` clears the mark of each group the
  run completed: a re-analysis stales the old tiles, and an interrupted one
  leaves every unfinished group refused by `plan_blend` rather than
  half-replaced. `BlendSettings::validate`
  (finite feather > 0) guards every settings edit and `MosaicConfig::save`,
  so `mosaic.json` never holds a value JSON cannot represent; settings
  structs default missing fields so older files keep loading.
- **Concurrency contract** (tiles and mosaic). One `TileStore` writer per
  file, enforced: `create`/`open` take an exclusive advisory lock
  (`File::try_lock`, released on drop) and fail with `Error::Busy`
  (`is_busy()`) on contention; `create` locks before truncating. Readers
  take no lock.
  Readers must close before a store is recreated (`TileStore::create`
  truncates under their mapping); a matching store is reopened, never
  recreated. Plan on the owner thread, run on a worker with no `Mosaic`,
  apply on the owner thread (only the owner saves `mosaic.json`). Every
  `set_*` (and `plan_analyze`) locks every existing store it would
  invalidate before changing anything and returns `Error::Busy` with
  nothing changed while a writer holds one; `run_blend` locks an existing
  store before loading anything. A `set_*` landing while no store is locked
  is caught at apply time: `apply_analyze` keeps a group marked
  `needs_analyze` if its analyze settings changed since the plan, and
  `apply_blend` invalidates the store and returns an error if the group's
  blend settings changed or it was marked `needs_analyze`. A blend that is
  never applied (cancelled or failed) carries no such guarantee, so the app
  invalidates the group's store (`invalidate_group`, busy-aware) when it
  discards one whose settings were superseded. Invalidation flushes only
  the store's state table (`TileStore::flush_state`); `run_blend` flushes
  the whole store on its worker on success, so the owner thread never
  waits on tile write-back, and only the state table on an error or
  cancellation, so a cancel ends within about a second. `plan_analyze_with` plans from groups
  resolved elsewhere (the app resolves them, reading panel headers, on a
  worker over a copy of the mosaic). The app cancels and joins a
  running job before applying settings; busy refusal and the apply guards
  are the core's backstop.
- **Checks** (`checks`). Input checks (solution present, distortion model,
  scale mismatch, mixed channels, group count mismatch, filters differ
  ungrouped, ungrouped panels), file checks (changed or missing since added,
  by fingerprint of size + mtime) and analysis checks (no overlap, sky-level
  mismatch) as structured values: severity (problem / note / passed), a
  stable serialised `CheckCode`, affected groups and a remedy. The user can
  accept a warning; accepted `CheckKey`s persist in `mosaic.json`. `mmm check`
  renders them as text.
- **Health** (`health`). Seam steps with outlier flags and the photometric
  gain/offset spread, as data (the numbers `report` prints).
- **Display stretch** (`stretch`). Per-channel median/MAD statistics and an
  automatic midtones stretch (shadows clip at -2.8 robust sigmas, median
  mapped to 0.25). Display only: the data and the tile store stay linear and
  the viewer applies the stretch.

## Standalone app (phase 2, 2026-11)

Phase 2 adds the first runnable desktop app on top of the phase 1 core. No
algorithm changed. Spec: `docs/superpowers/specs/2026-11-02-standalone-app-design.md`.

- **Crates.** `crates/mega-ui` is a generic egui library (theme, status bar,
  background job runner, and a tiled pyramid viewer with smooth zoom/pan) and
  must not depend on `mmm-core`, so it can be lifted into other apps.
  `crates/mmm-app` is the eframe app (binary `mega-merge-mosaic`): start
  screen, session bar, and the Blend tab, with mmm-core adapters
  (`tile_source`, `jobs`). egui/eframe are pinned at 0.36 in the workspace
  `Cargo.toml`.
- **Job contract.** The app uses the plan / run / apply split and the
  concurrency contract above unchanged: plan and apply on the UI (owner)
  thread, run on a worker with no `Mosaic`. The job runner lets newer work
  win: a settings edit cancels and joins a running job before applying; an
  analysis cancels everything; a blend cancels (or replaces) only a running
  or queued blend of the same group, and blends of other groups queue and
  run in order.
  Group resolution (reads panel headers) runs on a worker via
  `plan_analyze_with`. The `apply_*` saves of `mosaic.json` stay on the UI
  thread (small atomic write; the core requires apply on the owner thread).
  A blend discarded after its settings were superseded invalidates the
  group's store (`invalidate_group`).
- **Viewer.** Reads the memory-mapped tile store while a blend writes it,
  picks a pyramid level from the zoom, shows stale tiles dimmed, and falls
  back to coarser ancestors while tiles load on loader threads (which wake
  the UI with `request_repaint`).
- **Display stretch is CPU, per visible tile**, on the loader threads, not a
  GPU shader. This keeps the viewer renderer-agnostic and needs no paint
  callbacks; cost is screen-size work per slider change (may lag on 4K). A GPU
  path is a follow-up. A saved `blend_stretch` entry means manual (kept across
  opens and blends); Auto clears it.
- **Renderer.** eframe `glow` (OpenGL), matching workflow-platform; `wgpu` is
  deferred.
- **WSLg record (2026-11-03).** `mega-merge-mosaic --selftest` (renders the
  start screen, then the session if given, logs the GL renderer, closes itself,
  exit 0 on success) exits 0 under WSLg. Default WSLg Mesa selects `llvmpipe`
  (software, GL 4.5). Under WSL with `GALLIUM_DRIVER` unset, the app sets
  `GALLIUM_DRIVER=d3d12` at the top of `main` (before any GL init), which gives
  `D3D12 (NVIDIA GeForce RTX 2080 Ti)`, GL 4.6 (Core Profile) Mesa 25.2.8.
  Users can override by setting `GALLIUM_DRIVER` themselves. libEGL "failed to
  get driver name for fd -1" warnings on stderr are harmless.
- **Self-test session.** `cargo run -p mmm-app --example make_selftest_session -- DIR`
  builds a synthetic analysed and blended two-group session with an HOO
  composition (`DIR/m.mmm-session`) for `--selftest`, which visits Blend, then
  Plan's first group and the composition, each once settled. Automated checks
  launch the GUI only through `--selftest`.
- **Plan tab (phase 3).** Previews (one ≤512 px image per panel, placed in the
  shared frame) become RGBA8 through the Plan display stretch on a small
  converter pool; the UI thread uploads at most 32 textures a frame. The
  canvas extent is the frame canvas (read on a worker), so arriving previews
  never reset the zoom; a panel without a preview yet is a dashed outline at
  its analysed bbox or, in a solved frame, at its header-only WCS footprint
  (`preview::footprint`, found on a worker once per previews generation).
  Textures are dropped after Plan has been hidden 5 s and rebuilt on return. Plan stretches live in `ui.plan_stretch`, keyed by group
  name or `comp:<name>` for a composition (names unique and non-empty).
  Compositions draw each channel's group as additive layers tinted pure R, G
  or B × weight; overlaps within one group add up (a known limitation).
- **Blend ROIs and composition views** (phase 3): ROIs live in `ui.rois`
  (name, note, sky corners when the frame has a WCS, canvas rect), are
  reprojected through the current frame on a worker, and show their size
  and the fraction of fresh level-0 tiles. Blend ROI blends the selected
  ROI once per group of the view. Composition views mix each member
  group's tiles (through its own Blend stretch) on the viewer's loader
  threads (`CompositeTileSource`); members must share one canvas.
- **Not yet built:** export from the tile store, packaging and signing
  (later plans). The Panels and Plan tabs are built (phase 3).
