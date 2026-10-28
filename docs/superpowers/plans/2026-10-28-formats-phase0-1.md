# Formats consolidation — Phases 0–1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 0 pins today's mmm behaviour as golden tests that later
phases must pass unchanged. Phase 1 makes `mpp-core` a lean, feature-gated
home that already owns all FITS reading and writing.

**Architecture:** Phase 0 adds black-box tests in mmm only, with no
production change. Phase 1 works in the mpp repo, in three steps:
- put the TIFF/convert/publish code behind Cargo features;
- move mmm's format detection, FITS pixel reader and the format-pure half
  of the FITS writer into `mpp_core::formats`;
- share the Phase 0 wire-format golden file.

mmm does not depend on mpp until Phase 3.

**Tech Stack:** Rust 2024, `memmap2`, `serde`/`serde_json`, `thiserror`.
No new dependencies in this plan.

**Spec:** `docs/superpowers/plans/2026-10-28-formats-to-mpp-xisf-rev2.md`
(the roadmap; §3 "Architecture" and §4 Phases 0–1).

## Global Constraints

- **mmm branch** `feat/formats-to-mpp`; **mpp branch**
  `feat/formats-consolidation`. No push, no merge.
- Every commit leaves its repo green: `cargo fmt --all --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`,
  `cargo doc --no-deps`. mpp has `#![warn(missing_docs)]`, so every new
  public item gets a doc comment.
- **Tests:** no test depends on `test_data/`; inputs are synthesized.
- **Linear data:** zero is the no-data sentinel. FITS NaN, Inf, BLANK and
  negative integer samples decode to 0. Float samples keep their sign.
- **Wire format:** the serde form of `FitsKeyword`, `XisfProperty` and
  `PropertyValue` must not change. These types cross the PixInsight
  host ↔ worker IPC.
- **Commit trailer:** `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **FITS header near the size cap:** a header longer than one block (more
   than 36 cards) must parse; the existing mpp 1000-block cap stays. Covered
   by the ported `long_header_and_quoted_values_parse`.
2. **Truncated FITS data:** a file whose data is shorter than
   `NAXIS1*NAXIS2*NAXIS3*|BITPIX|/8` must be refused at open, naming the
   shortfall, rather than panicking in `decode_rows`. Task 1.3 adds
   `truncated_data_is_refused`.
3. **Non-ASCII bytes in a FITS card** (seen in the wild in OBJECT comments)
   must not panic. Task 1.3 adds `non_ascii_card_does_not_panic`.
4. **XISF keyword values with lowercase exponents** (`2.0496e+03`) must be
   written to FITS output as `2.0496E+03`, while string values containing
   `e` are left alone. Task 1.4 tests both.
5. **TIFF magic vs. extension:** a `.fits`-named file that is really XISF
   must be read as XISF (signature wins). Task 1.2 adds
   `signature_beats_extension`.

---

## Phase 0 — mmm safety nets (repo: mega-merge-mosaic)

### Task 0.1: Golden wire-format JSON

**Files:**
- Create: `crates/mmm-core/tests/wire_golden.rs`
- Create: `crates/mmm-core/tests/fixtures/wire_golden.json`

**Interfaces:**
- Produces: `wire_golden.json`, an array of tagged samples. Task 1.5 copies
  it byte-for-byte into mpp, and Phase 3 must pass it unchanged.

- [ ] **Step 1: Write the test.** `wire_golden_matches_fixture` builds a
  fixed `WireSample` (local struct in the test:
  `{ keywords: Vec<FitsKeyword>, properties: Vec<XisfProperty> }`). The
  sample covers:
  - **Keywords:** one string card (`'M42'`), one real card, one `COMMENT`
    card with an empty value.
  - **One property per `PropertyValue` variant:**
    - `Str`;
    - `F64` holding −0.5;
    - `I64` holding −7;
    - `F64Vec` with `[1.0, 2.5e-7, -3.0]`;
    - `F64Mat` with 2×2 data;
    - `Unread`.
  - **Location:** the `F64Vec` property has `location: Some((4096, 24))`;
    the rest have `None`.

  The test serializes the sample with `serde_json::to_string_pretty` and
  asserts it equals the fixture file contents (trailing newline trimmed).
  When the env var `MMM_BLESS=1` is set, it writes the fixture instead of
  comparing.
- [ ] **Step 2:** Run `MMM_BLESS=1 cargo test -p mmm-core --test wire_golden`,
  then without the variable. Expected: PASS. Inspect the fixture by eye:
  the field names are `name/value/comment`, `id/type_/value/location`, and
  externally tagged enum variants.
- [ ] **Step 3: Commit:** `test(core): golden serde form of the IPC wire value types`.

### Task 0.2: Golden decode fixtures

**Files:**
- Create: `crates/mmm-core/tests/decode_golden.rs`
- Create: `crates/mmm-core/tests/fixtures/decode_golden.json`

**Interfaces:**
- Consumes: `mmm_core::formats::InputPanel`,
  `mmm_core::panel_reader::PanelReader::open_file`,
  `PanelReader::row(c, y) -> Option<(u64, &[f32])>`, and the `synth`
  writers (`write_xisf`, `write_xisf_with_header_xml`, `write_fits`).
- Produces: a fixture keyed by case name. Each entry is
  `{ width, height, channels, keywords: [[name, value]…], property_ids: […], pixel_fnv: "<16 hex>" }`.

The test cases, written into a temp dir:
- `xisf_rgb` (7×5×3);
- `xisf_mono_with_keywords` (6×4×1, with `OBJECT` and `EXPTIME` cards via
  header XML);
- `fits_f32_bottomup` (7×5×3, BITPIX −32);
- `fits_i16_topdown` (6×4×1, BITPIX 16, `ROWORDER='TOP-DOWN'`);
- `fits_u8` (5×3×1, BITPIX 8);
- `fits_nan_blank` (4×3×1, BITPIX −32, planes containing NaN and ±Inf).

Pixels are the existing synth ramp. `pixel_fnv` is FNV-1a 64 over the
little-endian bytes of every `f32` from `PanelReader::row`, in channel
order then row order. A missing row (`None`) hashes as `width` zeros.

- [ ] **Step 1:** Write `decode_golden_matches_fixture` with an
  `MMM_BLESS=1` write mode as in Task 0.1. Put the FNV helper in the test
  file.
- [ ] **Step 2:** Bless, re-run, PASS. Spot-check that the `fits_nan_blank`
  pixels are finite: assert in-test that every sample is finite and ≥ 0.
- [ ] **Step 3: Commit:** `test(core): golden decode of synthesized XISF/FITS panels`.

### Task 0.3: Golden WCS fixtures

**Files:**
- Create: `crates/mmm-core/tests/wcs_golden.rs`
- Create: `crates/mmm-core/tests/fixtures/wcs_golden.json`

**Interfaces:**
- Consumes:
  - `InputPanel::wcs_model() -> Result<WcsModel, String>` and
    `WcsModel::pixel_to_sky(x, y) -> (f64, f64)`;
  - `synth::{write_xisf_solved, write_xisf_solved_standard, write_fits_solved, SynthWcs, SynthSip}`.
- Produces: `{ case: [[x, y, ra, dec]…] }` on a 5×5 grid over the image,
  including the corners and the centre. Phase 4 must reproduce it to 1e-9°.
  The grid sits on pixel corners and centres (0, w/4 … w).

Cases, each 64×48×1:
- `xisf_legacy`;
- `xisf_standard`;
- `fits_tan`;
- `fits_tan_sip`. Use the `SynthSip` values from the existing synth
  round-trip test (`synth.rs:1092`).

`SynthWcs` for all cases: crval [84.199, −3.240], refimg [32, 24], CD
diag(−4.4368e-4, 4.4368e-4) with off-diagonal 1e-5.

- [ ] **Step 1:** Write `wcs_golden_matches_fixture` with bless mode.
  Compare values with tolerance `1e-9`, not by string, so a JSON float
  round-trip cannot flake.
- [ ] **Step 2:** Bless, re-run, PASS.
- [ ] **Step 3: Commit:** `test(core): golden pixel→sky for legacy/standard/TAN/TAN-SIP solutions`.

---

## Phase 1 — mpp-core restructure and FITS consolidation (repo: mega-pixel-processor)

### Task 1.1: Feature-gate TIFF / convert / publish

**Files:**
- Modify: `crates/mpp-core/Cargo.toml`, `crates/mpp-core/src/lib.rs`,
  `crates/mpp-core/src/formats/mod.rs`
- Modify: `crates/mpp/Cargo.toml` and `crates/mpp-ipc-worker/Cargo.toml`,
  only if they declare `mpp-core` with `default-features = false`
  (expected: no change)
- Modify: `Cargo.toml` (workspace) to add `rust-version = "1.88"` under
  `[workspace.package]`, plus `rust-version.workspace = true` in each
  member

**Interfaces:**
- Produces:
  - Features: `default = ["tiff", "convert", "publish"]`;
    `tiff = ["dep:tiff"]`; `convert = ["tiff"]`; `publish = ["convert"]`.
  - Behind the gates:
    - `formats::{tiff, tiff_read, tiff_write}` and `pipeline` need `tiff`;
    - `convert` and `wcs::sidecar` need `convert`;
    - `vips`, `viewer` and `publish` need `publish`.
  - Ungated: `formats::{mod, xisf, fits}`, `header`, `astrometry`,
    `wcs` (minus sidecar) and `error`.
  - If a gated module is used by an ungated one, the plan's split is wrong.
    Stop and gate the use site with the same feature. Do not widen the
    always-on set.

- [ ] **Step 1:** Add the features and the `#[cfg(feature = "…")]` gates.
  `read_header`'s TIFF arm no longer needs the tiff crate (it returns an
  empty `Header`), so it stays ungated. Gate `#[cfg(test)]` helpers that
  use TIFF the same way.
- [ ] **Step 2: Verify:**
  - `cargo build -p mpp-core --no-default-features` succeeds.
  - `cargo tree -p mpp-core --no-default-features -e normal | grep -c -E '^.*(tiff|weezl|zune)'`
    prints `0`.
  - `cargo test --workspace` passes, and
    `cargo test -p mpp-core --no-default-features` passes.
- [ ] **Step 3:** Check the `rust-version` claim with
  `cargo +1.88 check -p mpp-core` if that toolchain is installed. If it
  isn't, install it with `rustup toolchain install 1.88 --profile minimal`.
  If 1.88 fails, raise the value to the lowest version that passes and
  record the reason in the commit body.
- [ ] **Step 4:** Add a CLAUDE.md line in the mpp "Commands" section:
  `cargo test -p mpp-core --no-default-features   # lean build mmm uses`.
- [ ] **Step 5: Commit:** `build(core): feature-gate tiff/convert/publish so format consumers get a lean core`.

### Task 1.2: Signature-based format detection

**Files:**
- Modify: `crates/mpp-core/src/formats/mod.rs`

**Interfaces:**
- Produces:
  - `pub enum FileFormat { Xisf, Fits, Tiff }`, derived
    `Debug, Clone, Copy, PartialEq, Eq`.
  - `pub fn sniff_format(path: &Path) -> Result<FileFormat>`. The magic
    bytes: `XISF0100`; `SIMPLE  `; TIFF `II*\0`, `MM\0*`, and BigTIFF
    `II+\0`, `MM\0+`.
  - A short file is not an I/O error. It falls through to
    `Error::format(path, "not an XISF, FITS or TIFF file (no XISF0100 signature, SIMPLE card or TIFF byte-order mark)")`.
  - `read_header` dispatches on `sniff_format`, never on the extension.

- [ ] **Step 1: Failing tests:**
  - `sniff_detects_each_signature` covers the four TIFF marks, XISF and
    FITS;
  - `sniff_rejects_short_and_junk_files` covers empty, 3-byte and junk
    files, asserting the error text contains `"not an XISF, FITS or TIFF file"`;
  - `signature_beats_extension` writes `synth_xisf` to `x.fits` and asserts
    that `read_header` returns its properties.
- [ ] **Step 2:** Run, FAIL. Implement. Port mmm's `sniff_format` doc
  comment (`mmm-core/src/formats/mod.rs:27-40`), widened to TIFF. Then
  PASS.
- [ ] **Step 3: Commit:** `feat(core): detect XISF/FITS/TIFF by signature, never extension`.

### Task 1.3: FITS pixel reader

**Files:**
- Modify: `crates/mpp-core/src/formats/fits.rs`. Port the body of
  `mmm-core/src/formats/fits.rs` and merge it with the existing header
  reader.
- Modify: `crates/mpp-core/src/formats/mod.rs`, which gets `RowOrder`,
  `card_number` and `card_string`, ported verbatim with their docs.
- Modify: `crates/mpp-core/src/testsupport.rs`, which gets
  `synth_fits(path, w, h, ch, planes: &[f32], bitpix: i32, extra: &[FitsKeyword])`,
  ported from mmm `synth::write_fits` (`synth.rs:753`) with the same
  quantization rules.

**Interfaces:**
- Produces:
  - `pub struct FitsHeader`: the same fields as mmm (`synth`
    `fits.rs:21-44`), with `fits_keywords` renamed to `keywords` for
    mpp's `Header` vocabulary.
  - `pub struct FitsImage` with:
    - `open(path) -> Result<FitsImage>`;
    - `path()`, `header() -> &FitsHeader`, `width()`, `height()`,
      `channels()`, `row_order()`;
    - `decode_rows(&self, c: u64, y0: u64, n: usize, out: &mut [f32])`;
    - `advise_sequential()`.
    `y0` is the **top-down** row index, and the flip for BOTTOM-UP happens
    inside, exactly as in mmm.
  - `fits::read_header(path) -> Result<Header>` now builds from
    `FitsHeader` (keywords + `image_size`). Its existing tests keep
    passing; it also caps at 1000 blocks.

- [ ] **Step 1:** Port mmm's 7 FITS tests unchanged in substance, switching
  `write_fits` to `synth_fits`:
  - `float32_bottom_up_round_trips_through_flip`;
  - `top_down_card_disables_the_flip`;
  - `integer_formats_normalize_to_unit_range`;
  - `nan_inf_blank_and_negative_integers_become_zero`;
  - `naxis2_is_mono_and_naxis3_of_one_is_mono`;
  - `long_header_and_quoted_values_parse`;
  - `refusals_name_the_reason`.

  Add two new tests:
  - `truncated_data_is_refused`: drop the last 100 bytes. Expected error
    contains `"data"` and `"short"`.
  - `non_ascii_card_does_not_panic`: write `0xE9` into an OBJECT comment;
    `open` succeeds.
- [ ] **Step 2:** Run, FAIL. Port the implementation. Keep mmm's
  `parse_card` boundary-checked slicing; it fixes Review Focus 3. If the
  port does not already refuse truncated data at open, add the check.
- [ ] **Step 3:** PASS. Also run `cargo test -p mpp-core --no-default-features`.
- [ ] **Step 4: Commit:** `feat(core): FITS pixel reader (BITPIX 8/16/32/-32/-64, BZERO/BSCALE/BLANK, ROWORDER), ported from mmm`.

### Task 1.4: FITS writer core

**Files:**
- Create: `crates/mpp-core/src/formats/fits_write.rs`
- Modify: `crates/mpp-core/src/formats/mod.rs` to add
  `pub mod fits_write;`, ungated.

**Interfaces:**
- Produces:
  - `pub struct FitsWriter`:
    - `create(path, keywords: Vec<FitsKeyword>) -> Result<Self>`;
    - `begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()>`;
    - `write_band(&mut self, c: u64, y0: u64, rows: &[f32]) -> Result<()>`
      writes `rows.len()/w` top-down rows of channel `c` starting at `y0`,
      at their absolute offsets;
    - `finish(&mut self) -> Result<()>`.
  - Output: BITPIX −32, NAXIS 3 (NAXIS 2 when `ch == 1`, matching mmm's
    sink as written; check `output/fits.rs:82-130`), `ROWORDER='TOP-DOWN'`,
    big-endian data, 2880-byte padding of both header and data.
  - Before writing `band`, compare mmm's `RowSink::band(y0, rows)` signature
    (`output/fits.rs:131`). It takes all channels' rows for a band. If so,
    mirror that exactly: `write_band(y0, rows)` with rows laid out the same
    way. The Phase 3 adapter must be a pass-through.
  - `pub fn keywords_for_output(src: &[FitsKeyword], crop: (u64, u64)) -> Vec<FitsKeyword>`,
    verbatim from `output/fits.rs:186`.
- Card formatting fixes (DESIGN.md follow-up):
  - **Real values:** an unquoted value that parses as `f64` and contains
    `e` is written with the exponent letter uppercased, `2.0496e+03` →
    `2.0496E+03`. Quoted strings are untouched.
  - **Commentary cards** (`COMMENT`, `HISTORY`, and a blank name) are
    written as the name padded to 8 columns, then the text from column 9.
    There is no `= ` value indicator. For an XISF `COMMENT` keyword, the
    text is the comment field if the value is empty, otherwise the value.

- [ ] **Step 1:** Port mmm's 4 `output/fits.rs` tests onto `FitsWriter`:
  - `writes_valid_fits_with_padding_and_byteswap`;
  - `passes_through_keywords`;
  - `finish_rejects_missing_rows`;
  - `keywords_for_output_drops_structural_and_shifts_crpix`.

  Add three new tests:
  - `real_exponents_are_uppercased`: `CRVAL1 = 2.0496e+03` → bytes 10..30
    of the card contain `2.0496E+03`. `OBJECT = 'eel'` is unchanged.
  - `commentary_cards_have_no_value_indicator`: a `COMMENT` card's bytes
    8..10 are not `"= "`, and the text starts at byte 8.
  - `round_trips_through_reader`: write 7×5×3 with `FitsWriter`, read it
    with `FitsImage::decode_rows`, and get identical pixels.
- [ ] **Step 2:** Run, FAIL; implement; PASS.
- [ ] **Step 3: Commit:** `feat(core): streaming FITS writer core with standard real/commentary cards`.

### Task 1.5: Shared wire golden in mpp

**Files:**
- Create: `crates/mpp-core/tests/fixtures/wire_golden.json`, copied
  byte-for-byte from mmm Task 0.1.
- Create: `crates/mpp-core/tests/wire_golden.rs`, the same test as mmm's
  but importing `mpp_core::formats::{FitsKeyword, XisfProperty, PropertyValue}`.
  It has no bless mode: mpp may only *match* mmm's fixture.

- [ ] **Step 1:** Write the test, run it. Expected: PASS. The types are
  already serde-identical (audit 2026-10-28). A failure here is a finding:
  stop and reconcile toward mmm's form.
- [ ] **Step 2: Commit:** `test(core): pin wire value types to mmm's golden serde form`.

### Task 1.6: Docs

**Files:**
- Modify: `crates/mpp-core/src/lib.rs`, whose public-API list names the
  features and the new FITS reader and writer.
- Modify: `docs/DESIGN.md` in mpp, adding a short "Formats home" note.
  mpp-core now owns FITS read and write and signature detection; mmm will
  consume `default-features = false`; this supersedes the "FITS read/write
  evaluation" open item.

- [ ] **Step 1:** Edit. `cargo doc -p mpp-core --no-deps` must produce no
  warnings with default features or with `--no-default-features`.
- [ ] **Step 2: Commit:** `docs(core): formats home, feature gates, FITS reader/writer`.

---

**Moved to the Phase 2 plan.** The roadmap's Phase 1 item 6 (`XisfFile`
multi-image and the `PixelSource` split) is deferred. That API is shaped by
the decode pipeline, so it is designed alongside it.
