# Phase 4 Pyramid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Read `docs/DESIGN.md` §Phase 4 first. Phase-1..3 Global Constraints apply (rayon, clippy clean, no dense canvas allocations, tests never touch `test_data/`, commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`; 12-panel targets: analyze < 10 s, full blend < 30 s).

**Goal:** `BlendMode::Pyramid` — the star-free base blended as an L8-grid Laplacian pyramid with scale-proportional seam transitions; all phase-3 star/defect guarantees preserved verbatim. Then real-data validation.

---

### Task P1: L8-grid Laplacian pyramid base

**Files:** NEW `crates/mmm-core/src/pyramid.rs` (+ lib.rs line), blend.rs (`BlendMode::Pyramid` default, base construction swap), synth.rs (mid-frequency mismatch injection), main.rs (`--mode pyramid|twoband|feather`, default pyramid), test literals.

**Interfaces:**
```rust
// pyramid.rs — all on the L8 cell grid (w8 × h8), f32 planes
pub struct CellPyramid { pub levels: Vec<Vec<f32>>, pub w8: u32, pub h8: u32 } // Laplacian levels 0..n + residual last
pub fn build(plane: &[f32], w8: u32, h8: u32, n_levels: u32) -> CellPyramid;   // 5-tap Gaussian [1,4,6,4,1]/16, downsample ×2, upsample bilinear
pub fn collapse(p: &CellPyramid) -> Vec<f32>;
/// Blend per level: out_ℓ = Σ_i m_iℓ·p_iℓ / Σ_i m_iℓ, where m_iℓ is panel i's
/// ownership mask pyramid (Gaussian pyramid of the hard owner indicator,
/// itself smoothed at each level so transition width ≈ level scale).
pub fn blend_pyramids(panels: &[(CellPyramid /*data*/, CellPyramid /*mask*/)]) -> Vec<f32>;
```

**Algorithm (binding):**
- Per channel: each covered panel's star-free corrected L8 base plane (existing onion-fill machinery) and its hard ownership indicator (1.0 where `owner == panel`, else 0; cells uncovered by the panel are excluded from both data and mask, with mask renormalization at each level over valid panels only — no bleed from outside a panel's coverage).
- `n_levels = ceil(log2(feather_px / 8))` clamped to [2, 6] (feather 256 → 5).
- Base for the detail stage = collapse of the mask-blended pyramid; everything downstream (detail ownership, ramp, star-lock, veto, upsample to full res) is byte-for-byte the TwoBand path.
- Guard: where Σ masks ≈ 0 at a level (numerical), fall back to the feather-weighted base value for that cell.

**Mandatory tests:**
1. Pyramid identity: build→collapse == input within 1e-6 (random plane, several sizes incl. non-power-of-two).
2. Mid-frequency ghost reduction: synth two panels, inject `mid_blobs` (new SynthSpec field: Gaussian blobs σ≈3 cells at L8 scale = ~24 px) displaced 3 px between the panels (reuse `panel_shift` applying to blobs too — extend it or add `shift_blobs: bool`); metric = RMS of (merged − either panel) over blob region in the overlap. Pyramid mode must beat TwoBand's feathered base by ≥30% on that metric, and show no step at the seam line (profile across the boundary is monotone within noise).
3. All phase-3 guarantee tests pass in Pyramid mode: anti-pinching (shifted stars single-panel), spike integrity, defect veto, single-panel reconstruction (< 1e-5), e2e RMSE bound.
4. Feather/TwoBand modes bit-identical to before (regression guard on a fixed-seed synth hash or direct comparison).

- [ ] TDD → green → clippy → real-data smoke: full-res Pyramid blend timing (<30 s; expect ~7-8 s), numeric diff vs TwoBand output (expect: background/star cores identical or sub-noise; differences confined to mid-scale structure near seams — characterize honestly), seam-map + seam Δ unchanged (ownership identical) → commit `feat: Laplacian pyramid base blending with scale-proportional seam transitions`.

### Task P2 (orchestrator): validation + docs

- [ ] Crops of the busiest seams (M42 band, PANEL-5|6 ⚠ edge) Pyramid vs TwoBand; verify no regression; update DESIGN §Phase 4 results, README, memory; fresh orion_phase4.fits hand-off.
