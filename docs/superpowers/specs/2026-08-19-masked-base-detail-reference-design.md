# Masked-base detail reference (Bug 2: pyramid/twoband detail loss at signal-covered narrow overlaps)

## Problem

RickJay 25-panel Ha set: at M42's saturated core — which sits exactly on a
4-panel corner junction whose overlap bands (~450 px) are *entirely* covered
by bright structure — Pyramid mode renders a flat white L8-blocky square
(overshoot ≈ +0.2 over truth) beside a near-black bar (undershoot ≈ −0.03),
with high-frequency content stripped. TwoBand shows a milder flat-white
block; Feather is clean. Downsampled previews hide it (the L8 path skips the
detail machinery).

## Root cause

The two-band/pyramid output is `blended_base + detail`, with each panel's
detail defined against **its own** star-free base plane:
`detail_k = full_k − base_k`. Reconstruction of the owner is exact only where
`blended_base ≈ base_owner`.

Inside star/structure-masked regions the base planes are onion-peel **fill**
— each panel extrapolates its own surrounding background inward. Over a
giant masked region (M42's core, ~50+ cells) the per-panel fills diverge by
large amounts. Normally harmless: the seam DP routes ownership boundaries
around masked structure, one owner covers the whole region, and the identity
holds (Orion's ~1500 px overlaps always leave a background corridor). But
when the masked structure spans the *entire* overlap, boundaries are forced
through it: the blend then mixes divergent fictional fills — feather-averaged
in TwoBand (bounded error), Laplacian-recombined across scales in Pyramid
(large over/undershoot) — and the difference `blended_base − base_owner`
prints onto the output at L8 resolution.

## Fix (approved approach)

In the full-res detail mix, any contributing panel whose base at the current
cell is fill — its per-panel base-exclusion mask covers the cell — has its
detail computed against the **blended base value at the pixel** instead of
its own base plane:

```
base_ref_k = if base_excluded_k(cell) { blended_base(px) } else { base_k(px) }
detail_k   = full_k − base_ref_k
```

Wherever base content is invented, the output algebraically reduces to the
ramp-mix of the corrected full-res panels (exactly what Feather produces
there — verified clean on the real data); wherever real base content exists,
nothing changes. The switch is per-panel and continuous to first order:
fills anchor to the true background at the mask edge, so
`blended_base ≈ base_k` exactly where the definition changes. The star-lock
(hard) path gets the same treatment — output becomes the owner panel exactly
in masked cells, strengthening the star guarantee. The defect veto only acts
on mask-*clear* cells, so it never sees the changed reference.

Applies to TwoBand and Pyramid identically (the mixing loop is shared);
Feather has no detail path and is untouched.

## Implementation notes

- `blend.rs` full-res loop: the per-panel base-exclusion masks (compact ∪
  structure, the same planes `suppress_stars_in_base` uses) must be
  available per cell; detail reference selected per contributing panel.
- The blended base value at the pixel is already computed for the output
  sum; reuse it.
- Synth harness gains an optional giant-Gaussian `core` (rendered like a
  wide star, `None` byte-identical) so a saturated structure can be pinned
  onto a 4-corner junction deterministically.

## Tests

1. **Reproduction (must fail pre-fix):** 2×2 grid, narrow overlaps, a bright
   σ≈60 px core centred on the 4-corner junction, divergent per-panel
   gradients. Assert Pyramid ≈ Feather over the core region (bounded p99
   difference) and no undershoot below the local background.
2. All existing guarantees re-run green: anti-pinching, spike integrity,
   dark-moat, dark-streak, deep-single-coverage, defect veto, ghost metric
   (pyramid mid-frequency benefit must not regress).
3. Hash guards: Feather byte-identical; TwoBand/Pyramid recaptured with a
   comment if the spec's synthetic data exercises masked-cell mixing.
4. Real-data smoke: RickJay M42 ROI in Pyramid mode — blocks gone, detail
   present; diff vs Feather bounded.
