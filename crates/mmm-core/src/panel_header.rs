//! One-open, header-only facts about an input panel: size, filter,
//! integration time and plate solution. The panel list shows these without
//! reading any pixels.

use std::path::Path;

use crate::checks::SolutionKind;
use crate::error::Result;
use crate::formats::{InputPanel, card_number};

/// Header facts of one panel.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelHeader {
    /// Width in pixels.
    pub width: u64,
    /// Height in pixels.
    pub height: u64,
    /// Channel count.
    pub channels: u64,
    /// Filter name, if the header has one.
    pub filter: Option<String>,
    /// Exposure of one sub-frame in seconds (`EXPTIME`, else `EXPOSURE`).
    pub exposure_s: Option<f64>,
    /// Number of integrated sub-frames (`NCOMBINE`).
    pub frames: Option<u64>,
    /// What kind of solution the panel carries.
    pub solution: SolutionKind,
    /// Pixel scale in arcseconds per pixel, when solved.
    pub scale_arcsec: Option<f64>,
}

impl PanelHeader {
    /// Total integration time in seconds: exposure times frames when both
    /// are known, else `None`.
    pub fn integration_s(&self) -> Option<f64> {
        Some(self.exposure_s? * self.frames? as f64)
    }
}

/// Read a panel's header facts with a single open (no pixel data).
pub fn read_panel_header(path: &Path) -> Result<PanelHeader> {
    let panel = InputPanel::open(path)?;
    let (solution, scale_arcsec) = match panel.wcs_model() {
        Ok(m) => {
            let kind = if m.has_distortion() {
                SolutionKind::Distorted
            } else {
                SolutionKind::Linear
            };
            (kind, m.pixel_scale_arcsec())
        }
        Err(_) => (SolutionKind::None, None),
    };
    let cards = panel.fits_keywords();
    let positive = |name: &str| card_number(cards, name).filter(|v| v.is_finite() && *v > 0.0);
    Ok(PanelHeader {
        width: panel.width(),
        height: panel.height(),
        channels: panel.channels(),
        filter: panel.filter_name(),
        exposure_s: positive("EXPTIME").or_else(|| positive("EXPOSURE")),
        frames: positive("NCOMBINE")
            .map(|v| v.round() as u64)
            .filter(|n| *n > 0),
        solution,
        scale_arcsec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::panel_facts;
    use crate::synth::write_xisf_with_header_xml;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-phdr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn panel(dir: &Path, cards: &[(&str, &str)]) -> PathBuf {
        let p = dir.join("p.xisf");
        let xml: String = cards
            .iter()
            .map(|(n, v)| format!(r#"<FITSKeyword name="{n}" value="{v}" comment=""/>"#))
            .collect();
        write_xisf_with_header_xml(&p, 16, 12, 1, &vec![0.5; 16 * 12], &xml).unwrap();
        p
    }

    #[test]
    fn header_reads_size_filter_and_integration() {
        let d = tmp("full");
        let p = panel(
            &d,
            &[("FILTER", "'Ha'"), ("EXPTIME", "300"), ("NCOMBINE", "24")],
        );
        let h = read_panel_header(&p).unwrap();
        assert_eq!((h.width, h.height, h.channels), (16, 12, 1));
        assert_eq!(h.filter.as_deref(), Some("Ha"));
        assert_eq!(h.exposure_s, Some(300.0));
        assert_eq!(h.frames, Some(24));
        assert_eq!(h.integration_s(), Some(7200.0));
        assert_eq!(h.solution, SolutionKind::None);
    }

    #[test]
    fn integration_needs_both_cards() {
        let d = tmp("one");
        let h = read_panel_header(&panel(&d, &[("EXPTIME", "300")])).unwrap();
        assert_eq!(h.exposure_s, Some(300.0));
        assert_eq!(h.frames, None);
        assert_eq!(h.integration_s(), None);
    }

    #[test]
    fn bad_cards_are_unknown() {
        let d = tmp("bad");
        let h = read_panel_header(&panel(&d, &[("EXPTIME", "abc"), ("NCOMBINE", "-1")])).unwrap();
        assert_eq!(
            (h.exposure_s, h.frames, h.integration_s()),
            (None, None, None)
        );
        let h = read_panel_header(&panel(&d, &[("EXPTIME", "-1"), ("NCOMBINE", "0")])).unwrap();
        assert_eq!((h.exposure_s, h.frames), (None, None));
    }

    #[test]
    fn panel_facts_match_the_header() {
        let d = tmp("facts");
        let p = panel(&d, &[("FILTER", "'OIII'")]);
        let h = read_panel_header(&p).unwrap();
        let f = panel_facts(&p).unwrap();
        assert_eq!(f.path, p);
        assert_eq!(f.channels, h.channels);
        assert_eq!(f.filter, h.filter);
        assert_eq!(f.solution, h.solution);
        assert_eq!(f.scale_arcsec, h.scale_arcsec);
    }
}
