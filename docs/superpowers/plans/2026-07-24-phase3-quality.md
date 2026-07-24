# Phase 3 Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Read `docs/DESIGN.md` first (§Phase 2 results + user validation notes). Interfaces/algorithms binding; implementers own bodies. TDD; tests never touch `test_data/`. Phase-1/2 Global Constraints still apply (no dense canvas allocations, rayon, clippy clean, commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`; analyze < 10 s, full blend < 30 s on the 12-panel set).

**Goal:** Fix the one observed artifact class (seam boundary kinking a diffraction spike), restore the defect-suppression advantage that averaging had (cosmic rays/trails in overlaps), add seam diagnostics, and an opt-in global flatten. No GPU work (user directive).

**Context from user validation:** GraXpert-differenced comparison proves phase-2 gradients objectively better; stars "perfect" except **one barely-visible kink in one diffraction spike** — diagnosis: spikes are elongated detail whose cells fall below the 3×-median star-lock threshold, so an ownership boundary crossed one; with per-panel spike-angle differences the two halves disagree slightly.

---

### Task E: Connected star masks (spike-kink fix)

**Files:** seam.rs (mask + use in DP cost and exports), blend.rs (star-lock + base exclusion consume the mask), synth.rs (diffraction spikes), tests. Owns lib.rs only if adding a module (prefer keeping mask in seam.rs).

**Interfaces:**
```rust
// seam.rs
/// Per-panel L8 star/structure mask: seeds = cells with channel-max detail
/// > 3.0× the panel's median detail; grown by 8-connectivity flood fill onto
/// cells > 1.5× median. Covers spike arms connected to star cores.
pub fn star_mask(summary: &L8Summary) -> Vec<bool>;   // len w8*h8
```
- DP seam cost: masked cells (either panel) get + `100×` the edge's median cost (large finite — seams cross structure only when a band is entirely structure, choosing the least-bad line; M42-band behaviour must not regress to worse than today).
- blend.rs star-lock: snap where the boundary cell is masked in either panel (replaces the raw 3× check); base-band star exclusion switches to the same mask (∪ existing rim logic).
- synth.rs: `panel_spike_angle: Vec<f32>` (radians; empty = none) — 4-armed diffraction spikes on the brightest stars (arm length ∝ amplitude, width ~1 px, additive), rotated per panel by the given offset. Angle offsets between panels simulate session rotation.

**Tests (mandatory):** (1) mask connectivity: synthetic star with spikes → mask covers core AND arm cells ≥ 2× arm length of the 3×-seed-only mask; background cells unmasked; (2) owner boundary never crosses masked cells when a clear background corridor exists (construct one); (3) end-to-end spike integrity: 2 panels, shared bright spiked star in overlap, spike angles differing 0.02 rad → every pixel along each merged arm matches ONE panel (anti-pinching check applied to arms; must FAIL with the mask disabled — prove it bites); (4) existing 74 tests stay green (M42-like high-detail bands: seam falls back gracefully).

- [ ] TDD → green → clippy → real-data smoke (analyze unchanged; TwoBand full blend timing; owner-map spot check via Task G once available) → commit `feat: connected star masks protect diffraction spikes from seams`.

---

### Task F: Overlap defect veto (cosmic rays / satellite trails)

**Files:** blend.rs (TwoBand detail stage), synth.rs (defect injection), main.rs (`--defect-veto on|off`, default on), tests.

**Algorithm (binding):** in the TwoBand detail stage, for output pixels where ≥2 panels are covered: let `d_o` = owner detail, `d_p` = detail of the highest-weight other covered panel. If `|d_o − d_p| > 6.0 ×` the owner cell's detail RMS **and** the cell is star-mask-clear in both panels (Task E's mask): use `min_by(|d| d.abs())` of the two details instead of `d_o`. Feather mode untouched. Document the trade-off (a genuine transient in one panel is suppressed too — that is the desired behaviour for trails/cosmics).

- `BlendParams { pub defect_veto: bool }` default `true` (update literals).
- synth.rs: `panel_defects: Vec<(usize, u64, u64, u32, f32)>` — (panel, x, y, length_px, amplitude): a bright 1-px-wide line segment (length 1 = cosmic ray, longer = trail), added after gain/offset inside the window.

**Tests (mandatory):** (1) trail in one panel's overlap → vetoed: merged matches the clean panel within noise; `defect_veto: false` → trail present (both directions); (2) anti-pinching test still passes with veto ON (star mask exemption works — shifted stars are NOT vetoed); (3) defect *outside* any overlap is untouched (nothing to compare against — owner value stands).

- [ ] TDD → green → clippy → real-data smoke + timing → commit `feat: cross-panel defect veto suppresses cosmics and trails in overlaps`.

---

### Task G: Seam & ownership diagnostics

**Files:** NEW crates/mmm-core/src/diag.rs (+ lib.rs line), main.rs (`mmm report --seam-png <path>` and a `seam Δ` column), reuse output/png.rs stretch helpers (refactor small shared fns into diag.rs or pub helpers — do not duplicate).

**Deliverables:** (a) seam-map PNG: autostretched L8 luminance of the blended preview, owner regions tinted (12-hue palette, subtle), owner boundaries drawn dark, panel ids labelled at region centroids (simple 3×5 bitmap digits — no font deps); (b) `mmm report` gains per-edge `seam Δ` = mean |corrected_i − corrected_j| over that edge's boundary cells, with ⚠ > 3× median (a high seam Δ predicts a visible seam better than fit rms).

**Tests:** seam Δ computed correctly on a hand-built 2-panel case; PNG writes and round-trips dimensions; boundary cells found = cells whose 4-neighbourhood contains a different owner.

- [ ] TDD → green → clippy → generate the real 12-panel seam map, LOOK at it (Read), verify seams avoid M42/star cores visibly, note seam Δ table → commit `feat: seam map diagnostics and per-edge seam residuals`.

---

### Task H: Opt-in global flatten

**Files:** NEW crates/mmm-core/src/flatten.rs (+ lib.rs line), blend.rs (subtract during output), main.rs (`--flatten off|1|2`, default off), synth.rs (optional common gradient `global_gradient: (f32,f32,f32)` a+bx+cy added to ALL panels' truth), tests.

**Algorithm:** after prep, build the L8 *merged* background: per fully-covered cell take the weighted blend of corrected means; mask out star cells (Task E masks) and cells > bg median + 3×MAD; fit order-1/2 poly (reuse surfaces.rs term machinery / linalg::solve_dense); subtract `f(x,y) − f(center)` during output (preserves central level, per channel). Errors if < 20% of cells are background (refuse to flatten pure-nebula mosaics).

**Tests:** synth with `global_gradient` on all panels → `--flatten 1` removes it (post-flatten background plane fit amplitude < 10% of injected); flatten off → gradient intact; nebula-heavy refusal path.

- [ ] TDD → green → clippy → real-data: blend with `--flatten 2` (order-2 flatten of the merged mosaic) into a separate FITS + PNG for the user to compare against GraXpert; honest visual notes → commit `feat: opt-in global background flatten`.

---

**Sequencing:** E → F → G → H (all touch blend.rs or seam.rs; no parallelism). After H: orchestrator refreshes DESIGN/README/memory, produces final user hand-off (updated orion_phase2.fits → orion_phase3.fits + flattened variant + seam map).
