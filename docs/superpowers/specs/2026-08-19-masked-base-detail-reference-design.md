# Masked-base detail reference (Bug 2: pyramid/twoband detail loss at signal-covered narrow overlaps)

Status: implemented 2026-08-19. The root-cause narrative below reflects what
the real data showed during implementation, which went deeper than the
original diagnosis (base-fill divergence); the approved fix direction
(blended-base detail reference) survived and two fill-hygiene mechanisms
joined it.

## Problem

RickJay 25-panel Ha set: at M42's saturated core — which sits exactly on a
4-panel corner junction whose overlap bands (~450 px) are *entirely* covered
by bright structure — Pyramid mode rendered a flat white L8-blocky square
(overshoot ≈ +0.2 over truth) beside a near-black bar (undershoot ≈ −0.03),
with high-frequency content stripped. TwoBand showed a milder flat-white
block; Feather was clean. Downsampled previews hide it (the L8 path skips
the detail machinery).

## Root cause (as measured on the real data)

The two-band/pyramid output is `blended_base B + detail`, with each panel's
detail referenced to **its own** star-free base plane. Reconstruction of the
owner is exact only where `B ≈ base_owner`. Three mechanisms broke that
around M42:

1. **Fill flood from a saturated plateau.** M42's flat saturated core is
   detail-free, so the detail-energy star mask floods *around* it but leaves
   the plateau itself unmasked — a trusted onion-peel fill source in the
   middle of a giant masked complex. One panel's fill anchored there and
   flooded ~0.95 across tens of thousands of cells of its base; its
   partners' fills carried background (~0.008). The pyramid mixed that
   disagreement into the giant over/undershoot blocks.
2. **Long-range fill divergence.** Even without a rogue source, each panel's
   fill deep inside a large masked complex extrapolates *its own* anchor
   geometry; fills agree only locally.
3. **Coarse-level smear.** The pyramid's mask/validity machinery spreads
   whatever the masked-zone bases contain (and validity-edge extrapolations
   of it) over level-scale distances beyond the mask.

Orion's ~1500 px overlaps never showed this: the seam DP routes ownership
around masked structure, one owner covers the complex, and the identity
holds. RickJay's 450 px bands are narrower than the M42 complex, forcing
ownership boundaries through it.

## Fix (as built, `blend.rs`)

1. **Two-phase onion fill** (`suppress_stars_in_base`): the first
   `FILL_LOCAL_LAYERS = 8` passes propagate any brightness (star blobs and
   rim bands rightly fill from their immediate, possibly bright,
   surroundings — locally, fills are cross-panel-consistent); beyond that,
   only cells at background level (≤ source-median + `ANCHOR_MAD_K = 20`
   MADs) keep propagating, so a saturated plateau can no longer flood a
   complex. Genuine bright unmasked content keeps its values — it just
   doesn't propagate far.
2. **Blended-base detail reference** (the approved approach): a shared
   switch plane `ref_t` moves every contributing panel's detail reference
   from its own base to the blended base `B`. Where `t = 1`, the base's
   sins cancel exactly and the output is the ramp-mix of the corrected
   full-res panels (what Feather produces there); where `t = 0`, pyramid
   semantics are untouched. The switch must be shared (union over panels) —
   a per-panel switch pits one panel's genuine content against another's
   fill and prints the difference.
3. **Switch coverage = deep mask + halo**: `ref_t = 1` on cells deeper than
   the fill's local layers inside the (2-cell-dilated, so thin rim channels
   don't cut it) union mask, extended over enclosed unmasked pockets
   (hole-fill — a plateau inside a nebular core shares its fate), ramping
   to 0 over a feather-scaled halo (`feather_px/8` cells, clamped 4–32)
   that covers the coarse-level smear. Star- and blob-sized masks never
   reach the depth threshold, preserving the pyramid's scale-matched base
   transitions — and its mid-frequency ghosting benefit — everywhere
   genuine base content lives.

Feather mode is byte-identical (all-false masks); TwoBand changes only where
deep masked complexes exist; star-lock cells inside switched zones now
reproduce the owner exactly (stronger than before).

## Validation

- New reproduction test `masked_core_on_narrow_overlap_corner_reconstructs_cleanly`
  (synth harness gained an optional `core`: flat saturated plateau + clumpy
  masked annulus at a 2×2 4-corner with narrow bands): pre-fix TwoBand
  p99 |blend−feather| = 0.030 / max 0.64; post-fix TwoBand ≤ 1e-4,
  Pyramid p99 ≤ 5×noise with no undershoot.
- All 192 tests green (debug + release), including every phase-3/4
  guarantee (ghost ratio 0.175, spike integrity, moats, streaks, veto).
- Hash guards: only the Pyramid hash on the flooded-mask spec moved
  (recaptured with comment); Feather/TwoBand and all analyze artifacts
  byte-identical.
- Real data: M42 ROI in Pyramid mode fully detailed, blocks gone
  (background p99 vs feather 0.0016; no negative dip); Orion M42 ROI
  unchanged and clean.
