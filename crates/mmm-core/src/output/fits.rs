//! Streaming FITS output: a [`RowSink`] over mpp-core's
//! [`FitsWriter`].
//!
//! Output: single HDU, `BITPIX = -32`, `NAXIS = 3`, big-endian samples,
//! rows stored **top-down** (`ROWORDER= 'TOP-DOWN'`) with WCS cards in the
//! same top-down convention (`astrometry::wcs_cards`) — empirically the one
//! layout on which PixInsight, Astropy and DS9 agree. Bands may arrive in
//! any order; each channel slice lands at its absolute file offset.

use std::path::Path;

use mpp_core::formats::fits_write::FitsWriter;
pub use mpp_core::formats::fits_write::keywords_for_output;

use crate::Result;
use crate::astrometry::{wcs_cards, wcs_cards_flipped};
use crate::blend::RowSink;
use crate::formats::{FitsKeyword, InputPanel};
use crate::session::Session;

/// Streaming FITS sink. Construct with the (already filtered/shifted) keyword
/// list from [`keywords_for_output`]; dimensions arrive via `begin`.
pub struct FitsSink(FitsWriter);

impl FitsSink {
    /// Create/truncate the output file; the header is written on `begin`.
    pub fn create(path: &Path, keywords: Vec<FitsKeyword>) -> Result<Self> {
        Ok(FitsSink(FitsWriter::create(path, keywords)?))
    }
}

impl RowSink for FitsSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        Ok(self.0.begin(w, h, ch)?)
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        Ok(self.0.write_band(y0, rows)?)
    }

    fn finish(&mut self) -> Result<()> {
        Ok(self.0.finish()?)
    }
}

/// Whether a card describes the *input panel's* geometry or pointing (WCS,
/// SIP, `RA`/`DEC`, ...). These must not pass through to a solved-session
/// output: its canvas is a fresh [`crate::align::MosaicFrame`], and the
/// correct cards are emitted from that frame instead. Non-geometric
/// metadata (`EXPTIME`, `INSTRUME`, ...) still passes through from panel 0.
/// astrometry.net's `IMAGEW`/`IMAGEH` describe the input panel's own pixel
/// dimensions, not the output's.
pub fn is_geometry_card(name: &str) -> bool {
    let n = name.trim().to_ascii_uppercase();
    matches!(
        n.as_str(),
        "RA" | "DEC"
            | "OBJCTRA"
            | "OBJCTDEC"
            | "RADESYS"
            | "EQUINOX"
            | "EPOCH"
            | "LONPOLE"
            | "LATPOLE"
            | "IMAGEW"
            | "IMAGEH"
    ) || [
        "CRVAL", "CRPIX", "CDELT", "CROTA", "CTYPE", "CUNIT", "CD1_", "CD2_", "PC1_", "PC2_",
        "PV1_", "PV2_", "A_", "B_", "AP_", "BP_",
    ]
    .iter()
    .any(|p| n.starts_with(p))
}

/// Where an output's WCS cards sit on the canvas ([`session_keywords`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WcsPlacement {
    /// Canvas pixel of the output's top-left corner (the crop origin).
    pub origin: (u64, u64),
    /// Output height in pixels.
    pub height: u64,
    /// Write the reflected bottom-up convention ([`wcs_cards_flipped`])
    /// instead of the top-down one ([`wcs_cards`]).
    pub flipped: bool,
}

/// Which solution [`session_keywords`] attached as WCS cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WcsSource {
    /// No placement was given (e.g. a downsampled preview): no cards.
    NotRequested,
    /// The session's own mosaic frame (solved input).
    SessionFrame,
    /// The reference panel's solution (aligned input).
    ReferencePanel,
    /// Requested, but neither the session nor its reference panel has a
    /// solution.
    Absent,
}

/// The FITS keywords for a full-resolution or cropped output of `session`:
/// its reference panel's cards passed through ([`keywords_for_output`],
/// shifted by `crop`), and — when `wcs` places the output — WCS cards from
/// the session's mosaic frame (solved input) or else the reference panel's
/// linear solution.
///
/// For solved sessions, and for a FITS reference panel (whose own cards
/// may be bottom-up), the reference panel's geometry and pointing cards
/// ([`is_geometry_card`]) are dropped: the fresh WCS cards replace them.
/// Opens the reference panel header-only (the original input, not the
/// reprojection cache, for solved sessions).
pub fn session_keywords(
    session: &Session,
    crop: (u64, u64),
    wcs: Option<WcsPlacement>,
) -> Result<(Vec<FitsKeyword>, WcsSource)> {
    let p0 = &session.panels[0];
    let ref_panel = InputPanel::open(p0.source.as_ref().unwrap_or(&p0.path))?;
    let mut keywords = keywords_for_output(ref_panel.fits_keywords(), crop);
    if session.frame.is_some() || ref_panel.format_name() == "FITS" {
        keywords.retain(|kw| !is_geometry_card(&kw.name));
    }
    let Some(place) = wcs else {
        return Ok((keywords, WcsSource::NotRequested));
    };
    let (solution, source) = match &session.frame {
        Some(frame) => (Some(frame.linear_wcs()), WcsSource::SessionFrame),
        None => (ref_panel.linear_wcs(), WcsSource::ReferencePanel),
    };
    let Some(solution) = solution else {
        return Ok((keywords, WcsSource::Absent));
    };
    if place.flipped {
        keywords.extend(wcs_cards_flipped(&solution, place.origin, place.height));
    } else {
        keywords.extend(wcs_cards(&solution, place.origin, place.height));
    }
    Ok((keywords, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::fits::FitsPanel;

    #[test]
    fn geometry_card_drops_pointing_wcs_and_sip_and_imagewh() {
        for name in [
            "IMAGEW", "IMAGEH", "A_ORDER", "A_2_0", "AP_ORDER", "BP_1_1", "CRPIX1", "CD1_1",
        ] {
            assert!(is_geometry_card(name), "{name} should be a geometry card");
        }
        for name in [
            "AIRMASS", "APTDIA", "BAYERPAT", "BZERO", "EXPTIME", "OBJECT",
        ] {
            assert!(
                !is_geometry_card(name),
                "{name} should not be a geometry card"
            );
        }
    }

    #[test]
    fn fits_sink_streams_through_row_sink() {
        let dir = std::env::temp_dir().join(format!("mmm-fits-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.fits");
        let (w, h, ch) = (5u64, 4u64, 2u64);
        let plane = (w * h) as usize;
        let src: Vec<f32> = (0..plane * ch as usize).map(|i| i as f32 / 40.0).collect();
        let kws = vec![FitsKeyword {
            name: "OBJECT".into(),
            value: "'M42'".into(),
            comment: String::new(),
        }];
        let mut sink: Box<dyn RowSink> = Box::new(FitsSink::create(&path, kws).unwrap());
        sink.begin(w, h, ch).unwrap();
        // Two bands, the second first: positional writes.
        let band = |y0: usize, rows: usize| -> Vec<f32> {
            (0..ch as usize)
                .flat_map(|c| src[c * plane + y0 * w as usize..][..rows * w as usize].to_vec())
                .collect()
        };
        sink.band(3, &band(3, 1)).unwrap();
        sink.band(0, &band(0, 3)).unwrap();
        sink.finish().unwrap();
        let img = FitsPanel::open(&path).unwrap();
        let mut out = vec![0f32; plane];
        for c in 0..ch {
            img.decode_rows(c, 0, h as usize, &mut out);
            assert_eq!(out, src[c as usize * plane..][..plane], "channel {c}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
