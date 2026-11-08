//! Regions of interest (spec §5.4): named rectangles on the Blend canvas,
//! with an optional note, saved in `MosaicConfig.ui` under [`ROIS_KEY`].
//!
//! When the shared frame has a WCS an ROI is anchored on the sky (two
//! opposite corners, RA/Dec) so it survives a frame change (spec §6); its
//! canvas rect is reprojected through the current frame on load
//! ([`reproject`]). Without a WCS the canvas rect is what counts. The frame
//! is read and the conversion run on a worker ([`RoiPanel`]'s sync); the
//! list itself is UI state, saved like the display stretch.
//!
//! [`RoiPanel`] is the ROI list (the session window's ROIs section) and its
//! workers: the list (new, select and zoom, rename, note, delete), the
//! reprojection, and how fresh each group of the view is inside each ROI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::RichText;
use mega_ui::theme::palette;
use mega_ui::viewer::{Overlay, TiledViewer, Tool};
use mmm_core::astrometry::LinearWcs;
use mmm_core::reference::ReferenceFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tile_source::{MosaicTileSource, fresh_fraction};
use crate::util::{Poll, Task};

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

/// Per member group of the view: `(valid, total)` level-0 tiles inside an
/// ROI, or `None` when the group has no result open.
pub type MemberCounts = Vec<(String, Option<(usize, usize)>)>;
/// [`MemberCounts`] of each ROI rect.
type FreshCounts = Vec<([u64; 4], MemberCounts)>;

/// The session's ROIs: the list (kept in `MosaicConfig.ui`), the
/// selection, and the sync and freshness workers. Methods that change the
/// list write it into the session's `ui` and return true; the view then
/// saves (debounced).
#[derive(Default)]
pub struct RoiPanel {
    /// The ROIs, as in `ui.rois`.
    pub(crate) rois: Vec<Roi>,
    loaded: bool,
    /// The selected ROI.
    pub(crate) selected: Option<usize>,
    /// A pending reprojection: the list it started from and the result.
    sync: Option<Task<(Vec<Roi>, Vec<Roi>)>>,
    /// Reproject (read the frame) once possible.
    pub(crate) want_sync: bool,
    /// [`MemberCounts`] by ROI rect.
    fresh: BTreeMap<[u64; 4], MemberCounts>,
    fresh_task: Option<Task<FreshCounts>>,
    /// Count fresh tiles once possible.
    pub(crate) want_fresh: bool,
}

impl RoiPanel {
    /// Read the list from `ui` the first time only, and ask for a sync.
    pub fn load_once(&mut self, ui: &Value) {
        if !self.loaded {
            self.loaded = true;
            self.rois = load_rois(ui);
            self.want_sync = true;
        }
    }

    /// The ROIs.
    pub fn rois(&self) -> &[Roi] {
        &self.rois
    }

    /// The selected ROI's canvas rect.
    pub fn selected_rect(&self) -> Option<[u64; 4]> {
        self.rois.get(self.selected?).map(|r| r.canvas)
    }

    /// No reprojection running or wanted: the canvas rects follow the
    /// current frame (as far as the last analysis knows it).
    pub fn synced(&self) -> bool {
        self.sync.is_none() && !self.want_sync
    }

    /// No worker running.
    pub fn idle(&self) -> bool {
        self.sync.is_none() && self.fresh_task.is_none()
    }

    /// The view (its groups) changed: freshness is counted again.
    pub fn view_changed(&mut self) {
        self.fresh.clear();
        self.want_fresh = true;
    }

    /// A rectangle drawn on the canvas (image coordinates of a
    /// `canvas`-sized result) becomes a new ROI "ROI n", selected; its sky
    /// corners follow from the sync. True if the list changed.
    pub fn on_drawn(&mut self, ui: &mut Value, r: [f64; 4], canvas: (u64, u64)) -> bool {
        let Some(rect) = canvas_rect(r, canvas) else {
            return false;
        };
        let taken: Vec<String> = self.rois.iter().map(|r| r.name.clone()).collect();
        self.rois.push(Roi {
            name: unique_roi_name(&taken),
            note: String::new(),
            sky: None,
            canvas: rect,
        });
        self.selected = Some(self.rois.len() - 1);
        self.want_sync = true;
        self.geometry_changed(ui);
        true
    }

    /// The rects changed (draw, delete, reprojection): keep the list in
    /// `ui` and count freshness again.
    fn geometry_changed(&mut self, ui: &mut Value) {
        save_rois(ui, &self.rois);
        self.want_fresh = true;
    }

    /// Start the sync (reading the frame at `frame`) and freshness counts
    /// over `members` (each group of the view and its open result) on
    /// workers when asked for. A freshness task holds the sources: the
    /// caller tracks any it gives up meanwhile (`mosaic_view`).
    pub fn start_tasks(
        &mut self,
        ctx: &egui::Context,
        frame: PathBuf,
        members: Vec<(String, Option<Arc<MosaicTileSource>>)>,
    ) {
        if self.want_sync && self.sync.is_none() {
            self.want_sync = false;
            if !self.rois.is_empty() {
                let snapshot = self.rois.clone();
                self.sync = Task::spawn("mmm-roi-sync", ctx, move || {
                    let frame = ReferenceFrame::load(&frame).ok();
                    let out = reproject(&snapshot, frame.as_ref());
                    (snapshot, out)
                });
            }
        }
        if self.want_fresh && self.fresh_task.is_none() {
            self.want_fresh = false;
            let rects: Vec<[u64; 4]> = self.rois.iter().map(|r| r.canvas).collect();
            if rects.is_empty() || members.is_empty() {
                self.fresh.clear();
                return;
            }
            self.fresh_task = Task::spawn("mmm-roi-fresh", ctx, move || {
                rects
                    .into_iter()
                    .map(|r| {
                        let per = members
                            .iter()
                            .map(|(g, s)| (g.clone(), s.as_ref().map(|s| s.fresh_counts(r))))
                            .collect();
                        (r, per)
                    })
                    .collect()
            });
        }
    }

    /// Apply finished workers; true if the list changed (written to `ui`).
    pub fn poll(&mut self, ui: &mut Value) -> bool {
        let mut changed = false;
        if let Some(task) = &self.sync {
            match task.poll() {
                Poll::Pending => {}
                Poll::Lost => self.sync = None,
                Poll::Done((snapshot, out)) => {
                    self.sync = None;
                    // Apply to the ROIs still as they were (the user may
                    // have drawn or deleted meanwhile); names and notes
                    // are the user's and stay.
                    for roi in &mut self.rois {
                        let Some(j) = snapshot
                            .iter()
                            .position(|s| s.sky == roi.sky && s.canvas == roi.canvas)
                        else {
                            continue;
                        };
                        if (out[j].sky, out[j].canvas) != (roi.sky, roi.canvas) {
                            roi.sky = out[j].sky;
                            roi.canvas = out[j].canvas;
                            changed = true;
                        }
                    }
                    if changed {
                        self.geometry_changed(ui);
                    }
                }
            }
        }
        if let Some(task) = &self.fresh_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Lost => self.fresh_task = None,
                Poll::Done(counts) => {
                    self.fresh_task = None;
                    self.fresh = counts.into_iter().collect();
                }
            }
        }
        changed
    }

    /// The ROIs as viewer overlays: the selected one solid in the accent
    /// colour, the others dashed.
    pub fn overlays(&self) -> Vec<Overlay> {
        self.rois
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let selected = self.selected == Some(i);
                Overlay {
                    rect: r.canvas.map(|v| v as f64),
                    color: if selected {
                        palette::ACCENT_400
                    } else {
                        palette::TEXT_BODY
                    },
                    label: r.name.clone(),
                    dashed: !selected,
                }
            })
            .collect()
    }

    /// The panel: "+ new ROI" (the viewer's draw tool), and each ROI with
    /// its size and freshness; the selected one with Name, Note and
    /// Delete. Selecting zooms `viewer` to the ROI. True if the list
    /// changed (written to `ui`); a rename or note edit does not count
    /// freshness again.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        cfg_ui: &mut Value,
        mut viewer: Option<&mut TiledViewer>,
    ) -> bool {
        ui.label(RichText::new("Regions of interest").color(palette::TEXT_STRONG));
        let drawing = viewer.as_ref().is_some_and(|v| v.tool() == Tool::DrawRect);
        if ui
            .add_enabled(viewer.is_some() && !drawing, egui::Button::new("+ new ROI"))
            .on_hover_text("Drag a rectangle on the image")
            .on_disabled_hover_text("Shown once there is a result to draw on")
            .clicked()
            && let Some(v) = viewer.as_deref_mut()
        {
            v.set_tool(Tool::DrawRect);
        }
        let muted = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).small().color(palette::TEXT_MUTED));
        };
        if drawing {
            muted(ui, "Drag on the image to draw; Esc cancels.");
        }
        if self.rois.is_empty() {
            muted(ui, "None yet.");
            return false;
        }
        let mut select = None;
        let mut delete = None;
        let mut edited = false;
        for i in 0..self.rois.len() {
            let selected = self.selected == Some(i);
            ui.push_id(("roi", i), |ui| {
                let roi = &mut self.rois[i];
                if ui.selectable_label(selected, &roi.name).clicked() {
                    select = Some(i);
                }
                let fresh = self
                    .fresh
                    .get(&roi.canvas)
                    .map_or_else(|| "…".to_string(), fresh_text);
                muted(ui, &format!("{} · {fresh}", roi_size_text(roi.canvas)));
                if selected {
                    ui.horizontal(|ui| {
                        let l = ui.label(RichText::new("Name").color(palette::TEXT_BODY));
                        edited |= ui
                            .text_edit_singleline(&mut roi.name)
                            .labelled_by(l.id)
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        let l = ui.label(RichText::new("Note").color(palette::TEXT_BODY));
                        edited |= ui
                            .text_edit_singleline(&mut roi.note)
                            .labelled_by(l.id)
                            .changed();
                    });
                    if ui.button("Delete").clicked() {
                        delete = Some(i);
                    }
                } else if !roi.note.is_empty() {
                    muted(ui, &roi.note);
                }
            });
        }
        if let Some(i) = select {
            self.selected = Some(i);
            if let Some(v) = viewer {
                v.zoom_to_rect(self.rois[i].canvas.map(|c| c as f64));
            }
        }
        if let Some(i) = delete {
            self.rois.remove(i);
            self.selected = None;
            self.geometry_changed(cfg_ui);
            return true;
        }
        if edited {
            // Names and notes only: saved, but nothing to recount.
            save_rois(cfg_ui, &self.rois);
        }
        edited
    }
}

/// How fresh an ROI is for the view's groups: "fresh", "stale (NN%
/// fresh)", "not blended" or "no tiles"; with several groups, each
/// prefixed by its name.
pub fn fresh_text(counts: &MemberCounts) -> String {
    let one = |c: &Option<(usize, usize)>| match c.map(fresh_fraction) {
        None => "not blended".to_string(),
        Some(None) => "no tiles".to_string(),
        Some(Some(f)) if f >= 1.0 => "fresh".to_string(),
        Some(Some(f)) => format!("stale ({:.0}% fresh)", (f * 100.0).floor()),
    };
    match counts.as_slice() {
        [(_, c)] => one(c),
        _ => counts
            .iter()
            .map(|(g, c)| format!("{g} {}", one(c)))
            .collect::<Vec<_>>()
            .join(", "),
    }
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

    #[test]
    fn fresh_text_per_member() {
        let one = |c| vec![("Ha".to_string(), c)];
        assert_eq!(fresh_text(&one(Some((2, 2)))), "fresh");
        assert_eq!(fresh_text(&one(Some((1, 3)))), "stale (33% fresh)");
        assert_eq!(fresh_text(&one(Some((0, 0)))), "no tiles");
        assert_eq!(fresh_text(&one(None)), "not blended");
        let two = vec![("Ha".to_string(), Some((4, 4))), ("OIII".to_string(), None)];
        assert_eq!(fresh_text(&two), "Ha fresh, OIII not blended");
    }
}
