//! Input format readers.
//!
//! All readers expose the same shape of data: per-channel planar `f32` planes
//! over a memory-mapped file, plus passthrough metadata (FITS keywords, WCS).

pub mod fits;
pub mod xisf;

use std::path::Path;

use crate::astrometry::{LinearWcs, WcsModel, describe_unsolved, fits_wcs, wcs_from_properties};
use crate::panel_reader::PanelStorage;
use crate::{Error, Result};
use fits::FitsPanel;
use xisf::XisfPanel;

/// Format identified from a file's first 8 bytes by [`sniff_format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileFormat {
    /// `XISF0100` signature.
    Xisf,
    /// A FITS `SIMPLE` primary-header card.
    Fits,
}

/// Identify a panel file's format from its first 8 bytes — the XISF
/// `XISF0100` signature, or the FITS `SIMPLE` card's 8-byte keyword field
/// (`SIMPLE` padded with spaces) — never from the file extension.
///
/// A short read (file under 8 bytes, including empty) is tolerated: it
/// cannot match either signature, so it falls straight through to the
/// "neither" error below rather than propagating an I/O error. A file
/// matching neither signature errors naming exactly that, rather than being
/// handed to the FITS reader and surfacing an unrelated FITS-specific
/// complaint (e.g. "no END card") for what may not be a FITS file at all.
///
/// Shared by [`InputPanel::open`] and
/// [`crate::panel_reader::PanelReader::open_file`] so both name the same
/// failure for a file that is neither.
pub(crate) fn sniff_format(path: &Path) -> Result<FileFormat> {
    use std::io::Read;
    let mut magic = [0u8; 8];
    let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    match f.read_exact(&mut magic) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(Error::io(path, e)),
    }
    if magic == *b"XISF0100" {
        Ok(FileFormat::Xisf)
    } else if magic == *b"SIMPLE  " {
        Ok(FileFormat::Fits)
    } else {
        Err(Error::format(
            path,
            "not an XISF or FITS file (no XISF0100 signature or SIMPLE card)",
        ))
    }
}

/// A panel file of either supported format, opened for metadata access.
/// Pixel rows are read through [`crate::panel_reader::PanelReader`]
/// (`open_file`, or `open` with [`Self::storage`]); the XISF variant also
/// exposes its zero-copy planes via [`Self::as_xisf`].
#[derive(Debug)]
pub enum InputPanel {
    /// A monolithic XISF file.
    Xisf(XisfPanel),
    /// A FITS primary-HDU image.
    Fits(FitsPanel),
}

impl InputPanel {
    /// Open a panel, detecting the format from its magic bytes
    /// (`sniff_format`) — never from the file extension.
    pub fn open(path: &Path) -> Result<InputPanel> {
        match sniff_format(path)? {
            FileFormat::Xisf => Ok(InputPanel::Xisf(XisfPanel::open(path)?)),
            FileFormat::Fits => Ok(InputPanel::Fits(FitsPanel::open(path)?)),
        }
    }

    /// The file this panel was opened from.
    pub fn path(&self) -> &Path {
        match self {
            Self::Xisf(p) => p.path(),
            Self::Fits(p) => p.path(),
        }
    }
    /// Image width in pixels.
    pub fn width(&self) -> u64 {
        match self {
            Self::Xisf(p) => p.width(),
            Self::Fits(p) => p.width(),
        }
    }
    /// Image height in pixels.
    pub fn height(&self) -> u64 {
        match self {
            Self::Xisf(p) => p.height(),
            Self::Fits(p) => p.height(),
        }
    }
    /// Channel count.
    pub fn channels(&self) -> u64 {
        match self {
            Self::Xisf(p) => p.channels(),
            Self::Fits(p) => p.channels(),
        }
    }
    /// Header FITS keywords (XISF `<FITSKeyword>` elements, or every FITS
    /// card).
    pub fn fits_keywords(&self) -> &[FitsKeyword] {
        match self {
            Self::Xisf(p) => &p.header().fits_keywords,
            Self::Fits(p) => &p.header().fits_keywords,
        }
    }
    /// XISF properties; always empty for FITS.
    pub fn properties(&self) -> &[XisfProperty] {
        match self {
            Self::Xisf(p) => &p.header().properties,
            Self::Fits(_) => &[],
        }
    }
    /// The XISF panel, for zero-copy plane access.
    pub fn as_xisf(&self) -> Option<&XisfPanel> {
        match self {
            Self::Xisf(p) => Some(p),
            Self::Fits(_) => None,
        }
    }
    /// `"XISF"` or `"FITS"`.
    pub fn format_name(&self) -> &'static str {
        match self {
            Self::Xisf(_) => "XISF",
            Self::Fits(_) => "FITS",
        }
    }
    /// The [`PanelStorage`] a full-canvas session records for this file.
    pub fn storage(&self) -> PanelStorage {
        match self {
            Self::Xisf(_) => PanelStorage::FullCanvasXisf,
            Self::Fits(_) => PanelStorage::FullCanvasFits,
        }
    }
    /// The panel's full astrometric model (top-down image coordinates), or
    /// a human-readable reason it has none.
    pub fn wcs_model(&self) -> std::result::Result<WcsModel, String> {
        match self {
            Self::Xisf(p) => {
                let h = p.header();
                WcsModel::from_properties(&h.properties, h.width, h.height)
                    .ok_or_else(|| describe_unsolved(&h.properties))
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

    /// The panel's filter name, for grouping UIs: the FITS `FILTER` card
    /// (either format; quotes and padding stripped), else — XISF only — the
    /// `Instrument:Filter:Name` property. `None` when neither is present or
    /// the value is empty.
    pub fn filter_name(&self) -> Option<String> {
        let clean = |s: &str| {
            let t = s.trim().trim_matches('\'').trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        if let Some(k) = self.fits_keywords().iter().find(|k| k.name == "FILTER")
            && let Some(name) = clean(&k.value)
        {
            return Some(name);
        }
        match self {
            Self::Xisf(p) => p
                .header()
                .properties
                .iter()
                .find(|pr| pr.id == "Instrument:Filter:Name")
                .and_then(|pr| match &pr.value {
                    PropertyValue::Str(s) => clean(s),
                    _ => None,
                }),
            Self::Fits(_) => None,
        }
    }
}

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

/// A FITS header keyword carried through from input to output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FitsKeyword {
    /// Keyword name (e.g. `OBJECT`, `CRVAL1`), as written in the header.
    pub name: String,
    /// Raw value text, quoting included for strings (e.g. `'M42'`).
    pub value: String,
    /// Free-text comment, empty when the card has none.
    pub comment: String,
}

/// One XISF `<Property>` element carried through from the input header.
///
/// PixInsight stores plate solutions (and much other metadata) as properties,
/// not FITS keywords. Values arrive in three shapes, all parsed by the XISF
/// reader:
/// - scalar `value="…"` attributes (Float64, Int*, TimePoint, …),
/// - element text (String properties, and `location="inline:base64"` /
///   `inline:hex` encoded vector/matrix data),
/// - attachment data blocks (`location="attachment:offset:size"`), exposed via
///   [`XisfProperty::location`] and resolved on open for f64 vectors/matrices.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct XisfProperty {
    /// Property identifier (e.g. `PCL:AstrometricSolution:ProjectionSystem`).
    pub id: String,
    /// XISF type name as written in the header (e.g. `Float64`, `F64Vector`).
    pub type_: String,
    /// Decoded value (see [`PropertyValue`] for the shapes).
    pub value: PropertyValue,
    /// Byte offset and size of an attachment-located data block, if any.
    pub location: Option<(u64, u64)>,
}

/// Decoded value of an XISF property.
///
/// Attachment-located numeric vector/matrix properties parse with empty
/// `data` (dimensions from the header attributes); `XisfPanel::open` resolves
/// them from the file. `Unread` marks types we do not decode.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PropertyValue {
    /// String-like value (`String`, `TimePoint`, or any `value` attribute of
    /// an undecoded type).
    Str(String),
    /// Floating-point scalar (`Float32`/`Float64`).
    F64(f64),
    /// Integer scalar (all `Int*`/`UInt*` widths, and `Boolean` as 0/1).
    I64(i64),
    /// Vector data of any XISF numeric vector type (`F64Vector`, `I32Vector`,
    /// `F32Vector`, …), converted to f64 in header order; the property's
    /// `type_` records the original type. Integers are exact up to 2^53.
    F64Vec(Vec<f64>),
    /// Matrix data of any XISF numeric matrix type, converted to f64,
    /// row-major; `type_` records the original type.
    F64Mat {
        /// Number of matrix rows.
        rows: u32,
        /// Number of matrix columns.
        cols: u32,
        /// Row-major element data, `rows × cols` long once resolved.
        data: Vec<f64>,
    },
    /// A type this reader does not decode.
    Unread,
}

impl PropertyValue {
    /// The value as a float: `F64` directly, `I64` converted; else `None`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(v) => Some(*v),
            Self::I64(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// The value as a string slice, for `Str` values only.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The value as an f64 slice, for `F64Vec` values only.
    pub fn as_f64_vec(&self) -> Option<&[f64]> {
        match self {
            Self::F64Vec(v) => Some(v),
            _ => None,
        }
    }

    /// The value as `(rows, cols, row-major data)`, for `F64Mat` values only.
    pub fn as_f64_mat(&self) -> Option<(u32, u32, &[f64])> {
        match self {
            Self::F64Mat { rows, cols, data } => Some((*rows, *cols, data)),
            _ => None,
        }
    }

    /// True for f64 vector/matrix values whose data still lives in an
    /// attachment block (they parse with empty `data`; the reader fills them).
    pub fn needs_attachment_data(&self) -> bool {
        match self {
            Self::F64Vec(v) => v.is_empty(),
            Self::F64Mat { data, .. } => data.is_empty(),
            _ => false,
        }
    }
}

/// Sample formats we recognize in headers. Only `Float32` is readable for now —
/// MosaicByCoordinates output is always Float32/Float64, and Float32 is the
/// overwhelmingly common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // variants are the XISF sampleFormat names verbatim
pub enum SampleFormat {
    UInt8,
    UInt16,
    UInt32,
    Float32,
    Float64,
}

impl SampleFormat {
    /// Parse an XISF `sampleFormat` attribute value; `None` if unrecognized.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "UInt8" => Self::UInt8,
            "UInt16" => Self::UInt16,
            "UInt32" => Self::UInt32,
            "Float32" => Self::Float32,
            "Float64" => Self::Float64,
            _ => return None,
        })
    }

    /// Size of one sample of this format, in bytes.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::UInt8 => 1,
            Self::UInt16 => 2,
            Self::UInt32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }
}

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
        FitsKeyword {
            name: name.into(),
            value: value.into(),
            comment: String::new(),
        }
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
        assert_eq!(
            card_string(pf.fits_keywords(), "OBJECT").as_deref(),
            Some("x")
        );
        assert_eq!(
            pf.storage(),
            crate::panel_reader::PanelStorage::FullCanvasFits
        );
        assert_eq!(
            px.storage(),
            crate::panel_reader::PanelStorage::FullCanvasXisf
        );
        assert!(px.as_xisf().is_some() && pf.as_xisf().is_none());
        // A file matching neither magic is reported as an unrecognized
        // format by `sniff_format` itself, before either reader ever opens
        // it — not as a FITS- or XISF-specific complaint (e.g. FitsPanel's
        // "no END card in the primary header", which a plain text file like
        // this would otherwise hit first).
        std::fs::write(dir.join("junk"), b"hello world, not an image at all").unwrap();
        let e = InputPanel::open(&dir.join("junk")).unwrap_err().to_string();
        assert!(e.contains("not an XISF or FITS file"), "{e}");
        // Same for an empty file (too short even for the magic read).
        std::fs::write(dir.join("empty"), b"").unwrap();
        let e = InputPanel::open(&dir.join("empty"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("not an XISF or FITS file"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wcs_model_and_linear_wcs_for_both_formats() {
        let dir = tmpdir("wcs");
        let (w, h) = (200u64, 100u64);
        let planes = vec![0.5f32; (w * h) as usize];
        let wcs = SynthWcs {
            crval: [80.0, -5.0],
            refimg: [100.0, 50.0],
            cd: [[-1e-3, 0.0], [0.0, 1e-3]],
        };
        let x = dir.join("s.xisf");
        write_xisf_solved(&x, w, h, 1, &planes, &wcs).unwrap();
        let px = InputPanel::open(&x).unwrap();
        let mx = px.wcs_model().unwrap();
        let lx = px.linear_wcs().unwrap();
        assert_eq!(lx.crpix, [100.5, 50.5]);

        // The same solution as a bottom-up FITS: file CRPIX2 = H + 1 − 50.5.
        let f = dir.join("s.fits");
        let cards = vec![
            kw("CTYPE1", "'RA---TAN'"),
            kw("CTYPE2", "'DEC--TAN'"),
            kw("CRVAL1", "80.0"),
            kw("CRVAL2", "-5.0"),
            kw("CRPIX1", "100.5"),
            kw("CRPIX2", &format!("{}", h as f64 + 1.0 - 50.5)),
            kw("CD1_1", "-1.0E-3"),
            kw("CD1_2", "0"),
            kw("CD2_1", "0"),
            kw("CD2_2", "-1.0E-3"),
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
        write_fits(&u, 4, 4, 1, &[0.5; 16], -32, &[]).unwrap();
        let e = InputPanel::open(&u).unwrap().wcs_model().unwrap_err();
        assert!(e.contains("CTYPE1"), "{e}");
        let ux = dir.join("u.xisf");
        write_xisf(&ux, 4, 4, 1, &[0.5; 16]).unwrap();
        let e = InputPanel::open(&ux).unwrap().wcs_model().unwrap_err();
        assert!(e.contains("no astrometric solution"), "{e}");
        assert!(InputPanel::open(&u).unwrap().linear_wcs().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
