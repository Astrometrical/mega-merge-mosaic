# Phase 5: Unaligned Input (bypass MosaicByCoordinates)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Read `docs/DESIGN.md` first. Phase-1..4 Global Constraints apply (rayon, clippy clean, TDD, no dense canvas allocations, tests never touch `test_data/`, commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`).

**Goal:** Accept *unaligned* panels that carry accurate PixInsight astrometric solutions: derive the mosaic reference frame, reproject panels ourselves (including spline distortion), and feed the existing session pipeline. Aligned full-canvas input remains supported and byte-identical; input kind auto-detected.

**Why it's tractable:** verified on `test_data/orion_mosaic_raw_panels/`: PI stores `PCL:AstrometricSolution:SplineWorldTransformation:PointGridInterpolation:{ImageToNative,NativeToImage}` — dense precomputed grids (`GridX`/`GridY` F64Matrix attachments + `Delta` + `Rect`) for BOTH directions. We interpolate PI's own grids (bicubic); no thin-plate-spline evaluation needed. Linear-only solutions (registered frames) remain the fallback. Also present for validation: `LinearApproximation`, `Projective:{ImageToNative,NativeToImage}`, `RBFType`, `Version`, `SplineOrder`.

**The killer validation asset:** the same 12 panels exist BOTH raw-with-solutions AND MosaicByCoordinates-registered. Our reprojection can be compared field-wide against PI's.

---

### Task S1: Full WCS model

**Files:** `crates/mmm-core/src/astrometry.rs` (extend), maybe `formats/xisf.rs` (only if attachment property resolution needs work — it already resolves F64Vector/F64Matrix attachments on open), tests.

**Interfaces (binding):**
```rust
pub struct Grid2D { pub rect: [f64;4], pub delta: f64, pub rows: u32, pub cols: u32,
    pub gx: Vec<f64>, pub gy: Vec<f64> }        // node (i,j) ↦ (gx,gy); layout to be verified empirically
impl Grid2D { pub fn eval(&self, x: f64, y: f64) -> (f64, f64); }  // bicubic (Catmull-Rom), clamped at rect edges
pub struct WcsModel {
    pub linear: LinearWcs,                       // always present (fallback + sanity)
    pub image_to_native: Option<Grid2D>,
    pub native_to_image: Option<Grid2D>,
    pub width: u64, pub height: u64,             // solved image geometry
}
impl WcsModel {
    pub fn from_properties(props: &[XisfProperty], w: u64, h: u64) -> Option<WcsModel>;
    pub fn pixel_to_sky(&self, x: f64, y: f64) -> (f64, f64);   // RA,Dec deg; grid path when present
    pub fn sky_to_pixel(&self, ra: f64, dec: f64) -> Option<(f64, f64)>; // None outside grid rect+margin
    pub fn is_spline(&self) -> bool;
}
```

**Empirical mandate:** dump actual values first (a `#[ignore]` test or scratch script on a real raw panel): `Version`, `RBFType`, grid dims vs `Rect`/`Delta` (deduce row/col ordering by consistency: grid corners must map ≈ linear solution), what "native" coordinates are (expected: native projection plane/spherical coords in degrees per Calabretta–Greisen; Gnomonic native↔celestial via `ReferenceNativeCoordinates`(0,90) & `CelestialPoleNativeCoordinates`(180,90) — i.e. the standard TAN rotation about `ReferenceCelestialCoordinates`). Document every verified fact in module docs. Do NOT guess layouts silently — every assumption gets either a runtime validation or a documented check.

**Mandatory validations (as `#[ignore]` real-data tests, run + reported):**
1. Grid vs linear at frame center: agree to < 2″; corner deviation reported (that's the distortion magnitude — expect tens of arcsec on a RASA).
2. Round-trip: pixel → sky → pixel through the grids < 0.05 px RMS over a 20×20 sample grid.
3. **Cross-frame ground truth:** for the SAME raw panel, map 10 bright-star pixel positions (detect locally) raw→sky via the spline model, then sky→canvas via the *registered* panel's linear WCS; compare against the same stars detected in the registered frame (`test_data/orion_mosaic/*PANEL-N*`). Median residual < 0.5 px proves the whole model chain. This is the acceptance test for S1.
4. Synthetic unit tests for `Grid2D::eval` (bicubic exactness on polynomial fields) and the native↔celestial rotation (round-trip, pole handling).

- [ ] TDD → green → clippy → commit `feat: full astrometric model with PI spline grid interpolation`.

---

### Task S2: Reference frame + reprojection into session cache + PanelReader

**Files:** NEW `crates/mmm-core/src/align.rs` (+ lib.rs), NEW `crates/mmm-core/src/panel_reader.rs` (+ lib.rs), `session.rs` (PanelMeta gains `storage: PanelStorage`), `analyze.rs` + `blend.rs` (read via PanelReader — mechanical refactor, behaviour identical), tests.

**Interfaces (binding):**
```rust
// align.rs
pub struct MosaicFrame { pub crval: [f64;2], pub scale_deg: f64, pub width: u64, pub height: u64 }
    // TAN, north-up (CD = diag(-s, s) in the standard frame), CRPIX = canvas center
pub fn choose_frame(models: &[WcsModel]) -> MosaicFrame;   // center = spherical mean of panel centers,
    // scale = median panel scale, canvas = union of reprojected footprints + 16 px margin
pub fn reproject_panel(panel: &XisfPanel, model: &WcsModel, frame: &MosaicFrame,
    out_dir: &Path) -> Result<AlignedPanel>;  // Lanczos-3, rayon over rows; writes cropped cache
pub struct AlignedPanel { pub bbox: [u64;4], pub path: PathBuf } // canvas bbox + planar f32 file

// panel_reader.rs
pub enum PanelStorage { FullCanvasXisf, CroppedCache { bbox: [u64;4] } }
pub struct PanelReader { /* mmap either way */ }
impl PanelReader {
    pub fn open(meta: &PanelMeta, canvas: (u64,u64,u64)) -> Result<PanelReader>;
    /// Channel row clipped to the panel's x extent: (x0, slice) in canvas coords;
    /// canvas rows outside the panel bbox return None.
    pub fn row(&self, c: u64, canvas_y: u64) -> Option<(u64, &[f32])>;
    pub fn advise_sequential(&self);
}
```

**Reprojection (binding):** for each output pixel in the panel's canvas bbox: canvas → sky (analytic inverse TAN of `MosaicFrame`), sky → source pixel (`model.sky_to_pixel`), sample source with **Lanczos-3** (window clamped at frame edge; output zero — no-data — unless the full 6×6 support is inside the source AND source pixels are finite). Per-row cache: the mapping is smooth, but do NOT approximate in POC — evaluate per pixel (grids are cheap). Cache file: raw planar f32 little-endian, bbox dims, mmap-able; per-panel meta in session.json. Scale/rotation differences between source and frame are handled naturally by the mapping; note in docs that strongly different input scales make Lanczos slightly non-flux-conserving (acceptable; matches MosaicByCoordinates behaviour).

**Regression guard (mandatory):** aligned-input sessions must produce byte-identical analyze artifacts and blend output after the PanelReader refactor (hash test on a synth session, before vs after — capture the hash from committed HEAD first).

- [ ] TDD → green → clippy → commit `feat: mosaic frame selection and spline-aware panel reprojection`.

---

### Task S3: Auto-detect + synth WCS + e2e + real validation

**Files:** `analyze.rs` (input-kind detection + align stage orchestration), `main.rs` (`--input auto|aligned|solved`, default auto; align progress/timing output), `synth.rs` (write linear WCS properties into synthetic XISF: `LinearTransformationMatrix`, `ReferenceCelestialCoordinates`, `ReferenceImageCoordinates`, `ProjectionSystem` 'Gnomonic', + geometry; optional small synthetic distortion grids), `tests/e2e.rs`, docs.

**Auto-detect (binding):** all inputs same geometry AND ≥2 panels AND every panel's nonzero fraction < 50% → `aligned` (current path). Otherwise → `solved`: every input MUST yield a `WcsModel` (clear per-file error listing what's missing). Mixed geometries with solutions → solved. `--input` overrides.

**Mandatory tests:** (1) synth e2e: truth sky → cut overlapping panels, write each at its own offset geometry with a correct linear WCS (+ modest known rotation per panel) → `analyze --input solved` → blend → RMSE vs truth (bounds as phase-1 e2e); (2) auto-detect chooses right mode on both kinds; (3) real-data acceptance (manual, reported): full pipeline on the 12 RAW panels → compare blended output against the registered-input phase-4 output: star positions must agree < 1 px (sample ≥ 10 stars across the field via local centroid), overall diff characterized honestly; timings reported (align stage target: < 60 s for 12 panels cold).

- [ ] TDD → green → clippy → real-data run + honest comparison → commit `feat: unaligned solved-panel input with automatic alignment`.

**Sequencing:** S1 → S2 → S3 (S2 needs the model; S3 needs both). Orchestrator wraps with DESIGN/README/memory updates and user hand-off.
