# Formats consolidation — Phase 4: one set of astrometric-solution decoders

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** mmm stops decoding astrometric solutions itself. PixInsight
solutions (the 1.9.5 standard block and the legacy ≤ 1.9.4 block) and FITS
TAN / TAN-SIP solutions are decoded by mpp-core. mmm builds its
grid-accelerated `WcsModel` from mpp's `AstrometricSolution`.

**Architecture:**
- **mpp gains:**
  - `PiSolution::projection_to_image`, the inverse direction mmm samples
    its native → image grid from;
  - a real `WcsSolution` (FITS linear WCS plus optional SIP, ported from
    mmm's `fits_wcs.rs` + `sip.rs`);
  - `AstrometricSolution::from_header` reading FITS WCS when the header has
    no PixInsight block.
- **mmm keeps:**
  - `WcsModel` and its lazy grid sampling (`standard::sample_fn`,
    `native_rect` and `matrix_scale` move into `astrometry/mod.rs`, or into
    a small `sampling.rs`);
  - its own `Grid2D` and `eval` (mpp's legacy grids are converted field by
    field, so evaluation is unchanged);
  - `LinearWcs` and the output WCS cards;
  - `describe_unsolved` and reprojection.
- **mmm deletes:** `standard.rs`, `spline.rs`, `legacy.rs` and the decoding
  parts of `fits_wcs.rs`/`sip.rs`. `fits_wcs` stays as thin adapters
  (`model_from_keywords`, `linear_for_panel`, `linear_from_keywords`) over
  mpp, so callers and integration tests keep their names.

**Spec:** roadmap §4 Phase 4; Phase 0 `crates/mmm-core/tests/wcs_golden.rs`.

## Global Constraints

- Branches and green rules as in Phases 0–3.
- **Parity gates**, all three required:
  1. `wcs_golden` passes **unchanged**, to 1e-9°.
  2. mmm's existing astrometry behaviour tests pass. The decoder-internal
     ones move to mpp with their assertions.
  3. Real data: re-running analyze + blend on `orion_mosaic_raw_panels`
     (legacy solutions) and `orion_mosaic_raw_panels_195` (standard block)
     matches the pre-Phase-4 baseline in `test_data/phase4_baseline/`
     (binary `mmm-before`, commit in `commit.txt`). Panel placement
     (`session.json` bboxes and the frame) must be equal. Blend output
     pixels must be equal, or differ by at most 1e-6 relative where float
     evaluation order changed. Any difference beyond that is a finding to
     debug, not to bless.
- **Behaviour that must not change:**
  - A solution whose distortion layer fails validation is refused (`None`
    model with a reason), never approximated by its linear part.
  - `linear_wcs()` still returns the linear layer for any panel whose
    layer 1 decodes, even if its higher layers are bad (mmm's
    `linear_from_standard` reads layer 1 only).

## Review Focus

1. **Broken higher layer:** a standard block with a broken layer 3 must
   give `linear_wcs() == Some` and `wcs_model() == Err(reason)`, exactly as
   before. Task 4.3, `broken_distortion_keeps_linear_but_refuses_model`.
2. **Bottom-up FITS with SIP:** the reflection into the top-down frame
   matches the old model at the image corners (golden `fits_tan_sip`).
3. **Unsupported FITS WCS** (TPV, PV, FK4): refused with the same reason
   text as before (mmm's `fits_wcs` tests, ported to mpp).
4. **Standard block with only layer 1:** a linear-only model, no grids.
5. **Grid sampling** (inverse direction) uses
   `PiSolution::projection_to_image`. It must round-trip image → native →
   image within 1e-3 px over the image. Task 4.1.

---

### Task 4.1 (mpp): `PiSolution::projection_to_image`

- `pub fn projection_to_image(&self, xi: f64, eta: f64) -> (f64, f64)`
  chooses its path in this order:
  - the legacy `Grids::native_to_image` when present;
  - else `projective.projection_to_image`, plus the `Rbf` projection→image
    residual;
  - else the inverse of the linear layer.
- **Test** `projection_to_image_inverts_image_to_projection`:
  - **Inputs:** the `pi195_solution` fixture (tests/), plus synthesized
    linear and projective-only solutions.
  - **Assertion:** image → projection → image round-trips within 1e-3 px
    on a 9×9 grid over the image.
- **Commit:** `feat(core): PiSolution::projection_to_image (inverse direction)`.

### Task 4.2 (mpp): FITS WCS / SIP as `WcsSolution`

- **Port mmm's `fits_wcs.rs` and `sip.rs`** into `mpp-core/src/astrometry/fits.rs`
  (and `sip.rs`), using `crate::wcs::LinearWcs` (the same fields as mmm's)
  and `formats::{RowOrder, card_number, card_string}`.
- **`WcsSolution` becomes**
  `pub struct WcsSolution { pub linear: LinearWcs, pub sip: Option<SipSolution>, pub row_order: RowOrder }`.
  It provides:
  - `linear_top_down()`;
  - `image_to_native` / `native_to_image`;
  - `validate`;
  - `pixel_to_sky` in PixInsight top-down image coordinates.

  It is no longer uninhabited, so the `AstrometricSolution` methods gain
  real `Wcs` arms. JSON/postcard tag `"Wcs"` is kept.
- **Constructors:**
  - `pub fn from_keywords(cards: &[FitsKeyword], width: u64, height: u64, row_order: RowOrder) -> Result<WcsSolution, String>`
    keeps mmm's error texts verbatim.
  - `AstrometricSolution::from_header` falls back to it when there is no
    PixInsight block but there are `CTYPE1`/`CTYPE2` cards. `ROWORDER`
    comes from the keywords; the size comes from `image_size` or `NAXIS`.
- **Tests:** port mmm's 6 `fits_wcs` tests and 9 `sip` tests unchanged in
  substance, and copy `sip_oracle.json` into mpp's `tests/fixtures/`.
- **Commit:** `feat(core): FITS TAN / TAN-SIP solutions (WcsSolution), ported from mmm`.

### Task 4.3 (mmm): `WcsModel` from mpp's solutions

- `WcsModel::from_solution(sol: &mpp_core::astrometry::AstrometricSolution, width, height) -> Option<WcsModel>`:
  - PixInsight with legacy `Grids`: `with_grids`, converting mpp's
    `Grid2D` to mmm's.
  - PixInsight with `projective` (± `Rbf`): `Distortion::Standard` over an
    `Arc<PiSolution>`. Sampling uses `image_to_projection` and
    `projection_to_image`, and the model is validated with mmm's existing
    `validate_model` logic, ported to `PiSolution`.
  - PixInsight linear only: `linear_only`.
  - `Wcs` with SIP: `Distortion::Sip` over mpp's `SipSolution`. Without
    SIP: `linear_only`.
- `WcsModel::from_properties` and `fits_wcs::model_from_keywords` build an
  `mpp_core::header::Header` and go through `from_solution`.
  `wcs_from_properties` (linear only) reads layer 1 through mpp's standard
  and legacy linear readers, adding
  `pub fn linear_layer(props) -> Result<Option<LinearWcs>, String>` to mpp
  if needed, so that a broken higher layer still yields the linear WCS.
- Delete mmm `standard.rs`, `spline.rs` and `legacy.rs`, plus the decoding
  parts of `fits_wcs.rs`/`sip.rs`. Move their tests to mpp where they test
  decoding, or keep them in mmm where they test `WcsModel`.
- **Tests:**
  - `wcs_golden` passes unchanged;
  - the mmm astrometry tests that remain pass;
  - `broken_distortion_keeps_linear_but_refuses_model` (new).
- **Commit:** `refactor(core): astrometric solutions decoded by mpp-core`.

### Task 4.4: real-data parity and docs

- Re-run the baseline commands with the new binary into
  `test_data/phase4_after/`. Compare:
  - `session.json` panel bboxes and frame, which must be equal;
  - blend output FITS headers and pixel data (`cmp`, or the max abs
    difference if not identical).
- Docs:
  - mmm `DESIGN.md`: astrometry comes from mpp; record the parity results.
  - mpp `DESIGN.md`: FITS WCS / SIP is supported; `WcsSolution` is real.
- **Commit:** `docs: astrometry from mpp-core; real-data parity`.
