# Phase 2 Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Read `docs/DESIGN.md` (§Phase 2 and §POC results — including the user-validation guard rail) before any task. Interfaces/algorithms here are binding; implementers own function bodies. TDD throughout; tests never touch `test_data/`.

**Goal:** Kill the remaining artifact classes: residual colour-tint gradients (A), star safety under misregistration via seams + two-band blending (B), WCS in output (C), ROI mode (D).

**Architecture:** All additions ride the existing session pipeline; analyze gains a detail-energy plane (summary format v2), blend gains a two-band mode and surface application. No new crates.

## Global Constraints

- Everything from the phase-1 plan's Global Constraints still applies (no dense canvas allocations, rayon, clippy clean, commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`).
- **Signal-protection guard rail (from user validation):** any fit to overlap *residuals* must exclude signal-dominated cells (cell excluded if corrected value > panel background median + 3×MAD, computed over the panel's covered cells per channel), sigma-clip survivors at 2.5σ, and cap the applied correction magnitude (warn in report if max|s| > 5× background MAD).
- Real-data sanity after each task on the existing session; analyze time must stay < 10 s, full blend < 30 s.

---

### Task A: Residual surface correction

**Files:** Create `crates/mmm-core/src/surfaces.rs`. Modify `analyze.rs` (fit after photometry, save `analysis/surfaces.json`), `session.rs` (`surfaces_path()`), `blend.rs` (apply during accumulation), `synth.rs` (add `panel_gradient_range: (f32,f32)` to `SynthSpec` — per-panel plane `a + b·x/w + c·y/h` with a,b,c drawn from the range, applied inside the window with gain/offset), `crates/mmm/src/main.rs` (`--surface off|1|2` on analyze, default 2; report prints per-panel max|s| with ⚠), `tests/e2e.rs` (extend with gradient perturbation, tighter final RMSE).

**Interfaces:**
```rust
pub struct Surfaces { pub order: u32,                       // 0=constant,1=plane,2=quadratic
    pub coeffs: Vec<Vec<Vec<f64>>> }                        // [channel][panel][n_terms]; terms 1,x,y,x²,xy,y² (normalized coords x=X/canvas_w, y=Y/canvas_h)
impl Surfaces { pub fn eval(&self, ch: usize, panel: usize, x: f64, y: f64) -> f64;
    pub fn save(&self, p: &Path) -> Result<()>; pub fn load(p: &Path) -> Result<Surfaces>; }
pub fn fit_surfaces(summaries: &[L8Summary], graph: &OverlapGraph, phot: &Photometry,
                    canvas: (u64,u64,u64), order: u32) -> Result<Surfaces>;
```

**Algorithm:** residual per shared background cell (guard rail!): `r = (g_i·x+o_i) − (g_j·y+o_j)`. Minimize `Σ_e Σ_cells (r + s_i(p) − s_j(p))² + λ Σ_i Σ_{cells∈i} s_i(p)²` with λ = 1e-3 (normalized by cell counts), gauge: reference panel (same as photometry's) constant term = 0. Normal equations (T·N)×(T·N), T = terms; solve with existing `solve_dense`. Per channel. Blend applies `g·v + o + s(x,y)` (Horner per pixel; per-8px-cell eval + bilinear is an acceptable optimization).

**Tests (mandatory):** (1) synth 2×2 with injected per-panel gradients, no stars/noise → fitted surfaces cancel injected differences: post-correction overlap residual RMS < 5% of injected gradient amplitude; (2) same with stars → surfaces unchanged within 20% (signal exclusion works — the star flux must not bend the fit); (3) e2e with gradients + stars + noise → final RMSE bound as phase-1 but with gradients present; (4) `--surface off` bypasses cleanly.

- [ ] Steps: failing tests → red → implement → green → clippy → real-data smoke (analyze + report: max|s| values sane, i.e. same order as the colour tints ~1e-3; full blend; eyeball downsampled PNG for reduced tints) → commit `feat: residual surface correction with signal protection`.

---

### Task B: Two-band blend with star-avoiding seams

**Files:** Create `crates/mmm-core/src/seam.rs`. Modify `summary.rs` (**format v2**: add per-cell per-channel `detail` plane = RMS of (pixel − cell mean) over covered pixels; bump magic to `MMM9`, reject v1 files with "re-run analyze"), `analyze.rs` (accumulate detail plane), `blend.rs` (add `BlendMode::{Feather, TwoBand}` to `BlendParams`, default TwoBand; implement base+detail path), `synth.rs` (add `panel_shift: Vec<(f32,f32)>` optional per-panel sub-pixel shift of star positions only — simulates misregistration), `main.rs` (`--mode feather|twoband`), tests.

**Interfaces:**
```rust
// seam.rs
pub struct OwnerMap { pub w8: u32, pub h8: u32, pub owner: Vec<u16> } // panel idx per L8 cell; u16::MAX = none
pub fn compute_owner_map(summaries: &[L8Summary], graph: &OverlapGraph,
    phot: &Photometry, surfaces: Option<&Surfaces>, canvas: (u64,u64,u64),
    feather_px: f32) -> OwnerMap;
```

**Algorithm:** start owner = argmax feather weight (existing distance maps). Per edge (rayon), within the overlap bbox: DP min-cost seam over the L8 band, oriented along the band's long axis (one cell per row/column, ±1 step); cost(cell) = |corrected_i − corrected_j| (channel-max) + β·max over channels of max(detail_i, detail_j) with β = 4.0 (star avoidance); relabel cells on each side accordingly. Diagonal corner edges (< 64 cells wide either axis): keep Voronoi labels. One shared owner map for all channels. Blend TwoBand: `base_i(x,y)` = bilinear-upsampled corrected L8 mean; `detail_i = corrected_full_i − base_i`; output = feathered Σw·base / Σw + detail from owner, ramped linearly over ±16 px of the owner boundary **except** snapped hard where the boundary-adjacent cells' detail energy > 3× that panel's median detail (star-lock). Uncovered-owner fallback: nearest covered panel by weight.

**Tests (mandatory):** (1) owner map on synthetic two-panel overlap with a bright fake star mid-band → seam routes around the star's cells (no owner boundary within 2 cells of the star); (2) synth 2×2 with `panel_shift` 0.6 px on one panel, blend TwoBand → for each bright star in the overlap, the merged peak neighbourhood matches ONE panel's corrected pixels (max abs diff < noise), never the average (this is the anti-pinching assertion; with Feather mode the same check must FAIL, proving the test bites); (3) base+detail reconstruction: single panel, TwoBand output == corrected input within 1e-5 away from boundaries; (4) e2e still green in TwoBand mode.

- [ ] Steps: failing tests → red → implement → green → clippy → re-run real analyze (v2 summaries) + TwoBand full blend + crops of M42 seam band and a star-dense seam via the scratchpad crop script (orchestrator will eyeball) → commit `feat: two-band blend with star-avoiding seams`.

---

### Task C: WCS passthrough from XISF properties

**Files:** Modify `crates/mmm-core/src/formats/xisf.rs` (parse `<Property>` elements: id, type, inline value or element text; attachment-located properties: expose offset/size + a typed reader for f64 vectors/matrices), `crates/mmm-core/src/formats/mod.rs` (property types). Create `crates/mmm-core/src/astrometry.rs`: extract linear WCS from PixInsight astrometric-solution properties, produce FITS cards. Modify `output/fits.rs` only to accept extra cards (if not already generic). Do NOT touch main.rs/blend.rs (orchestrator wires CLI after).

**Interfaces:**
```rust
pub struct XisfProperty { pub id: String, pub type_: String, pub value: PropertyValue }
pub enum PropertyValue { Str(String), F64(f64), I64(i64), F64Vec(Vec<f64>), F64Mat{rows:u32,cols:u32,data:Vec<f64>}, Unread }
// astrometry.rs
pub struct LinearWcs { pub crval: [f64;2], pub crpix: [f64;2], pub cd: [[f64;2];2], pub ctype: [String;2] }
pub fn wcs_from_properties(props: &[XisfProperty]) -> Option<LinearWcs>;
pub fn wcs_cards(w: &LinearWcs, crop_origin: (u64,u64)) -> Vec<FitsKeyword>;
```

**Process note:** FIRST inspect a real panel (`test_data/orion_mosaic/*PANEL-1_*.xisf`) — dump all property ids/types (write a tiny throwaway example or test) and identify the solution properties PixInsight writes (expect ids like `PCL:AstrometricSolution:*` / `Observation:Center:*`). Build `wcs_from_properties` against what is actually there; document the mapping in astrometry.rs doc comments. Sanity: computed center sky coords of the canvas must match the RA/DEC FITS keywords (84.199, −3.240) within arcseconds; CRPIX shift math must place the same sky coord at the same star after cropping.

**Tests (mandatory):** property parsing on a synthetic XISF with inline + attachment properties; `wcs_cards` CRPIX shift; real-data test is a manual smoke (assert via the RA/DEC cross-check printed by an example/test helper, and report it).

- [ ] Steps: inspect real properties → failing tests → implement → green → clippy → commit `feat: WCS extraction from XISF astrometric properties`.

---

### Task D: ROI + validation (orchestrator)

- [ ] Wire WCS cards into `mmm blend` (main.rs) once A–C land; `--roi x,y,w,h` (canvas coords → clamps output bbox; analyze artifacts unchanged).
- [ ] Full real-data validation: TwoBand + surfaces + WCS blend; crops incl. M42 seam; verify WCS center; PixInsight hand-off to user.
- [ ] Update DESIGN.md POC/phase-2 results, README, memory; commit docs.
