# FITS input with WCS/SIP alignment — design

Date: 2026-09-26. Status: approved design, pre-implementation.

## Goal

Accept FITS panels as a peer input format to XISF everywhere mmm reads
panels: the CLI (`analyze`, `info`, `blend`'s keyword passthrough) and the
PixInsight module's Files path (which runs through the worker's
`--probe-panels` and the `JobMode::Files` run). Solved-mode alignment uses
the panel's FITS WCS, including SIP distortion polynomials. The verified
producer is astrometry.net; other producers (ASTAP, Siril, PixInsight's own
FITS export) follow the same rules but are not verified in this work.

### Non-goals

- Tile-compressed FITS (`.fz`, `ZIMAGE`), image data in extension HDUs,
  and cubes whose planes are not the last axis — refused with clear errors.
- Projections other than gnomonic (`TAN`, `TAN-SIP`), and the TPV / `PV`
  distortion conventions — refused, never approximated by the linear part
  (the existing astrometry policy).
- Sidecar `wcs.fits` headers next to an unsolved image — later, if wanted.
- Any new IPC protocol field, module UI control, or CLI flag. Nothing in
  this design needs one.
- Writing SIP to the output. The output keeps its linear frame WCS; input
  SIP cards are filtered out of the passthrough.

## Conventions

**Internal frame.** Unchanged: canvas rows are top-down; PixInsight image
coordinates are 0-based with pixel `k` spanning `[k, k+1]` (center at
`k + 0.5`); `LinearWcs` is FITS 1-based in that same top-down row order —
the convention `wcs_cards` emits and `MosaicFrame::linear_wcs` uses.

**FITS storage order.** A FITS file stores pixel row `j = 1` first. The
standard convention (astrometry.net, Astropy, DS9) treats the first stored
row as the bottom of the image and the WCS as a function of stored pixel
indices `(i, j)`.

**Orientation rule (fixed, no flag).**

- `ROWORDER = 'TOP-DOWN'` present: rows are read verbatim (stored row
  `r` is canvas row `r`); WCS cards are used verbatim. Image coordinates:
  `x_img = i − 0.5`, `y_img = j − 0.5`.
- Otherwise (`ROWORDER` absent or `'BOTTOM-UP'`): rows are flipped on read
  (stored row `r` is canvas row `H − 1 − r`), and the WCS is reflected into
  the top-down frame. Image coordinates: `x_img = i − 0.5`,
  `y_img = H − j + 0.5`.

The reflection of a linear WCS is the inverse of the existing
`wcs_cards_flipped`: `CRPIX2' = H + 1 − CRPIX2`, `CD1_2' = −CD1_2`,
`CD2_2' = −CD2_2` (CRPIX1, CD1_1, CD2_1 unchanged). Proof: with
`y' = y_img + 0.5 = H − j + 1`, `y' − CRPIX2' = −(j − CRPIX2)`, so the
negated second column reproduces the file's `(ξ, η)` exactly.

PixInsight-authored bottom-up FITS falls under the second bullet. PixInsight
may interpret cards in display space (see the mirror saga in DESIGN.md), in
which case its exported files would come out reflected. This is documented
as unverified; PixInsight export was explicitly not a first-class producer.

**Pixel values.** `phys = BZERO + BSCALE · raw` for every BITPIX. Integer
data is then normalized to `[0, 1]` by dividing by `2^|BITPIX| − 1`, which
matches PixInsight's load of the conventional unsigned encodings (`BZERO =
2^(bits−1)`); values below 0 clamp to 0. Floating data (`BITPIX −32/−64`) is
taken as-is after BZERO/BSCALE. `NaN`, `±Inf`, and `BLANK`-valued integer
samples become 0 — the no-data sentinel — so the covered/uncovered rule
(all channels nonzero) is unchanged.

**Celestial frame.** `RADESYS` when present; else `'FK5'` when `EQUINOX` is
present, else `'ICRS'`. No frame conversion is performed (FK5 J2000 vs ICRS
differ by tens of milliarcseconds, far below the pixel scale); the value is
carried for the output cards.

## Components

### 1. `formats/fits.rs` — `FitsPanel`

Memory-mapped reader for the primary HDU.

- Header: 2880-byte blocks of 80-char cards until `END`. Parsed into the
  existing `FitsKeyword { name, value, comment }` list (value text kept raw,
  quotes included, so passthrough and `keywords_for_output` behave as with
  XISF keywords). Continuation of long strings (`CONTINUE`) is not decoded;
  those cards pass through untouched.
- Geometry: `NAXIS = 2` (1 channel) or `NAXIS = 3` (`NAXIS3` channels,
  planar). `NAXIS3 == 1` is accepted as mono. Anything else, `SIMPLE != T`,
  or a data block extending past the file is an `Error::format`.
- `FitsHeader { width, height, channels, bitpix: i32, bzero, bscale,
  blank: Option<i64>, row_order: RowOrder, data_offset, fits_keywords }`.
- `decode_rows(c, canvas_y0, n, out: &mut [f32])`: fills `n` canvas rows of
  channel `c` (top-down canvas order, applying the flip rule from the header)
  with byte-swapped, scaled, normalized, sentinel-mapped samples. This is
  the only pixel entry point; there is no zero-copy view.
- `advise_sequential()` as for XISF (bottom-up files are read in reverse
  band order; the advice is still harmless).

Also `synth::write_fits(path, w, h, ch, planes, bitpix, cards)` — a
minimal writer used by tests (stores rows bottom-up unless the caller passes
a `ROWORDER` card), plus `synth::write_fits_solved(...)` that adds linear
WCS cards and optional SIP cards from a `SynthSip` description.

### 2. `formats/mod.rs` — `InputPanel`

```rust
pub enum InputPanel { Xisf(XisfPanel), Fits(FitsPanel) }
impl InputPanel {
    pub fn open(path: &Path) -> Result<InputPanel>;   // sniff: "XISF0100" | "SIMPLE  ="
    pub fn path(&self) -> &Path;
    pub fn width/height/channels(&self) -> u64;
    pub fn fits_keywords(&self) -> &[FitsKeyword];
    pub fn properties(&self) -> &[XisfProperty];      // empty for FITS
    pub fn wcs_model(&self) -> std::result::Result<WcsModel, String>;  // Err = human diagnostic
    pub fn linear_wcs(&self) -> Option<LinearWcs>;    // top-down convention, for output cards
    pub fn storage(&self) -> PanelStorage;            // FullCanvasXisf | FullCanvasFits
    pub fn format_name(&self) -> &'static str;        // "XISF" | "FITS", for `info`
}
```

Detection is by magic bytes, never by extension, so `.fit`, `.fits`,
`.fts`, and mis-named files all work. `wcs_model` wraps
`WcsModel::from_properties` + `describe_unsolved` for XISF and
`fits_wcs::model_from_keywords` for FITS, so the solved path's per-file
error listing reads identically for both.

### 3. `panel_reader.rs` — `Backing::Fits` and the shared band cache

The per-thread band-cache machinery in `ipc/reader.rs` (`ThreadBand`,
the `UnsafeCell` cells, the `unsafe impl Sync`, the latched error) moves
into a new `panel_reader::band_cache::BandCache` with one generic entry
point:

```rust
pub(crate) fn row(&self, c: u64, y: u64,
                  fetch: impl FnOnce(u64 /*y0*/, usize /*rows*/, &mut Vec<f32>) -> Result<(), String>)
                  -> Option<(u64, &[f32])>;
```

`IpcBacking` becomes a thin wrapper whose fetch is `request_band`;
`FitsBacking` wraps a `FitsPanel` whose fetch is `decode_rows` for every
channel of the band. The concurrency invariant and its proof move with the
code and are stated once. `IpcBacking`'s existing unit tests keep passing
unchanged; new tests cover `FitsBacking` (row identity against
`decode_rows`, flip, band boundaries, concurrent reads from a rayon pool).

`PanelStorage` gains `FullCanvasFits` (serde-compatible: the default variant
stays `FullCanvasXisf`, so old session files still load). `PanelReader::open`
dispatches on it; `PanelReader::open_xisf` is replaced by
`PanelReader::open_file(path)` which opens either format. The FITS band is a
fixed 64 rows (a per-thread buffer of `64 × width × channels` f32, about
3.6 MB per thread for a 4880-wide RGB panel and 7 MB for the 9255-wide
Orion canvas); no parameter is exposed.

### 4. `align.rs`

`reproject_panel` takes `&InputPanel`: XISF keeps the zero-copy plane
gather; FITS goes through the existing `reproject_from_reader` on a
`PanelReader` over `FullCanvasFits` (which materializes the raw panel's
planes once, bounded to one panel at a time as today).

### 5. `astrometry/fits_wcs.rs` — WCS from keywords

`pub fn model_from_keywords(cards: &[FitsKeyword], width, height, row_order)
-> Result<WcsModel, String>`:

- Requires `CTYPE1 = 'RA---TAN'` or `'RA---TAN-SIP'` and the matching
  `DEC--` in `CTYPE2` (either axis order is accepted; if `CTYPE1` is the
  Dec axis, axes are swapped into RA-first form). Other projection codes →
  `Err("unsupported projection ...")`.
- `CRVAL1/2`, `CRPIX1/2` required. Linear matrix from, in priority order:
  `CD1_1..CD2_2` (missing elements = 0); else `PC1_1..PC2_2` (default
  identity) times `CDELT1/2`; else `CDELT1/2` with `CROTA2` (default 0).
  No matrix at all → `Err`.
- Any `PV1_*`/`PV2_*` card with a nonzero value, or `TPV` in CTYPE → `Err`.
- Builds the file-frame `LinearWcs`, reflects it when `row_order` is
  bottom-up, then: no `A_ORDER` → `WcsModel::linear_only`; else parses SIP
  (below) and returns `WcsModel::with_sip`, after validation.

### 6. `astrometry/sip.rs` — SIP distortion

```rust
pub struct SipSolution {
    pub linear: LinearWcs,          // top-down (already reflected) — same as model.linear
    file_crpix: [f64; 2],           // CRPIX as in the file (unreflected)
    file_cd: [[f64; 2]; 2],         // CD as in the file
    a: Poly, b: Poly,               // forward, orders A_ORDER/B_ORDER
    ap: Option<Poly>, bp: Option<Poly>,  // inverse, AP_ORDER/BP_ORDER
    row_order: RowOrder, height: u64,
}
```

`Poly` is a dense `(order+1)²` coefficient table with `p + q ≤ order`;
missing `A_p_q` cards are 0. Orders up to 9 accepted; a single-axis order
(only `A_ORDER`) implies the same for `B`.

- **Forward** (image → native, degrees): image `(x_img, y_img)` → file
  `(i, j)` by the orientation rule → `(u, v) = (i − CRPIX1, j − CRPIX2)` →
  `(ξ, η) = CD · (u + f(u, v), v + g(u, v))`.
- **Inverse** (native → image): `(U, V) = CD⁻¹ · (ξ, η)`; start at
  `(U + AP(U, V), V + BP(U, V))` when AP/BP exist, else `(U, V)`; then Newton
  on the forward map with the analytic Jacobian (polynomial partials), at
  most 8 steps, stopping when the step is below 1e-9 px. Divergence
  (non-finite, or > 8 steps without converging) yields a non-finite result,
  which the grid sampler rejects.
- **Validation** (`validate_sip`), mirroring `standard::validate_model`:
  forward at the reference pixel lands within 0.01° of `(0, 0)`; forward at
  the four image corners deviates from the linear map by < 0.05°; inverse ∘
  forward at center and corners returns within 1e-3 px. Failure →
  `Err` with the reason (never a silent linear fallback).

`Distortion` gets a `Sip { sol: Arc<SipSolution>, image_to_native:
OnceLock<Option<Grid2D>>, native_to_image: OnceLock<Option<Grid2D>> }`
variant, sampled exactly like `Standard` (image rect at 16 px; native rect
from the forward-mapped corners/edge midpoints with a one-cell margin, at
`16 px × scale`). `standard::sample` is generalized to take
`impl Fn(f64, f64) -> (f64, f64) + Sync` so both variants share it.
`is_spline()` is true for SIP (renaming it is out of scope; its doc comment
gains "or SIP").

### 7. Pipeline call sites

Every `XisfPanel::open` outside tests becomes `InputPanel::open`:

- `analyze::analyze_full`: channel-uniformity pre-pass and the Auto geometry
  pass.
- `analyze::analyze_solved`: opens `InputPanel`, uses `wcs_model()`; the
  aligned-panel `source` still records the original path.
- `analyze::scan_panel` (aligned mode): `PanelReader::open_file`, storage
  from `InputPanel::storage()`.
- `analyze::probe_panels`: `InputPanel`; `PanelDesc.properties` stays
  `x.properties().to_vec()` (empty for FITS — the Files-mode host discards
  it anyway).
- `mmm blend`: reference panel via `InputPanel`; keyword passthrough via
  `fits_keywords()`; aligned-session WCS cards via `linear_wcs()` (replacing
  `wcs_from_properties` on the raw properties).
- `mmm info`: `InputPanel`; prints the format name and, for FITS, BITPIX
  and row order; `--stats` streams rows through a `PanelReader` instead of
  whole-plane slices, for both formats.
- `main.rs::geometry_card` gains the SIP prefixes `A_`, `B_`, `AP_`, `BP_`
  (and `A_ORDER` etc. by the same prefix rule) so no input distortion cards
  reach the output header.

### 8. CLI text, docs, PixInsight

- Help strings: "Input panel files (XISF)" → "(XISF or FITS)".
- `docs/DESIGN.md`: new section "FITS input (2026-09-26)" recording the
  orientation rule, value normalization, SIP handling, and the verification
  results; the phase-2 "deferred: FITS input" line is updated.
- `integration/pixinsight`: the Add Files tooltip mentions FITS; the module
  doc (`doc/`) gets a sentence; PROTOCOL.md §11 notes that `paths` may be
  FITS. No code or protocol change; the worker rebuild ships with the next
  module release.
- `README.md`: input formats line.

## Error handling

All reader errors are `Error::format(path, msg)` naming the file and the
exact unsupported feature ("image data in extension HDU", "ZIMAGE (fpack)
compression", "BITPIX 64", "NAXIS 4"). Unsolved/unsupported WCS is reported
through the solved path's existing aggregated listing ("solved input
requires an astrometric solution in every panel: …"), one line per file,
with `fits_wcs`'s message. A FITS decode failure mid-scan surfaces through
the band cache's latched error exactly as an IPC transport failure does
(`PanelReader::ipc_error` is renamed `backing_error`; its IPC callers are
updated).

## Testing

Unit tests never touch `test_data/`.

1. **Reader** (`formats/fits.rs`): round-trip through `write_fits` for
   BITPIX 8/16/32/−32/−64 with and without BZERO/BSCALE, NaN/BLANK → 0,
   NAXIS 2 vs 3, `ROWORDER` present vs absent (flip), refusal of extension
   HDUs / `ZIMAGE` / bad NAXIS, header longer than one block, data
   truncated. `InputPanel::open` sniffing on both formats and on garbage.
2. **Band cache / backing**: `IpcBacking` tests unchanged; `FitsBacking`
   rows equal `decode_rows` for every channel and row; band-boundary rows;
   parallel readers from a rayon pool agree with sequential.
3. **WCS parsing**: CD / PC+CDELT / CDELT+CROTA2 forms give the same
   `LinearWcs`; axis-swapped CTYPE; reflection matches the inverse of
   `wcs_cards_flipped`; unsupported projections, PV, TPV → `Err`.
4. **SIP oracle**: `scripts/gen_sip_fixture.py` (astropy) writes
   `crates/mmm-core/tests/fixtures/sip_oracle.json`: a few headers (order 2
   and 3, with and without AP/BP, bottom-up and TOP-DOWN) each with ~50
   sampled `(i, j) → (ra, dec)` pairs from `all_pix2world` and the reverse
   from `all_world2pix`. Rust tests assert forward agreement < 1e-9° and
   inverse agreement < 1e-4 px, via both the direct `SipSolution` and the
   sampled grids through `WcsModel::pixel_to_sky` / `sky_to_pixel`.
   The fixture is committed; the script is for regeneration.
5. **Synthetic end-to-end**: `SynthSpec` gains an output-format switch.
   The existing solved-pipeline test runs with FITS panels carrying a
   synthetic SIP (small quadratic terms, forward + AP/BP) and must reach the
   phase-5 RMSE bound (< 4e-3 vs truth); a mixed XISF + FITS set is also
   run once.
6. **Real-data smoke** (manual, recorded in DESIGN.md):
   `scripts/nova_solve_panels.py` converts each raw Orion panel (start with
   panels 3, 4, 7, 8; extend to all 12 if quick) to a mono 16-bit bottom-up
   FITS (luminance = channel mean, no header solution), uploads it to
   nova.astrometry.net with `publicly_visible: n` and `tweak_order: 3`,
   polls to completion, downloads the `wcs.fits` header, and splices its
   WCS + SIP cards into an RGB float32 bottom-up FITS of the original planes
   under `test_data/orion_mosaic_fits/`. The API key is read from
   `~/.config/astrometry/apikey` (never logged). Then
   `mmm analyze --input solved` + `blend` on those files, compared with the
   XISF-solved run on the same panels: star centroid offsets at a handful
   of catalog stars (the checker from the mirror saga) and a visual check of
   the seams. Acceptance: no mirror, seams clean, centroid offsets within
   what nova's order-3 SIP supports (a few pixels; the XISF spline solution
   remains the more accurate one and DESIGN.md says so).

## Files touched

- New: `crates/mmm-core/src/formats/fits.rs`,
  `crates/mmm-core/src/panel_reader/band_cache.rs` (or a sibling module),
  `crates/mmm-core/src/astrometry/fits_wcs.rs`,
  `crates/mmm-core/src/astrometry/sip.rs`,
  `crates/mmm-core/tests/fixtures/sip_oracle.json`,
  `scripts/gen_sip_fixture.py`, `scripts/nova_solve_panels.py`.
- Modified: `formats/mod.rs`, `panel_reader.rs`, `ipc/reader.rs`,
  `align.rs`, `analyze.rs`, `astrometry/mod.rs`, `astrometry/standard.rs`
  (sampler signature), `session.rs` (storage variant), `synth.rs`,
  `lib.rs` (API docs), `crates/mmm/src/main.rs`, `docs/DESIGN.md`,
  `README.md`, `integration/pixinsight/PROTOCOL.md`, module tooltip/doc.

## Risks

- **Orientation on non-astrometry.net files.** The rule is fixed by the
  standard; the only known deviant is PixInsight's display-space
  interpretation, documented as unverified.
- **nova SIP accuracy.** Order-2 was measured ~9″ off at the edges of a
  distorted frame in the astrometrical project; `tweak_order: 3` is
  requested. The smoke test judges the code path, not nova's fit quality.
- **Memory for solved FITS panels.** `reproject_from_reader` materializes
  one raw panel (same footprint as the XISF mmap path, but in anonymous
  memory rather than page cache). Acceptable; noted in DESIGN.md.
