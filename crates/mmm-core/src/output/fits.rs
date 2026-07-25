//! Streaming FITS writer implementing [`RowSink`].
//!
//! Output: single HDU, `BITPIX = -32`, `NAXIS = 3` (planar: NAXIS1 = width,
//! NAXIS2 = height, NAXIS3 = channels), big-endian samples, 2880-byte header
//! blocks and data padding. Rows are stored **top-down** (first data row =
//! top image row, `ROWORDER= 'TOP-DOWN'`) with WCS cards in the same top-down
//! convention (`astrometry::wcs_cards`). Empirically settled after three
//! rounds of PixInsight annotation testing: PI normalizes pixels via ROWORDER
//! but interprets WCS cards in display (top-down) space, and with top-down
//! storage the display space IS the data-index space, so Astropy/DS9 agree
//! too (verified against catalog star positions). Bottom-up storage with
//! reflected cards — tried in between — mirrors in PI. Bands may arrive in
//! any order — each channel slice is written at its absolute file offset.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::blend::RowSink;
use crate::formats::FitsKeyword;
use crate::{Error, Result};

/// FITS block size: headers and data are padded to multiples of this.
const BLOCK: u64 = 2880;
/// Length of one header card.
const CARD: usize = 80;

/// Streaming FITS sink. Construct with the (already filtered/shifted) keyword
/// list from [`keywords_for_output`]; dimensions arrive via `begin`.
pub struct FitsSink {
    path: PathBuf,
    file: File,
    keywords: Vec<FitsKeyword>,
    dims: (u64, u64, u64),
    data_start: u64,
    rows_written: u64,
}

impl FitsSink {
    /// Create/truncate the output file; the header is written on `begin`.
    pub fn create(path: &Path, keywords: Vec<FitsKeyword>) -> Result<Self> {
        let file = File::create(path).map_err(|e| Error::io(path, e))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            keywords,
            dims: (0, 0, 0),
            data_start: 0,
            rows_written: 0,
        })
    }
}

/// One 80-byte header card: `name` padded to 8, `= `, then the value —
/// left-justified from column 11 for quoted strings, right-justified to
/// column 30 otherwise (fixed format) — plus ` / comment` if it fits.
fn card(name: &str, value: &str, comment: &str) -> [u8; CARD] {
    let mut s = if value.starts_with('\'') {
        format!("{name:<8}= {value}")
    } else {
        format!("{name:<8}= {value:>20}")
    };
    if !comment.is_empty() {
        s.push_str(" / ");
        s.push_str(comment);
    }
    let mut out = [b' '; CARD];
    for (o, b) in out.iter_mut().zip(s.bytes()) {
        // FITS headers are restricted ASCII; anything else becomes a space.
        *o = if (0x20..=0x7e).contains(&b) { b } else { b' ' };
    }
    out
}

impl FitsSink {
    fn io(&self, e: std::io::Error) -> Error {
        Error::io(&self.path, e)
    }
}

impl RowSink for FitsSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        if w == 0 || h == 0 || ch == 0 {
            return Err(Error::format(
                &self.path,
                "FITS output dimensions must be nonzero",
            ));
        }
        self.dims = (w, h, ch);

        let mut cards: Vec<[u8; CARD]> = vec![
            card("SIMPLE", "T", "conforms to FITS standard"),
            card("BITPIX", "-32", "32-bit IEEE floating point"),
            card("NAXIS", "3", "number of axes"),
            card("NAXIS1", &w.to_string(), "width"),
            card("NAXIS2", &h.to_string(), "height"),
            card("NAXIS3", &ch.to_string(), "channels"),
            card("ROWORDER", "'TOP-DOWN'", "row order"),
        ];
        cards.extend(
            self.keywords
                .iter()
                .map(|k| card(&k.name, &k.value, &k.comment)),
        );
        cards.push(card("END", "", ""));
        // The END card must not carry the "= " marker.
        *cards.last_mut().unwrap() = {
            let mut c = [b' '; CARD];
            c[..3].copy_from_slice(b"END");
            c
        };

        let cards_per_block = BLOCK as usize / CARD;
        let n_blocks = cards.len().div_ceil(cards_per_block);
        let mut header = Vec::with_capacity(n_blocks * BLOCK as usize);
        for c in &cards {
            header.extend_from_slice(c);
        }
        header.resize(n_blocks * BLOCK as usize, b' ');
        self.file.write_all(&header).map_err(|e| self.io(e))?;
        self.data_start = header.len() as u64;

        // Pre-size the file to the padded data end: the trailing FITS padding
        // is all zeros, and unwritten holes read back as zeros too.
        let data_size = w * h * ch * 4;
        let total = self.data_start + data_size.div_ceil(BLOCK) * BLOCK;
        self.file.set_len(total).map_err(|e| self.io(e))?;
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let (w, h, ch) = self.dims;
        let plane_stride = (w * ch) as usize;
        if plane_stride == 0 || !rows.len().is_multiple_of(plane_stride) {
            return Err(Error::format(
                &self.path,
                format!(
                    "band length {} is not a multiple of ch*w = {plane_stride}",
                    rows.len()
                ),
            ));
        }
        let band_rows = (rows.len() / plane_stride) as u64;
        if y0 + band_rows > h {
            return Err(Error::format(
                &self.path,
                format!("band rows {y0}..{} exceed image height {h}", y0 + band_rows),
            ));
        }

        let mut buf = Vec::with_capacity((band_rows * w) as usize * 4);
        for c in 0..ch {
            let plane = &rows[(c * band_rows * w) as usize..][..(band_rows * w) as usize];
            buf.clear();
            for &v in plane {
                buf.extend_from_slice(&v.to_be_bytes());
            }
            let off = self.data_start + (c * h + y0) * w * 4;
            self.file
                .seek(SeekFrom::Start(off))
                .map_err(|e| self.io(e))?;
            self.file.write_all(&buf).map_err(|e| self.io(e))?;
        }
        self.rows_written += band_rows;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let h = self.dims.1;
        if self.rows_written != h {
            return Err(Error::format(
                &self.path,
                format!("only {} of {h} rows were written", self.rows_written),
            ));
        }
        self.file.flush().map_err(|e| self.io(e))
    }
}

/// Filter panel-0 keywords for the output header: drop structural cards
/// (SIMPLE/BITPIX/NAXIS*/EXTEND/BSCALE/BZERO, plus END and ROWORDER — the sink
/// emits its own), and shift CRPIX1/CRPIX2 by minus the crop origin so the
/// canvas WCS stays valid on the cropped output.
pub fn keywords_for_output(src: &[FitsKeyword], crop: (u64, u64)) -> Vec<FitsKeyword> {
    src.iter()
        .filter_map(|kw| {
            let name = kw.name.trim().to_ascii_uppercase();
            if name.starts_with("NAXIS")
                || matches!(
                    name.as_str(),
                    "SIMPLE" | "BITPIX" | "EXTEND" | "BSCALE" | "BZERO" | "END" | "ROWORDER"
                )
            {
                return None;
            }
            let mut kw = kw.clone();
            if name == "CRPIX1" || name == "CRPIX2" {
                let shift = if name == "CRPIX1" { crop.0 } else { crop.1 } as f64;
                // CRPIX is 1-based but the shift is a pure translation, so the
                // base cancels: new = old − crop_origin.
                if let Ok(v) = kw.value.trim().parse::<f64>() {
                    let nv = v - shift;
                    kw.value = if nv.fract() == 0.0 {
                        format!("{nv:.1}")
                    } else {
                        format!("{nv}")
                    };
                }
            }
            Some(kw)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-fits-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Find the 80-byte card starting with `name` (padded to 8) in a header.
    fn card(header: &[u8], name: &str) -> Option<String> {
        let prefix = format!("{name:<8}");
        header
            .chunks(CARD)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .find(|s| s.starts_with(&prefix))
    }

    /// Fixed-format value field of a card (before any comment).
    fn value(card: &str) -> String {
        card[10..].split('/').next().unwrap().trim().to_string()
    }

    #[test]
    fn writes_valid_fits_with_padding_and_byteswap() {
        let dir = tmpdir("basic");
        let path = dir.join("out.fits");
        let vals: Vec<f32> = (1..=12).map(|i| i as f32 * 0.5).collect();

        let mut sink = FitsSink::create(&path, vec![]).unwrap();
        sink.begin(4, 3, 1).unwrap();
        // Bands split mid-image: rows 0..2 then row 2.
        sink.band(0, &vals[0..8]).unwrap();
        sink.band(2, &vals[8..12]).unwrap();
        sink.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes.len(),
            5760,
            "one header block + one padded data block"
        );

        let header = &bytes[..2880];
        assert!(String::from_utf8_lossy(&header[..CARD]).starts_with("SIMPLE  ="));
        assert_eq!(value(&card(header, "SIMPLE").unwrap()), "T");
        assert_eq!(value(&card(header, "BITPIX").unwrap()), "-32");
        assert_eq!(value(&card(header, "NAXIS").unwrap()), "3");
        assert_eq!(value(&card(header, "NAXIS1").unwrap()), "4");
        assert_eq!(value(&card(header, "NAXIS2").unwrap()), "3");
        assert_eq!(value(&card(header, "NAXIS3").unwrap()), "1");
        assert!(card(header, "ROWORDER").unwrap().contains("'TOP-DOWN'"));
        assert!(card(header, "END").is_some());

        // Payload is big-endian f32 at 2880 in top-down row order,
        // zero-padded to the block end.
        for (i, &v) in vals.iter().enumerate() {
            let off = 2880 + i * 4;
            assert_eq!(bytes[off..off + 4], v.to_be_bytes(), "sample {i}");
        }
        assert!(
            bytes[2880 + 48..].iter().all(|&b| b == 0),
            "data padding must be zero"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn passes_through_keywords() {
        let dir = tmpdir("kw");
        let path = dir.join("kw.fits");
        let kws = vec![
            FitsKeyword {
                name: "OBJECT".into(),
                value: "'M42'".into(),
                comment: "target".into(),
            },
            FitsKeyword {
                name: "CRVAL1".into(),
                value: "83.822".into(),
                comment: "".into(),
            },
        ];
        let mut sink = FitsSink::create(&path, kws).unwrap();
        sink.begin(2, 2, 1).unwrap();
        sink.band(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        sink.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let header = &bytes[..2880];
        assert!(card(header, "OBJECT").unwrap().contains("'M42'"));
        assert_eq!(value(&card(header, "CRVAL1").unwrap()), "83.822");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finish_rejects_missing_rows() {
        let dir = tmpdir("short");
        let path = dir.join("short.fits");
        let mut sink = FitsSink::create(&path, vec![]).unwrap();
        sink.begin(4, 3, 1).unwrap();
        sink.band(0, &[1.0, 2.0, 3.0, 4.0]).unwrap(); // only 1 of 3 rows
        assert!(sink.finish().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keywords_for_output_drops_structural_and_shifts_crpix() {
        let kw = |name: &str, value: &str| FitsKeyword {
            name: name.into(),
            value: value.into(),
            comment: String::new(),
        };
        let src = vec![
            kw("SIMPLE", "T"),
            kw("BITPIX", "-32"),
            kw("NAXIS", "3"),
            kw("NAXIS1", "9255"),
            kw("NAXIS2", "18310"),
            kw("EXTEND", "T"),
            kw("BSCALE", "1.0"),
            kw("BZERO", "0.0"),
            kw("ROWORDER", "'TOP-DOWN'"),
            kw("CRPIX1", "4.6275000000e+03"),
            kw("CRPIX2", "9155"),
            kw("OBJECT", "'M42'"),
            kw("CRVAL1", "83.822"),
        ];
        let out = keywords_for_output(&src, (100, 200));

        for gone in [
            "SIMPLE", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "EXTEND", "BSCALE", "BZERO",
            "ROWORDER",
        ] {
            assert!(out.iter().all(|k| k.name != gone), "{gone} must be dropped");
        }
        let get = |name: &str| out.iter().find(|k| k.name == name).unwrap().value.clone();
        assert_eq!(get("CRPIX1").parse::<f64>().unwrap(), 4527.5);
        assert_eq!(get("CRPIX2").parse::<f64>().unwrap(), 8955.0);
        assert_eq!(
            get("OBJECT"),
            "'M42'",
            "non-structural values pass through untouched"
        );
        assert_eq!(get("CRVAL1"), "83.822");
    }
}
