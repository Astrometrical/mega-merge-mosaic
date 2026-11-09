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
  "Formats come from mpp-core" below. An uncompressed integer panel (or one
  whose `bounds` are not 0:1) is mapped too and converted on read.
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
  decoded/<key>.<u16|f32|…> # decode cache: compressed, big-endian or interleaved
                            # XISF panels decoded once, planar little-endian
                            # in their own sample format; deletable
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
  session.lock              # cross-process advisory lock (see phase 3)
  previews/<key>.mmmp       # cached panel previews (phase 3)
  previews/frame.mmm-frame.json # the previews' frame (Plan; phase 3)
  decoded/                  # decode cache of the app (phase 3)
  reference.mmm-frame.json  # the analysed frame all groups adopt (written
                            # only by analysis)
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
  (`band_cache.rs`, shared with the IPC backing and with XISF panels
  converted on read). Integer data normalized to
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

- **Decode cache.** An uncompressed full-canvas Float32 panel in 0:1 is
  still read from its own mmap. Any other XISF panel that is uncompressed,
  planar and little-endian (UInt8/16/32, Float64, a `bounds` other than 0:1,
  an unaligned attachment) is read from its own mmap too, its samples
  converted to f32 a band at a time through mpp-core's `NativePlanes`
  (`PanelReader`'s `Native` backing, on the band cache FITS uses), to
  `decode_into`'s exact values. Only a compressed, big-endian or interleaved
  panel is decoded once (`crate::decoded`) into
  `<session>/decoded/<key>.<u16|f32|…>`, planar and little-endian in its own
  sample format — written under a temporary name and renamed, keyed by a
  layout tag, canonical path, size, mtime, image id and block checksum.
  Normalised Float32 (a WBPP master) is served from it through
  `PanelReader`'s `Cache` backing as before, so analyze, reprojection and
  blending are unchanged; anything else is converted on read like the
  in-place case. Cost: time on first use and as much disk as the panel's
  uncompressed samples (a 2 GB Float32 panel → 2 GB cache; a UInt16 one
  half that). The layout tag (2026-10-30) means 1.7.0's caches, which held
  normalised f32 for every format, are never read: a session from 1.7.0
  decodes its compressed panels once more and leaves the old files behind
  in `decoded/` (deletable). Known cost: a block read in place that
  carries a checksum is hashed on every open, as the zero-copy Float32
  block always was; 1.7.0 hashed an uncompressed integer panel once, at
  its decode.
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
  odd = invalid, and the next write recovers it. `write_tile_stale` writes
  under the same protocol but leaves the valid bit clear (complete data,
  `Stale`); the sequence skips 0 on wrap so a written tile never reads as
  missing. Samples are accessed as
  relaxed atomics so racing copies are never UB. `state()` reports a tile's
  `TileState` (`Missing` / `Stale` / `Valid`) from its entry alone, and
  `read_tile_any` copies a stale tile too under the same protocol, so the
  viewer can show stale tiles dimmed; `read_tile` accepts valid tiles only.
  `TileSink` is a `RowSink` that fills level 0 and, after each level-0 tile
  row, runs `build_parents` over that row, so the upper levels follow the
  blend. A parent is **always rebuilt from its children's current data**,
  valid or stale (a missing child is uncovered and not copied; a parent with
  no child to read is left unwritten), and written valid only when every
  in-image child is valid, otherwise stale (`write_tile_stale`). So an ROI
  re-blend after a settings change — every tile stale, the ROI's edges not
  on parent boundaries — still shows its new data zoomed out: the edge
  parents mix new and old children and stay dimmed. (Before, such a parent
  was only invalidated, keeping its old pixels at every level.) The rebuild
  checks cancellation once per parent tile row, and `blend_tiles` blends a
  canvas rect (an ROI re-blend invalidates only the tiles it touches)
  cancellably. A cancelled or failed blend keeps its finished work (spec
  §8): level-0 tiles it fully wrote stay valid, the rest of the rect stays
  stale, and only the parents over rows the sink wrote but had not yet
  rebuilt are rebuilt (the rest are already current, or wholly over
  unfinished rows and as stale as their untouched children), so a late
  cancel re-reads no finished tiles. A store
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
  `apply_blend` invalidates the store and returns an error if the group was
  marked `needs_analyze` meanwhile (it no longer refuses changed blend
  settings: a merge keeps the settings it started with, and the `"blend"`
  provenance entry records them, see "Draft merge settings" below). A blend
  that is never applied (cancelled or failed) carries no such guarantee, so
  the app invalidates the group's store (`invalidate_group`, busy-aware)
  when it discards one whose analysis was superseded. Invalidation flushes only
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

- **Crates.** `crates/mega-ui` is a generic egui library (theme, toasts,
  background job runner, and a tiled pyramid viewer with smooth zoom/pan) and
  must not depend on `mmm-core`, so it can be lifted into other apps.
  `crates/mmm-app` is the eframe app (binary `mega-merge-mosaic`): start
  screen and the session window (below), with mmm-core adapters
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
  the UI with `request_repaint`). Superseded work never delays current
  work: the loader queue holds only the newest wanted set (uncached tiles
  coarse to fine, then refreshes finest first), and each frame takes every
  finished fetch but uploads at most 32 textures, current generation first;
  a superseded result is uploaded only if it beats the cached fallback and
  budget remains, otherwise it is dropped (never backlogged). Refreshes of
  one level go longest-stale first, so a long drag cycles through every
  tile. The tile cache grows with the wanted set (1.5x, 512 up to 2048
  tiles, about 512 MB of GPU textures at 256-pixel RGBA8 tiles; it never
  shrinks), so a 4K viewport does not evict tiles it still draws.
- **Display stretch is CPU, per visible tile**, on the loader threads, not a
  GPU shader. This keeps the viewer renderer-agnostic and needs no paint
  callbacks; cost is screen-size work per slider change (may lag on 4K). A GPU
  path is a follow-up. The stretch is always automatic (see the session
  window); the hand-set `blend_stretch` and `plan_stretch` entries of earlier
  versions are ignored there, and only the start screen still reads them.
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
  composition (`DIR/m.mmm-session`) for `--selftest`, which opens the session,
  shows the Mosaic of the first group, then of the composition, then the
  Plan, each once settled. Automated checks
  launch the GUI only through `--selftest`.
- **Plan canvas.** Previews (one ≤512 px image per panel, placed in the
  shared frame) become RGBA8 through the automatic display stretch on a small
  converter pool; the UI thread uploads at most 32 textures a frame. The
  canvas extent is the frame canvas (read on a worker), so arriving previews
  never reset the zoom; a panel without a preview yet is a dashed outline at
  its analysed bbox or, failing that, at its outline (`plan_view::Outline`):
  an earlier preview's rect, or read from the file on a worker — the
  header-only WCS footprint in a solved frame (`preview::footprint`), a
  strided covered-area scan in an aligned one (`preview::aligned_bbox`).
  Outlines are kept per frame across input edits (keyed by path and file
  fingerprint; see "Plan (additions)" below).
  Textures are dropped after the plan has been hidden 5 s and rebuilt on
  return. Composites draw each channel's group as additive layers tinted pure
  R, G or B × weight; overlaps within one group add up (a known limitation).
- **ROIs and composite views**: ROIs live in `ui.rois` (name, note, sky
  corners when the frame has a WCS, canvas rect), are reprojected through
  the analysed frame on a worker, and show their size and the fraction of
  fresh level-0 tiles per group of the view. Merging an ROI merges it once
  per group of the view (not while the ROIs are being reprojected). Composite
  views mix each member group's tiles (through its own stretch) on the
  viewer's loader threads (`CompositeTileSource`); members must share one
  canvas, and a member not merged yet adds nothing and is named above the
  image.
- **Not yet built (phase 2 list, superseded):** export and the panel list
  were built in phase 3, below.

## Standalone app (phase 3, 2026-11)

Phase 3 adds the panel list and the plan, cached previews, an automatic
preview-then-analysis pipeline, a checks engine, Blend ROIs, composition
views and export. No algorithm changed. Core additions are bookkeeping
(`set_inputs`, the lock), `preview` and `export`.

- **Session lock.** A mosaic directory holds `session.lock`, an advisory
  file lock (`File::try_lock`) taken for as long as a process uses the
  session: the app while a session is open, and `mmm init`, `set`, `run` and
  `export` for the length of the command (`check` is read-only and does not
  lock). A second process gets `Error::Busy` with "is open in another window
  or process". It is cross-process; the in-process one-writer-per-store rule
  is unchanged. The app drops its lock before launching a child process for a
  just-created session.
- **`set_inputs`** (`mosaic::set_inputs`) edits the panel list, manual group
  placements and the grouping rule. Like `set_settings` it is busy-aware: it
  locks every group's store first, and a busy store refuses the edit with
  nothing changed. An unchanged input is a no-op; otherwise every group is
  marked `needs_analyze`, the change is recorded in provenance ("inputs"),
  stores are invalidated and `mosaic.json` is saved once. A manual group
  assignment survives a rule change. `plan_analyze_with` drops
  `needs_analyze` entries for groups that no longer exist (`group_dirs` is
  kept so caches stay stable if a name returns).
- **Previews** (`mmm_core::preview`). One image per panel, at most 512 px
  on the long side (`PREVIEW_MAX_SIDE`), placed in the shared frame and cached
  in `<session>/previews/<hash of path, size, mtime>.mmmp` (JSON header + f32).
  Aligned panels are block means over the covered bbox; solved panels are a
  WCS warp. The reference frame is always re-derived header-only on a preview
  run (cheap) and saved atomically, only when it changed and the full panel
  set was used, to **`previews/frame.mmm-frame.json`** (each cached preview's
  header also records its frame), so previews never see a stale frame.
  `preview::footprint` gives a solved panel's outline from its header alone
  (aligned gives `None`) and `preview::aligned_bbox` an aligned panel's from
  a strided scan of channel 0 (every 18th row of a 9255 × 18310 panel, about
  37 MB), so Plan shows dashed outlines of 100+ panels before any preview
  exists.
  Compressed XISF and FITS panels are decoded once into `<session>/decoded`
  (the app points the decode cache there), which analysis reuses. Per-panel
  failures are logged; the run still finishes.
- **Two frames.** `reference.mmm-frame.json` is the **analysed frame**: the
  frame of the last analysis, written only by analysis (and `mmm init` /
  `run`). Previews derive and keep their own frame
  (`previews/frame.mmm-frame.json`) and never write the analysed one. Plan
  places panels in the previews' frame (the analysed one until a complete
  previews run has saved one); ROI reprojection, Blend ROI and blends use the
  analysed frame, so adding panels never moves a persisted ROI before the
  analysis that changes the blend's canvas. The app keeps the analysed frame
  in memory (`OpenSession::frame`, read on open and after each analysis).
  When a complete previews run's frame differs from the analysed frame and
  nothing but exports is running (no analysis pending or running), the
  pipeline marks every analysed group `needs_analyze` with
  `mosaic::mark_needs_analyze` (busy-aware: it invalidates the newly marked
  groups' stores, saves once, and is refused with nothing changed while a
  store is held), so panel files changed on disk since the analysis are
  caught; once per previews run.
- **Automatic pipeline** (`pipeline.rs`). On open, and 1.5 s (`DEBOUNCE`) after
  the last input edit, it runs previews, then analysis when nothing else is
  running (so it never cancels a user's merge) and some group needs it.
  Analysis is always automatic: there is no toggle, and a stored
  `auto_analyze: false` is ignored. The panel list cancels running jobs before an edit and applies it once they have stopped;
  exports are spared and not waited for (they read the result without a
  writer lock and write stale tiles as they are);
  the pipeline waits while an edit is pending and re-runs a job that ended for
  stale inputs. User cancels are final: a cancelled or failed previews run is
  not retried, and an ended analysis is not restarted, until the inputs
  change (or the user presses Resume or **Analyse**). An analysis still
  auto-starts after a cancelled previews run. An analysis the pipeline left to
  the user (cancelled or failed, `Pipeline::analysis_stalled`) is the one case
  where the Checks step offers **Analyse**.
- **Checks engine** (`checks.rs`). Runs `panel_facts`, input and file checks,
  and analysis checks for every analysed, current group on a worker, on open,
  when groups or panels change and after any pipeline job ends; the Checks step and the
  inspector's Problems section show problems, notes and passed. Accepting a check adds its keys to
  `accepted_checks` in `mosaic.json`; accepted problems stop counting.
- **Panel list** (`panel_list.rs`, edits in `panel_edits.rs`; opened from
  the Panels step, a resizable column left of the canvas). A sectioned
  table (`mega_ui::table`): PANEL, FILTER, INTEGRATION, SCALE, SIZE from
  one header read per panel (`read_panel_header`, cached by path and
  fingerprint on a worker; a reader that panics makes its row "Unreadable"
  and the rest still group, and a resolution that stops unexpectedly is
  tried again for the same inputs after 2 s, doubling up to a minute, never
  every frame); with grouping on, **No group** first (its heading says why:
  "FILTER missing", "no FILTER in the path", "no single FILTER value in the
  path") then a rolled-up section per group ("Ha · 12 · 38h 20m", "1 fewer
  than Ha"). While a new rule resolves, rows stay where the last rule put
  them (the flat list when grouping was just turned on) instead of all
  dropping into No group.
  **Sorting** (spec §10.2): column headings are buttons; a click sorts the
  rows within each section by that column (`TableState::sort`, kept while
  the session is open; sections keep their order), a second click the other
  way, and the sorted heading shows a chevron (up: smallest first) and its
  accessible sort direction. Numbers compare as numbers (`Row::sort_keys`:
  integration in seconds, scale in arcseconds, size in pixels), text
  case-insensitively, unknown cells ("—", "Reading…", "Unreadable") last
  either way; ties keep the configuration order, which is the order until a
  heading is clicked. A rolled-up section shows the first rows of the
  sorted order (plus its problem rows); Shift ranges, reveal and drags
  follow the sorted order. Headings keep room for the arrow, so sorting
  moves no column.
  Rows select with click, Shift-click (the rows on screen) and
  Ctrl/Cmd-click; folding a section deselects its rows (Delete and Move to
  act only on what shows). The actions menu (on the selection when the row
  is selected) has **Move to** (each group, **New group…** named in the
  menu and made of the panels moved, **No group**, **Let the rule decide**)
  and **Remove**; rows also drag onto a section heading. A panel moved by
  hand (`manual_group`) is tagged "by hand" and keeps its FILTER cell.
  Remove and Delete (Backspace on macOS) on the selection offer Undo in a
  toast. Rows and the plan are linked (spec §7.4): hovering either lights
  the other (`PlacedItem::highlighted`, `Row::highlighted`; only panels the
  plan's view shows), a row click selects the panel on the plan (the
  inspector shows it), first switching the plan to the panel's group when
  its view does not have it, and a plan click selects and reveals its row
  (`TableState::reveal`); Esc clears both. Columns hide from the right as
  the list narrows (the first column elides, the actions stay pinned);
  unknown cells show "—", "Reading…" or "Unreadable". Row ids are stable
  (path based), so selection, folds and hover survive a re-sort or
  regroup. Panels are added by **Add files…**, **Add folder…** (top-level
  files only), dropped files and folders, or More → Type a path… (one `*`
  in the file name); duplicates are ignored, and an add that adds nothing
  says why in one toast ("Couldn't read {folder}", "No panels at the top
  level of {folder}", "Already in the session", "Only XISF and FITS panel
  files can be added"). The empty list reads "No panels yet: add files or
  folders, or drop them anywhere on the window."
  **Undo toasts** travel from the window to the shell
  (`SessionOutput.toasts`, the `Undo` enum, `SessionWindow::undo`) and are
  taken down (`Toasts::dismiss`) when the session closes or another opens.
  The removal Undo (`Undo::Readd`) puts the removed panels back at their
  old indices into the inputs as they are now (keeping later edits; one
  already back is left alone); the grouping Undo (`Undo::Grouping`)
  restores only the grouping, and is a no-op once the user has chosen
  another rule.
  **Group by** chooses Don't group, FILTER header, Header keyword…, Path
  keyword… or By hand (`GroupRule::{Header, PathKeyword, Manual}`; the
  legacy `FilenameToken` is shown as Path keyword… until edited).
  Re-picking the shown choice changes nothing, nor does a header key or
  path keyword that differs only in case (e.g. a legacy
  `Header { key: "filter" }` picked as Header keyword… or typed "FILTER"):
  it groups alike and the edit would only throw the analysis away. **Path
  keyword** is WBPP-style: the whole path is split on `_ - space . / \`,
  the keyword matches a whole token case-insensitively and the next token
  is the value; conflicting values in one path mean no group
  (`grouping::path_keyword` says why: `Missing` or `Conflicting`;
  `path_keyword_value` is its value). **By hand** groups only what the user
  moves. **Automatic FILTER grouping** happens once: with at least two
  FILTER values, grouping off and no earlier choice (an accepted
  FiltersDifferUngrouped note, or a disabled non-default rule, counts as a
  choice; a session opened with analysis results is skipped until its
  panels change) it groups by FILTER and toasts "Grouped by FILTER: {k}
  filters" with Undo.
  `mmm_core::panel_header::read_panel_header` opens a panel once for the
  filter, exposure (EXPTIME/EXPOSURE), NCOMBINE and integration (exposure
  times frames).
  **Early outlines.** An aligned panel, until its preview arrives, gets its
  outline on the plan from `preview::aligned_bbox` (a solved one from
  `preview::footprint`). The plan keeps outlines per frame across input
  edits (`plan_view::Outline`, keyed by path and file fingerprint): an
  arrived preview's rect is kept as its panel's outline while an edit drops
  and reloads the previews; only a panel with no analysed rect, no preview
  and no outline is read, and those read before are checked again by
  fingerprint once per previews generation (a changed file is read again),
  so a regroup or move reads nothing. The running search passes over panels
  placed meanwhile (`plan_view::find_outlines`); a new frame drops every
  outline; and dropping the `PlanView` (leaving the session) cancels the
  search, so it never reads panels (or holds the decode lock) for the next
  session.
  Group colours are stable and persisted in `ui.group_colors`: well-known
  filters get physical colours (Ha red, OIII cyan, SII orange-red `#E0703A`,
  R/G/B, L white), other names take the first unused entry after the six
  physical ones, cycling through those (never a physical colour).
- **Plan (additions).** The view bar lists the groups and the composites
  (hidden when grouping is off). Layers are additive. Clicking a panel
  selects it: the inspector shows its path, **Show in folder**, Copy path,
  header facts, overlapping panels and its checks (with Accept). Outline
  toggle. When previews are incomplete and none are running, "Previews
  paused" with **Resume** (`Pipeline::request_previews`). Overlap-handling is
  topmost-wins in group views; composites add overlapping previews of one
  group, so overlap bands look brighter (accepted for a rough preview).
- **ROIs** are sky-anchored: a rectangle drawn with the viewer's draw
  tool is stored as two opposite sky corners plus a canvas rect and is
  reprojected through the analysed frame after each analysis, so it follows
  the frame when the analysis of changed panels moves it (a rotated
  re-derive grows the box slightly; without a WCS the canvas rect is used as
  stored). Merging an ROI merges it once per group of the current view
  with the merge settings draft, uncommitted (ROI merges stay out of the
  step strip; see "Merge jobs carry their settings" below); **Merge** merges
  every group.
- **Export.** `mmm_core::export` writes a group's result for the whole canvas
  as linear 32-bit float, **unclamped**: FITS (WCS cards plus `HISTORY`
  provenance: mmm version and the group's latest blend settings) or TIFF
  (`ImageDescription` carries the same provenance). It reads the tile store
  tile row by tile row without a writer lock; stale tiles are exported as
  they are, missing tiles as zeros. `export_check` counts stale and missing
  level-0 tiles first and both frontends warn (the app offers *Merge whole
  mosaic first* or *Export anyway*). The app's Export is disabled while a
  blend of a chosen group is queued or running (the check maps the store the
  blend may recreate); it takes the groups, destinations and format when
  pressed (later edits in the dialog do not change that export) and lists
  destinations that exist already, exporting over them only after an
  explicit *Overwrite*. Output goes to `<out>.part` and is
  renamed on success; failure or cancel leaves an existing destination
  untouched. FITS uses the top-down WCS convention only. The TIFF writer
  needs random access, so TIFF export stages a scratch file the size of the
  image next to the output (about twice the disk, briefly); a cancelled TIFF
  export finishes at disk speed before cleaning up. CLI: `mmm export`.
  **XISF export is deferred**: formats live in mpp-core, which has no XISF
  writer yet; the spec's XISF export is missing in v1 until it gains one
  (and a streaming TIFF writer would remove the scratch file).
- **Not yet built:** XISF export (needs an XISF writer in mpp-core), a
  streaming TIFF writer, the Plan overlap-handling setting (group views are
  topmost-wins), the FITS flipped-WCS option in export, packaging and signing.

## Standalone app: the session window (project 3, 2026-11)

Spec: `docs/superpowers/specs/2026-11-08-in-session-experience-design.md`
(§4–§7, §15). It replaces the Panels, Plan and Blend tabs, the session bar,
the status bar and the Blend settings panel of phases 2 and 3 with one window
(`crates/mmm-app/src/session_window/`). The UI says "merge" where the engine
says blend; `mmm-core` keeps its identifiers, provenance actions and
progress stage codes, but every engine message the app can show (check
messages and remedies, job errors, the export check) says merge too (a
test makes a check of every `CheckCode` and asserts none says blend). No
algorithm changed. The UI copy is British English ("Analyse", "colour").

- **Top bar.** ‹ (back to the start screen), the session name, the **step
  strip** and the main button. The window title is "{name} — Mega Merge
  Mosaic". There is no status bar and no session bar. It is one row when
  everything fits (measured each frame: the name, `step_strip_width`, and
  what the main button leaves); otherwise the strip takes a row of its own
  below (and wraps if even that is too narrow), and a name too long for what
  is left is cut short with "…" (hover shows it whole). Nothing overlaps at
  the app's 720 px minimum.
- **Step strip** (`steps.rs`): Panels · Checks · Merge · Export. It is the
  status, the progress and the navigation at once: each step shows its state
  (what is done, what is running, what is next) and clicking one acts:
  Panels opens or closes the panel list, Checks opens the Problems section,
  Merge merges every group while that is to do (merging or merged, it shows
  the mosaic), Export opens the Export dialog. Checks reads "Checks" until
  there is something to say, "Analysing…" while analysis runs (cancellable),
  "Checking…", "{n} problem(s)" or "Checks passed", and **Analyse** only when
  the analysis stalled after a cancel or failure
  (`Pipeline::analysis_stalled`; clicking it analyses, whichever step is
  highlighted; once running, a click on "Analysing…" opens Problems and
  never restarts it); otherwise analysis starts by itself. Merging reads "Merging
  {group} · {i} of {n} · {pct} %", without "{i} of {n}" for a single merge and
  without the group in a session of one group (an OSC mosaic: "Merging · 43
  %"; its jobs and toasts are "Merge" and "Merge finished"). A step that
  cannot act now says why in its tooltip (Merge: the merge guard's reason;
  Export: `MosaicView::export_block_reason`). ROI merges are not shown in
  the strip.
- **Merge states and predicates** (`steps.rs`). One set of predicates
  derives every place's Merge state. `out_of_date(session, group)`: the
  group's result is not Complete, it needs analysis, or a region of it was
  merged with other settings than its last whole merge
  (`mmm_core::mosaic::region_differs`: the store is complete but not of one
  merge). `draft_changed`:
  not out of date, but its last whole-canvas merge used different effective
  merge settings from the draft (`mmm_core::mosaic::settings_changed`, the
  draft with the group's override applied). The Merge step reads, in order:
  "Merge queued after export" (a whole-canvas merge waiting behind a running
  export; cancellable, no fraction), "Merging …" (above), "Merge" (no
  groups, analysis needed or running; or nothing ever merged, as the next
  step), "Out of date" (some group `out_of_date`; next), "Settings changed"
  (some group `draft_changed`; next) or "Merged" (done). The step strip, the
  main button, the chips, the canvas tag and the start screen's card all
  agree because they call these predicates (the start screen through
  `GroupSummary.settings_changed` and `GroupSummary.region_differs`,
  below).
- **Main button.** **Add panels…** with no panels, **Merge** (with a summary
  of the merge settings, which opens Merge settings) while any group is out
  of date or its settings changed (`steps::merge_needed`: the groups
  `groups_to_merge` returns), **Export…** once every group's result is
  complete (disabled, with its reason as a tooltip, while the Export dialog
  cannot open). A group whose settings changed is still complete, so it
  never blocks export. `Ctrl/Cmd+Enter` presses **Merge**.
- **Canvas.** The plan (`plan_view.rs`) or the merged mosaic
  (`mosaic_view.rs`) of the chosen group or composite, with one shared
  camera: switching between them, or to another view, hands the shown
  canvas's camera to both first, so nothing moves. Going to an ROI (a click
  on it in the ROIs section) hands both canvases the camera that frames it
  (`Camera::framing`), so it works on the plan and the mosaic alike and a
  later switch moves nothing (`TiledViewer::set_camera` drops view commands
  queued before it). A composite's members not merged yet are named over
  the image's bottom-left corner, so the image keeps the plan's place. The
  window opens on the mosaic once something is merged, else on the plan;
  Mosaic cannot be chosen before something is merged or a merge has
  started. `MosaicView::poll` runs
  every frame whether or not the mosaic is drawn. The panel list sits in a
  resizable column left of the canvas while open (view bar and canvas stay
  beside it; switching plan/mosaic leaves it open); a session without panels
  opens on it, and files dropped on the window are added whatever shows.
- **View bar.** Plan / Mosaic, a chip per group and per composite and **+**
  (with two or more groups), and the Outlines, ROIs, Linked and Strong
  toggles. It wraps when the window is narrow. A group chip carries a
  `mega_ui::widgets::ChipMark`: `Attention` (the warning tone's icon,
  "Out of date") when the group is out of date, `Changed` (the info tone's
  icon, "Settings changed") when only its settings changed; a composite's
  chip takes its members' worst. The mark is the tone's glyph after the
  label in the tone's colour, never a bare colour dot (the info tone is the
  accent, as is a selected or hovered chip's border); the chip's hover text
  says the state above the caller's own (the view bar's shortcut hint), and
  its accessible description says it too. The chip's context
  menu has **Merge this group**, which commits the draft and merges that
  group only (disabled, "Merging {group}…", while that group's whole merge
  runs or is queued, and when merging is blocked). In Mosaic mode a tag over
  the image's top-left says why the shown group is out of date, in the order
  of `mmm_core::mosaic::out_of_date_reason`: analysis settings changed
  (`needs_analyze` with analysis settings that differ from the group's
  last analysis), panels changed (`needs_analyze` otherwise), region merged
  with other settings (`region_differs`: "Out of date — region merged with
  other settings"), Not merged yet (no whole merge recorded), panels changed
  (an `"inputs"` provenance entry newer than the group's last whole merge
  that touches it), analysis settings changed (an `"analyze"` entry newer
  than the last merge with different settings), else merge didn't finish.
  A group whose settings merely changed gets the quieter "Settings changed
  since this merge": muted text on a translucent panel
  (`mega_ui::theme::scrim`: `SURFACE_800` at the lowest opacity that keeps
  the text AA over a white image), where the out-of-date tags sit on an
  opaque panel in body text. Clicking the tag reveals Merge settings.
- **Inspector.** Foldable, at the right: Problems, Panels, ROIs, Merge
  settings, Composites and Notes, in that order; only the sections' contents
  fold (Merge settings has no fold of its own inside). The merge settings
  are the
  mosaic-wide `AnalyzeSettings` / `BlendSettings` with the CLI's value
  domains. Merge fields edit a draft kept in `ui.merge_draft`
  (`blend_settings::MERGE_DRAFT_KEY`; saved with the window's debounced save;
  nothing is invalidated or cancelled; a merge commits it). Analysis fields
  apply at once (applied once the cancelled jobs have stopped; a change
  marks every analysed group `needs_analyze`), show one muted cost line
  under the analysis fields once a mosaic exists ("Re-runs the analysis;
  the mosaic goes out of date"), and raise an "Analysis settings changed"
  toast with Undo (`Undo::Analyze`; undoing queues like any edit and raises
  its own toast whose Undo redoes the change). A field whose draft value
  differs from the group's last merge (or last analysis) is marked with an
  accent dot (`changed_fields`).
- **Draft merge settings and commit-at-merge** (project 3, plan 3). The
  draft is `load_draft(cfg)`: `ui.merge_draft` if it parses and validates,
  else the committed `config.blend` (an old session has no draft and
  behaves as before). Pressing Merge (or **Merge this group**) first calls
  `mmm_core::mosaic::commit_blend_settings`, which validates the base and
  every group's effective settings, stores them as `config.blend` and
  saves once, with no store lock, no invalidation and `needs_analyze`
  untouched (the app does this save on the UI thread, the accepted
  exception). A commit therefore never invalidates a finished mosaic; the
  groups merged with other settings read "Settings changed" until
  re-merged. `mmm set` still goes through `set_settings` and invalidates.
- **Merge jobs carry their settings.** A merge job captures its
  settings when it is queued and plans with `plan_blend_with`; a running
  merge keeps the settings it started with across later commits and its
  group then shows "Settings changed". The `"blend"` provenance entry
  records the plan's settings and `whole` (false for an ROI merge; an
  entry without the key counts as whole), and
  `mmm_core::mosaic::last_merge_settings` reads the newest whole entry's
  settings (`None` if never merged or unparseable). A whole-canvas merge
  whose settings differ from that is `BlendPlan.fresh`: `run_blend`
  invalidates the whole store before writing, so a merge stopped part-way
  never leaves old- and new-settings tiles both valid (the group reads Out
  of date). `TileSink::new` also invalidates the whole aligned rect of any
  whole-canvas merge, fresh or not, so a finished whole merge rewrites every
  tile; `fresh` is kept for durability (it flushes the invalidation before
  the possibly long blend reads the panels, while the sink's sits unflushed
  in the mapping until the run ends, and `blend_tiles` can fail before the
  sink exists), so neither replaces the other.
- **ROI merges are gone from the app (plan 4).** The ROI list no longer
  has "Merge ROI" (`MosaicView::merge_roi`, `JobRunner::start_blend_with` and
  `blend_settings::draft_for` are removed); a region is drawn on either
  canvas (**Draw ROI** or `D` arms the shown canvas's draw tool), named
  "Region n", and previews of it are transient (tasks 5-6) rather than
  written into a store. `JobRunner::start_blend` still accepts a rect and
  mmm-core keeps ROI plans (never `fresh`) and
  `mmm_core::mosaic::region_differs` (a `whole: false` entry newer than the
  newest whole entry records other settings; no whole entry: false; only a
  later whole merge clears it), because the CLI's `--roi` and sessions
  touched by plan-3 builds still carry such entries: they make the group
  out of date, never "Settings changed", so Merge re-merges it over the
  whole canvas (the sink rewrites the patch even when that merge is not
  `fresh`).
- **Per-group panel edits.** An add, remove, move or rule change works out
  on a worker which groups it touches (`affected_groups(old, new)`) and
  applies with `set_inputs_affecting`: only those groups' stores are
  invalidated and marked `needs_analyze`, and the `"inputs"` entry lists
  them (`affected`; absent = all, as `set_inputs`). The analysis that follows
  is `plan_analyze_scoped` with `AnalyzeScope::Needed` (marked groups plus
  groups never analysed); its plan records the saved frame
  (`expected_frame`) and `run_analyze` fails with `Error::FrameMoved` if the
  frame derived from the panels differs (a solved panel growing the
  canvas). The app treats that as a fallback: it marks every analysed group
  and re-runs a whole analysis, reporting no failure. A panel edit
  cancels every non-export job (`JobRunner::cancel_for_panel_edit`,
  unaffected groups' merges included); a whole merge it stopped raises
  "Merge stopped: panels changed" once per edit.
- **Merges behind exports.** The runner has a single slot, so every merge
  waits behind a running export (not only a merge of the group being
  exported); the strip says "Merge queued after export"
  (`JobRunner::merge_waits_for_export`).
- **Display stretch** (`display_stretch.rs`). Always automatic, never set by
  hand, one rule for the plan and the mosaic: an automatic stretch per
  channel, with **Linked** (one stretch for all channels) and **Strong** (a
  harder stretch) toggles saved under `ui.display_stretch`. The old
  `plan_stretch` / `blend_stretch` are ignored by the session window.
- **View state** (`view_state.rs`). Plan or mosaic, the view, the camera, the
  inspector's fold and open sections, the panel list and the Outlines and ROIs
  toggles are saved in `ui.session_view` and restored on reopen: the mosaic
  only if it can show, else the plan; a view the session no longer has falls
  back to the first group (its camera is dropped); the camera reaches both
  canvases before either draws, so no fit is painted first. Saves are made in
  memory at once and written to `mosaic.json` at most once per
  `SAVE_INTERVAL` on the UI thread (the documented exception), and at once by
  the shared `Shell::flush_pending_saves` (closing, eframe's periodic save,
  back to the start screen).
- **Shortcuts.** `I` inspector, `M` plan/mosaic, `[` and `]` previous and next
  view, `O` outlines, `R` ROIs, `L` Linked, `S` Strong, `Ctrl/Cmd+Enter` the
  main button; each is named in its tooltip. `Delete` (also `Backspace` on
  macOS) removes the panels selected in the open panel list, and `Esc`
  clears the selection (unless it closes a menu or leaves a text field: egui
  lets go of the field before the shortcuts run, so the window remembers
  whether one had the keyboard last frame). None acts while a text field
  has the keyboard (`Context::text_edit_focused`; a button or chip focused
  by keyboard navigation does not stop them) or the Export dialog is open.
- **Back to start** (`Shell::close_session`). Cancels the running jobs and
  waits up to 3 s (`STOP_WAIT`) for them to stop, which can block the UI for
  that long, then flushes the pending saves (an edit waiting for the jobs
  applies only if they stopped). Disabled while an export runs.
  If a worker outlives the wait, the session is released from the UI's point
  of view at once, but a reaper thread (`mmm-session-reaper`) keeps the
  session lock until the abandoned workers have ended, so another window or
  the CLI never opens a session that is still being written.
- **Toasts** (`mega_ui::widgets::Toasts`) replace the notices: a merge or an
  export that ended, a failure. Never modal. A toast whose Undo would
  change the session is taken down when the session closes or another
  opens (`Toasts::dismiss`).

## Coming in project 3

Plans 2 (the panel list and grouping) and 3 (draft merge settings and
per-group staleness, in the session window above) are built. Plans 4-6 of
project 3 change the session window as follows; none of it is built yet.

- **Plan 4, ROIs.** An ROI list with transient previews, drawing an ROI on the
  plan, and the Draw ROI shortcut.
- **Plan 5, composites and export.** A composites popover and the new Export
  dialog.
- **Plan 6, checks and overlays.** New checks and badges, click-to-zoom from
  a problem, and a seams overlay.

## Mega UI standard (2026-11)

Spec: `docs/superpowers/specs/2026-11-07-ui-standard-and-start-screen-design.md`
(it supersedes §4 and §5.1 of the standalone app spec). `mega-ui` is the
shared UI standard of the Mega apps; it echoes the Astrometrical web app and
stays free of `mmm-core`.

- **Tokens** (`mega_ui::theme`): `palette` (the web's colours and AA text
  roles, plus `HA_400` for Hα text and SII gold `SII_300/400/600` for
  warnings), `tokens` (spacing 4·8·12·16·24·32, radii 4/6/8, 30 px controls,
  100 ms hover fades), `text::Type` (the type scale), `Tone` (success, info,
  warning, problem, neutral: colour, accent, tint and icon), and
  `contrast_ratio`/`legible_text` for checking new colours, plus `scrim`
  (a surface colour at the lowest opacity that keeps given text AA over
  any image) for translucent overlays. UI code takes
  colours, sizes, radii and durations from these, never literals.
- **Fonts** (`theme::fonts`): Inter (UI), JetBrains Mono (numbers) and
  Lucide (icons), embedded; licences in `crates/mega-ui/assets/fonts/` and
  `NOTICE`. Icons render only in the Lucide family (`icons::font`,
  `icons::rich`): Inter has its own glyphs in Lucide's private-use range.
- **`theme::apply`** installs the fonts, forces the dark theme and sets the
  complete `egui::Style`. Fonts bind on the next frame and egui panics on an
  unbound family or a missing named text style, so apps call `apply` before
  their first frame; hosts and test harnesses that can't use
  `theme::ensure_applied` and skip drawing for one frame.
- **Components** (`mega_ui::widgets`): `MegaButton` (primary, secondary,
  quiet, danger, ghost; large; icon buttons), `step_strip` (pills with
  state, progress, Cancel and an optional tooltip; `step_strip_width`
  measures one row, and the strip wraps when it does not fit), `chip`,
  `choice_chip` (a toggle; a `ChipMark` adds its tone's icon, its state as
  hover text and its accessible description), `status`, `overline`,
  `property_grid`, `segmented`, `text_input`, `notes_view`, `progress`,
  `submenu` (a menu entry with an icon-font chevron, staying open for a
  text field inside),
  `Toasts` (the only notifications; never modal; `Toasts::dismiss` takes
  one down whose action no longer means anything), plus the restyled
  `checks::checks_list` and `jobs::job_panel`, `groups` (the group palette,
  whose order is frozen because sessions store its indices) and `brand`
  (the lockup; app names are images pre-rendered from Angora by
  `assets/brand/make-name.sh`).
- **Table** (`mega_ui::table::sectioned_table`, `TableState`): sections with
  a fold and a roll-up row ("… N more"; problem rows always shown), tags,
  selection and highlight fills, "⋯" row actions (`icons::ELLIPSIS`), rows
  that drag onto headings, columns that hide from the right and a first
  column that elides when narrow, actions pinned; `TableState::reveal`
  scrolls to a row. Column headings are buttons that sort the rows within
  each section (`TableState::sort`, `Sort`; `Row::sort_keys` with
  `SortKey::{Number, Text, Unknown}`, a cell's text by default, "—" and
  empty cells unknown and last), a second click reversing, the sorted one
  marked with a chevron (`icons::CHEVRON_UP` / `CHEVRON_DOWN`) and its
  accessible sort direction; roll-up shows the first rows of the sorted
  order. The gallery shows its table sorted by integration. Layout tokens `PANEL_LIST_MIN_WIDTH` and
  `CANVAS_MIN_WIDTH`; `PlacedItem.highlighted` lights a tile-viewer item.
- **Gallery**: `cargo run -p mega-ui --example gallery` shows every token and
  component; `crates/mega-ui/tests/snapshots.rs` keeps image snapshots of it
  (ignored by default, see CLAUDE.md).

## Start screen (2026-11)

Spec: `docs/superpowers/specs/2026-11-07-ui-standard-and-start-screen-design.md`
(§5). The start screen is the first thing the app shows: the most recently
opened available session as a full-bleed hero (a drifting, crossfading
view of its mosaic; a starfield stands in until its first image is on
screen, and when there is none), its details and **Resume session** on
the left, a shelf of recent sessions along the bottom and an "All sessions"
grid with a search. It lives in `crates/mmm-app/src/start/`; the generic
parts (`starfield`, `hero::HeroStage`, `widgets::session_card`,
`viewer::ImageTileSource`) are in `mega-ui`.

- **It never opens a session.** It takes no lock, runs no analysis and does
  no file I/O on the UI thread. Everything it shows is read on background
  threads that the screen polls each frame, and a session found busy (open
  in another window or the CLI) is never read live.
- **The summary file** (`summary.json` at the session root;
  `mmm_core::session_summary`): panel and group counts, each group's result
  state and blend/export stamps, check counts, canvas size and the view last
  shown. The app keeps it current (`summary_writer::SummaryWriter`, written
  atomically in the background whenever a fact changes: what it shows, the
  settings the results depend on (an analysis or panel edit marks results
  stale with no job running; a merge-settings commit makes
  `GroupSummary.settings_changed` true for the groups merged with other
  settings, with no invalidation; a region merged with other settings makes
  `GroupSummary.region_differs` true, and such a group is never also
  settings changed), or a job ending, cancelled ones included) so the start
  screen can show a session without opening it. It is advisory: `read_summary`
  returns `None` for a file that is absent, corrupt or from a newer mmm
  (`SUMMARY_FORMAT`), and the card then falls back to what `mosaic.json`
  and the cached previews say.
- **The facts loader** (`start::facts`): per recent session, off the UI
  thread, the availability (`Missing` for a gone folder or unplugged
  drive, `Busy`, `Unreadable` for a corrupt or newer `mosaic.json`, else
  `Available`), the state, group colours and notes. The states are no
  panels, plan ready, partly merged (some groups merged, the rest without a
  result), merged, merged and exported, settings changed (every group
  complete but one's last whole merge used other settings from the draft;
  it ranks ahead of merged and exported, since an export of the old
  settings does not settle the session; `CardState::SettingsChanged`,
  "Settings changed") and re-merge needed (any stale or partial group, or
  any with `region_differs`, whatever the others). Facts load on a pool of 4 workers (`FACTS_WORKERS`), so a session on a dead network path stalls
  one worker, never the screen or the other cards.
- **The hero loader** (`start::hero_views`): a session with any group
  result (merged, partly merged or needing a re-merge) gives one view per
  group with a result, read live from its store through
  `MosaicTileSource::open_plain`, plus each saved composition whose groups
  all have a result, each through its saved Blend stretch (else an automatic
  one). A planned session with no group result gives one picture per
  group, rendered from its cached previews (`preview::load_cached_previews`,
  `render_overview`) through the saved Plan stretch. Otherwise, no views
  and a line saying why (a session whose state is unknown: "Nothing to
  show yet — resume to build the preview"). Every failure degrades to fewer
  views, never a panic. Each view fills the whole window (a cover fit, as
  the web's `DziHeroLayer` does), whatever the mosaic's aspect: a portrait
  mosaic in a landscape window is cropped top and bottom, never shown as a
  narrow strip. The drift (zoom from the cover fit to `KEN_BURNS_ZOOM`
  times it, alternating in and out per view, with a pan of up to 2 % of
  the view that grows with the zoom and takes at most half the margin the
  zoom opens) stays inside the image, so the image's own edges never show
  (only a zoom past the viewer's `MAX_ZOOM`, an image far smaller than its
  area, could). Only two feathers of page colour darken the image (no
  other gradient or vignette; this supersedes the spec's §5.2 gradients):
  along the left, `LEFT_FEATHER_OPACITY` (below 1, so the image shows
  through) over the content column (the session block and the shelf's
  heading row, whose right edge the start screen passes to
  `HeroStage::show`), easing to clear (a smoothstep) over
  `LEFT_FEATHER_EASE` of the width past it; and along the top,
  `TOP_FEATHER_OPACITY` down to the bottom of the view's label and the
  slideshow controls, which sit at the top right, easing to clear below
  them. Both were tuned by eye on real mosaics, which are far from white:
  over a pure white image the label and the dimmer greys in the column
  fall below AA (revisit if that bites). Text laid over the hero all sits in the left feather's column,
  so "All sessions" is beside the "Recent sessions" heading, and the grid's
  Back and search are on the left. The cards are opaque; a missing
  session's faded card keeps an opaque backing, so the image never shows
  through it.
- **Load order.** The first frame draws at once with no facts. The default
  hero candidate's facts load first, then its hero views (so the first
  image is on screen in under a second with a warm disk), then the other
  cards' facts, and card thumbnails only once the hero has drawn. The
  default hero is the most recently opened session that is `Available`: a
  missing, busy or unreadable one never is. A newest session whose facts
  haven't arrived stays the candidate for `FACTS_WAIT_S` (about 1 s); after
  that the hero falls back to the most recent session known to be
  available, so a folder that never answers never holds up the screen.
  Once settled the default hero stays put, so the name, notes and
  **Resume** target the user is reading never jump: it latches as soon as
  the pick is a session whose facts show it available (at the latest when
  the wait ends), or at the user's first press, key or scroll (not a mere
  pointer move: the window may open under the cursor) even while the
  pick's facts are still on their way. It moves again only if the latched
  session turns out missing, busy or unreadable, is removed or is opened;
  a newer session answering late doesn't take over. Hero dwell is 8 s and
  the crossfade 2 s; with reduced motion (probed in the background) views
  cut and don't drift.
- **`FileTileReader`** (`mmm_core::tiles`): the tile reader for a store
  another process may recreate. It follows the same per-tile seqlock
  protocol as the memory-mapped `TileReader` but uses positioned file reads,
  and a short read, an I/O error or an out-of-range tile reads as
  missing; it never faults. Live reads from the start screen use it, never
  `TileReader`, because a mapping of a truncated or replaced file raises a
  fatal fault. **On Windows a held store reads as missing:** a
  `TileStore`'s writer lock is mandatory there, so while any `TileStore`
  holds the store (a blend, an invalidation or a settings edit, in another
  process or the reader's own) every `FileTileReader` read fails (os error
  33) and opening one fails; reads succeed again once the writer lets go.
  So the hero shows no tiles while another process blends the session, and
  `session_summary::result_state` reads a store being written as `None`.
  That is why the summary writer is marked dirty on every job end,
  cancelled ones included (the store has been released by then), and
  re-reads the results whenever the settings change.
- **The lock probe** (`mosaic::session_busy`): opens `.lock` without
  creating it and tries a shared lock, released at once. It is the only time
  the start screen touches a session's lock, and an error other than
  contention reads as "not busy" (opening the session takes the real lock
  and reports a real error). A busy session's notes are read-only on the
  screen, so a save never overwrites the other window's config.
- **Notes** (`widgets::notes_view`, `start::notes`): read as text, the
  first `NOTES_LINES` (8) lines with **More** for the rest (**Less**
  folds them again); **Edit** (**Add notes** when there are none) swaps in
  a text box, focused with the cursor at the end, and **Done** or Esc
  swaps it back, returning the focus to **Edit**. They are saved a second
  after typing stops, or at once when editing ends or the box loses focus.
  The session block's scroll bar takes its own room (egui's solid scroll
  style), so long notes being edited never run under it.
- **Recent list** (`start::recent`): up to 100 sessions, pinned first on the
  shelf, then by last opened.
- **Testing.** Tests build sessions in temp folders from `testutil` /
  `synth_mosaic`, never from `test_data/`. The gallery (`mega_ui::gallery`)
  has Starfield, Hero stage (a landscape and a portrait mosaic, the latter
  with its panel seams showing the crop) and Session cards sections. Two ignored image
  snapshots need a GPU: the gallery's
  (`crates/mega-ui/tests/snapshots/`) and the real start screen's
  (`crates/mmm-app/tests/snapshots/start_screen.png`, from
  `start::snapshot_tests`: a merged, an empty and a missing session, the
  stage held still). The start-screen reference shows the machine's temp
  path in the hero's details line (the home directory is shown as `~`, so
  no user name, but the temp location and separators are the OS's), so it
  differs on another OS; refresh it there with `UPDATE_SNAPSHOTS=1`.
