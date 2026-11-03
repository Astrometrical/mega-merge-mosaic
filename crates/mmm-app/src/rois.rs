//! Regions of interest (spec §5.4): named rectangles on the Blend canvas,
//! with an optional note, saved in `MosaicConfig.ui` under [`ROIS_KEY`].
//!
//! When the shared frame has a WCS an ROI is anchored on the sky (two
//! opposite corners, RA/Dec) so it survives a frame change (spec §6); its
//! canvas rect is reprojected through the current frame on load
//! ([`reproject`]). Without a WCS the canvas rect is what counts. The frame
//! is read and the conversion run on a worker (the Blend tab's ROI sync);
//! the list itself is UI state, saved like the display stretch.

use mmm_core::astrometry::LinearWcs;
use mmm_core::reference::ReferenceFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Key in `MosaicConfig.ui` of the ROI list.
pub const ROIS_KEY: &str = "rois";

/// A region of interest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roi {
    /// Shown name.
    pub name: String,
    /// The user's note (may be empty).
    #[serde(default)]
    pub note: String,
    /// Two opposite corners `[RA, Dec]` (degrees) of the canvas rect, when
    /// the frame has a WCS: the anchor the canvas rect is derived from.
    #[serde(default)]
    pub sky: Option<[[f64; 2]; 2]>,
    /// `[x0, y0, x1, y1]` in level-0 canvas pixels (exclusive ends).
    pub canvas: [u64; 4],
}

/// The ROIs saved in `ui` (none when absent or not a list); a malformed
/// entry is skipped and the rest kept.
pub fn load_rois(ui: &Value) -> Vec<Roi> {
    let Some(list) = ui.get(ROIS_KEY).and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|v| match serde_json::from_value::<Roi>(v.clone()) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("a saved ROI is unreadable and skipped: {e}");
                None
            }
        })
        .collect()
}

/// Store `rois` in `ui`, keeping everything else there (a `ui` that is not
/// a JSON object becomes one).
pub fn save_rois(ui: &mut Value, rois: &[Roi]) {
    if !ui.is_object() {
        *ui = Value::Object(Default::default());
    }
    if let (Some(obj), Ok(v)) = (ui.as_object_mut(), serde_json::to_value(rois)) {
        obj.insert(ROIS_KEY.to_string(), v);
    }
}

/// A name for a new ROI that none of `taken` has: "ROI 1", "ROI 2", ….
pub fn unique_roi_name(taken: &[String]) -> String {
    (1..)
        .map(|i| format!("ROI {i}"))
        .find(|n| !taken.contains(n))
        .unwrap_or_default()
}

/// The canvas WCS of `frame`, if it has one.
pub fn frame_wcs(frame: &ReferenceFrame) -> Option<LinearWcs> {
    match frame {
        ReferenceFrame::Solved { frame } => Some(frame.linear_wcs()),
        ReferenceFrame::Aligned { wcs, .. } => wcs.clone(),
    }
}

/// Canvas pixel edge `e` → FITS pixel coordinate (pixel `i` spans edges
/// `i..i + 1` and has its centre at FITS `i + 1`).
const FITS_OFFSET: f64 = 0.5;
/// Values this close to a whole pixel are that pixel (float noise of a
/// sky round trip).
const SNAP: f64 = 1e-6;

/// The sky corners `[RA, Dec]` of canvas rect `r`'s top-left and
/// bottom-right corners.
pub fn canvas_to_sky(wcs: &LinearWcs, r: [u64; 4]) -> [[f64; 2]; 2] {
    let corner = |x: u64, y: u64| {
        let (ra, dec) = wcs.pixel_to_sky(x as f64 + FITS_OFFSET, y as f64 + FITS_OFFSET);
        [ra, dec]
    };
    [corner(r[0], r[1]), corner(r[2], r[3])]
}

/// The canvas rect spanned by sky corners `sky` in a `canvas`-sized frame
/// with `wcs`: whole pixels, outward, clamped to the canvas. `None` if it
/// falls off the canvas (or is degenerate there).
pub fn sky_to_canvas(wcs: &LinearWcs, sky: [[f64; 2]; 2], canvas: (u64, u64)) -> Option<[u64; 4]> {
    let p = |c: [f64; 2]| {
        let (x, y) = wcs.sky_to_pixel(c[0], c[1]);
        (x - FITS_OFFSET, y - FITS_OFFSET)
    };
    let (a, b) = (p(sky[0]), p(sky[1]));
    if ![a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite()) {
        return None;
    }
    canvas_rect(
        [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)],
        canvas,
    )
}

/// A drawn image rect (`[x0, y0, x1, y1]`, level-0 pixels) as whole canvas
/// pixels covering it, clamped to the canvas; `None` if empty there.
pub fn canvas_rect(r: [f64; 4], canvas: (u64, u64)) -> Option<[u64; 4]> {
    let snap = |v: f64| {
        if (v - v.round()).abs() < SNAP {
            v.round()
        } else {
            v
        }
    };
    let (w, h) = (canvas.0 as f64, canvas.1 as f64);
    let x0 = snap(r[0]).floor().clamp(0.0, w);
    let y0 = snap(r[1]).floor().clamp(0.0, h);
    let x1 = snap(r[2]).ceil().clamp(0.0, w);
    let y1 = snap(r[3]).ceil().clamp(0.0, h);
    (x1 > x0 && y1 > y0).then_some([x0 as u64, y0 as u64, x1 as u64, y1 as u64])
}

/// `rois` through `frame` (blocking only in the sense of being run on a
/// worker; no I/O). With a WCS, an ROI's canvas rect is derived from its
/// sky corners (kept if they fall off the canvas), and an ROI without sky
/// corners gains them. Without a WCS (or no frame yet) the canvas rects are
/// kept as stored.
pub fn reproject(rois: &[Roi], frame: Option<&ReferenceFrame>) -> Vec<Roi> {
    let Some((wcs, canvas)) = frame.and_then(|f| Some((frame_wcs(f)?, f.canvas()))) else {
        return rois.to_vec();
    };
    rois.iter()
        .map(|r| {
            let mut r = r.clone();
            match r.sky {
                Some(sky) => {
                    if let Some(c) = sky_to_canvas(&wcs, sky, canvas) {
                        r.canvas = c;
                    }
                }
                None => r.sky = Some(canvas_to_sky(&wcs, r.canvas)),
            }
            r
        })
        .collect()
}

/// "W × H (x.x MP)" of canvas rect `r`.
pub fn roi_size_text(r: [u64; 4]) -> String {
    let (w, h) = (r[2].saturating_sub(r[0]), r[3].saturating_sub(r[1]));
    format!("{w} × {h} ({:.1} MP)", (w * h) as f64 / 1e6)
}

#[cfg(test)]
mod tests {
    use mmm_core::align::MosaicFrame;
    use mmm_core::mosaic::MosaicConfig;
    use mmm_core::reference::ReferenceFrame;

    use super::*;
    use crate::start::tests::temp_dir;

    fn solved() -> ReferenceFrame {
        ReferenceFrame::Solved {
            frame: MosaicFrame {
                crval: [83.82, -5.39],
                scale_deg: 1.5 / 3600.0,
                width: 2000,
                height: 1500,
                rotation_deg: 0.0,
            },
        }
    }

    #[test]
    fn roi_sky_roundtrip() {
        let frame = solved();
        let roi = Roi {
            name: "ROI 1".into(),
            note: "faint kink in the trapezium spike".into(),
            sky: None,
            canvas: [100, 200, 600, 500],
        };
        // In a frame with a WCS, an ROI gains its sky corners and keeps its
        // canvas rect exactly.
        let synced = reproject(std::slice::from_ref(&roi), Some(&frame));
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].canvas, roi.canvas);
        let sky = synced[0].sky.expect("sky corners");
        assert!((sky[0][1] - sky[1][1]).abs() > 0.1, "{sky:?}");

        // Saved in the session and read back unchanged (other ui kept).
        let mut cfg = MosaicConfig::new(Vec::new());
        cfg.ui = serde_json::json!({ "other": 1 });
        save_rois(&mut cfg.ui, &synced);
        let dir = temp_dir("rois-save");
        cfg.save(&dir).unwrap();
        let back = MosaicConfig::load(&dir).unwrap();
        assert_eq!(load_rois(&back.ui), synced);
        assert_eq!(back.ui["other"], 1);
        assert!(back.ui[ROIS_KEY][0]["sky"].is_array());

        // Reloaded through the same frame: the same canvas rect.
        assert_eq!(reproject(&synced, Some(&frame)), synced);

        // Through a frame whose canvas is shifted by (+50, −30) and grown:
        // the ROI stays on the same sky, at the shifted canvas rect.
        let mut wcs = frame_wcs(&frame).unwrap();
        wcs.crpix[0] += 50.0;
        wcs.crpix[1] -= 30.0;
        let moved = ReferenceFrame::Aligned {
            width: 2100,
            height: 1600,
            wcs: Some(wcs),
        };
        let r = reproject(&synced, Some(&moved));
        assert_eq!(r[0].canvas, [150, 170, 650, 470]);
        assert_eq!(r[0].sky, synced[0].sky, "sky is the anchor");
        assert_eq!(
            (r[0].name.as_str(), r[0].note.as_str()),
            ("ROI 1", roi.note.as_str())
        );

        // No WCS: canvas coordinates are kept as stored.
        let plain = ReferenceFrame::Aligned {
            width: 2000,
            height: 1500,
            wcs: None,
        };
        assert_eq!(
            reproject(std::slice::from_ref(&roi), Some(&plain)),
            vec![roi.clone()]
        );
        assert_eq!(reproject(&synced, None), synced);

        // A sky ROI off the new canvas keeps its last canvas rect.
        let mut far = frame_wcs(&frame).unwrap();
        far.crval[0] += 10.0;
        let elsewhere = ReferenceFrame::Aligned {
            width: 2000,
            height: 1500,
            wcs: Some(far),
        };
        assert_eq!(reproject(&synced, Some(&elsewhere))[0].canvas, roi.canvas);

        // Drawn rects become whole canvas pixels, outward, on the canvas.
        assert_eq!(
            canvas_rect([10.2, 20.7, 99.1, 50.0], (2000, 1500)),
            Some([10, 20, 100, 50])
        );
        assert_eq!(
            canvas_rect([-5.0, 0.0, 3000.0, 9.5], (2000, 1500)),
            Some([0, 0, 2000, 10])
        );
        assert_eq!(
            canvas_rect([10.0, 20.0, 10.0, 90.0], (2000, 1500)),
            None,
            "empty"
        );
        assert_eq!(roi_size_text([0, 0, 2000, 1500]), "2000 × 1500 (3.0 MP)");
        let taken = ["ROI 1".to_string(), "ROI 3".to_string()];
        assert_eq!(unique_roi_name(&taken), "ROI 2");

        // Lenient load: malformed entries are skipped.
        let ui = serde_json::json!({ ROIS_KEY: [
            { "name": "a", "canvas": [0, 0, 10, 10] },
            { "name": "b" },
            7
        ]});
        let loaded = load_rois(&ui);
        assert_eq!(loaded.len(), 1);
        assert_eq!((loaded[0].note.as_str(), loaded[0].sky), ("", None));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
