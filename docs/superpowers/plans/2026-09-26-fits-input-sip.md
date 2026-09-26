# FITS Input with WCS/SIP Alignment — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Read FITS panels (any BITPIX, primary HDU) as a peer input format to XISF across the CLI and the PixInsight Files path, using the panel's FITS WCS including SIP distortion for solved-mode alignment.

**Architecture:** A memory-mapped `FitsPanel` reader decodes rows on demand into a shared per-thread band cache (extracted from the IPC backing) behind `PanelReader`; a format-agnostic `InputPanel` replaces every direct `XisfPanel::open`; `astrometry::fits_wcs` builds a `WcsModel` from keywords, with a new `Distortion::Sip` variant sampled onto the existing lookup grids. Internal convention stays top-down: standard (bottom-up) FITS is flipped on read and its WCS reflected.

**Tech Stack:** Rust (edition per workspace), `memmap2`, `bytemuck`, `rayon`, `serde_json` (fixture), Python 3 + astropy 6 + numpy (fixture generation and the real-data smoke script only; never a test dependency).

**Spec:** `docs/superpowers/specs/2026-09-26-fits-input-sip-design.md` — read it first; every task below argues from it.

## Global Constraints

- Every public item in `mmm-core` needs a doc comment (`#![warn(missing_docs)]`); `cargo fmt`, `cargo clippy --all-targets`, `cargo doc` must stay warning-free.
- Tests must not depend on `test_data/` (synthesize inputs). Real-data runs are manual smoke tests.
- Linear data end-to-end; zero = no-data sentinel (all channels zero).
- No new IPC protocol field, module UI control, or CLI flag.
- Orientation rule (spec "Conventions"): `ROWORDER = 'TOP-DOWN'` → rows verbatim, WCS verbatim; otherwise rows flipped on read (stored row `r` is canvas row `H − 1 − r`) and WCS reflected: `CRPIX2' = H + 1 − CRPIX2`, `CD1_2' = −CD1_2`, `CD2_2' = −CD2_2`.
- Pixel values: `phys = BZERO + BSCALE · raw`; integer BITPIX normalized by `2^|BITPIX| − 1`; negatives clamp to 0; NaN/Inf/BLANK → 0; BITPIX 64 refused.
- Never approximate an unsupported distortion by its linear part: refuse with a message.
- Work on branch `feat/fits-input` in the main checkout (not a worktree: the gitignored `test_data/` is needed by the final smoke task). Commit after every task with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` trailer.
- Run `source ~/.cargo/env` if `cargo` is not on PATH.

---

### Task 0: Branch

- [ ] **Step 1: Create the branch**

```bash
cd /home/dpaull/dev/mega-merge-mosaic && git checkout -b feat/fits-input && cargo test -p mmm-core --lib 2>&1 | tail -3
```
Expected: branch created; existing tests pass.

---

### Task 1: FITS reader (`formats/fits.rs`) and synth writer

**Files:**
- Create: `crates/mmm-core/src/formats/fits.rs`
- Modify: `crates/mmm-core/src/formats/mod.rs` (add `pub mod fits;`, `RowOrder`, `card` helpers)
- Modify: `crates/mmm-core/src/synth.rs` (add `write_fits`)

**Interfaces:**
- Produces:
  - `formats::RowOrder { TopDown, BottomUp }` (Copy, Eq, Debug).
  - `formats::card_number(cards: &[FitsKeyword], name: &str) -> Option<f64>` and `formats::card_string(cards, name) -> Option<String>` (unquoted, `''` unescaped, trailing spaces trimmed).
  - `formats::fits::FitsHeader { width, height, channels: u64, bitpix: i32, bzero: f64, bscale: f64, blank: Option<i64>, row_order: RowOrder, data_offset: u64, fits_keywords: Vec<FitsKeyword> }`.
  - `formats::fits::FitsPanel::open(path) -> Result<FitsPanel>`; `path()`, `header()`, `width()`, `height()`, `channels()`, `row_order()`, `decode_rows(&self, c: u64, canvas_y0: u64, n: usize, out: &mut [f32])` (infallible; `out.len() == n * width`), `advise_sequential()`.
  - `synth::write_fits(path, w, h, ch, planes: &[f32] /* top-down planar */, bitpix: i32, extra_cards: &[FitsKeyword]) -> Result<()>` — stores rows bottom-up unless `extra_cards` contains `ROWORDER = 'TOP-DOWN'`; integer BITPIX quantizes `[0,1]` to the full range with the conventional BZERO (16 → 32768, 32 → 2147483648, 8 → 0).

- [ ] **Step 1: Add `RowOrder` and card helpers to `formats/mod.rs`**

Append to `crates/mmm-core/src/formats/mod.rs` (after `pub mod xisf;` add `pub mod fits;`):

```rust
/// Stored row order of a FITS image, from its `ROWORDER` card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOrder {
    /// `ROWORDER = 'TOP-DOWN'`: the first stored row is the top image row
    /// (mmm's internal convention); rows and WCS cards are used verbatim.
    TopDown,
    /// Standard FITS (`ROWORDER` absent or `'BOTTOM-UP'`): the first stored
    /// row is the bottom image row; rows are flipped on read and the WCS is
    /// reflected into the top-down frame.
    BottomUp,
}

/// Numeric value of the card `name` (case-insensitive), accepting Fortran
/// `D` exponents; `None` when absent or not a number.
pub fn card_number(cards: &[FitsKeyword], name: &str) -> Option<f64> {
    let v = cards.iter().find(|k| k.name.eq_ignore_ascii_case(name))?;
    v.value.trim().replace(['D', 'd'], "E").parse().ok()
}

/// String value of the card `name`: quotes stripped, `''` unescaped,
/// trailing spaces trimmed; `None` when absent or not a quoted string.
pub fn card_string(cards: &[FitsKeyword], name: &str) -> Option<String> {
    let v = cards.iter().find(|k| k.name.eq_ignore_ascii_case(name))?;
    let t = v.value.trim();
    let inner = t.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace("''", "'").trim_end().to_string())
}
```

- [ ] **Step 2: Write the failing reader tests**

Create `crates/mmm-core/src/formats/fits.rs` with the module skeleton and tests only (implementation in Step 4):

```rust
//! Minimal FITS reader for mosaic panels: primary HDU, NAXIS 2 or 3
//! (planar channels on the last axis), any BITPIX except 64, big-endian
//! samples decoded on demand into f32 rows (see the design spec for the
//! orientation and value rules).

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use super::{FitsKeyword, RowOrder, card_number, card_string};
use crate::{Error, Result};

/// FITS block size: headers and data are padded to multiples of this.
const BLOCK: usize = 2880;
/// Length of one header card.
const CARD: usize = 80;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::write_fits;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-fits-in-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn kw(name: &str, value: &str) -> FitsKeyword {
        FitsKeyword { name: name.into(), value: value.into(), comment: String::new() }
    }

    fn planes(w: u64, h: u64, ch: u64) -> Vec<f32> {
        (0..w * h * ch).map(|i| (i as f32 + 1.0) / (w * h * ch) as f32).collect()
    }

    fn read_all(p: &FitsPanel) -> Vec<f32> {
        let (w, h, ch) = (p.width() as usize, p.height() as usize, p.channels());
        let mut out = vec![0f32; w * h * ch as usize];
        for c in 0..ch {
            p.decode_rows(c, 0, h, &mut out[c as usize * w * h..(c as usize + 1) * w * h]);
        }
        out
    }

    #[test]
    fn float32_bottom_up_round_trips_through_flip() {
        let dir = tmpdir("f32");
        let (w, h, ch) = (5u64, 4u64, 3u64);
        let src = planes(w, h, ch);
        let path = dir.join("p.fits");
        write_fits(&path, w, h, ch, &src, -32, &[kw("OBJECT", "'M42'")]).unwrap();
        let p = FitsPanel::open(&path).unwrap();
        assert_eq!((p.width(), p.height(), p.channels()), (w, h, ch));
        assert_eq!(p.row_order(), RowOrder::BottomUp);
        assert_eq!(p.header().bitpix, -32);
        assert_eq!(read_all(&p), src, "top-down planes come back identical");
        assert_eq!(card_string(&p.header().fits_keywords, "OBJECT").as_deref(), Some("M42"));
        // Partial band in the middle of the image, channel 2.
        let mut band = vec![0f32; 2 * w as usize];
        p.decode_rows(2, 1, 2, &mut band);
        let off = 2 * (w * h) as usize + w as usize;
        assert_eq!(&band[..], &src[off..off + 2 * w as usize]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn top_down_card_disables_the_flip() {
        let dir = tmpdir("td");
        let (w, h, ch) = (3u64, 3u64, 1u64);
        let src = planes(w, h, ch);
        let path = dir.join("p.fits");
        write_fits(&path, w, h, ch, &src, -32, &[kw("ROWORDER", "'TOP-DOWN'")]).unwrap();
        let p = FitsPanel::open(&path).unwrap();
        assert_eq!(p.row_order(), RowOrder::TopDown);
        assert_eq!(read_all(&p), src);
        // The file itself stores row 0 first: reading the raw block confirms
        // no flip happened on write either.
        let bytes = std::fs::read(&path).unwrap();
        let first = f32::from_be_bytes(bytes[p.header().data_offset as usize..][..4].try_into().unwrap());
        assert_eq!(first, src[0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn integer_formats_normalize_to_unit_range() {
        let dir = tmpdir("int");
        let (w, h, ch) = (4u64, 2u64, 1u64);
        let src: Vec<f32> = vec![0.0, 0.25, 0.5, 1.0, 0.1, 0.9, 0.0, 0.75];
        for (bitpix, tol) in [(8i32, 1.0 / 255.0), (16, 1.0 / 65535.0), (32, 1e-6)] {
            let path = dir.join(format!("p{bitpix}.fits"));
            write_fits(&path, w, h, ch, &src, bitpix, &[]).unwrap();
            let p = FitsPanel::open(&path).unwrap();
            assert_eq!(p.header().bitpix, bitpix);
            let got = read_all(&p);
            for (a, b) in got.iter().zip(&src) {
                assert!((a - b).abs() <= tol, "bitpix {bitpix}: {a} vs {b}");
            }
            assert_eq!(got[0], 0.0, "zero stays the exact sentinel");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nan_inf_negative_and_blank_become_zero() {
        let dir = tmpdir("nan");
        let (w, h) = (4u64, 1u64);
        let path = dir.join("f.fits");
        write_fits(&path, w, h, 1, &[f32::NAN, f32::INFINITY, -0.5, 0.5], -32, &[]).unwrap();
        let p = FitsPanel::open(&path).unwrap();
        assert_eq!(read_all(&p), vec![0.0, 0.0, 0.0, 0.5]);
        // BLANK on integer data: write raw 16-bit with BZERO 32768; raw value
        // -32768 (phys 0) is also declared BLANK — both map to 0 anyway, so
        // declare BLANK = 0 raw (phys 32768 → 0.5) and check it vanishes.
        let path = dir.join("i.fits");
        write_fits(&path, w, h, 1, &[0.5, 0.25, 0.5, 1.0], 16, &[kw("BLANK", "0")]).unwrap();
        let p = FitsPanel::open(&path).unwrap();
        let got = read_all(&p);
        assert_eq!(got[0], 0.0);
        assert_eq!(got[2], 0.0);
        assert!((got[1] - 0.25).abs() < 1e-4 && (got[3] - 1.0).abs() < 1e-4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn naxis2_is_mono_and_naxis3_of_one_is_mono() {
        let dir = tmpdir("mono");
        let src = planes(3, 2, 1);
        let p2 = dir.join("n2.fits");
        write_fits(&p2, 3, 2, 1, &src, -32, &[]).unwrap();
        let bytes = std::fs::read(&p2).unwrap();
        let hdr = String::from_utf8_lossy(&bytes[..BLOCK]).to_string();
        assert!(hdr.contains("NAXIS   =                    2"), "mono is written as NAXIS 2");
        assert_eq!(FitsPanel::open(&p2).unwrap().channels(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn long_header_and_quoted_values_parse() {
        let dir = tmpdir("hdr");
        let cards: Vec<FitsKeyword> = (0..60)
            .map(|i| kw(&format!("K{i:03}"), &format!("{i}")))
            .chain([kw("TELESCOP", "'Rob''s RASA'"), kw("COMMENT", "")])
            .collect();
        let path = dir.join("p.fits");
        write_fits(&path, 2, 2, 1, &planes(2, 2, 1), -32, &cards).unwrap();
        let p = FitsPanel::open(&path).unwrap();
        assert!(p.header().data_offset >= 2 * BLOCK as u64, "header spans two blocks");
        assert_eq!(card_number(&p.header().fits_keywords, "K059"), Some(59.0));
        assert_eq!(card_string(&p.header().fits_keywords, "TELESCOP").as_deref(), Some("Rob's RASA"));
        assert_eq!(read_all(&p), planes(2, 2, 1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refusals_name_the_reason() {
        let dir = tmpdir("bad");
        let write = |name: &str, cards: &[&str], data: usize| {
            let mut h = String::new();
            for c in cards {
                h.push_str(&format!("{c:<80}"));
            }
            h.push_str(&format!("{:<80}", "END"));
            while h.len() % BLOCK != 0 {
                h.push(' ');
            }
            let mut bytes = h.into_bytes();
            bytes.extend(std::iter::repeat_n(0u8, data));
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let e = FitsPanel::open(&write("ext.fits", &["SIMPLE  =                    T", "BITPIX  =                  -32", "NAXIS   =                    0", "EXTEND  =                    T"], 0)).unwrap_err().to_string();
        assert!(e.contains("extension"), "{e}");
        let e = FitsPanel::open(&write("b64.fits", &["SIMPLE  =                    T", "BITPIX  =                   64", "NAXIS   =                    2", "NAXIS1  =                    2", "NAXIS2  =                    2"], 32)).unwrap_err().to_string();
        assert!(e.contains("BITPIX 64"), "{e}");
        let e = FitsPanel::open(&write("n4.fits", &["SIMPLE  =                    T", "BITPIX  =                  -32", "NAXIS   =                    4", "NAXIS1  =                    2", "NAXIS2  =                    2", "NAXIS3  =                    1", "NAXIS4  =                    1"], 16)).unwrap_err().to_string();
        assert!(e.contains("NAXIS 4"), "{e}");
        let e = FitsPanel::open(&write("short.fits", &["SIMPLE  =                    T", "BITPIX  =                  -32", "NAXIS   =                    2", "NAXIS1  =                   10", "NAXIS2  =                   10"], 8)).unwrap_err().to_string();
        assert!(e.contains("past end of file"), "{e}");
        let e = FitsPanel::open(&write("notfits.fits", &["HELLO   =                    1"], 0)).unwrap_err().to_string();
        assert!(e.contains("SIMPLE"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
```

- [ ] **Step 3: Run the tests to see them fail to compile**

Run: `cargo test -p mmm-core --lib formats::fits 2>&1 | tail -5`
Expected: compile errors (`FitsPanel`, `write_fits` not found).

- [ ] **Step 4: Implement `FitsPanel`**

Insert above the `#[cfg(test)]` block in `fits.rs`:

```rust
/// Parsed primary-HDU metadata of a FITS file.
#[derive(Debug, Clone)]
pub struct FitsHeader {
    /// Image width (`NAXIS1`).
    pub width: u64,
    /// Image height (`NAXIS2`).
    pub height: u64,
    /// Channel count (`NAXIS3`, or 1 for a 2-D image).
    pub channels: u64,
    /// Sample type code (8, 16, 32, −32, −64).
    pub bitpix: i32,
    /// `BZERO` (default 0).
    pub bzero: f64,
    /// `BSCALE` (default 1).
    pub bscale: f64,
    /// `BLANK` raw value marking undefined integer samples, if declared.
    pub blank: Option<i64>,
    /// Stored row order (see [`RowOrder`]).
    pub row_order: RowOrder,
    /// Byte offset of the data block.
    pub data_offset: u64,
    /// Every header card except `END`, in header order.
    pub fits_keywords: Vec<FitsKeyword>,
}

/// A memory-mapped, read-only FITS panel decoding rows on demand.
pub struct FitsPanel {
    path: PathBuf,
    mmap: Mmap,
    header: FitsHeader,
}

/// Split one 80-byte card into a keyword; `None` for blank cards.
fn parse_card(card: &[u8]) -> Option<FitsKeyword> {
    let text = String::from_utf8_lossy(card);
    let name = text[..8].trim_end().to_string();
    if name.is_empty() {
        return None;
    }
    let has_value = text.len() >= 10 && &text[8..10] == "= ";
    if !has_value || name == "COMMENT" || name == "HISTORY" {
        return Some(FitsKeyword { name, value: String::new(), comment: text[8..].trim().to_string() });
    }
    let body = &text[10..];
    let s = body.trim_start();
    let (value, comment) = if s.starts_with('\'') {
        let b = s.as_bytes();
        let mut i = 1;
        while i < b.len() {
            if b[i] == b'\'' {
                if i + 1 < b.len() && b[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                break;
            }
            i += 1;
        }
        let end = (i + 1).min(s.len());
        let comment = s[end..].split_once('/').map(|(_, c)| c.trim().to_string()).unwrap_or_default();
        (s[..end].trim_end().to_string(), comment)
    } else {
        match body.split_once('/') {
            Some((v, c)) => (v.trim().to_string(), c.trim().to_string()),
            None => (body.trim().to_string(), String::new()),
        }
    };
    Some(FitsKeyword { name, value, comment })
}

/// Parse the primary header; `data_offset` is the first block after `END`.
fn parse_header(path: &Path, bytes: &[u8]) -> Result<FitsHeader> {
    let mut cards = Vec::new();
    let mut end_at = None;
    for (i, card) in bytes.chunks_exact(CARD).enumerate() {
        if &card[..8] == b"END     " && card[8..].iter().all(|&b| b == b' ') {
            end_at = Some(i);
            break;
        }
        if let Some(k) = parse_card(card) {
            cards.push(k);
        }
    }
    let end_at = end_at.ok_or_else(|| Error::format(path, "no END card in the primary header"))?;
    let data_offset = ((end_at + 1) * CARD).div_ceil(BLOCK) * BLOCK;
    if cards.first().map(|k| k.name.as_str()) != Some("SIMPLE") || card_string_logical(&cards, "SIMPLE") != Some(true) {
        return Err(Error::format(path, "not a FITS file (SIMPLE = T missing)"));
    }
    let int = |name: &str| card_number(&cards, name).and_then(|v| (v.fract() == 0.0).then_some(v as i64));
    let naxis = int("NAXIS").ok_or_else(|| Error::format(path, "NAXIS missing"))?;
    let (width, height, channels) = match naxis {
        0 => return Err(Error::format(path, "no image in the primary HDU (image data in an extension HDU, e.g. fpack-compressed .fz, is unsupported)")),
        2 | 3 => {
            let w = int("NAXIS1").filter(|&v| v > 0).ok_or_else(|| Error::format(path, "NAXIS1 missing or zero"))?;
            let h = int("NAXIS2").filter(|&v| v > 0).ok_or_else(|| Error::format(path, "NAXIS2 missing or zero"))?;
            let c = if naxis == 3 { int("NAXIS3").filter(|&v| v > 0).ok_or_else(|| Error::format(path, "NAXIS3 missing or zero"))? } else { 1 };
            (w as u64, h as u64, c as u64)
        }
        n => return Err(Error::format(path, format!("NAXIS {n} is unsupported (need a 2-D image or a planar 3-D cube)"))),
    };
    let bitpix = int("BITPIX").ok_or_else(|| Error::format(path, "BITPIX missing"))? as i32;
    if !matches!(bitpix, 8 | 16 | 32 | -32 | -64) {
        return Err(Error::format(path, format!("BITPIX {bitpix} is unsupported")));
    }
    let row_order = match card_string(&cards, "ROWORDER").as_deref() {
        Some("TOP-DOWN") => RowOrder::TopDown,
        _ => RowOrder::BottomUp,
    };
    Ok(FitsHeader {
        width,
        height,
        channels,
        bitpix,
        bzero: card_number(&cards, "BZERO").unwrap_or(0.0),
        bscale: card_number(&cards, "BSCALE").unwrap_or(1.0),
        blank: int("BLANK"),
        row_order,
        data_offset: data_offset as u64,
        fits_keywords: cards,
    })
}

/// Logical card value (`T`/`F`).
fn card_string_logical(cards: &[FitsKeyword], name: &str) -> Option<bool> {
    match cards.iter().find(|k| k.name == name)?.value.trim() {
        "T" => Some(true),
        "F" => Some(false),
        _ => None,
    }
}

impl FitsPanel {
    /// Open and validate a FITS file's primary image HDU (module docs list
    /// the supported subset; anything else errors naming the feature).
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        // SAFETY: read-only map; truncation by another process during a run
        // is undefined, as is conventional for mmap readers.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| Error::io(path, e))?;
        let header = parse_header(path, &mmap)?;
        let bps = (header.bitpix.unsigned_abs() / 8) as u64;
        let need = header.data_offset + header.width * header.height * header.channels * bps;
        if need > mmap.len() as u64 {
            return Err(Error::format(path, format!("data block extends past end of file ({} bytes needed, {} present)", need, mmap.len())));
        }
        Ok(Self { path: path.to_path_buf(), mmap, header })
    }

    /// The file this panel was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Parsed header.
    pub fn header(&self) -> &FitsHeader {
        &self.header
    }
    /// Image width in pixels.
    pub fn width(&self) -> u64 {
        self.header.width
    }
    /// Image height in pixels.
    pub fn height(&self) -> u64 {
        self.header.height
    }
    /// Channel count.
    pub fn channels(&self) -> u64 {
        self.header.channels
    }
    /// Stored row order.
    pub fn row_order(&self) -> RowOrder {
        self.header.row_order
    }

    /// Decode `n` canvas rows of channel `c` starting at top-down canvas row
    /// `canvas_y0` into `out` (`n * width` values, rows contiguous): the
    /// flip rule, byte order, BZERO/BSCALE, integer normalization, and the
    /// zero sentinel for NaN/Inf/negative/BLANK samples are all applied.
    pub fn decode_rows(&self, c: u64, canvas_y0: u64, n: usize, out: &mut [f32]) {
        let h = &self.header;
        let w = h.width as usize;
        assert!(c < h.channels, "channel {c} out of range");
        assert!(canvas_y0 + n as u64 <= h.height, "rows out of range");
        assert_eq!(out.len(), n * w, "output buffer size");
        let bps = (h.bitpix.unsigned_abs() / 8) as usize;
        let norm = match h.bitpix {
            8 => 255.0,
            16 => 65535.0,
            32 => 4294967295.0,
            _ => 1.0,
        };
        let plain_float = h.bitpix < 0 && h.bzero == 0.0 && h.bscale == 1.0;
        for k in 0..n {
            let y = canvas_y0 + k as u64;
            let r_file = match h.row_order {
                RowOrder::TopDown => y,
                RowOrder::BottomUp => h.height - 1 - y,
            };
            let off = h.data_offset as usize + ((c * h.height + r_file) as usize * w) * bps;
            let src = &self.mmap[off..off + w * bps];
            let dst = &mut out[k * w..(k + 1) * w];
            for (i, d) in dst.iter_mut().enumerate() {
                let b = &src[i * bps..(i + 1) * bps];
                let (raw, raw_int): (f64, Option<i64>) = match h.bitpix {
                    8 => (b[0] as f64, Some(b[0] as i64)),
                    16 => { let v = i16::from_be_bytes([b[0], b[1]]); (v as f64, Some(v as i64)) }
                    32 => { let v = i32::from_be_bytes([b[0], b[1], b[2], b[3]]); (v as f64, Some(v as i64)) }
                    -32 => (f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64, None),
                    _ => (f64::from_be_bytes(b.try_into().expect("8 bytes")), None),
                };
                let v = if plain_float {
                    raw
                } else if raw_int.is_some() && raw_int == h.blank {
                    0.0
                } else {
                    (h.bzero + h.bscale * raw) / norm
                };
                *d = if v.is_finite() && v > 0.0 { v as f32 } else { 0.0 };
            }
        }
    }

    /// Advise the OS that access will be sequential (a no-op off unix).
    pub fn advise_sequential(&self) {
        #[cfg(unix)]
        let _ = self.mmap.advise(memmap2::Advice::Sequential);
    }
}
```

- [ ] **Step 5: Implement `synth::write_fits`**

Add to `crates/mmm-core/src/synth.rs` (near `write_xisf`; add `use crate::formats::FitsKeyword;` if not imported):

```rust
/// Minimal FITS writer for tests: primary HDU, `NAXIS` 2 (one channel) or 3
/// (planar), big-endian samples, rows stored bottom-up unless `extra_cards`
/// carries `ROWORDER = 'TOP-DOWN'`. `planes` is top-down planar canvas data
/// in `[0, 1]`; integer `bitpix` (8/16/32) quantizes to the full unsigned
/// range with the conventional `BZERO` (0 / 32768 / 2147483648); −32/−64
/// store the floats. Round-trips through [`crate::formats::fits::FitsPanel`].
pub fn write_fits(
    path: &Path,
    w: u64,
    h: u64,
    ch: u64,
    planes: &[f32],
    bitpix: i32,
    extra_cards: &[FitsKeyword],
) -> Result<()> {
    let n = (w * h * ch) as usize;
    if planes.len() != n {
        return Err(Error::format(path, format!("planes length {} does not match geometry {w}x{h}x{ch} ({n})", planes.len())));
    }
    let top_down = extra_cards.iter().any(|k| k.name == "ROWORDER" && k.value.contains("TOP-DOWN"));
    let card = |name: &str, value: &str| -> String {
        let s = if value.starts_with('\'') { format!("{name:<8}= {value}") } else { format!("{name:<8}= {value:>20}") };
        format!("{s:<80}")
    };
    let mut hdr = String::new();
    hdr.push_str(&card("SIMPLE", "T"));
    hdr.push_str(&card("BITPIX", &bitpix.to_string()));
    hdr.push_str(&card("NAXIS", if ch == 1 { "2" } else { "3" }));
    hdr.push_str(&card("NAXIS1", &w.to_string()));
    hdr.push_str(&card("NAXIS2", &h.to_string()));
    if ch != 1 {
        hdr.push_str(&card("NAXIS3", &ch.to_string()));
    }
    let bzero: f64 = match bitpix { 16 => 32768.0, 32 => 2147483648.0, _ => 0.0 };
    if bitpix > 0 {
        hdr.push_str(&card("BZERO", &format!("{bzero}")));
        hdr.push_str(&card("BSCALE", "1"));
    }
    for k in extra_cards {
        if k.name == "COMMENT" || k.name == "HISTORY" {
            hdr.push_str(&format!("{:<80}", format!("{} {}", k.name, k.comment)));
        } else {
            hdr.push_str(&card(&k.name, &k.value));
        }
    }
    hdr.push_str(&format!("{:<80}", "END"));
    while hdr.len() % 2880 != 0 {
        hdr.push(' ');
    }
    let mut bytes = hdr.into_bytes();
    let (wu, hu) = (w as usize, h as usize);
    let norm: f64 = match bitpix { 8 => 255.0, 16 => 65535.0, 32 => 4294967295.0, _ => 1.0 };
    for c in 0..ch as usize {
        for r_file in 0..hu {
            let r_img = if top_down { r_file } else { hu - 1 - r_file };
            let row = &planes[(c * hu + r_img) * wu..(c * hu + r_img + 1) * wu];
            for &v in row {
                match bitpix {
                    8 => bytes.push((v as f64 * norm).round().clamp(0.0, 255.0) as u8),
                    16 => bytes.extend((((v as f64 * norm).round().clamp(0.0, 65535.0) - bzero) as i16).to_be_bytes()),
                    32 => bytes.extend((((v as f64 * norm).round().clamp(0.0, 4294967295.0) - bzero) as i32).to_be_bytes()),
                    -32 => bytes.extend(v.to_be_bytes()),
                    -64 => bytes.extend((v as f64).to_be_bytes()),
                    other => return Err(Error::format(path, format!("write_fits: unsupported BITPIX {other}"))),
                }
            }
        }
    }
    while bytes.len() % 2880 != 0 {
        bytes.push(0);
    }
    std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p mmm-core --lib formats::fits 2>&1 | tail -15`
Expected: all 7 tests PASS. If `integer_formats_normalize_to_unit_range` fails for BITPIX 32 on the `0.25`-type values, check the `i32` cast of `(v·norm − bzero)`: it must be computed in f64 and only then cast.

- [ ] **Step 7: Lint and commit**

```bash
cargo fmt && cargo clippy -p mmm-core --all-targets 2>&1 | grep -E "^(warning|error)" | head; cargo doc -p mmm-core --no-deps 2>&1 | grep -E "^warning" | head
git add -A crates/mmm-core/src/formats crates/mmm-core/src/synth.rs
git commit -m "feat(formats): FITS primary-HDU reader with on-demand row decoding

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Shared band cache, `FitsBacking`, `PanelStorage::FullCanvasFits`

**Files:**
- Create: `crates/mmm-core/src/band_cache.rs`
- Modify: `crates/mmm-core/src/lib.rs` (add `pub(crate) mod band_cache;`)
- Modify: `crates/mmm-core/src/ipc/reader.rs` (thin wrapper over `BandCache`)
- Modify: `crates/mmm-core/src/panel_reader.rs` (`Backing::Fits`, `open_file`, `backing_error`)
- Modify: callers of `PanelReader::ipc_error` / `open_xisf` (find with `grep -rn "ipc_error\|open_xisf" crates`)

**Interfaces:**
- Consumes: `formats::fits::FitsPanel` (Task 1).
- Produces:
  - `pub(crate) struct band_cache::BandCache`; `BandCache::new(canvas: (u64,u64,u64), band_rows: usize)`; `BandCache::row(&self, c, canvas_y, fetch: impl FnOnce(u64 /*y0*/, u64 /*y1*/, &mut [f32]) -> Result<()>) -> Option<(u64, &[f32])>`; `BandCache::error(&self) -> Option<Error>`.
  - `PanelStorage::FullCanvasFits` (serde variant name `FullCanvasFits`).
  - `PanelReader::open_file(path: &Path) -> Result<PanelReader>` (sniffs XISF vs FITS by magic; replaces `open_xisf`).
  - `PanelReader::backing_error(&self) -> Option<Error>` (replaces `ipc_error`).
  - `pub const FITS_BAND_ROWS: usize = 64;` in `panel_reader.rs`.

- [ ] **Step 1: Write the failing `FitsBacking` tests in `panel_reader.rs`**

Append to the `tests` module of `crates/mmm-core/src/panel_reader.rs`:

```rust
    #[test]
    fn fits_backing_rows_match_decode_rows_and_flip() {
        use crate::formats::fits::FitsPanel;
        use crate::synth::write_fits;
        let dir = tmpdir("fits");
        let (w, h, ch) = (7u64, 150u64, 2u64); // > 2 bands of 64 rows, last band short
        let planes: Vec<f32> = (0..w * h * ch).map(|i| (i % 251) as f32 / 251.0 + 0.001).collect();
        let path = dir.join("p.fits");
        write_fits(&path, w, h, ch, &planes, 16, &[]).unwrap();
        let src = FitsPanel::open(&path).unwrap();
        let m = meta(path.clone(), PanelStorage::FullCanvasFits);
        let r = PanelReader::open(&m, (w, h, ch)).unwrap();
        assert_eq!(r.bbox(), [0, 0, w, h]);
        r.advise_sequential();
        let mut expect = vec![0f32; w as usize];
        for y in [0u64, 1, 63, 64, 65, 127, 128, 149] {
            for c in 0..ch {
                src.decode_rows(c, y, 1, &mut expect);
                let (x0, row) = r.row(c, y).expect("in range");
                assert_eq!(x0, 0);
                assert_eq!(row, &expect[..], "c{c} y{y}");
            }
        }
        assert!(r.row(0, h).is_none());
        assert!(r.backing_error().is_none());
        // Geometry mismatch and open_file sniffing.
        assert!(PanelReader::open(&m, (w + 1, h, ch)).is_err());
        let r2 = PanelReader::open_file(&path).unwrap();
        assert_eq!(r2.canvas(), (w, h, ch));
        assert_eq!(r2.row(1, 70).unwrap().1, r.row(1, 70).unwrap().1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fits_backing_is_consistent_under_rayon() {
        use rayon::prelude::*;
        use crate::synth::write_fits;
        let dir = tmpdir("fitspar");
        let (w, h, ch) = (33u64, 300u64, 3u64);
        let planes: Vec<f32> = (0..w * h * ch).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 + 0.001).collect();
        let path = dir.join("p.fits");
        write_fits(&path, w, h, ch, &planes, -32, &[]).unwrap();
        let r = PanelReader::open_file(&path).unwrap();
        let sums: Vec<f64> = (0..h)
            .into_par_iter()
            .map(|y| (0..ch).map(|c| r.row(c, y).unwrap().1.iter().map(|&v| v as f64).sum::<f64>()).sum())
            .collect();
        let (wu, hu) = (w as usize, h as usize);
        for (y, s) in sums.iter().enumerate() {
            let want: f64 = (0..ch as usize)
                .flat_map(|c| planes[(c * hu + y) * wu..(c * hu + y + 1) * wu].iter())
                .map(|&v| v as f64)
                .sum();
            assert!((s - want).abs() < 1e-3, "row {y}: {s} vs {want}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p mmm-core --lib panel_reader 2>&1 | grep -E "error\[" | head -5`
Expected: `FullCanvasFits`, `open_file`, `backing_error` not found.

- [ ] **Step 3: Create `band_cache.rs` by moving the cell machinery out of `ipc/reader.rs`**

Create `crates/mmm-core/src/band_cache.rs`. Move `ThreadBand`, the `cells`/`error` fields, the `unsafe impl Sync` with its full justification comment (copy the comment verbatim from `ipc/reader.rs`, replacing "IpcBacking" with "BandCache"), and the body of `row` — with the fetch step replaced by the closure:

```rust
//! Per-thread band cache shared by every [`crate::panel_reader::PanelReader`]
//! backing that cannot hand out zero-copy rows (IPC-served and FITS
//! panels). Rows are produced a *band* (`band_rows` canvas rows, all
//! channels, planar) at a time into a buffer owned by the calling thread,
//! because the producers are expensive per call (a blocking IPC round trip;
//! a byte-swapping decode) and must not serialize concurrent callers.
//!
//! (Concurrent-use invariant and SAFETY argument: moved verbatim from the
//! former `ipc::reader::IpcBacking`.)

use std::cell::UnsafeCell;
use std::sync::Mutex;

use crate::{Error, Result};

struct ThreadBand {
    y0: u64,
    valid: bool,
    buf: Vec<f32>,
}

pub(crate) struct BandCache {
    canvas: (u64, u64, u64),
    band_rows: usize,
    cells: Vec<UnsafeCell<ThreadBand>>,
    error: Mutex<Option<String>>,
}

// SAFETY: (verbatim justification from ipc/reader.rs)
unsafe impl Sync for BandCache {}

impl BandCache {
    pub(crate) fn new(canvas: (u64, u64, u64), band_rows: usize) -> BandCache {
        let (w, _h, ch) = canvas;
        let cap = ch as usize * band_rows * w as usize;
        let num_threads = rayon::current_num_threads();
        let cells = (0..=num_threads)
            .map(|_| UnsafeCell::new(ThreadBand { y0: 0, valid: false, buf: vec![0f32; cap] }))
            .collect();
        BandCache { canvas, band_rows, cells, error: Mutex::new(None) }
    }

    /// One channel row `(0, slice)` of canvas row `canvas_y`, producing the
    /// containing band through `fetch(y0, y1, buf)` on a miss (`buf` is the
    /// planar `channels × (y1−y0) × width` region to fill). `None` when out
    /// of range or when `fetch` failed (the error is latched, see
    /// [`Self::error`]).
    pub(crate) fn row<F>(&self, c: u64, canvas_y: u64, fetch: F) -> Option<(u64, &[f32])>
    where
        F: FnOnce(u64, u64, &mut [f32]) -> Result<()>,
    {
        let (w, h, ch) = self.canvas;
        if canvas_y >= h || c >= ch {
            return None;
        }
        let num_threads = self.cells.len() - 1;
        let idx = rayon::current_thread_index().unwrap_or(num_threads);
        debug_assert!(idx < self.cells.len(), "BandCache::row called from a larger rayon pool than the one it was constructed in");
        let band_rows = self.band_rows as u64;
        let by0 = (canvas_y / band_rows) * band_rows;
        // SAFETY: see the `unsafe impl Sync` justification.
        let need_fetch = {
            let band = unsafe { &*self.cells[idx].get() };
            !band.valid || band.y0 != by0
        };
        if need_fetch {
            // SAFETY: see the `unsafe impl Sync` justification.
            let band = unsafe { &mut *self.cells[idx].get() };
            let by1 = (by0 + band_rows).min(h);
            let want = ch as usize * (by1 - by0) as usize * w as usize;
            if let Err(e) = fetch(by0, by1, &mut band.buf[..want]) {
                self.latch_error(e);
                band.valid = false;
                return None;
            }
            band.y0 = by0;
            band.valid = true;
        }
        // SAFETY: see the `unsafe impl Sync` justification.
        let band = unsafe { &*self.cells[idx].get() };
        let by1 = (band.y0 + band_rows).min(h);
        let bh = (by1 - band.y0) as usize;
        let w_usize = w as usize;
        let ly = (canvas_y - band.y0) as usize;
        let row_start = c as usize * bh * w_usize + ly * w_usize;
        Some((0, &band.buf[row_start..row_start + w_usize]))
    }

    fn latch_error(&self, e: Error) {
        let mut guard = self.error.lock().unwrap();
        if guard.is_none() {
            *guard = Some(e.to_string());
        }
    }

    /// The first producer error latched by any thread, if any.
    pub(crate) fn error(&self) -> Option<Error> {
        self.error.lock().unwrap().clone().map(Error::compute)
    }
}
```

Add `pub(crate) mod band_cache;` to `lib.rs` (alphabetically after `pub mod astrometry;`).

- [ ] **Step 4: Rewrite `IpcBacking` as a wrapper**

In `crates/mmm-core/src/ipc/reader.rs` keep the type-level docs (pointing at `band_cache` for the invariant), and replace the struct and impl:

```rust
pub struct IpcBacking {
    link: Arc<HostLink>,
    panel_id: u32,
    cache: BandCache,
}

impl IpcBacking {
    pub fn new(link: Arc<HostLink>, panel_id: u32, canvas: (u64, u64, u64), band_rows: usize) -> IpcBacking {
        IpcBacking { link, panel_id, cache: BandCache::new(canvas, band_rows) }
    }

    pub fn row(&self, c: u64, canvas_y: u64) -> Option<(u64, &[f32])> {
        self.cache.row(c, canvas_y, |y0, y1, buf| self.link.request_band(self.panel_id, y0, y1, buf))
    }

    pub fn ipc_error(&self) -> Option<Error> {
        self.cache.error()
    }
}
```
(Keep the existing doc comments on `new`, `row`, `ipc_error`; delete `ThreadBand`, `latch_error`, the `UnsafeCell`/`Mutex` imports, and the `unsafe impl Sync`.) The existing tests in that file must pass unchanged.

- [ ] **Step 5: Add `FullCanvasFits`, `Backing::Fits`, `open_file`, `backing_error`**

In `panel_reader.rs`:

```rust
pub const FITS_BAND_ROWS: usize = 64;  // doc: rows per decoded FITS band (per-thread buffer of 64 × width × channels f32)

// PanelStorage: add after FullCanvasXisf
    /// A full-canvas FITS frame (primary HDU): geometry equals the session
    /// canvas; rows are decoded on demand through the shared band cache,
    /// with the file's row order flipped to top-down when needed.
    FullCanvasFits,

// Backing: add variant
    Fits(FitsBacking),

/// FITS rows decoded band-wise into the shared per-thread cache.
struct FitsBacking {
    panel: FitsPanel,
    cache: BandCache,
}

impl FitsBacking {
    fn new(panel: FitsPanel) -> FitsBacking {
        let canvas = (panel.width(), panel.height(), panel.channels());
        FitsBacking { cache: BandCache::new(canvas, FITS_BAND_ROWS), panel }
    }
    fn row(&self, c: u64, y: u64) -> Option<(u64, &[f32])> {
        self.cache.row(c, y, |y0, y1, buf| {
            let (w, _, ch) = (self.panel.width() as usize, 0, self.panel.channels());
            let bh = (y1 - y0) as usize;
            for k in 0..ch {
                let plane = &mut buf[k as usize * bh * w..(k as usize + 1) * bh * w];
                self.panel.decode_rows(k, y0, bh, plane);
            }
            Ok(())
        })
    }
}
```

`PanelReader::open`: add the `FullCanvasFits` arm mirroring the XISF one (open `FitsPanel`, compare geometry to `canvas` with the same error text, `Backing::Fits(FitsBacking::new(x))`). Replace `open_xisf` with:

```rust
    /// Open a plain full-canvas panel file of either format directly (no
    /// session metadata): the canvas geometry is the file's own. The format
    /// is sniffed from the file's magic bytes, never its extension.
    pub fn open_file(path: &Path) -> Result<PanelReader> {
        let mut magic = [0u8; 8];
        {
            use std::io::Read;
            let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
            let _ = f.read(&mut magic).map_err(|e| Error::io(path, e))?;
        }
        if &magic == b"XISF0100" {
            let x = XisfPanel::open(path)?;
            let canvas = (x.width(), x.height(), x.channels());
            Ok(PanelReader { backing: Backing::Xisf(x), bbox: [0, 0, canvas.0, canvas.1], canvas })
        } else {
            let x = FitsPanel::open(path)?;
            let canvas = (x.width(), x.height(), x.channels());
            Ok(PanelReader { backing: Backing::Fits(FitsBacking::new(x)), bbox: [0, 0, canvas.0, canvas.1], canvas })
        }
    }
```

Rename `ipc_error` → `backing_error` returning `Ipc(i) => i.ipc_error(), Fits(f) => f.cache.error(), _ => None`; `row`: `Backing::Fits(f) => f.row(c, canvas_y)`; `advise_sequential`: `Backing::Fits(f) => f.panel.advise_sequential()`. Update every caller: `grep -rn "ipc_error()\|open_xisf" crates --include=*.rs` (analyze.rs `scan_panel` and the IPC scan/blend error checks in analyze.rs/blend.rs) — rename mechanically; `open_xisf` callers become `open_file`.

- [ ] **Step 6: Run the whole core test suite**

Run: `cargo test -p mmm-core 2>&1 | grep -E "^test result|FAILED|panicked" | head`
Expected: all PASS (IPC reader tests unchanged, the two new tests pass).

- [ ] **Step 7: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | head; cargo doc -p mmm-core --no-deps 2>&1 | grep -E "^warning" | head
git add -A crates && git commit -m "feat(panel_reader): FITS backing over a shared per-thread band cache

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Linear WCS from FITS keywords (`astrometry/fits_wcs.rs`) and row reflection

**Files:**
- Create: `crates/mmm-core/src/astrometry/fits_wcs.rs`
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (add `pub mod fits_wcs;`, `LinearWcs::reflect_rows`)

**Interfaces:**
- Consumes: `formats::{FitsKeyword, RowOrder, card_number, card_string}` (Task 1).
- Produces:
  - `LinearWcs::reflect_rows(&self, height: u64) -> LinearWcs` (top-down ↔ bottom-up; an involution).
  - `fits_wcs::linear_from_keywords(cards: &[FitsKeyword]) -> Result<LinearWcs, String>` — the WCS exactly as the file states it (file frame).
  - `fits_wcs::linear_for_panel(cards, height: u64, row_order: RowOrder) -> Result<LinearWcs, String>` — reflected into the top-down frame when `row_order` is `BottomUp`.
  - (Task 4 adds `fits_wcs::model_from_keywords`.)

- [ ] **Step 1: Write the failing tests**

Create `crates/mmm-core/src/astrometry/fits_wcs.rs`:

```rust
//! Linear WCS (and, with `sip`, SIP distortion) from FITS header cards.
//!
//! Supported: `CTYPE` `RA---TAN` / `RA---TAN-SIP` (either axis order),
//! `CRVAL`, `CRPIX`, and the matrix as `CD`, `PC × CDELT`, or `CDELT` with
//! `CROTA2`. Refused (never approximated): other projections, `TPV`, and
//! nonzero `PV` terms.

use crate::astrometry::LinearWcs;
use crate::formats::{FitsKeyword, RowOrder, card_number, card_string};

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(name: &str, value: &str) -> FitsKeyword {
        FitsKeyword { name: name.into(), value: value.into(), comment: String::new() }
    }

    fn base() -> Vec<FitsKeyword> {
        vec![
            kw("CTYPE1", "'RA---TAN'"),
            kw("CTYPE2", "'DEC--TAN'"),
            kw("CRVAL1", "83.8"),
            kw("CRVAL2", "-5.4"),
            kw("CRPIX1", "2440.5"),
            kw("CRPIX2", "1618.0"),
        ]
    }

    const CD: [[f64; 2]; 2] = [[-4.0e-4, 1.0e-5], [1.0e-5, 4.0e-4]];

    fn with_cd(mut c: Vec<FitsKeyword>) -> Vec<FitsKeyword> {
        c.extend([kw("CD1_1", "-4.0E-4"), kw("CD1_2", "1.0E-5"), kw("CD2_1", "1.0D-5"), kw("CD2_2", "4.0E-4")]);
        c
    }

    #[test]
    fn cd_pc_and_crota_forms_agree() {
        let a = linear_from_keywords(&with_cd(base())).unwrap();
        assert_eq!(a.cd, CD);
        assert_eq!(a.crval, [83.8, -5.4]);
        assert_eq!(a.crpix, [2440.5, 1618.0]);
        assert_eq!(a.ctype, ["RA---TAN".to_string(), "DEC--TAN".to_string()]);
        assert_eq!(a.radesys, "ICRS");

        let mut pc = base();
        pc.extend([kw("PC1_1", "1"), kw("PC1_2", "-0.025"), kw("PC2_1", "-0.025"), kw("PC2_2", "1"), kw("CDELT1", "-4.0E-4"), kw("CDELT2", "4.0E-4")]);
        let b = linear_from_keywords(&pc).unwrap();
        for i in 0..2 {
            for j in 0..2 {
                assert!((b.cd[i][j] - CD[i][j]).abs() < 1e-15, "pc form {i}{j}");
            }
        }

        let mut rot = base();
        rot.extend([kw("CDELT1", "-4.0E-4"), kw("CDELT2", "4.0E-4"), kw("CROTA2", "30")]);
        let c = linear_from_keywords(&rot).unwrap();
        let (s, co) = 30f64.to_radians().sin_cos();
        let want = [[-4.0e-4 * co, 4.0e-4 * s], [-4.0e-4 * s, -4.0e-4 * co * -1.0]];
        // CDELT+CROTA2: CD1_1 = CDELT1·cos, CD1_2 = −CDELT2·sin, CD2_1 = CDELT1·sin, CD2_2 = CDELT2·cos
        let want = [[-4.0e-4 * co, -4.0e-4 * s], [-4.0e-4 * s, 4.0e-4 * co]];
        for i in 0..2 {
            for j in 0..2 {
                assert!((c.cd[i][j] - want[i][j]).abs() < 1e-15, "crota form {i}{j}: {} vs {}", c.cd[i][j], want[i][j]);
            }
        }
        let _ = want;
    }

    #[test]
    fn radesys_defaults_and_axis_swap() {
        let mut c = with_cd(base());
        c.push(kw("EQUINOX", "2000.0"));
        assert_eq!(linear_from_keywords(&c).unwrap().radesys, "FK5");
        c.push(kw("RADESYS", "'ICRS    '"));
        assert_eq!(linear_from_keywords(&c).unwrap().radesys, "ICRS");

        // Dec on axis 1: everything is swapped into RA-first form.
        let swapped = vec![
            kw("CTYPE1", "'DEC--TAN'"), kw("CTYPE2", "'RA---TAN'"),
            kw("CRVAL1", "-5.4"), kw("CRVAL2", "83.8"),
            kw("CRPIX1", "2440.5"), kw("CRPIX2", "1618.0"),
            kw("CD1_1", "1.0E-5"), kw("CD1_2", "4.0E-4"), kw("CD2_1", "-4.0E-4"), kw("CD2_2", "1.0E-5"),
        ];
        let s = linear_from_keywords(&swapped).unwrap();
        assert_eq!(s.crval, [83.8, -5.4]);
        assert_eq!(s.cd, CD, "rows swapped so row 0 is the RA (ξ) axis");
        assert_eq!(s.crpix, [2440.5, 1618.0], "pixel axes are untouched");
    }

    #[test]
    fn refusals() {
        let mut c = with_cd(base());
        c[0] = kw("CTYPE1", "'RA---SIN'");
        c[1] = kw("CTYPE2", "'DEC--SIN'");
        assert!(linear_from_keywords(&c).unwrap_err().contains("projection"));
        let mut c = with_cd(base());
        c[0] = kw("CTYPE1", "'RA---TPV'");
        c[1] = kw("CTYPE2", "'DEC--TPV'");
        assert!(linear_from_keywords(&c).unwrap_err().contains("TPV"));
        let mut c = with_cd(base());
        c.push(kw("PV2_3", "0.001"));
        assert!(linear_from_keywords(&c).unwrap_err().contains("PV"));
        let mut c = with_cd(base());
        c.push(kw("PV2_1", "0.0"));
        assert!(linear_from_keywords(&c).is_ok(), "zero PV terms are harmless");
        assert!(linear_from_keywords(&base()).unwrap_err().contains("CD"), "no matrix");
        let mut c = with_cd(base());
        c.retain(|k| k.name != "CRVAL2");
        assert!(linear_from_keywords(&c).unwrap_err().contains("CRVAL2"));
    }

    #[test]
    fn reflection_matches_wcs_cards_flipped_and_is_an_involution() {
        let w = linear_from_keywords(&with_cd(base())).unwrap();
        let h = 3235u64;
        let r = w.reflect_rows(h);
        assert_eq!(r.crpix, [2440.5, h as f64 + 1.0 - 1618.0]);
        assert_eq!(r.cd, [[-4.0e-4, -1.0e-5], [1.0e-5, -4.0e-4]]);
        assert_eq!(r.reflect_rows(h), w);
        // The same numbers `wcs_cards_flipped` writes for the bottom-up frame.
        let cards = crate::astrometry::wcs_cards_flipped(&w, (0, 0), h);
        let get = |n: &str| cards.iter().find(|k| k.name == n).unwrap().value.parse::<f64>().unwrap();
        assert_eq!(get("CRPIX2"), r.crpix[1]);
        assert_eq!(get("CD1_2"), r.cd[0][1]);
        assert_eq!(get("CD2_2"), r.cd[1][1]);
        // Same sky at mirrored rows: file (i, j) ↔ top-down (i, H+1−j).
        let (ra, dec) = w.pixel_to_sky(100.0, 200.0);
        let (ra2, dec2) = r.pixel_to_sky(100.0, h as f64 + 1.0 - 200.0);
        assert!((ra - ra2).abs() < 1e-12 && (dec - dec2).abs() < 1e-12);
        assert_eq!(linear_for_panel(&with_cd(base()), h, RowOrder::BottomUp).unwrap(), r);
        assert_eq!(linear_for_panel(&with_cd(base()), h, RowOrder::TopDown).unwrap(), w);
    }
}
```

Note for the implementer: in `cd_pc_and_crota_forms_agree` delete the first, wrong `want` binding and the trailing `let _ = want;` — keep only the CDELT+CROTA2 formula given in the comment (`CD1_1 = CDELT1·cos ρ`, `CD1_2 = −CDELT2·sin ρ`, `CD2_1 = CDELT1·sin ρ`, `CD2_2 = CDELT2·cos ρ`).

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p mmm-core --lib astrometry::fits_wcs 2>&1 | grep -E "error" | head -3`
Expected: `linear_from_keywords`, `reflect_rows` not found.

- [ ] **Step 3: Implement**

In `astrometry/mod.rs`, add `pub mod fits_wcs;` next to the other submodules and this method on `LinearWcs`:

```rust
    /// The same solution expressed for the vertically mirrored row order
    /// (`H` rows): `CRPIX2' = H + 1 − CRPIX2`, second matrix column negated.
    /// Converts a standard bottom-up FITS WCS into mmm's top-down frame and
    /// back (it is its own inverse). See the FITS-input design spec.
    pub fn reflect_rows(&self, height: u64) -> LinearWcs {
        let mut r = self.clone();
        r.crpix[1] = height as f64 + 1.0 - self.crpix[1];
        r.cd[0][1] = -self.cd[0][1];
        r.cd[1][1] = -self.cd[1][1];
        r
    }
```

In `fits_wcs.rs`, above the tests:

```rust
/// Projection code of a CTYPE value (`RA---TAN-SIP` → `TAN`), and whether
/// it is the RA or Dec axis.
fn ctype_parts(v: &str) -> Option<(bool, String)> {
    let v = v.trim();
    let is_ra = v.starts_with("RA--");
    let is_dec = v.starts_with("DEC-");
    if !(is_ra || is_dec) || v.len() < 8 {
        return None;
    }
    let code = v[5..8].to_string();
    Some((is_ra, code))
}

/// The linear WCS exactly as the header states it (file pixel frame,
/// 1-based, stored row order). `Err` carries the user-facing reason.
pub fn linear_from_keywords(cards: &[FitsKeyword]) -> Result<LinearWcs, String> {
    let ctype1 = card_string(cards, "CTYPE1").ok_or("CTYPE1 missing")?;
    let ctype2 = card_string(cards, "CTYPE2").ok_or("CTYPE2 missing")?;
    let (ra1, code1) = ctype_parts(&ctype1).ok_or_else(|| format!("CTYPE1 '{ctype1}' is not a celestial axis"))?;
    let (ra2, code2) = ctype_parts(&ctype2).ok_or_else(|| format!("CTYPE2 '{ctype2}' is not a celestial axis"))?;
    if ra1 == ra2 {
        return Err(format!("CTYPE1/CTYPE2 '{ctype1}'/'{ctype2}' are not one RA and one Dec axis"));
    }
    if code1 == "TPV" || code2 == "TPV" {
        return Err("TPV distortion is unsupported (re-solve with TAN-SIP)".into());
    }
    if code1 != "TAN" || code2 != "TAN" {
        return Err(format!("unsupported projection '{code1}' (only the gnomonic TAN / TAN-SIP projection is supported)"));
    }
    if let Some(k) = cards.iter().find(|k| (k.name.starts_with("PV1_") || k.name.starts_with("PV2_")) && card_number(cards, &k.name).unwrap_or(0.0) != 0.0) {
        return Err(format!("{} distortion terms are unsupported (re-solve with TAN-SIP)", k.name.split('_').next().unwrap_or("PV")));
    }
    let num = |n: &str| card_number(cards, n).ok_or_else(|| format!("{n} missing"));
    let crval = [num("CRVAL1")?, num("CRVAL2")?];
    let crpix = [num("CRPIX1")?, num("CRPIX2")?];
    let has = |n: &str| cards.iter().any(|k| k.name.eq_ignore_ascii_case(n));
    let cd = if ["CD1_1", "CD1_2", "CD2_1", "CD2_2"].iter().any(|n| has(n)) {
        let g = |n: &str| card_number(cards, n).unwrap_or(0.0);
        [[g("CD1_1"), g("CD1_2")], [g("CD2_1"), g("CD2_2")]]
    } else if has("CDELT1") && has("CDELT2") {
        let (d1, d2) = (num("CDELT1")?, num("CDELT2")?);
        if ["PC1_1", "PC1_2", "PC2_1", "PC2_2"].iter().any(|n| has(n)) {
            let g = |n: &str, dflt: f64| card_number(cards, n).unwrap_or(dflt);
            [[d1 * g("PC1_1", 1.0), d1 * g("PC1_2", 0.0)], [d2 * g("PC2_1", 0.0), d2 * g("PC2_2", 1.0)]]
        } else {
            let (s, c) = card_number(cards, "CROTA2").unwrap_or(0.0).to_radians().sin_cos();
            [[d1 * c, -d2 * s], [d1 * s, d2 * c]]
        }
    } else {
        return Err("no linear transformation (CD matrix, PC + CDELT, or CDELT + CROTA2) found".into());
    };
    let radesys = card_string(cards, "RADESYS")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| if has("EQUINOX") { "FK5".into() } else { "ICRS".into() });
    // Normalize to RA-first: swap the two world axes (rows of CD, CRVAL).
    let (crval, cd, ctype) = if ra1 {
        (crval, cd, [ctype1.trim().to_string(), ctype2.trim().to_string()])
    } else {
        ([crval[1], crval[0]], [cd[1], cd[0]], [ctype2.trim().to_string(), ctype1.trim().to_string()])
    };
    let ctype = [ctype[0][..8].to_string(), ctype[1][..8].to_string()];
    Ok(LinearWcs { crval, crpix, cd, ctype, radesys })
}

/// The panel's linear WCS in mmm's top-down frame: the file's solution,
/// reflected over `height` rows when the file is stored bottom-up.
pub fn linear_for_panel(cards: &[FitsKeyword], height: u64, row_order: RowOrder) -> Result<LinearWcs, String> {
    let w = linear_from_keywords(cards)?;
    Ok(match row_order {
        RowOrder::TopDown => w,
        RowOrder::BottomUp => w.reflect_rows(height),
    })
}
```

(`ctype` is truncated to the 8-character `RA---TAN` / `DEC--TAN` form so output cards never carry `-SIP`; the SIP presence is detected from `A_ORDER` in Task 4.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mmm-core --lib astrometry::fits_wcs 2>&1 | tail -8`
Expected: 4 PASS.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy -p mmm-core --all-targets 2>&1 | grep -E "^(warning|error)" | head
git add -A crates/mmm-core/src/astrometry && git commit -m "feat(astrometry): linear WCS from FITS keywords with row reflection

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: SIP distortion (`astrometry/sip.rs`) and `Distortion::Sip`

**Files:**
- Create: `crates/mmm-core/src/astrometry/sip.rs`
- Modify: `crates/mmm-core/src/astrometry/standard.rs` (generalize `sample` and the native-rect computation)
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (`Distortion::Sip`, `WcsModel::with_sip`, match arms)
- Modify: `crates/mmm-core/src/astrometry/fits_wcs.rs` (`model_from_keywords`)

**Interfaces:**
- Consumes: `linear_from_keywords`, `LinearWcs::reflect_rows` (Task 3); `RowOrder` (Task 1).
- Produces:
  - `standard::sample_fn(f: impl Fn(f64, f64) -> (f64, f64) + Sync, rect: [f64; 4], delta: f64, parallel: bool) -> Option<Grid2D>` (pub(crate)); `standard::native_rect(forward: impl Fn(f64,f64)->(f64,f64), width, height, delta) -> [f64; 4]` (pub(crate)).
  - `sip::SipSolution` with `parse(cards, file_linear: &LinearWcs, row_order, height) -> Result<Option<SipSolution>, String>` (`Ok(None)` when no `A_ORDER`), `image_to_native(&self, x_img, y_img) -> (f64, f64)`, `native_to_image(&self, xi, eta) -> (f64, f64)` (NaN on divergence), `inverse_seed(&self, xi, eta) -> (f64, f64)` (pub(crate)), `validate(&self, width, height) -> Result<(), String>`, `linear(&self) -> &LinearWcs` (top-down).
  - `WcsModel::with_sip(sol: SipSolution, width, height) -> WcsModel` (pub(crate)).
  - `fits_wcs::model_from_keywords(cards, width, height, row_order) -> Result<WcsModel, String>`.

- [ ] **Step 1: Generalize the sampler in `standard.rs`**

Replace `fn sample(dir: &Direction, ...)` with a generic version and factor the rect computation; the two existing `sample_*` functions become thin callers (behavior unchanged):

```rust
/// Sample one 2-D map onto a grid (see the former `sample` for the
/// `parallel` caveat: never run a nested parallel sampling from inside a
/// rayon worker).
pub(crate) fn sample_fn<F>(f: F, rect: [f64; 4], delta: f64, parallel: bool) -> Option<Grid2D>
where
    F: Fn(f64, f64) -> (f64, f64) + Sync,
{
    // body identical to the old `sample`, with `dir.map(x, y)` → `f(x, y)`
}

/// The native-plane rectangle covering the image's forward-mapped corners
/// and edge midpoints, padded by one `delta`.
pub(crate) fn native_rect<F>(forward: F, width: u64, height: u64, delta: f64) -> [f64; 4]
where
    F: Fn(f64, f64) -> (f64, f64),
{
    let (w, h) = (width as f64, height as f64);
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h), (w / 2.0, 0.0), (w / 2.0, h), (0.0, h / 2.0), (w, h / 2.0)] {
        let (xi, eta) = forward(x, y);
        x0 = x0.min(xi);
        y0 = y0.min(eta);
        x1 = x1.max(xi);
        y1 = y1.max(eta);
    }
    [x0 - delta, y0 - delta, x1 + delta, y1 + delta]
}

/// Pixel scale (deg/px) of a matrix: sqrt(|det|); `None` unless positive
/// and finite.
pub(crate) fn matrix_scale(m: [[f64; 2]; 2]) -> Option<f64> {
    let s = (m[0][0] * m[1][1] - m[0][1] * m[1][0]).abs().sqrt();
    positive(s).then_some(s)
}
```

`sample_image_to_native` → `sample_fn(|x, y| i2p.map(x, y), [0,0,w,h], IMAGE_DELTA_PX, parallel)`; `sample_native_to_image` → `let scale = matrix_scale(sol.linear.cd)?; let delta = IMAGE_DELTA_PX * scale; sample_fn(|x, y| p2i.map(x, y), native_rect(|x, y| i2p.map(x, y), width, height, delta), delta, parallel)`. Make `IMAGE_DELTA_PX` `pub(crate)`. Run `cargo test -p mmm-core --lib astrometry` — must still pass.

- [ ] **Step 2: Write the failing SIP tests**

Create `crates/mmm-core/src/astrometry/sip.rs` with docs, imports, and tests:

```rust
//! SIP distortion (Shupe et al. 2005) for TAN-SIP FITS solutions.
//!
//! Forward: file pixel `(i, j)` → `(u, v) = (i − CRPIX1, j − CRPIX2)` →
//! `(ξ, η) = CD · (u + f(u, v), v + g(u, v))` with `f = Σ A_p_q uᵖ vᵠ`,
//! `g = Σ B_p_q uᵖ vᵠ`, `p + q ≤ order`. Inverse: the `AP`/`BP` polynomials
//! (when present) only seed a Newton iteration on the forward map, so the
//! inverse is exact to floating-point precision either way. The model is
//! evaluated in file coordinates; mmm's top-down image coordinates are
//! converted at the boundary per the panel's [`RowOrder`].

use crate::astrometry::LinearWcs;
use crate::formats::{FitsKeyword, RowOrder, card_number};

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(name: &str, value: &str) -> FitsKeyword {
        FitsKeyword { name: name.into(), value: value.into(), comment: String::new() }
    }

    /// A quadratic SIP with modest coefficients on a 1000×800 field.
    fn cards(with_inverse: bool) -> Vec<FitsKeyword> {
        let mut c = vec![
            kw("A_ORDER", "2"), kw("B_ORDER", "2"),
            kw("A_2_0", "1.5E-6"), kw("A_1_1", "-2.0E-6"), kw("A_0_2", "1.0E-6"),
            kw("B_2_0", "-1.0E-6"), kw("B_1_1", "1.2E-6"), kw("B_0_2", "2.0E-6"),
        ];
        if with_inverse {
            // Approximate inverse (first-order negation of the forward terms).
            c.extend([
                kw("AP_ORDER", "2"), kw("BP_ORDER", "2"),
                kw("AP_2_0", "-1.5E-6"), kw("AP_1_1", "2.0E-6"), kw("AP_0_2", "-1.0E-6"),
                kw("BP_2_0", "1.0E-6"), kw("BP_1_1", "-1.2E-6"), kw("BP_0_2", "-2.0E-6"),
            ]);
        }
        c
    }

    fn file_linear() -> LinearWcs {
        LinearWcs {
            crval: [83.8, -5.4],
            crpix: [500.5, 400.5],
            cd: [[-2.0e-4, 0.0], [0.0, 2.0e-4]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        }
    }

    #[test]
    fn forward_matches_hand_evaluation_in_both_row_orders() {
        for ro in [RowOrder::BottomUp, RowOrder::TopDown] {
            let s = SipSolution::parse(&cards(false), &file_linear(), ro, 800).unwrap().unwrap();
            // File pixel (700, 150): u = 199.5, v = −250.5.
            let (u, v) = (199.5f64, -250.5f64);
            let f = 1.5e-6 * u * u - 2.0e-6 * u * v + 1.0e-6 * v * v;
            let g = -1.0e-6 * u * u + 1.2e-6 * u * v + 2.0e-6 * v * v;
            let want = (-2.0e-4 * (u + f), 2.0e-4 * (v + g));
            let y_img = match ro { RowOrder::TopDown => 150.0 - 0.5, RowOrder::BottomUp => 800.0 - 150.0 + 0.5 };
            let got = s.image_to_native(700.0 - 0.5, y_img);
            assert!((got.0 - want.0).abs() < 1e-15 && (got.1 - want.1).abs() < 1e-15, "{ro:?}: {got:?} vs {want:?}");
            // At the reference pixel the distortion vanishes.
            let ref_img = [s.linear().crpix[0] - 0.5, s.linear().crpix[1] - 0.5];
            let at_ref = s.image_to_native(ref_img[0], ref_img[1]);
            assert!(at_ref.0.abs() < 1e-18 && at_ref.1.abs() < 1e-18);
        }
    }

    #[test]
    fn inverse_round_trips_with_and_without_ap_bp() {
        for with_inv in [false, true] {
            let s = SipSolution::parse(&cards(with_inv), &file_linear(), RowOrder::BottomUp, 800).unwrap().unwrap();
            for (x, y) in [(0.0, 0.0), (999.0, 0.0), (0.0, 799.0), (999.0, 799.0), (123.4, 567.8), (500.0, 400.0)] {
                let (xi, eta) = s.image_to_native(x, y);
                let (bx, by) = s.native_to_image(xi, eta);
                assert!((bx - x).abs() < 1e-7 && (by - y).abs() < 1e-7, "inv={with_inv} ({x},{y}) → ({bx},{by})");
            }
            s.validate(1000, 800).unwrap();
        }
        // The AP/BP seed alone (no Newton) is already close: proves the
        // inverse cards were parsed and applied.
        let s = SipSolution::parse(&cards(true), &file_linear(), RowOrder::BottomUp, 800).unwrap().unwrap();
        let (xi, eta) = s.image_to_native(950.0, 50.0);
        let (sx, sy) = s.inverse_seed(xi, eta);
        assert!((sx - 950.0).abs() < 0.5 && (sy - 50.0).abs() < 0.5, "seed ({sx},{sy})");
        let none = SipSolution::parse(&cards(false), &file_linear(), RowOrder::BottomUp, 800).unwrap().unwrap();
        let (nx, ny) = none.inverse_seed(xi, eta);
        assert!((nx - 950.0).abs() > 0.5 || (ny - 50.0).abs() > 0.5, "linear seed is worse than AP/BP");
    }

    #[test]
    fn parse_edge_cases() {
        assert!(SipSolution::parse(&[], &file_linear(), RowOrder::BottomUp, 800).unwrap().is_none());
        let mut c = cards(false);
        c.retain(|k| k.name != "B_ORDER");
        assert!(SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800).unwrap().is_some(), "B_ORDER defaults to A_ORDER");
        let mut c = cards(false);
        c[0] = kw("A_ORDER", "12");
        assert!(SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800).unwrap_err().contains("A_ORDER"));
        let mut c = cards(false);
        c.push(kw("A_3_0", "1.0"));
        assert!(SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800).unwrap_err().contains("A_3_0"), "term above the declared order");
    }

    #[test]
    fn validation_rejects_wild_distortion() {
        let mut c = cards(false);
        c[2] = kw("A_2_0", "1.0E-3"); // 1000× too large: corners deviate by degrees
        let s = SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800).unwrap().unwrap();
        let e = s.validate(1000, 800).unwrap_err();
        assert!(e.contains("linear"), "{e}");
    }
}
```

- [ ] **Step 3: Run to see failures**

Run: `cargo test -p mmm-core --lib astrometry::sip 2>&1 | grep -E "^error" | head -3`
Expected: `SipSolution` not found.

- [ ] **Step 4: Implement `SipSolution`**

Above the tests in `sip.rs`:

```rust
/// Highest polynomial order accepted (astrometry.net uses 2–5).
const MAX_ORDER: usize = 9;

/// Dense SIP polynomial: `coef[p * (order + 1) + q]` multiplies `uᵖ vᵠ`.
#[derive(Debug, Clone)]
struct Poly {
    order: usize,
    coef: Vec<f64>,
}

impl Poly {
    fn parse(cards: &[FitsKeyword], prefix: &str, order: usize) -> Result<Poly, String> {
        let n = order + 1;
        let mut coef = vec![0.0; n * n];
        for k in cards {
            let Some(rest) = k.name.strip_prefix(prefix) else { continue };
            let Some((p, q)) = rest.split_once('_') else { continue };
            let (Ok(p), Ok(q)) = (p.parse::<usize>(), q.parse::<usize>()) else { continue };
            if p + q > order {
                return Err(format!("{} exceeds {}ORDER = {order}", k.name, prefix));
            }
            coef[p * n + q] = card_number(cards, &k.name).ok_or_else(|| format!("{} is not a number", k.name))?;
        }
        Ok(Poly { order, coef })
    }

    fn eval(&self, u: f64, v: f64) -> f64 {
        let n = self.order + 1;
        let mut s = 0.0;
        let mut up = 1.0;
        for p in 0..n {
            let mut vq = 1.0;
            for q in 0..(n - p) {
                s += self.coef[p * n + q] * up * vq;
                vq *= v;
            }
            up *= u;
        }
        s
    }

    /// `(∂/∂u, ∂/∂v)`.
    fn grad(&self, u: f64, v: f64) -> (f64, f64) {
        let n = self.order + 1;
        let (mut du, mut dv) = (0.0, 0.0);
        for p in 0..n {
            for q in 0..(n - p) {
                let c = self.coef[p * n + q];
                if c == 0.0 {
                    continue;
                }
                if p > 0 {
                    du += c * p as f64 * u.powi(p as i32 - 1) * v.powi(q as i32);
                }
                if q > 0 {
                    dv += c * q as f64 * u.powi(p as i32) * v.powi(q as i32 - 1);
                }
            }
        }
        (du, dv)
    }
}

/// A parsed TAN-SIP solution (see the module docs for the conventions).
#[derive(Debug, Clone)]
pub struct SipSolution {
    linear: LinearWcs,
    file_crpix: [f64; 2],
    file_cd: [[f64; 2]; 2],
    file_cd_inv: [[f64; 2]; 2],
    a: Poly,
    b: Poly,
    ap: Option<Poly>,
    bp: Option<Poly>,
    row_order: RowOrder,
    height: u64,
}

impl SipSolution {
    /// Parse the SIP cards accompanying `file_linear` (the header's own,
    /// unreflected linear solution). `Ok(None)` when the header carries no
    /// `A_ORDER`; `Err` on malformed or oversized polynomials.
    pub fn parse(cards: &[FitsKeyword], file_linear: &LinearWcs, row_order: RowOrder, height: u64) -> Result<Option<SipSolution>, String> {
        let order_of = |name: &str| -> Result<Option<usize>, String> {
            match card_number(cards, name) {
                None => Ok(None),
                Some(v) if v >= 0.0 && v.fract() == 0.0 && (v as usize) <= MAX_ORDER => Ok(Some(v as usize)),
                Some(v) => Err(format!("{name} = {v} is not an order in 0..={MAX_ORDER}")),
            }
        };
        let Some(a_order) = order_of("A_ORDER")? else { return Ok(None) };
        let b_order = order_of("B_ORDER")?.unwrap_or(a_order);
        let a = Poly::parse(cards, "A_", a_order)?;
        let b = Poly::parse(cards, "B_", b_order)?;
        let ap = match order_of("AP_ORDER")? { Some(o) => Some(Poly::parse(cards, "AP_", o)?), None => None };
        let bp = match order_of("BP_ORDER")? { Some(o) => Some(Poly::parse(cards, "BP_", o)?), None => None };
        let m = file_linear.cd;
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        if !(det.is_finite() && det != 0.0) {
            return Err("CD matrix is singular".into());
        }
        let file_cd_inv = [[m[1][1] / det, -m[0][1] / det], [-m[1][0] / det, m[0][0] / det]];
        let linear = match row_order {
            RowOrder::TopDown => file_linear.clone(),
            RowOrder::BottomUp => file_linear.reflect_rows(height),
        };
        Ok(Some(SipSolution { linear, file_crpix: file_linear.crpix, file_cd: m, file_cd_inv, a, b, ap, bp, row_order, height }))
    }

    /// The linear solution in mmm's top-down frame.
    pub fn linear(&self) -> &LinearWcs {
        &self.linear
    }

    /// Image (0-based, top-down, span) → file pixel offsets `(u, v)`.
    fn image_to_uv(&self, x_img: f64, y_img: f64) -> (f64, f64) {
        let i = x_img + 0.5;
        let j = match self.row_order {
            RowOrder::TopDown => y_img + 0.5,
            RowOrder::BottomUp => self.height as f64 - y_img + 0.5,
        };
        (i - self.file_crpix[0], j - self.file_crpix[1])
    }

    fn uv_to_image(&self, u: f64, v: f64) -> (f64, f64) {
        let i = u + self.file_crpix[0];
        let j = v + self.file_crpix[1];
        let y_img = match self.row_order {
            RowOrder::TopDown => j - 0.5,
            RowOrder::BottomUp => self.height as f64 - j + 0.5,
        };
        (i - 0.5, y_img)
    }

    fn forward_uv(&self, u: f64, v: f64) -> (f64, f64) {
        let du = u + self.a.eval(u, v);
        let dv = v + self.b.eval(u, v);
        let m = self.file_cd;
        (m[0][0] * du + m[0][1] * dv, m[1][0] * du + m[1][1] * dv)
    }

    /// Image coordinate → native tangent plane `(ξ, η)` in degrees.
    pub fn image_to_native(&self, x_img: f64, y_img: f64) -> (f64, f64) {
        let (u, v) = self.image_to_uv(x_img, y_img);
        self.forward_uv(u, v)
    }

    /// The Newton starting point for `(ξ, η)`: the AP/BP inverse when the
    /// header has one, else the plain linear inverse. Image coordinates.
    pub(crate) fn inverse_seed(&self, xi: f64, eta: f64) -> (f64, f64) {
        let (u, v) = self.seed_uv(xi, eta);
        self.uv_to_image(u, v)
    }

    fn seed_uv(&self, xi: f64, eta: f64) -> (f64, f64) {
        let inv = self.file_cd_inv;
        let (bu, bv) = (inv[0][0] * xi + inv[0][1] * eta, inv[1][0] * xi + inv[1][1] * eta);
        match (&self.ap, &self.bp) {
            (Some(ap), Some(bp)) => (bu + ap.eval(bu, bv), bv + bp.eval(bu, bv)),
            _ => (bu, bv),
        }
    }

    /// Native tangent plane → image coordinate; NaN when Newton fails.
    pub fn native_to_image(&self, xi: f64, eta: f64) -> (f64, f64) {
        let (mut u, mut v) = self.seed_uv(xi, eta);
        for _ in 0..8 {
            let (fx, fy) = self.forward_uv(u, v);
            let (rx, ry) = (fx - xi, fy - eta);
            // Jacobian of forward_uv: CD · [[1 + a_u, a_v], [b_u, 1 + b_v]].
            let (au, av) = self.a.grad(u, v);
            let (bu, bv) = self.b.grad(u, v);
            let m = self.file_cd;
            let j00 = m[0][0] * (1.0 + au) + m[0][1] * bu;
            let j01 = m[0][0] * av + m[0][1] * (1.0 + bv);
            let j10 = m[1][0] * (1.0 + au) + m[1][1] * bu;
            let j11 = m[1][0] * av + m[1][1] * (1.0 + bv);
            let det = j00 * j11 - j01 * j10;
            if !(det.is_finite() && det != 0.0) {
                return (f64::NAN, f64::NAN);
            }
            let du = (j11 * rx - j01 * ry) / det;
            let dv = (j00 * ry - j10 * rx) / det;
            u -= du;
            v -= dv;
            if !(u.is_finite() && v.is_finite()) {
                return (f64::NAN, f64::NAN);
            }
            if du.abs() < 1e-9 && dv.abs() < 1e-9 {
                return self.uv_to_image(u, v);
            }
        }
        (f64::NAN, f64::NAN)
    }

    /// Consistency checks before the model is used (mirrors
    /// `standard::validate_model`): the reference pixel maps near the
    /// origin, corners stay within 0.05° of the linear map, and the inverse
    /// round-trips at center and corners to 1e-3 px.
    pub fn validate(&self, width: u64, height: u64) -> Result<(), String> {
        let refimg = [self.linear.crpix[0] - 0.5, self.linear.crpix[1] - 0.5];
        let (xi, eta) = self.image_to_native(refimg[0], refimg[1]);
        if !xi.is_finite() || !eta.is_finite() || xi.hypot(eta) > 0.01 {
            return Err(format!("SIP maps the reference pixel {:.4}° from the projection origin", xi.hypot(eta)));
        }
        let (w, h) = (width as f64, height as f64);
        let m = self.linear.cd;
        for (cx, cy) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h), (w / 2.0, h / 2.0)] {
            let (gx, gy) = self.image_to_native(cx, cy);
            let (dx, dy) = (cx - refimg[0], cy - refimg[1]);
            let (lx, ly) = (m[0][0] * dx + m[0][1] * dy, m[1][0] * dx + m[1][1] * dy);
            let dev = (gx - lx).hypot(gy - ly);
            if !dev.is_finite() || dev > 0.05 {
                return Err(format!("SIP deviates {dev:.3}° from its own linear solution at ({cx:.0}, {cy:.0}); the model is inconsistent"));
            }
            let (bx, by) = self.native_to_image(gx, gy);
            let rt = (bx - cx).hypot(by - cy);
            if !rt.is_finite() || rt > 1e-3 {
                return Err(format!("SIP inverse does not round-trip at ({cx:.0}, {cy:.0}): {rt:.2e} px"));
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 5: Wire `Distortion::Sip` into `WcsModel`**

In `astrometry/mod.rs` add `pub mod sip;` and:

```rust
// Distortion enum: new variant
    /// TAN-SIP FITS solution: the polynomials, sampled onto grids lazily
    /// like `Standard`.
    Sip {
        sol: Arc<sip::SipSolution>,
        image_to_native: OnceLock<Option<Grid2D>>,
        native_to_image: OnceLock<Option<Grid2D>>,
    },

// WcsModel
    /// A model over a parsed SIP solution; grids are sampled on demand.
    pub(crate) fn with_sip(sol: sip::SipSolution, width: u64, height: u64) -> WcsModel {
        WcsModel {
            linear: sol.linear().clone(),
            distortion: Distortion::Sip { sol: Arc::new(sol), image_to_native: OnceLock::new(), native_to_image: OnceLock::new() },
            width,
            height,
        }
    }
```

In `image_to_native()` add the arm (same shape as `Standard`, same warning text):

```rust
            Distortion::Sip { sol, image_to_native, .. } => image_to_native
                .get_or_init(|| {
                    let s = sol.clone();
                    let g = standard::sample_fn(move |x, y| s.image_to_native(x, y), [0.0, 0.0, self.width as f64, self.height as f64], standard::IMAGE_DELTA_PX, rayon::current_thread_index().is_none());
                    if g.is_none() { tracing::warn!("astrometric solution: sampling the image→projection grid failed; using the linear solution"); }
                    g
                })
                .as_ref(),
```

and in `native_to_image()`:

```rust
            Distortion::Sip { sol, native_to_image, .. } => native_to_image
                .get_or_init(|| {
                    let s = sol.clone();
                    let g = standard::matrix_scale(self.linear.cd).and_then(|scale| {
                        let delta = standard::IMAGE_DELTA_PX * scale;
                        let rect = standard::native_rect(|x, y| s.image_to_native(x, y), self.width, self.height, delta);
                        let s2 = s.clone();
                        standard::sample_fn(move |xi, eta| s2.native_to_image(xi, eta), rect, delta, rayon::current_thread_index().is_none())
                    });
                    if g.is_none() { tracing::warn!("astrometric solution: sampling the projection→image grid failed; using the linear solution"); }
                    g
                })
                .as_ref(),
```

Update `grids_sampled` (test helper) with a `Sip` arm identical to `Standard`, and the `is_spline` doc: "True when the solution carries (and the model uses) a distortion model or grids — a PixInsight spline solution or a FITS SIP polynomial."

- [ ] **Step 6: Add `model_from_keywords` to `fits_wcs.rs`**

```rust
use crate::astrometry::WcsModel;
use crate::astrometry::sip::SipSolution;

/// Full model for a FITS panel: the linear solution (reflected into the
/// top-down frame for bottom-up files) plus SIP distortion when the header
/// carries `A_ORDER`. `Err` explains why the header is unusable; a SIP
/// solution that fails validation is refused rather than approximated.
pub fn model_from_keywords(cards: &[FitsKeyword], width: u64, height: u64, row_order: RowOrder) -> Result<WcsModel, String> {
    let file_linear = linear_from_keywords(cards)?;
    match SipSolution::parse(cards, &file_linear, row_order, height)? {
        None => {
            let linear = match row_order { RowOrder::TopDown => file_linear, RowOrder::BottomUp => file_linear.reflect_rows(height) };
            Ok(WcsModel::linear_only(linear, width, height))
        }
        Some(sip) => {
            sip.validate(width, height)?;
            Ok(WcsModel::with_sip(sip, width, height))
        }
    }
}
```

Add a test in `fits_wcs.rs`:

```rust
    #[test]
    fn model_from_keywords_uses_grids_for_sip_and_linear_otherwise() {
        let lin = model_from_keywords(&with_cd(base()), 4880, 3235, RowOrder::BottomUp).unwrap();
        assert!(!lin.is_spline());
        let mut c = with_cd(base());
        c.extend([kw("A_ORDER", "2"), kw("B_ORDER", "2"), kw("A_2_0", "1.0E-7"), kw("B_0_2", "1.0E-7")]);
        let m = model_from_keywords(&c, 4880, 3235, RowOrder::BottomUp).unwrap();
        assert!(m.is_spline());
        // Grid path and direct evaluation agree, and the inverse round-trips.
        let s = SipSolution::parse(&c, &linear_from_keywords(&c).unwrap(), RowOrder::BottomUp, 3235).unwrap().unwrap();
        for (x, y) in [(10.0, 10.0), (4000.0, 3000.0), (2440.0, 1617.5)] {
            let (ra, dec) = m.pixel_to_sky(x, y);
            let (xi, eta) = s.image_to_native(x, y);
            let (ra2, dec2) = crate::astrometry::tan_deproject(m.linear.crval, xi, eta);
            assert!((ra - ra2).abs() < 1e-9 && (dec - dec2).abs() < 1e-9, "grid vs direct at ({x},{y})");
            let (bx, by) = m.sky_to_pixel(ra, dec).unwrap();
            assert!((bx - x).abs() < 2e-3 && (by - y).abs() < 2e-3, "round trip ({x},{y}) → ({bx},{by})");
        }
    }
```

(`tan_deproject` is private in `mod.rs`; make it `pub(crate)`.)

- [ ] **Step 7: Run the astrometry tests**

Run: `cargo test -p mmm-core --lib astrometry 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: all PASS.

- [ ] **Step 8: Lint and commit**

```bash
cargo fmt && cargo clippy -p mmm-core --all-targets 2>&1 | grep -E "^(warning|error)" | head; cargo doc -p mmm-core --no-deps 2>&1 | grep -E "^warning" | head
git add -A crates/mmm-core/src/astrometry && git commit -m "feat(astrometry): SIP distortion model with Newton inverse and lazy grids

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: SIP oracle fixture from astropy

**Files:**
- Create: `scripts/gen_sip_fixture.py`
- Create: `crates/mmm-core/tests/fixtures/sip_oracle.json` (generated, committed)
- Modify: `crates/mmm-core/src/astrometry/sip.rs` (oracle tests)

**Interfaces:**
- Consumes: `fits_wcs::model_from_keywords`, `SipSolution::parse`, `linear_from_keywords` (Tasks 3–4).
- Produces: the committed fixture; regeneration is `python3 scripts/gen_sip_fixture.py`.

- [ ] **Step 1: Write the generator**

`scripts/gen_sip_fixture.py`:

```python
#!/usr/bin/env python3
"""Generate crates/mmm-core/tests/fixtures/sip_oracle.json with astropy.

Each case: a FITS header (as [name, value, comment] cards, values in FITS
text form), the image size, ~48 forward samples (i, j, ra, dec) from
astropy's all_pix2world (origin 1 = FITS pixel coordinates) and ~48 inverse
samples (ra, dec, i, j) from all_world2pix at tight tolerance. Cases cover
SIP order 2 and 3, with and without AP/BP, bottom-up (no ROWORDER) and
TOP-DOWN. Run from the repo root; needs astropy >= 5 and numpy.
"""
import json, warnings
import numpy as np
from astropy.io import fits
from astropy.wcs import WCS

warnings.simplefilter("ignore")
OUT = "crates/mmm-core/tests/fixtures/sip_oracle.json"
rng = np.random.default_rng(20260926)


def fit_inverse(a, b, order, crpix, w, h):
    """Least-squares AP/BP of the same order over a grid of (U, V)."""
    us = np.linspace(-crpix[0], w - crpix[0], 25)
    vs = np.linspace(-crpix[1], h - crpix[1], 25)
    U0, V0 = np.meshgrid(us, vs)
    u, v = U0.ravel(), V0.ravel()
    f = sum(a[p][q] * u**p * v**q for p in range(order + 1) for q in range(order + 1 - p))
    g = sum(b[p][q] * u**p * v**q for p in range(order + 1) for q in range(order + 1 - p))
    U, V = u + f, v + g
    terms = [(p, q) for p in range(order + 1) for q in range(order + 1 - p)]
    M = np.stack([U**p * V**q for p, q in terms], axis=1)
    ap = np.linalg.lstsq(M, u - U, rcond=None)[0]
    bp = np.linalg.lstsq(M, v - V, rcond=None)[0]
    return terms, ap, bp


def make_case(name, order, with_inverse, top_down, w, h, seed):
    r = np.random.default_rng(seed)
    hdr = fits.Header()
    hdr["CTYPE1"], hdr["CTYPE2"] = "RA---TAN-SIP", "DEC--TAN-SIP"
    hdr["CRVAL1"], hdr["CRVAL2"] = 83.8 + r.uniform(-1, 1), -5.4 + r.uniform(-1, 1)
    hdr["CRPIX1"], hdr["CRPIX2"] = w / 2 + r.uniform(-30, 30), h / 2 + r.uniform(-30, 30)
    scale = 4.4e-4
    th = np.deg2rad(r.uniform(-20, 20))
    hdr["CD1_1"], hdr["CD1_2"] = -scale * np.cos(th), scale * np.sin(th)
    hdr["CD2_1"], hdr["CD2_2"] = scale * np.sin(th), scale * np.cos(th)
    hdr["EQUINOX"] = 2000.0
    if top_down:
        hdr["ROWORDER"] = "TOP-DOWN"
    a = np.zeros((order + 1, order + 1))
    b = np.zeros((order + 1, order + 1))
    for p in range(order + 1):
        for q in range(order + 1 - p):
            if p + q >= 2:
                mag = 10.0 ** (-(2 * (p + q) + 1.5))  # ~ few px at the corners
                a[p][q] = r.uniform(-1, 1) * mag
                b[p][q] = r.uniform(-1, 1) * mag
    hdr["A_ORDER"], hdr["B_ORDER"] = order, order
    for p in range(order + 1):
        for q in range(order + 1 - p):
            if a[p][q] != 0:
                hdr[f"A_{p}_{q}"] = float(a[p][q])
            if b[p][q] != 0:
                hdr[f"B_{p}_{q}"] = float(b[p][q])
    if with_inverse:
        terms, ap, bp = fit_inverse(a, b, order, (hdr["CRPIX1"], hdr["CRPIX2"]), w, h)
        hdr["AP_ORDER"], hdr["BP_ORDER"] = order, order
        for (p, q), x, y in zip(terms, ap, bp):
            hdr[f"AP_{p}_{q}"], hdr[f"BP_{p}_{q}"] = float(x), float(y)
    wcs = WCS(hdr)
    i = np.concatenate([[1, w, 1, w, hdr["CRPIX1"]], rng.uniform(1, w, 43)])
    j = np.concatenate([[1, 1, h, h, hdr["CRPIX2"]], rng.uniform(1, h, 43)])
    ra, dec = wcs.all_pix2world(i, j, 1)
    ra2 = ra + rng.uniform(-0.01, 0.01, ra.size)
    dec2 = dec + rng.uniform(-0.01, 0.01, dec.size)
    i2, j2 = wcs.all_world2pix(ra2, dec2, 1, tolerance=1e-10, maxiter=100)
    cards = [[c.keyword, hdr.tostring().__class__ and _card_value(c), c.comment] for c in hdr.cards]
    return {
        "name": name, "width": w, "height": h, "cards": cards,
        "forward": np.stack([i, j, ra, dec], axis=1).tolist(),
        "inverse": np.stack([ra2, dec2, i2, j2], axis=1).tolist(),
    }


def _card_value(c):
    """The value as it appears in the 80-char card text (FITS text form)."""
    img = c.image
    body = img[10:] if img[8:10] == "= " else ""
    if body.lstrip().startswith("'"):
        s = body.lstrip()
        k = 1
        while k < len(s):
            if s[k] == "'":
                if k + 1 < len(s) and s[k + 1] == "'":
                    k += 2
                    continue
                break
            k += 1
        return s[: k + 1].rstrip()
    return body.split("/")[0].strip()


cases = [
    make_case("order2_bottomup_noinv", 2, False, False, 1600, 1200, 1),
    make_case("order2_bottomup_inv", 2, True, False, 1600, 1200, 2),
    make_case("order3_bottomup_inv", 3, True, False, 4880, 3235, 3),
    make_case("order2_topdown_inv", 2, True, True, 1000, 700, 4),
]
with open(OUT, "w") as fh:
    json.dump(cases, fh, indent=1)
print(f"wrote {OUT}: {len(cases)} cases")
```

(In `make_case`, the `cards` line should simply be `cards = [[c.keyword, _card_value(c), c.comment] for c in hdr.cards]` — remove the stray `hdr.tostring().__class__ and` fragment.)

- [ ] **Step 2: Generate and sanity-check the fixture**

```bash
mkdir -p crates/mmm-core/tests/fixtures && python3 scripts/gen_sip_fixture.py && python3 -c "
import json; c=json.load(open('crates/mmm-core/tests/fixtures/sip_oracle.json'))
for k in c: print(k['name'], len(k['cards']), len(k['forward']), len(k['inverse']), [x for x in k['cards'] if x[0]=='CTYPE1'])"
ls -la crates/mmm-core/tests/fixtures/sip_oracle.json
```
Expected: 4 cases, 48 samples each, CTYPE1 value `'RA---TAN-SIP'` quoted; file well under 100 KB.

- [ ] **Step 3: Write the oracle test in `sip.rs`**

Append inside `mod tests`:

```rust
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        width: u64,
        height: u64,
        cards: Vec<(String, String, String)>,
        forward: Vec<[f64; 4]>,
        inverse: Vec<[f64; 4]>,
    }

    fn oracle() -> Vec<Case> {
        serde_json::from_str(include_str!("../../tests/fixtures/sip_oracle.json")).expect("fixture parses")
    }

    #[test]
    fn matches_astropy_oracle_directly_and_through_grids() {
        use crate::astrometry::fits_wcs::{linear_from_keywords, model_from_keywords};
        use crate::astrometry::tan_deproject;
        for case in oracle() {
            let cards: Vec<FitsKeyword> = case.cards.iter().map(|(n, v, c)| FitsKeyword { name: n.clone(), value: v.clone(), comment: c.clone() }).collect();
            let ro = if cards.iter().any(|k| k.name == "ROWORDER" && k.value.contains("TOP-DOWN")) { RowOrder::TopDown } else { RowOrder::BottomUp };
            let h = case.height;
            let file_lin = linear_from_keywords(&cards).unwrap();
            let sip = SipSolution::parse(&cards, &file_lin, ro, h).unwrap().expect("SIP present");
            let model = model_from_keywords(&cards, case.width, h, ro).unwrap();
            assert!(model.is_spline(), "{}", case.name);
            let to_img = |i: f64, j: f64| -> (f64, f64) {
                match ro { RowOrder::TopDown => (i - 0.5, j - 0.5), RowOrder::BottomUp => (i - 0.5, h as f64 - j + 0.5) }
            };
            for &[i, j, ra, dec] in &case.forward {
                let (x, y) = to_img(i, j);
                let (xi, eta) = sip.image_to_native(x, y);
                let (r1, d1) = tan_deproject(model.linear.crval, xi, eta);
                let dra = ((r1 - ra + 180.0).rem_euclid(360.0) - 180.0) * dec.to_radians().cos();
                assert!(dra.abs() < 1e-9 && (d1 - dec).abs() < 1e-9, "{} direct fwd at ({i},{j}): {dra:e} {:e}", case.name, d1 - dec);
                let (r2, d2) = model.pixel_to_sky(x, y);
                let dra2 = ((r2 - ra + 180.0).rem_euclid(360.0) - 180.0) * dec.to_radians().cos();
                assert!(dra2.abs() < 2e-7 && (d2 - dec).abs() < 2e-7, "{} grid fwd at ({i},{j}): {dra2:e} {:e}", case.name, d2 - dec);
            }
            for &[ra, dec, i, j] in &case.inverse {
                let (x, y) = to_img(i, j);
                let (xi, eta) = crate::astrometry::tan_project_checked(model.linear.crval, ra, dec).unwrap();
                let (bx, by) = sip.native_to_image(xi, eta);
                assert!((bx - x).abs() < 1e-4 && (by - y).abs() < 1e-4, "{} direct inv: ({bx},{by}) vs ({x},{y})", case.name);
                let (gx, gy) = model.sky_to_pixel(ra, dec).expect("inside native domain");
                assert!((gx - x).abs() < 5e-3 && (gy - y).abs() < 5e-3, "{} grid inv: ({gx},{gy}) vs ({x},{y})", case.name);
            }
        }
    }
```

(`tan_project_checked` becomes `pub(crate)` like `tan_deproject`.) The grid tolerances (2e-7° ≈ 0.7 mas; 5e-3 px) reflect 16 px Catmull-Rom sampling of a smooth polynomial; if a case exceeds them, print the worst residual and check the fixture's coefficient magnitudes before loosening anything.

- [ ] **Step 4: Run**

Run: `cargo test -p mmm-core --lib astrometry::sip 2>&1 | grep -E "^test |test result"`
Expected: 5 PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add scripts/gen_sip_fixture.py crates/mmm-core/tests/fixtures/sip_oracle.json crates/mmm-core/src/astrometry
git commit -m "test(astrometry): SIP oracle fixture generated with astropy

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: `InputPanel` and the unsolved diagnostic

**Files:**
- Modify: `crates/mmm-core/src/formats/mod.rs` (add `InputPanel`)
- Modify: `crates/mmm-core/src/analyze.rs` (move `describe_unsolved` out)
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (receive `describe_unsolved` as `pub fn`)

**Interfaces:**
- Consumes: `XisfPanel`, `FitsPanel`, `PanelStorage`, `fits_wcs::{model_from_keywords, linear_for_panel}`, `WcsModel::from_properties`, `wcs_from_properties`.
- Produces: `formats::InputPanel` with `open(path) -> Result<InputPanel>`, `path()`, `width()`, `height()`, `channels()`, `fits_keywords() -> &[FitsKeyword]`, `properties() -> &[XisfProperty]`, `wcs_model() -> Result<WcsModel, String>`, `linear_wcs() -> Option<LinearWcs>`, `storage() -> PanelStorage`, `format_name() -> &'static str`, `as_xisf() -> Option<&XisfPanel>`; `astrometry::describe_unsolved(props: &[XisfProperty]) -> String` (pub).

- [ ] **Step 1: Move `describe_unsolved`**

Cut `fn describe_unsolved` (and only it) from `analyze.rs` into `astrometry/mod.rs` as `pub fn describe_unsolved(props: &[XisfProperty]) -> String` with the doc comment "Explain why a panel's XISF properties yield no [`WcsModel`], for per-file error listings." Adjust the `use` of `has_standard_block`/`parse_standard` (they live in `standard`). In `analyze.rs` add `use crate::astrometry::describe_unsolved;`. Run `cargo test -p mmm-core --lib analyze` — passes.

- [ ] **Step 2: Write the failing `InputPanel` tests in `formats/mod.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{SynthWcs, write_fits, write_xisf, write_xisf_solved};
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-input-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn kw(name: &str, value: &str) -> FitsKeyword {
        FitsKeyword { name: name.into(), value: value.into(), comment: String::new() }
    }

    #[test]
    fn opens_both_formats_by_magic_not_extension() {
        let dir = tmpdir("magic");
        let planes = vec![0.5f32; 6 * 4];
        let x = dir.join("a.fits"); // XISF bytes behind a .fits name
        write_xisf(&x, 6, 4, 1, &planes).unwrap();
        let f = dir.join("b.xisf"); // FITS bytes behind a .xisf name
        write_fits(&f, 6, 4, 1, &planes, -32, &[kw("OBJECT", "'x'")]).unwrap();
        let px = InputPanel::open(&x).unwrap();
        let pf = InputPanel::open(&f).unwrap();
        assert_eq!(px.format_name(), "XISF");
        assert_eq!(pf.format_name(), "FITS");
        assert_eq!((pf.width(), pf.height(), pf.channels()), (6, 4, 1));
        assert!(pf.properties().is_empty());
        assert_eq!(card_string(pf.fits_keywords(), "OBJECT").as_deref(), Some("x"));
        assert_eq!(pf.storage(), crate::panel_reader::PanelStorage::FullCanvasFits);
        assert_eq!(px.storage(), crate::panel_reader::PanelStorage::FullCanvasXisf);
        assert!(px.as_xisf().is_some() && pf.as_xisf().is_none());
        std::fs::write(dir.join("junk"), b"hello world, not an image at all").unwrap();
        let e = InputPanel::open(&dir.join("junk")).unwrap_err().to_string();
        assert!(e.contains("SIMPLE"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wcs_model_and_linear_wcs_for_both_formats() {
        let dir = tmpdir("wcs");
        let (w, h) = (200u64, 100u64);
        let planes = vec![0.5f32; (w * h) as usize];
        let wcs = SynthWcs { crval: [80.0, -5.0], refimg: [100.0, 50.0], cd: [[-1e-3, 0.0], [0.0, 1e-3]] };
        let x = dir.join("s.xisf");
        write_xisf_solved(&x, w, h, 1, &planes, &wcs).unwrap();
        let px = InputPanel::open(&x).unwrap();
        let mx = px.wcs_model().unwrap();
        let lx = px.linear_wcs().unwrap();
        assert_eq!(lx.crpix, [100.5, 50.5]);

        // The same solution as a bottom-up FITS: file CRPIX2 = H + 1 − 50.5.
        let f = dir.join("s.fits");
        let cards = vec![
            kw("CTYPE1", "'RA---TAN'"), kw("CTYPE2", "'DEC--TAN'"),
            kw("CRVAL1", "80.0"), kw("CRVAL2", "-5.0"),
            kw("CRPIX1", "100.5"), kw("CRPIX2", &format!("{}", h as f64 + 1.0 - 50.5)),
            kw("CD1_1", "-1.0E-3"), kw("CD1_2", "0"), kw("CD2_1", "0"), kw("CD2_2", "-1.0E-3"),
        ];
        write_fits(&f, w, h, 1, &planes, -32, &cards).unwrap();
        let pf = InputPanel::open(&f).unwrap();
        let mf = pf.wcs_model().unwrap();
        let lf = pf.linear_wcs().unwrap();
        assert_eq!(lf, lx, "reflected FITS WCS equals the XISF one");
        for (px_, py_) in [(0.0, 0.0), (150.0, 20.0), (199.0, 99.0)] {
            let a = mx.pixel_to_sky(px_, py_);
            let b = mf.pixel_to_sky(px_, py_);
            assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
        }

        // Unsolved files explain themselves.
        let u = dir.join("u.fits");
        write_fits(&u, 4, 4, 1, &vec![0.5; 16], -32, &[]).unwrap();
        let e = InputPanel::open(&u).unwrap().wcs_model().unwrap_err();
        assert!(e.contains("CTYPE1"), "{e}");
        let ux = dir.join("u.xisf");
        write_xisf(&ux, 4, 4, 1, &vec![0.5; 16]).unwrap();
        let e = InputPanel::open(&ux).unwrap().wcs_model().unwrap_err();
        assert!(e.contains("no astrometric solution"), "{e}");
        assert!(InputPanel::open(&u).unwrap().linear_wcs().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
```

- [ ] **Step 3: Implement `InputPanel`**

In `formats/mod.rs`:

```rust
use std::path::Path;

use crate::astrometry::{LinearWcs, WcsModel, describe_unsolved, fits_wcs, wcs_from_properties};
use crate::panel_reader::PanelStorage;
use crate::{Error, Result};
use fits::FitsPanel;
use xisf::XisfPanel;

/// A panel file of either supported format, opened for metadata access.
/// Pixel rows are read through [`crate::panel_reader::PanelReader`]
/// (`open_file`, or `open` with [`Self::storage`]); the XISF variant also
/// exposes its zero-copy planes via [`Self::as_xisf`].
pub enum InputPanel {
    /// A monolithic XISF file.
    Xisf(XisfPanel),
    /// A FITS primary-HDU image.
    Fits(FitsPanel),
}

impl InputPanel {
    /// Open a panel, detecting the format from its magic bytes (`XISF0100`
    /// vs a FITS `SIMPLE` card) — never from the file extension.
    pub fn open(path: &Path) -> Result<InputPanel> {
        let mut magic = [0u8; 8];
        {
            use std::io::Read;
            let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
            let _ = f.read(&mut magic).map_err(|e| Error::io(path, e))?;
        }
        if &magic == b"XISF0100" {
            Ok(InputPanel::Xisf(XisfPanel::open(path)?))
        } else {
            Ok(InputPanel::Fits(FitsPanel::open(path)?))
        }
    }

    /// The file this panel was opened from.
    pub fn path(&self) -> &Path {
        match self { Self::Xisf(p) => p.path(), Self::Fits(p) => p.path() }
    }
    /// Image width in pixels.
    pub fn width(&self) -> u64 {
        match self { Self::Xisf(p) => p.width(), Self::Fits(p) => p.width() }
    }
    /// Image height in pixels.
    pub fn height(&self) -> u64 {
        match self { Self::Xisf(p) => p.height(), Self::Fits(p) => p.height() }
    }
    /// Channel count.
    pub fn channels(&self) -> u64 {
        match self { Self::Xisf(p) => p.channels(), Self::Fits(p) => p.channels() }
    }
    /// Header FITS keywords (XISF `<FITSKeyword>` elements, or every FITS card).
    pub fn fits_keywords(&self) -> &[FitsKeyword] {
        match self { Self::Xisf(p) => &p.header().fits_keywords, Self::Fits(p) => &p.header().fits_keywords }
    }
    /// XISF properties; always empty for FITS.
    pub fn properties(&self) -> &[XisfProperty] {
        match self { Self::Xisf(p) => &p.header().properties, Self::Fits(_) => &[] }
    }
    /// The XISF panel, for zero-copy plane access.
    pub fn as_xisf(&self) -> Option<&XisfPanel> {
        match self { Self::Xisf(p) => Some(p), Self::Fits(_) => None }
    }
    /// `"XISF"` or `"FITS"`.
    pub fn format_name(&self) -> &'static str {
        match self { Self::Xisf(_) => "XISF", Self::Fits(_) => "FITS" }
    }
    /// The [`PanelStorage`] a full-canvas session records for this file.
    pub fn storage(&self) -> PanelStorage {
        match self { Self::Xisf(_) => PanelStorage::FullCanvasXisf, Self::Fits(_) => PanelStorage::FullCanvasFits }
    }
    /// The panel's full astrometric model (top-down image coordinates), or
    /// a human-readable reason it has none.
    pub fn wcs_model(&self) -> std::result::Result<WcsModel, String> {
        match self {
            Self::Xisf(p) => {
                let h = p.header();
                WcsModel::from_properties(&h.properties, h.width, h.height).ok_or_else(|| describe_unsolved(&h.properties))
            }
            Self::Fits(p) => {
                let h = p.header();
                fits_wcs::model_from_keywords(&h.fits_keywords, h.width, h.height, h.row_order)
            }
        }
    }
    /// The linear solution in the top-down convention, for output WCS cards.
    pub fn linear_wcs(&self) -> Option<LinearWcs> {
        match self {
            Self::Xisf(p) => wcs_from_properties(&p.header().properties),
            Self::Fits(p) => {
                let h = p.header();
                fits_wcs::linear_for_panel(&h.fits_keywords, h.height, h.row_order).ok()
            }
        }
    }
}
```

- [ ] **Step 4: Run and commit**

Run: `cargo test -p mmm-core --lib formats 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: PASS.

```bash
cargo fmt && cargo clippy -p mmm-core --all-targets 2>&1 | grep -E "^(warning|error)" | head
git add -A crates/mmm-core/src && git commit -m "feat(formats): format-agnostic InputPanel over XISF and FITS

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: Pipeline call sites (analyze, align, probe)

**Files:**
- Modify: `crates/mmm-core/src/analyze.rs` (`analyze_full` pre-passes, `analyze_solved`, `scan_panel`, `probe_panels`)
- Modify: `crates/mmm-core/src/align.rs` (`reproject_panel` takes `&InputPanel`; its tests)
- Modify: `crates/mmm-core/src/lib.rs` (API map line for `formats`)
- Test: `crates/mmm-core/tests/analyze.rs`, `crates/mmm-core/tests/probe_panels.rs` (add FITS cases)

**Interfaces:**
- Consumes: `InputPanel` (Task 6), `PanelReader::open_file` and `PanelStorage::FullCanvasFits` (Task 2), `reproject_from_reader` (existing).
- Produces: `align::reproject_panel(panel: &InputPanel, model, frame, out_dir) -> Result<AlignedPanel>`.

- [ ] **Step 1: Write failing integration tests**

Append to `crates/mmm-core/tests/probe_panels.rs` (reuse its existing helpers; see `write_solved` there for the synthetic WCS values):

```rust
#[test]
fn fits_panels_probe_like_xisf() {
    use mmm_core::formats::FitsKeyword;
    use mmm_core::synth::write_fits;
    let dir = std::env::temp_dir().join(format!("mmm-probe-fits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let kw = |n: &str, v: &str| FitsKeyword { name: n.into(), value: v.into(), comment: String::new() };
    let (w, h) = (120u64, 90u64);
    let mut paths = Vec::new();
    for k in 0..2u64 {
        let planes = vec![0.4f32; (w * h) as usize];
        let cards = vec![
            kw("CTYPE1", "'RA---TAN'"), kw("CTYPE2", "'DEC--TAN'"),
            kw("CRVAL1", &format!("{}", 80.0 + 0.05 * k as f64)), kw("CRVAL2", "-5.0"),
            kw("CRPIX1", "60.5"), kw("CRPIX2", "45.5"),
            kw("CD1_1", "-1.0E-3"), kw("CD1_2", "0"), kw("CD2_1", "0"), kw("CD2_2", "1.0E-3"),
        ];
        let p = dir.join(format!("p{k}.fits"));
        write_fits(&p, w, h, 1, &planes, 16, &cards).unwrap();
        paths.push(p);
    }
    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert_eq!(reply.panels.len(), 2);
    assert_eq!((reply.panels[0].width, reply.panels[0].height, reply.panels[0].channels), (w, h, 1));
    let frame = reply.frame.expect("solved FITS panels yield a frame");
    assert!(frame[0] >= w && frame[1] >= h && frame[2] == 1);
    // Aligned select never reports a frame; a mono/colour mix is refused.
    assert!(probe_panels(&paths, InputSelect::Aligned).unwrap().frame.is_none());
    let rgb = dir.join("rgb.fits");
    write_fits(&rgb, w, h, 3, &vec![0.4f32; (w * h * 3) as usize], -32, &[]).unwrap();
    let e = probe_panels(&[paths[0].clone(), rgb], InputSelect::Auto).unwrap_err().to_string();
    assert!(e.contains("channel"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}
```

Append to `crates/mmm-core/tests/analyze.rs` (check its imports; it already uses `analyze_input`/`InputSelect`):

```rust
#[test]
fn aligned_fits_panels_analyze_like_xisf() {
    use mmm_core::analyze::{InputSelect, analyze_input};
    use mmm_core::panel_reader::PanelStorage;
    use mmm_core::session::InputKind;
    use mmm_core::synth::write_fits;
    let dir = std::env::temp_dir().join(format!("mmm-analyze-fits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (64u64, 48u64);
    // Two overlapping windows on a shared canvas, zeros elsewhere.
    let mut paths = Vec::new();
    for (k, x0) in [(0u64, 0u64), (1, 24)] {
        let mut planes = vec![0f32; (w * h) as usize];
        for y in 8..40u64 {
            for x in x0..x0 + 40 {
                planes[(y * w + x) as usize] = 0.2 + 0.001 * (x + y) as f32 + 0.01 * k as f32;
            }
        }
        let p = dir.join(format!("a{k}.fits"));
        write_fits(&p, w, h, 1, &planes, -32, &[]).unwrap();
        paths.push(p);
    }
    let session = analyze_input(&paths, &dir.join("s.mmm-session"), Some(0), InputSelect::Auto).unwrap();
    assert_eq!(session.input, InputKind::Aligned);
    assert_eq!(session.canvas, (w, h, 1));
    assert_eq!(session.panels[0].storage, PanelStorage::FullCanvasFits);
    assert_eq!(session.panels[0].bbox, [0, 8, 40, 40], "bbox in top-down canvas rows");
    assert_eq!(session.panels[1].bbox, [24, 8, 64, 40]);
    // The session reopens and reads the same rows back.
    let reopened = mmm_core::session::Session::open(&dir.join("s.mmm-session")).unwrap();
    let r = mmm_core::panel_reader::PanelReader::open(&reopened.panels[1], reopened.canvas).unwrap();
    let (x0, row) = r.row(0, 20).unwrap();
    assert_eq!(x0, 0);
    assert_eq!(row[23], 0.0);
    assert!((row[24] - (0.2 + 0.001 * 44.0 + 0.01)).abs() < 1e-6);
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p mmm-core --test probe_panels --test analyze 2>&1 | grep -E "FAILED|panicked|error" | head`
Expected: the FITS probe test fails inside `XisfPanel::open` (format error), the analyze test likewise.

- [ ] **Step 3: Switch the call sites**

In `analyze.rs`:
- Replace `use crate::formats::xisf::XisfPanel;` with `use crate::formats::InputPanel;`.
- `analyze_full` channel pre-pass and Auto geometry pass: `XisfPanel::open(p)` → `InputPanel::open(p)`.
- `analyze_solved`: `Vec<(InputPanel, WcsModel)>`; the match becomes
  ```rust
  match InputPanel::open(path) {
      Ok(p) => match p.wcs_model() {
          Ok(m) => panels.push((p, m)),
          Err(reason) => errors.push(format!("{}: {reason}", path.display())),
      },
      Err(e) => errors.push(e.to_string()),
  }
  ```
- `scan_panel`: `let panel = PanelReader::open_file(path)?;` and `storage: InputPanel::open(path)?.storage()` — better: open the `InputPanel` once, take `storage()`, then `PanelReader::open_file`. (Opening twice is fine: both are header-only mmaps.)
- `probe_panels`: `InputPanel::open(path)?` with `properties: x.properties().to_vec()`.

In `align.rs`:
```rust
pub fn reproject_panel(panel: &InputPanel, model: &WcsModel, frame: &MosaicFrame, out_dir: &Path) -> Result<AlignedPanel> {
    match panel.as_xisf() {
        Some(x) => {
            let (sw, sh) = (x.width() as usize, x.height() as usize);
            let nch = x.channels() as usize;
            let planes: Vec<&[f32]> = (0..nch as u64).map(|c| x.channel(c)).collect();
            reproject_core(&planes, sw, sh, nch, x.path(), model, frame, out_dir)
        }
        None => {
            let reader = PanelReader::open_file(panel.path())?;
            reproject_from_reader(&reader, model, frame, out_dir)
        }
    }
}
```
Update the doc comment ("XISF panels gather zero-copy planes; other formats stream through `reproject_from_reader`"). In its tests, `write_pattern_panel` returns `InputPanel::open(&path).unwrap()` and the real-data tests wrap `XisfPanel::open` in `InputPanel::Xisf(..)`. Check `reproject_from_reader`'s error path for a missing source path: it currently reports "IPC panel"; give it the reader's path when one exists — acceptable to leave as-is if the message is still actionable, but note it in the commit message.

`lib.rs` API map: "- [`formats`] + [`panel_reader`] — input access: the XISF and FITS readers behind the format-agnostic [`formats::InputPanel`], and the storage-agnostic row reader the pipeline consumes."

- [ ] **Step 4: Run the whole workspace**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|panicked" | head -20`
Expected: all PASS (including `mmm-ipc-worker` end-to-end, which drives the Files path through these functions).

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | head; cargo doc -p mmm-core --no-deps 2>&1 | grep -E "^warning" | head
git add -A crates && git commit -m "feat(analyze): accept FITS panels in every pipeline entry point

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: CLI (`mmm info`, `blend` passthrough, help text)

**Files:**
- Modify: `crates/mmm/src/main.rs` (`Command` help strings, `geometry_card`, `blend_cmd`, `info_panel`)

**Interfaces:**
- Consumes: `InputPanel` (Task 6), `PanelReader::open_file` (Task 2).

- [ ] **Step 1: Help text and `geometry_card`**

In the `Command` enum change both `/// Input panel files (XISF)` doc lines to `/// Input panel files (XISF or FITS)`. In `geometry_card`, add the SIP prefixes to the `starts_with` list: `"A_", "B_", "AP_", "BP_"` (this also drops `A_ORDER`/`B_ORDER`/`AP_ORDER`/`BP_ORDER`).

- [ ] **Step 2: `blend_cmd` passthrough via `InputPanel`**

Replace `use mmm_core::formats::xisf::XisfPanel;` with `use mmm_core::formats::InputPanel;`, and:

```rust
    let ref_panel = InputPanel::open(p0.source.as_ref().unwrap_or(&p0.path))?;
    let mut keywords = keywords_for_output(ref_panel.fits_keywords(), crop);
    ...
            None => ref_panel.linear_wcs(),
```
(the `wcs_from_properties` import goes away). Note the aligned-FITS case: a bottom-up FITS panel's own CRPIX/CD cards would have passed through unreflected, so for aligned sessions whose reference panel is FITS, drop geometry cards like the solved path does and rely on the fresh `wcs_cards` from `linear_wcs()`:

```rust
    if session.frame.is_some() || ref_panel.format_name() == "FITS" {
        keywords.retain(|kw| !geometry_card(&kw.name));
    }
```

- [ ] **Step 3: `info_panel` over both formats**

```rust
fn info_panel(path: &std::path::Path, stats: bool) -> anyhow::Result<()> {
    use mmm_core::formats::InputPanel;
    use mmm_core::panel_reader::PanelReader;

    let panel = InputPanel::open(path)?;
    println!("{}", path.display());
    match &panel {
        InputPanel::Xisf(x) => {
            let h = x.header();
            println!("  XISF geometry: {}x{} x{}ch  {:?}  data @ {} ({} bytes)", h.width, h.height, h.channels, h.sample_format, h.data_offset, h.data_size);
        }
        InputPanel::Fits(f) => {
            let h = f.header();
            println!("  FITS geometry: {}x{} x{}ch  BITPIX {}  rows {:?}  data @ {}", h.width, h.height, h.channels, h.bitpix, h.row_order, h.data_offset);
        }
    }
    for kw in panel.fits_keywords() {
        if matches!(kw.name.as_str(), "OBJECT" | "RA" | "DEC" | "INSTRUME" | "BAYERPAT" | "EXPTIME") {
            println!("  {:8} = {}", kw.name, kw.value);
        }
    }
    match panel.wcs_model() {
        Ok(m) => println!("  wcs: {} ({})", if m.is_spline() { "with distortion model" } else { "linear" }, m.linear.ctype[0]),
        Err(reason) => println!("  wcs: none ({reason})"),
    }
    if stats {
        let reader = PanelReader::open_file(path)?;
        reader.advise_sequential();
        let t0 = std::time::Instant::now();
        let (_, h, ch) = reader.canvas();
        for c in 0..ch {
            let (mut min, mut max, mut zeros, mut sum, mut n) = (f32::INFINITY, f32::NEG_INFINITY, 0u64, 0f64, 0u64);
            for y in 0..h {
                let (_, row) = reader.row(c, y).expect("full-canvas rows");
                n += row.len() as u64;
                for &v in row {
                    if v == 0.0 { zeros += 1; } else { min = min.min(v); max = max.max(v); sum += v as f64; }
                }
            }
            let nonzero = n - zeros;
            println!("  ch{c}: nonzero {:.1}%  min {:.6}  max {:.6}  mean {:.6}", 100.0 * nonzero as f64 / n as f64, min, max, if nonzero > 0 { sum / nonzero as f64 } else { 0.0 });
        }
        println!("  stats scan: {:.2}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}
```

- [ ] **Step 4: Build, smoke, commit**

```bash
cargo build --release 2>&1 | tail -2 && cargo test -p mmm 2>&1 | grep -E "^test result|FAILED"
# quick synthetic smoke: write a FITS via a tiny Rust test? Use the fixture writer through cargo test above; then:
target/release/mmm info test_data/orion_mosaic_raw_panels_195/*PANEL-4_*.xisf | head -4   # still works (manual, skip if test_data absent)
cargo fmt && cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | head
git add -A crates/mmm && git commit -m "feat(cli): FITS panels in info/blend; SIP cards never pass through

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: Synthetic FITS+SIP end-to-end test

**Files:**
- Modify: `crates/mmm-core/src/synth.rs` (`write_fits_solved`, `SynthSip`)
- Modify: `crates/mmm-core/tests/e2e.rs` (FITS variant of `solved_input_pipeline_recovers_ground_truth`, plus a mixed set)

**Interfaces:**
- Consumes: `write_fits` (Task 1), `fits_wcs::model_from_keywords` (Task 4), `SynthWcs` (existing).
- Produces: `synth::SynthSip { order: usize, a: Vec<(usize, usize, f64)>, b: Vec<(usize, usize, f64)>, ap: Vec<(usize, usize, f64)>, bp: Vec<(usize, usize, f64)> }` (file-frame coefficients) and `synth::write_fits_solved(path, w, h, ch, planes, wcs: &SynthWcs, sip: Option<&SynthSip>, bitpix: i32) -> Result<()>`; `synth::solved_fits_cards(wcs, h, sip) -> Vec<FitsKeyword>` (the cards it writes, so tests can build the same model the pipeline will see).

- [ ] **Step 1: Implement the writer**

In `synth.rs`:

```rust
/// SIP polynomial terms for a synthetic FITS panel, in the file's own pixel
/// frame (`(p, q, coefficient)` triples; `p + q ≤ order`).
#[derive(Debug, Clone, Default)]
pub struct SynthSip {
    /// Polynomial order (`A_ORDER`/`B_ORDER`, and `AP_`/`BP_` when given).
    pub order: usize,
    /// Forward `A_p_q` terms.
    pub a: Vec<(usize, usize, f64)>,
    /// Forward `B_p_q` terms.
    pub b: Vec<(usize, usize, f64)>,
    /// Inverse `AP_p_q` terms (empty = not written).
    pub ap: Vec<(usize, usize, f64)>,
    /// Inverse `BP_p_q` terms.
    pub bp: Vec<(usize, usize, f64)>,
}

/// The WCS (+SIP) cards `write_fits_solved` puts in a bottom-up file for a
/// solution given in top-down image coordinates: CRPIX from `refimg`
/// (+0.5), CRPIX2 and the second CD column reflected over `h` rows.
pub fn solved_fits_cards(wcs: &SynthWcs, h: u64, sip: Option<&SynthSip>) -> Vec<FitsKeyword> {
    let kw = |n: &str, v: String| FitsKeyword { name: n.into(), value: v, comment: String::new() };
    let suffix = if sip.is_some() { "-SIP" } else { "" };
    let mut cards = vec![
        kw("CTYPE1", format!("'RA---TAN{suffix}'")),
        kw("CTYPE2", format!("'DEC--TAN{suffix}'")),
        kw("CRVAL1", format!("{:.12}", wcs.crval[0])),
        kw("CRVAL2", format!("{:.12}", wcs.crval[1])),
        kw("CRPIX1", format!("{:.6}", wcs.refimg[0] + 0.5)),
        kw("CRPIX2", format!("{:.6}", h as f64 + 1.0 - (wcs.refimg[1] + 0.5))),
        kw("CD1_1", format!("{:e}", wcs.cd[0][0])),
        kw("CD1_2", format!("{:e}", -wcs.cd[0][1])),
        kw("CD2_1", format!("{:e}", wcs.cd[1][0])),
        kw("CD2_2", format!("{:e}", -wcs.cd[1][1])),
        kw("RADESYS", "'ICRS'".into()),
    ];
    if let Some(s) = sip {
        cards.push(kw("A_ORDER", s.order.to_string()));
        cards.push(kw("B_ORDER", s.order.to_string()));
        for (p, q, c) in &s.a { cards.push(kw(&format!("A_{p}_{q}"), format!("{c:e}"))); }
        for (p, q, c) in &s.b { cards.push(kw(&format!("B_{p}_{q}"), format!("{c:e}"))); }
        if !s.ap.is_empty() || !s.bp.is_empty() {
            cards.push(kw("AP_ORDER", s.order.to_string()));
            cards.push(kw("BP_ORDER", s.order.to_string()));
            for (p, q, c) in &s.ap { cards.push(kw(&format!("AP_{p}_{q}"), format!("{c:e}"))); }
            for (p, q, c) in &s.bp { cards.push(kw(&format!("BP_{p}_{q}"), format!("{c:e}"))); }
        }
    }
    cards
}

/// [`write_fits`] plus a plate solution: a bottom-up FITS carrying the
/// linear WCS of `wcs` (reflected into the file frame) and optional SIP
/// terms. The FITS counterpart of [`write_xisf_solved`].
pub fn write_fits_solved(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32], wcs: &SynthWcs, sip: Option<&SynthSip>, bitpix: i32) -> Result<()> {
    write_fits(path, w, h, ch, planes, bitpix, &solved_fits_cards(wcs, h, sip))
}
```

Add a unit test in `synth.rs`: write a solved FITS without SIP, open with `InputPanel`, and assert `linear_wcs().unwrap().crpix == [refimg[0] + 0.5, refimg[1] + 0.5]` and `cd == wcs.cd` (the reflection cancels).

- [ ] **Step 2: Add the e2e tests**

In `crates/mmm-core/tests/e2e.rs`, generalize `write_solved_panels` with an enum parameter:

```rust
#[derive(Clone, Copy)]
enum SolvedFormat { Xisf, Fits { sip: bool } }
```

The loop body renders pixel `(i, j)` (top-down array index) from a `WcsModel` built exactly as the pipeline will build it:

```rust
        let model: WcsModel = match format {
            SolvedFormat::Xisf => WcsModel::from_properties(&[], 0, 0).unwrap_or_else(|| /* keep the existing lin-based path */ unreachable!()),
            ...
        };
```

Simpler and exact: keep the existing XISF path untouched, and for FITS build the cards with `solved_fits_cards(&SynthWcs { crval, refimg, cd }, spec.h, sip.as_ref())` and the model with `mmm_core::astrometry::fits_wcs::model_from_keywords(&cards, spec.w, spec.h, RowOrder::BottomUp).unwrap()`, then render with `model.pixel_to_sky(i as f64, j as f64)` (image coordinates of the pixel's top-left corner... no: the existing XISF path samples pixel centers at solution `(i + 0.5, j + 0.5)` in FITS-style 1-based-from-image terms; in `WcsModel` image coordinates the center of array pixel `(i, j)` is `(i as f64 + 0.5 − 0.5, …)` = `(i as f64, j as f64)`? No — `WcsModel` pixel `k` spans `[k, k+1]` with center `k + 0.5`, so use `model.pixel_to_sky(i as f64 + 0.5, j as f64 + 0.5)`). The XISF branch already does the equivalent through `lin.pixel_to_sky(i + 1, j + 1)`; assert once in the test that both formulas agree for the SIP-free FITS case (difference < 1e-12°).

SIP for the FITS variant (file frame, modest, order 2, with an approximate inverse — the Newton polish makes the inverse exact regardless):

```rust
fn synth_sip(k: usize) -> SynthSip {
    let s = 1.0 + 0.3 * k as f64;
    SynthSip {
        order: 2,
        a: vec![(2, 0, 1.5e-6 * s), (1, 1, -2.0e-6 * s), (0, 2, 1.0e-6 * s)],
        b: vec![(2, 0, -1.0e-6 * s), (1, 1, 1.2e-6 * s), (0, 2, 2.0e-6 * s)],
        ap: vec![(2, 0, -1.5e-6 * s), (1, 1, 2.0e-6 * s), (0, 2, -1.0e-6 * s)],
        bp: vec![(2, 0, 1.0e-6 * s), (1, 1, -1.2e-6 * s), (0, 2, -2.0e-6 * s)],
    }
}
```
On a ~210×170 panel these move corners by ~0.03 px — too small to prove anything. Scale by 1e2 for the test panels (coefficients ~1e-4 → ~2–3 px at the corners) so a SIP bug fails the RMSE bound: use `1.5e-4 * s` etc.

Then refactor `solved_input_pipeline_recovers_ground_truth` body into `fn run_solved_pipeline(tag: &str, format: SolvedFormat)` and add:

```rust
#[test]
fn solved_input_pipeline_recovers_ground_truth() { run_solved_pipeline("solved", SolvedFormat::Xisf) }
#[test]
fn solved_fits_sip_pipeline_recovers_ground_truth() { run_solved_pipeline("solved-fits-sip", SolvedFormat::Fits { sip: true }) }
#[test]
fn solved_fits_linear_pipeline_recovers_ground_truth() { run_solved_pipeline("solved-fits-lin", SolvedFormat::Fits { sip: false }) }
#[test]
fn mixed_xisf_and_fits_panels_blend_together() {
    // Same scene, panels 0/2 as XISF, 1/3 as FITS+SIP: run_solved_pipeline with a per-panel format closure
}
```
Implement the mixed case by making `write_solved_panels` take `format: &dyn Fn(usize) -> SolvedFormat`. Assertions are the existing ones (frame persisted, RMSE < bound per channel, no NaN), plus for FITS: `session.panels[k].storage` is `CroppedCache` and `source` ends with `.fits`.

- [ ] **Step 3: Run**

Run: `cargo test -p mmm-core --test e2e solved 2>&1 | grep -E "^test |RMSE|test result"`
Expected: all four PASS with RMSE below the bound. If the SIP variant's RMSE is above the bound while the linear one passes, the render and the model disagree on the pixel-center convention — check the `+0.5` in the render call first.

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | head
git add -A crates && git commit -m "test(e2e): solved pipeline over FITS panels with SIP, and mixed XISF/FITS sets

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: Documentation and PixInsight text

**Files:**
- Modify: `docs/DESIGN.md`, `README.md`, `integration/pixinsight/PROTOCOL.md` (§11), `integration/pixinsight/module/MmmInterface.cpp` (tooltip), `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`

- [ ] **Step 1: DESIGN.md**

Change the phase-2 line "Deferred to phase 3: FITS *input*, compressed XISF ingest, …" to "Deferred to phase 3: compressed XISF ingest, … (FITS input landed 2026-09-26, below)". Add before "## Performance notes":

```markdown
## FITS input (2026-09-26)

FITS panels are a peer input format everywhere (CLI and the PixInsight
Files path); spec:
[2026-09-26-fits-input-sip-design.md](superpowers/specs/2026-09-26-fits-input-sip-design.md).

- **Reader** (`formats/fits.rs`): primary HDU, NAXIS 2/3 planar, BITPIX
  8/16/32/−32/−64; rows decoded on demand into the per-thread band cache
  (`band_cache.rs`, shared with the IPC backing). Integer data normalized to
  [0, 1] by the type range after BZERO/BSCALE; NaN/Inf/negative/BLANK → 0.
  Extension HDUs, fpack, BITPIX 64 refused by name.
- **Orientation**: `ROWORDER = 'TOP-DOWN'` files read verbatim; everything
  else (standard FITS) is flipped on read and the WCS reflected into the
  top-down frame (`LinearWcs::reflect_rows`). PixInsight-authored bottom-up
  FITS is unverified (PI may interpret cards in display space).
- **WCS** (`astrometry/fits_wcs.rs`): TAN / TAN-SIP only; CD, PC+CDELT, or
  CDELT+CROTA2. TPV/PV refused. **SIP** (`astrometry/sip.rs`): forward
  polynomials; AP/BP only seed a Newton inverse (exact either way);
  validated like the standard solution; sampled onto the same 16 px grids.
  Verified against astropy on four synthetic headers to < 1e-9° forward /
  1e-4 px inverse (`tests/fixtures/sip_oracle.json`).
- **Output**: SIP cards never pass through; aligned FITS sessions emit
  fresh linear cards from the reflected WCS.
- **Real data**: (filled in by Task 11)
```

- [ ] **Step 2: README, PROTOCOL, tooltip, module doc**

- README line 6: "`mmm` takes linear XISF or FITS panels — …"; the `analyze` example gains a comment `# .xisf and .fits panels can be mixed`; the "WCS in the output" bullet: "from the XISF astrometric solution or the FITS WCS (aligned input)".
- PROTOCOL.md §11 panel probe paragraph: after "`paths` order" add "Paths may be XISF or FITS files (detected by content, not extension); FITS panels carry their plate solution as WCS/SIP cards, which the worker reads itself."
- `MmmInterface.cpp`: tooltip → `"<p>Add image files (XISF or FITS) as mosaic panels.</p>"`.
- `MegaMergeMosaic.html`: in the Files-mode paragraph add one sentence: "Panels may be XISF or FITS files; FITS panels are aligned from their WCS solution, including SIP distortion terms."

- [ ] **Step 3: Commit**

```bash
git add -A docs README.md integration && git commit -m "docs: FITS input across CLI and PixInsight Files path

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: Real-data smoke test via nova.astrometry.net

**Files:**
- Create: `scripts/nova_solve_panels.py`
- Create: `scripts/compare_mosaics.py`
- Modify: `docs/DESIGN.md` ("Real data" bullet)
- Output (gitignored): `test_data/orion_mosaic_fits/PANEL-{3,4,7,8}.fits`, `test_data/orion_fits.mmm-session/`

**Interfaces:**
- Consumes: the release CLI (Task 8). API key at `~/.config/astrometry/apikey` (mode 600; never print it).

- [ ] **Step 1: Write `scripts/nova_solve_panels.py`**

```python
#!/usr/bin/env python3
"""Convert raw Orion XISF panels to FITS solved by nova.astrometry.net.

For each input XISF: build a 16-bit mono luminance FITS (bottom-up, no
solution), upload it to nova (private, tweak_order 3, scale/position hints
from the XISF header), poll, download the wcs.fits header, and write an RGB
float32 bottom-up FITS of the ORIGINAL planes with nova's WCS + SIP cards.

Usage: python3 scripts/nova_solve_panels.py OUT_DIR PANEL.xisf [...]
Requires: numpy, astropy, requests; the key in ~/.config/astrometry/apikey.
"""
import json, os, re, sys, time
import numpy as np
import requests
from astropy.io import fits

API = "https://nova.astrometry.net/api"


def read_xisf(path):
    with open(path, "rb") as fh:
        head = fh.read(16)
        assert head[:8] == b"XISF0100", path
        hlen = int.from_bytes(head[8:12], "little")
        xml = fh.read(hlen).decode("utf-8", "replace")
    m = re.search(r'<Image [^>]*geometry="(\d+):(\d+):(\d+)"[^>]*location="attachment:(\d+):(\d+)"', xml)
    if not m:
        m2 = re.search(r'<Image ([^>]*)>', xml).group(1)
        g = re.search(r'geometry="(\d+):(\d+):(\d+)"', m2)
        l = re.search(r'location="attachment:(\d+):(\d+)"', m2)
        w, h, ch, off, size = int(g[1]), int(g[2]), int(g[3]), int(l[1]), int(l[2])
    else:
        w, h, ch, off, size = (int(x) for x in m.groups())
    assert 'sampleFormat="Float32"' in xml
    data = np.memmap(path, dtype="<f4", mode="r", offset=off, shape=(ch, h, w))
    kw = {k: v for k, v in re.findall(r'<FITSKeyword name="([^"]+)" value="([^"]*)"', xml)}
    return np.array(data), kw


def api(session, path, **kw):
    r = requests.post(f"{API}/{path}", data={"request-json": json.dumps({"session": session, **kw})}, timeout=120)
    r.raise_for_status()
    return r.json()


def main():
    out_dir, inputs = sys.argv[1], sys.argv[2:]
    os.makedirs(out_dir, exist_ok=True)
    key = open(os.path.expanduser("~/.config/astrometry/apikey")).read().strip()
    r = requests.post(f"{API}/login", data={"request-json": json.dumps({"apikey": key})}, timeout=60).json()
    assert r.get("status") == "success", "login failed"
    session = r["session"]
    jobs = {}
    for path in inputs:
        planes, kw = read_xisf(path)
        name = re.search(r"PANEL-(\d+)", path).group(1)
        lum = planes.mean(axis=0)
        lum16 = np.clip(lum * 65535.0, 0, 65535).astype(np.uint16)
        up = os.path.join(out_dir, f"upload_{name}.fits")
        fits.PrimaryHDU(np.flipud(lum16)).writeto(up, overwrite=True)  # bottom-up storage
        args = {"publicly_visible": "n", "allow_modifications": "d", "allow_commercial_use": "n", "tweak_order": 3,
                "scale_units": "arcsecperpix", "scale_type": "ev", "scale_est": 1.6, "scale_err": 25}
        if "RA" in kw and "DEC" in kw:
            args.update(center_ra=float(kw["RA"]), center_dec=float(kw["DEC"]), radius=5.0)
        with open(up, "rb") as fh:
            r = requests.post(f"{API}/upload", data={"request-json": json.dumps({"session": session, **args})},
                              files={"file": (os.path.basename(up), fh, "application/octet-stream")}, timeout=600).json()
        assert r.get("status") == "success", r
        jobs[name] = {"subid": r["subid"], "planes": planes, "kw": kw, "job": None}
        print(f"panel {name}: submitted {r['subid']}", flush=True)
        time.sleep(2)
    pending = set(jobs)
    while pending:
        time.sleep(15)
        for name in list(pending):
            j = jobs[name]
            if j["job"] is None:
                s = requests.get(f"{API}/submissions/{j['subid']}", timeout=60).json()
                if s.get("jobs") and s["jobs"][0]:
                    j["job"] = s["jobs"][0]
                continue
            st = requests.get(f"{API}/jobs/{j['job']}", timeout=60).json().get("status")
            if st == "success":
                wcs = requests.get(f"https://nova.astrometry.net/wcs_file/{j['job']}", timeout=120).content
                wcs_path = os.path.join(out_dir, f"wcs_{name}.fits")
                open(wcs_path, "wb").write(wcs)
                hdr = fits.getheader(wcs_path)
                out = fits.PrimaryHDU(np.flip(j["planes"], axis=1))  # (ch, h, w) → bottom-up
                for c in hdr.cards:
                    if c.keyword in ("SIMPLE", "BITPIX", "EXTEND", "END", "COMMENT", "HISTORY") or c.keyword.startswith("NAXIS"):
                        continue
                    out.header[c.keyword] = (c.value, c.comment)
                for k in ("OBJECT", "EXPTIME", "INSTRUME", "TELESCOP", "FOCALLEN"):
                    if k in j["kw"]:
                        out.header[k] = j["kw"][k].strip("'")
                out.writeto(os.path.join(out_dir, f"PANEL-{name}.fits"), overwrite=True)
                print(f"panel {name}: solved, CTYPE1={hdr['CTYPE1']} A_ORDER={hdr.get('A_ORDER')}", flush=True)
                pending.discard(name)
            elif st == "failure":
                print(f"panel {name}: nova failed to solve", flush=True)
                pending.discard(name)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Run it on panels 3, 4, 7, 8**

```bash
cd /home/dpaull/dev/mega-merge-mosaic && python3 -c "import requests" || pip install --user requests
python3 scripts/nova_solve_panels.py test_data/orion_mosaic_fits test_data/orion_mosaic_raw_panels_195/*PANEL-{3,4,7,8}_*.xisf
target/release/mmm info test_data/orion_mosaic_fits/PANEL-4.fits
```
Expected: four `PANEL-N.fits` with `CTYPE1 = 'RA---TAN-SIP'`, `A_ORDER = 3`; `mmm info` prints "FITS geometry: 4880x3235 x3ch BITPIX -32 rows BottomUp" and "wcs: with distortion model (RA---TAN)". Typical nova turnaround is 1–5 minutes per image. If a solve fails, retry that panel once with `scale_err: 50` and no position hint.

- [ ] **Step 3: Run both pipelines and compare**

```bash
target/release/mmm analyze test_data/orion_mosaic_fits/PANEL-*.fits --session test_data/orion_fits.mmm-session --input solved
target/release/mmm blend --session test_data/orion_fits.mmm-session -o test_data/orion_fits.fits --png test_data/orion_fits.png
target/release/mmm analyze test_data/orion_mosaic_raw_panels_195/*PANEL-{3,4,7,8}_*.xisf --session test_data/orion_xisf4.mmm-session --input solved
target/release/mmm blend --session test_data/orion_xisf4.mmm-session -o test_data/orion_xisf4.fits --png test_data/orion_xisf4.png
```

Write `scripts/compare_mosaics.py`: for each output, use astropy `WCS` to predict the pixel of θ¹ Ori C (RA 83.81858, Dec −5.38970), ι Ori (83.85826, −5.90990), and 42 Ori (83.78462, −4.83860); find the brightest pixel within a 25 px box of the prediction in the luminance; print the offset in pixels. Then, over the intersection of both outputs' sky footprints, resample the FITS-run mosaic onto the XISF-run grid with `scipy`-free bilinear interpolation (or `astropy.wcs` pixel mapping on a 64 px grid of points) and report the RMS relative difference and the 99th-percentile absolute difference of channel 0. Print a one-line PASS/FAIL: all three star offsets < 4 px in both outputs, no NaN, RMS relative difference < 5%.

- [ ] **Step 4: Eyeball and record**

Open `test_data/orion_fits.png` and `test_data/orion_xisf4.png` (the Read tool renders PNGs): the two must have the same orientation (M42 in the same place, no vertical mirror) and clean seams. Record the results in DESIGN.md's "Real data" bullet: panels solved, tweak order, star offsets, RMS difference, run time, and the caveat that nova's order-3 SIP is less accurate than PixInsight's spline. Commit:

```bash
git add scripts docs/DESIGN.md && git commit -m "test: real-data FITS/SIP smoke via nova.astrometry.net, results recorded

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Self-review notes (done while writing)

- Spec coverage: reader (T1), value rules (T1), orientation (T1/T3), band cache + storage variant (T2), InputPanel (T6), align dispatch (T7), fits_wcs (T3/T4), SIP + Distortion::Sip + sampler (T4), oracle (T5), pipeline sites + probe (T7), CLI/geometry_card/info (T8), synth writer + e2e + mixed (T9), docs/PixInsight text (T10), real data (T11). Non-goals need no task.
- Names used across tasks: `RowOrder`, `card_number`, `card_string`, `FitsPanel::decode_rows`, `BandCache::{new,row,error}`, `PanelStorage::FullCanvasFits`, `PanelReader::{open_file,backing_error}`, `FITS_BAND_ROWS`, `LinearWcs::reflect_rows`, `fits_wcs::{linear_from_keywords,linear_for_panel,model_from_keywords}`, `sip::SipSolution::{parse,image_to_native,native_to_image,inverse_seed,validate,linear}`, `WcsModel::with_sip`, `standard::{sample_fn,native_rect,matrix_scale,IMAGE_DELTA_PX}`, `astrometry::describe_unsolved`, `InputPanel::{open,path,width,height,channels,fits_keywords,properties,as_xisf,format_name,storage,wcs_model,linear_wcs}`, `synth::{write_fits,write_fits_solved,solved_fits_cards,SynthSip}`.
