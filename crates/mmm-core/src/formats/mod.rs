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

/// Panel format identified from a file's first bytes by [`sniff_format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileFormat {
    /// `XISF0100` signature.
    Xisf,
    /// A FITS `SIMPLE` primary-header card.
    Fits,
}

/// Identify a panel file's format from its signature
/// ([`mpp_core::formats::sniff_format`]) — never from the file extension.
///
/// A file matching neither panel format errors naming exactly that, rather
/// than being handed to a reader that would surface an unrelated
/// format-specific complaint (e.g. "no END card"). TIFF, which mpp-core
/// recognizes, is not a panel format and is refused by name.
///
/// Shared by [`InputPanel::open`] and
/// [`crate::panel_reader::PanelReader::open_file`] so both name the same
/// failure for a file that is neither.
pub(crate) fn sniff_format(path: &Path) -> Result<FileFormat> {
    use mpp_core::formats::FileFormat as F;
    match mpp_core::formats::sniff_format(path) {
        Ok(F::Xisf) => Ok(FileFormat::Xisf),
        Ok(F::Fits) => Ok(FileFormat::Fits),
        Ok(F::Tiff) => Err(Error::format(
            path,
            "TIFF panels are not supported (XISF or FITS expected)",
        )),
        Err(mpp_core::Error::Format { .. }) => Err(Error::format(
            path,
            "not an XISF or FITS file (no XISF0100 signature or SIMPLE card)",
        )),
        Err(e) => Err(e.into()),
    }
}

/// A panel file of either supported format, opened for metadata access.
/// Pixel rows are read through [`crate::panel_reader::PanelReader`]
/// (`open_file`, or `open` with [`Self::storage`]); the XISF variant also
/// exposes its zero-copy planes via [`Self::as_xisf`].
#[derive(Debug)]
// One per open panel file, never held in bulk: boxing the larger XISF
// reader would buy nothing.
#[allow(clippy::large_enum_variant)]
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
            Self::Fits(p) => &p.header().keywords,
        }
    }
    /// XISF properties; always empty for FITS.
    pub fn properties(&self) -> &[XisfProperty] {
        match self {
            Self::Xisf(p) => &p.header().properties,
            Self::Fits(_) => &[],
        }
    }
    /// The XISF panel when its planes are mapped zero-copy from the file;
    /// `None` for FITS and for XISF panels that need decoding (compressed,
    /// non-Float32, …), which read through
    /// [`crate::panel_reader::PanelReader`] (backed by the decode cache).
    pub fn as_xisf(&self) -> Option<&XisfPanel> {
        match self {
            Self::Xisf(p) if p.is_mapped() => Some(p),
            _ => None,
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
                fits_wcs::model_from_keywords(&h.keywords, h.width, h.height, h.row_order)
            }
        }
    }
    /// The linear solution in the top-down convention, for output WCS cards.
    pub fn linear_wcs(&self) -> Option<LinearWcs> {
        match self {
            Self::Xisf(p) => wcs_from_properties(&p.header().properties),
            Self::Fits(p) => {
                let h = p.header();
                fits_wcs::linear_for_panel(&h.keywords, h.height, h.row_order).ok()
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

// The value types are mpp-core's (serde-identical to mmm's former ones:
// they cross the PixInsight IPC wire; see tests/wire_golden.rs).
pub use mpp_core::formats::{
    FitsKeyword, KeywordValue, PropertyValue, RowOrder, SampleFormat, XisfProperty, card_number,
    card_string,
};

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
