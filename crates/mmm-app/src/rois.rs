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
//! worker: the list (new, select, rename, note, delete) and the
//! reprojection. Each ROI's row and canvas label show its transient
//! preview's state, as the window reports it ([`RoiPanel::set_states`],
//! from [`crate::previews`]); an ROI whose label carries controls (Compare
//! A / B, spec §8.3) has it drawn by the window, so its overlay carries no
//! label ([`RoiPanel::set_labelled`]): the label is drawn once. Selecting an
//! ROI asks to go to it ([`RoiOutcome::go_to`]): the window frames it on
//! whichever canvas shows, through the camera both share.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use egui::{Color32, RichText};
use mega_ui::canvas_layers::Overlay;
use mega_ui::theme::palette;
use mega_ui::widgets::ordered_list::{ListEvent, ListRow, ListState, ordered_list_with};
use mmm_core::astrometry::LinearWcs;
use mmm_core::reference::ReferenceFrame;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::previews::PreviewState;
use crate::util::{Poll, Task};

/// Key in `MosaicConfig.ui` of the ROI list.
pub const ROIS_KEY: &str = "rois";

/// A region of interest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roi {
    /// Identifies the ROI in the list (selection, rename, reorder, undo)
    /// whatever its place; unique in the list and never reused while the
    /// session is open. 0 in entries saved before ids existed:
    /// [`load_rois`] gives those fresh ones.
    #[serde(default)]
    pub id: u64,
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
/// entry is skipped and the rest kept. An entry with no id (0, or one
/// already taken) gets a fresh one above all those in the list; the next
/// save keeps it.
pub fn load_rois(ui: &Value) -> Vec<Roi> {
    let Some(list) = ui.get(ROIS_KEY).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut rois: Vec<Roi> = list
        .iter()
        .filter_map(|v| match serde_json::from_value::<Roi>(v.clone()) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("a saved ROI is unreadable and skipped: {e}");
                None
            }
        })
        .collect();
    let mut next = rois.iter().map(|r| r.id).max().unwrap_or(0) + 1;
    let mut seen = std::collections::BTreeSet::new();
    for r in &mut rois {
        if r.id == 0 || !seen.insert(r.id) {
            r.id = next;
            next += 1;
            seen.insert(r.id);
        }
    }
    rois
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

/// What the ROI list did this frame ([`RoiPanel::ui`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RoiOutcome {
    /// The list changed (written to `ui`).
    pub changed: bool,
    /// An ROI was picked: go to its canvas rect (`[x0, y0, x1, y1]`).
    pub go_to: Option<[u64; 4]>,
}

/// A name for a new ROI that none of `taken` has: "Region 1", "Region 2",
/// ….
pub fn unique_roi_name(taken: &[String]) -> String {
    (1..)
        .map(|i| format!("Region {i}"))
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

/// The session's ROIs: the list (kept in `MosaicConfig.ui`), the
/// selection, the sync worker and each ROI's preview state (the window
/// reports it). Methods that change the list write it into the session's
/// `ui` and return true; the view then saves (debounced).
#[derive(Default)]
pub struct RoiPanel {
    /// The ROIs, as in `ui.rois`.
    pub(crate) rois: Vec<Roi>,
    loaded: bool,
    /// The selected ROI.
    pub(crate) selected: Option<u64>,
    /// A pending reprojection: the list it started from and the result.
    sync: Option<Task<(Vec<Roi>, Vec<Roi>)>>,
    /// Reproject (read the frame) once possible.
    pub(crate) want_sync: bool,
    /// Each ROI's preview state, by id, as the window last reported it.
    states: BTreeMap<u64, PreviewState>,
    /// The ROIs whose canvas label the window draws (with its controls).
    labelled: BTreeSet<u64>,
    /// The list widget's own state (a rename in progress).
    list: ListState,
    /// One above every id given so far this session: ids are never reused.
    next_id: u64,
    /// The ROI the list just deleted, and its index ([`RoiPanel::take_deleted`]).
    deleted: Option<(usize, Roi)>,
}

impl RoiPanel {
    /// Read the list from `ui` the first time only, and ask for a sync.
    pub fn load_once(&mut self, ui: &Value) {
        if !self.loaded {
            self.loaded = true;
            self.rois = load_rois(ui);
            self.next_id = self.rois.iter().map(|r| r.id).max().unwrap_or(0) + 1;
            self.want_sync = true;
        }
    }

    /// The ROIs.
    pub fn rois(&self) -> &[Roi] {
        &self.rois
    }

    /// No reprojection running or wanted: the canvas rects follow the
    /// current frame (as far as the last analysis knows it).
    pub fn synced(&self) -> bool {
        self.sync.is_none() && !self.want_sync
    }

    /// No worker running.
    pub fn idle(&self) -> bool {
        self.sync.is_none()
    }

    /// Each ROI's preview state, by id (an ROI missing is out of date): its
    /// row says it, and its canvas label while it merges.
    pub fn set_states(&mut self, states: BTreeMap<u64, PreviewState>) {
        self.states = states;
    }

    /// The ROIs whose canvas label the window draws, with its controls:
    /// their overlays carry no label.
    pub fn set_labelled(&mut self, ids: BTreeSet<u64>) {
        self.labelled = ids;
    }

    /// The colour ROI `id` is drawn in (outline and label): the accent
    /// when selected.
    pub fn color(&self, id: u64) -> Color32 {
        if self.selected == Some(id) {
            palette::ACCENT_400
        } else {
            palette::TEXT_BODY
        }
    }

    /// ROI `id`'s preview state.
    fn state(&self, id: u64) -> PreviewState {
        self.states
            .get(&id)
            .copied()
            .unwrap_or(PreviewState::OutOfDate)
    }

    /// A rectangle drawn on the canvas (image coordinates of a
    /// `canvas`-sized result) becomes a new ROI "Region n", selected; its sky
    /// corners follow from the sync. True if the list changed.
    pub fn on_drawn(&mut self, ui: &mut Value, r: [f64; 4], canvas: (u64, u64)) -> bool {
        let Some(rect) = canvas_rect(r, canvas) else {
            return false;
        };
        let taken: Vec<String> = self.rois.iter().map(|r| r.name.clone()).collect();
        let id = self
            .next_id
            .max(self.rois.iter().map(|r| r.id).max().unwrap_or(0) + 1);
        self.next_id = id + 1;
        self.rois.push(Roi {
            id,
            name: unique_roi_name(&taken),
            note: String::new(),
            sky: None,
            canvas: rect,
        });
        self.selected = Some(id);
        self.want_sync = true;
        self.geometry_changed(ui);
        true
    }

    /// The rects changed (draw, delete, reprojection): keep the list in
    /// `ui`.
    fn geometry_changed(&mut self, ui: &mut Value) {
        save_rois(ui, &self.rois);
    }

    /// Start the sync (reading the frame at `frame`) on a worker when asked
    /// for.
    pub fn start_sync(&mut self, ctx: &egui::Context, frame: PathBuf) {
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
    }

    /// Apply a finished sync; true if the list changed (written to `ui`).
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
                        // By id (and only if it has not moved since).
                        let Some(j) = snapshot.iter().position(|s| {
                            s.id == roi.id && s.sky == roi.sky && s.canvas == roi.canvas
                        }) else {
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
        changed
    }

    /// The ROIs as canvas overlays (drawn on both canvases): the selected
    /// one solid in the accent colour, the others dashed; each labelled
    /// with its name, followed by " · merging…" while its preview merges,
    /// except those whose label the window draws ([`RoiPanel::set_labelled`]).
    pub fn overlays(&self) -> Vec<Overlay> {
        self.rois
            .iter()
            .map(|r| {
                let selected = self.selected == Some(r.id);
                Overlay {
                    rect: r.canvas.map(|v| v as f64),
                    color: self.color(r.id),
                    label: match self.state(r.id) {
                        _ if self.labelled.contains(&r.id) => String::new(),
                        PreviewState::Merging => format!("{} · merging…", r.name),
                        _ => r.name.clone(),
                    },
                    dashed: !selected,
                }
            })
            .collect()
    }

    /// Take the ROI the list just deleted, with the index it had: the
    /// window offers to undo it.
    pub fn take_deleted(&mut self) -> Option<(usize, Roi)> {
        self.deleted.take()
    }

    /// Put `roi` (deleted from `index`) back there (the end, if the list
    /// is shorter by now) and select it, keeping its id; one with its id in
    /// the list already is left alone. True if the list changed.
    pub fn restore(&mut self, ui: &mut Value, index: usize, roi: Roi) -> bool {
        if self.rois.iter().any(|r| r.id == roi.id) {
            return false;
        }
        self.next_id = self.next_id.max(roi.id + 1);
        self.selected = Some(roi.id);
        self.rois.insert(index.min(self.rois.len()), roi);
        self.want_sync = true;
        self.geometry_changed(ui);
        true
    }

    /// The ROI list: each ROI's name, size and state, with the note of the
    /// selected one under it. Picking one asks to go to it
    /// ([`RoiOutcome::go_to`]; the caller moves the camera); rename,
    /// reorder and delete are the list's ([`ordered_list_with`]), a delete
    /// is kept for [`RoiPanel::take_deleted`]. A change of the list is
    /// written to `ui` ([`RoiOutcome::changed`]).
    pub fn ui(&mut self, ui: &mut egui::Ui, cfg_ui: &mut Value) -> RoiOutcome {
        if self.rois.is_empty() {
            ui.label(
                RichText::new("None yet.")
                    .small()
                    .color(palette::TEXT_MUTED),
            );
            return RoiOutcome::default();
        }
        // Owned, so the note editor below may borrow the list.
        let shown: Vec<(u64, String, String, bool)> = self
            .rois
            .iter()
            .map(|r| {
                let state = self.state(r.id).text();
                let detail = format!("{} · {state}", roi_size_text(r.canvas));
                (r.id, r.name.clone(), detail, self.selected == Some(r.id))
            })
            .collect();
        let rows: Vec<ListRow> = shown
            .iter()
            .map(|(id, name, detail, selected)| ListRow {
                id: *id,
                label: name,
                detail: Some(detail),
                selected: *selected,
            })
            .collect();
        let mut note_edited = false;
        let rois = &mut self.rois;
        let events = ordered_list_with(ui, "rois", &rows, &mut self.list, |ui, id| {
            if let Some(roi) = rois.iter_mut().find(|r| r.id == id) {
                ui.horizontal(|ui| {
                    let l = ui.label(RichText::new("Note").color(palette::TEXT_BODY));
                    note_edited |= ui
                        .text_edit_singleline(&mut roi.note)
                        .labelled_by(l.id)
                        .changed();
                });
            }
        });
        let mut out = RoiOutcome::default();
        let mut rewrite = note_edited;
        let mut deleted = false;
        for event in events {
            match event {
                ListEvent::Select(id) => self.selected = Some(id),
                ListEvent::Activate(id) => {
                    self.selected = Some(id);
                    out.go_to = self.rois.iter().find(|r| r.id == id).map(|r| r.canvas);
                }
                ListEvent::Rename(id, name) => {
                    if let Some(r) = self.rois.iter_mut().find(|r| r.id == id) {
                        r.name = name;
                        rewrite = true;
                    }
                }
                ListEvent::Move { id, to } => {
                    if let Some(from) = self.rois.iter().position(|r| r.id == id) {
                        let roi = self.rois.remove(from);
                        self.rois.insert(to.min(self.rois.len()), roi);
                        rewrite = true;
                    }
                }
                ListEvent::Delete(id) => {
                    if let Some(at) = self.rois.iter().position(|r| r.id == id) {
                        let roi = self.rois.remove(at);
                        if self.selected == Some(id) {
                            self.selected = None;
                        }
                        self.deleted = Some((at, roi));
                        deleted = true;
                    }
                }
            }
        }
        if deleted {
            self.geometry_changed(cfg_ui);
            out.go_to = None;
            out.changed = true;
        } else if rewrite {
            // Names, order and notes only.
            save_rois(cfg_ui, &self.rois);
            out.changed = true;
        }
        out
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
            id: 1,
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
        let taken = ["Region 1".to_string(), "Region 3".to_string()];
        assert_eq!(unique_roi_name(&taken), "Region 2");

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
    fn new_rois_are_named_region_n() {
        assert_eq!(unique_roi_name(&[]), "Region 1");
        // The first unused number; names the user chose are no obstacle.
        let taken = ["ROI 1", "Core", "Region 2"].map(String::from);
        assert_eq!(unique_roi_name(&taken), "Region 1");
        let taken = ["Region 1", "Region 2"].map(String::from);
        assert_eq!(unique_roi_name(&taken), "Region 3");

        // A drawn rect takes it, with an id, at the end of the list.
        let mut panel = RoiPanel::default();
        let mut ui = serde_json::json!({});
        assert!(panel.on_drawn(&mut ui, [0.0, 0.0, 50.0, 40.0], (1000, 1000)));
        assert!(panel.on_drawn(&mut ui, [10.0, 10.0, 60.0, 60.0], (1000, 1000)));
        let names: Vec<_> = panel.rois().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Region 1", "Region 2"]);
        let (a, b) = (panel.rois()[0].id, panel.rois()[1].id);
        assert!(a != 0 && b != 0 && a != b);
        assert_eq!(panel.selected, Some(b));
        // Deleting the last does not free its id for the next one.
        let gone = panel.rois.pop().unwrap();
        assert!(panel.on_drawn(&mut ui, [0.0, 0.0, 9.0, 9.0], (1000, 1000)));
        assert_ne!(panel.rois()[1].id, gone.id);
        assert_eq!(load_rois(&ui), panel.rois());
    }

    #[test]
    fn old_rois_get_ids_and_keep_names() {
        // Entries saved before ids existed (and a clash): fresh unique ids
        // above those taken; names, notes and rects as they were.
        let ui = serde_json::json!({ ROIS_KEY: [
            { "name": "ROI 1", "note": "kept", "canvas": [0, 0, 10, 10] },
            { "id": 5, "name": "Core", "canvas": [0, 0, 20, 20] },
            { "name": "ROI 2", "canvas": [5, 5, 30, 30] },
            { "id": 5, "name": "Clash", "canvas": [1, 1, 2, 2] }
        ]});
        let loaded = load_rois(&ui);
        let ids: Vec<u64> = loaded.iter().map(|r| r.id).collect();
        assert_eq!(ids[1], 5, "an id already there is kept");
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 4, "{ids:?}");
        assert!(ids.iter().all(|&i| i != 0));
        assert!(ids[0] > 5 && ids[2] > 5 && ids[3] > 5);
        let names: Vec<_> = loaded.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["ROI 1", "Core", "ROI 2", "Clash"]);
        assert_eq!(loaded[0].note, "kept");
        assert_eq!(loaded[2].canvas, [5, 5, 30, 30]);
        // Saved back, they stay.
        let mut ui = ui;
        save_rois(&mut ui, &loaded);
        assert_eq!(load_rois(&ui), loaded);
        // A panel that loaded them goes on from the highest.
        let mut panel = RoiPanel::default();
        panel.load_once(&ui);
        let top = panel.rois().iter().map(|r| r.id).max().unwrap();
        assert!(panel.on_drawn(&mut ui, [0.0, 0.0, 9.0, 9.0], (1000, 1000)));
        assert!(panel.rois().last().unwrap().id > top);
    }
}
