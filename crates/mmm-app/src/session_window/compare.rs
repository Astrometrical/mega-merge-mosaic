//! Compare A / B on an ROI preview (spec §8.3), on the ROIs' canvas labels.
//!
//! - **The labels.** An ROI up to date for the groups in view (while no
//!   analysis runs) has its label drawn here, with **Compare**, on one row:
//!   name, **Compare**, "up to date". The compared ROI's label holds, on its
//!   first row, the name, the **A | B** toggle, **Use A** and **Done**; the
//!   shown side's settings ("A · feather 40 px · flatten 2", "B · …", or
//!   "B · merging…" while B merges) go on a second row, so the buttons never
//!   move. Other ROIs keep the label the canvas paints. A label is drawn
//!   once: the canvas paints none for an ROI labelled here.
//! - **Compare** freezes the ROI's preview as A ([`Previews::start_compare`],
//!   with the draft it was made from); B, the live preview, is selected.
//!   One ROI at a time: Compare on another ends the current one first.
//! - **A | B** is a toggle: the selected side stays shown. Three momentary
//!   switches show the other side while held ([`Peek`]): Space (not while
//!   typing or in a dialog; letting go ends it wherever the keyboard focus
//!   has gone), pressing and holding the selected button (clicking the
//!   other switches to it), and pressing and holding the left mouse button
//!   on the compared ROI's preview on either canvas (a drag past the click
//!   distance still pans, and ends the peek). The label's accessible
//!   description says what shows, and "Peeking…" while a switch is held.
//! - **Use A** puts A's settings back in the draft through the Merge
//!   settings' own draft edit (saved with the window, as any draft edit),
//!   and leaves compare; A's preview is then the ROI's preview. **Done**
//!   leaves compare, keeping the draft.
//! - **Leaving.** Compare ends when its ROI is deleted or moved
//!   ([`Previews`]), hidden (the ROIs toggle), when the view's groups
//!   change, when an analysis starts and when the session is left.
//!
//! [`Previews`]: crate::previews::Previews
//! [`Previews::start_compare`]: crate::previews::Previews::start_compare

use std::collections::{BTreeMap, BTreeSet};

use egui::Modifiers;
use mega_ui::widgets::{CanvasLabel, LabelControl, canvas_label};
use mmm_core::settings::BlendSettings;

use super::shortcuts::{PEEK, keyed};
use super::{CanvasMode, SessionWindow};
use crate::blend_settings::{feather_text, load_draft};
use crate::jobs::JobRunner;
use crate::previews::{Peek, PreviewState, Side};
use crate::session::OpenSession;

/// **Compare**'s tooltip.
const COMPARE_TIP: &str = "Freeze this preview as A, then change the Merge settings to compare                            B, the live preview, with it";
/// **Use A**'s tooltip.
const USE_A_TIP: &str = "Put A's settings back in the Merge settings and leave compare";
/// **Done**'s tooltip.
const DONE_TIP: &str = "Leave compare, keeping the Merge settings as they are";

/// "{side} · feather {n} px · flatten {f}": what the side was (A) or is
/// being (B) merged with.
fn settings_text(side: Side, s: &BlendSettings) -> String {
    let flatten = s.flatten.map_or("off".to_string(), |f| f.to_string());
    format!(
        "{} · feather {} px · flatten {flatten}",
        side.letter(),
        feather_text(s.feather_px)
    )
}

/// What a label's controls asked for this frame.
enum Action {
    /// **Compare** on this ROI.
    Compare(u64),
    /// A or B chosen with the toggle.
    Select(Side),
    /// **Use A**.
    UseA,
    /// **Done**.
    Done,
}

/// The toggle's option index of `side` (A first).
fn index(side: Side) -> usize {
    match side {
        Side::A => 0,
        Side::B => 1,
    }
}

impl SessionWindow {
    /// The ROIs whose canvas label is drawn here, with controls: each up to
    /// date for the groups in view (`states`) while no analysis runs or is
    /// queued, and the one compared. None while the ROIs are hidden.
    pub(super) fn labelled_rois(
        &self,
        states: &BTreeMap<u64, PreviewState>,
        jobs: &JobRunner,
    ) -> BTreeSet<u64> {
        if !self.look.rois {
            return BTreeSet::new();
        }
        let mut out: BTreeSet<u64> = if jobs.analyzing() {
            BTreeSet::new()
        } else {
            states
                .iter()
                .filter(|(_, s)| **s == PreviewState::UpToDate)
                .map(|(id, _)| *id)
                .collect()
        };
        out.extend(self.previews.compare().map(|c| c.roi));
        out
    }

    /// End the compare once its view no longer holds: the ROIs hidden, an
    /// analysis running or queued, or the view's groups other than those
    /// A was frozen for (its ROI deleted or moved: the previews end it).
    pub(super) fn follow_compare(&mut self, jobs: &JobRunner) {
        let Some(c) = self.previews.compare() else {
            return;
        };
        let groups: BTreeSet<&String> = c.a.keys().collect();
        let members: BTreeSet<&String> = self.mosaic.members().iter().collect();
        if !self.look.rois || jobs.analyzing() || groups != members {
            self.previews.end_compare();
        }
    }

    /// Space, the momentary switch of the keyboard: while comparing, a press
    /// (not while `typing`: a text field has the keyboard, or a dialog is
    /// open) starts a peek and is consumed; the peek lasts while the key is
    /// down, so letting go ends it wherever the keyboard focus has gone.
    pub(super) fn follow_peek_key(&mut self, ctx: &egui::Context, typing: bool) {
        let comparing = self.previews.compare().is_some();
        let pressed = comparing
            && !typing
            && ctx.input_mut(|i| i.modifiers.is_none() && i.consume_key(Modifiers::NONE, PEEK));
        let down = ctx.input(|i| i.key_down(PEEK));
        self.peek_key = comparing && down && (self.peek_key || pressed);
        if self.previews.set_peek(Peek::Space, self.peek_key) {
            ctx.request_repaint();
        }
    }

    /// The labels with controls over the canvas shown (drawn after it, so on
    /// top), and what they and a press held on the compared preview asked
    /// for. Nothing while the canvas shows no ROIs or was not drawn this
    /// frame.
    pub(super) fn roi_labels(&mut self, ui: &mut egui::Ui, session: &mut OpenSession) {
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        let plan = self.mode() == CanvasMode::Plan;
        let frame = if plan {
            self.plan.canvas_frame()
        } else {
            self.mosaic.viewer_frame()
        };
        let shows_rois = self.look.rois && (!plan || self.plan.frame_matches(session));
        let canvas = frame
            .filter(|(_, t)| *t == now && shows_rois)
            .map(|(rect, _)| rect)
            .zip(self.shown_camera());
        let mut actions = Vec::new();
        let mut button_held = false;
        let mut preview_held = false;
        if let Some((rect, cam)) = canvas {
            let to_screen = |p: (f64, f64)| {
                rect.center()
                    + egui::vec2(
                        ((p.0 - cam.centre.0) * cam.zoom) as f32,
                        ((p.1 - cam.centre.1) * cam.zoom) as f32,
                    )
            };
            let labelled: Vec<_> = self
                .mosaic
                .rois()
                .iter()
                .filter(|r| self.labelled.contains(&r.id))
                .cloned()
                .collect();
            for roi in labelled {
                let c = roi.canvas.map(|v| v as f64);
                let on_screen =
                    egui::Rect::from_two_pos(to_screen((c[0], c[1])), to_screen((c[2], c[3])));
                if !on_screen.intersects(rect) {
                    continue;
                }
                let (resp, compared) = self.roi_label(ui, session, &roi, on_screen.min, rect);
                match compared {
                    Some(selected) => {
                        button_held = resp.held == Some((0, index(selected)));
                        match resp.clicked {
                            Some((0, k)) => {
                                actions.push(Action::Select(if k == 0 { Side::A } else { Side::B }))
                            }
                            Some((1, _)) => actions.push(Action::UseA),
                            Some((2, _)) => actions.push(Action::Done),
                            _ => {}
                        }
                    }
                    None => {
                        if resp.clicked.is_some() {
                            actions.push(Action::Compare(roi.id));
                        }
                    }
                }
            }
            let hold = if plan {
                self.plan.hold()
            } else {
                self.mosaic.hold()
            };
            preview_held = match (self.previews.compare().and_then(|c| c.rect()), hold) {
                (Some(r), Some(h)) => {
                    let (x, y) = h.origin;
                    !h.moved
                        && x >= r[0] as f64
                        && x < r[2] as f64
                        && y >= r[1] as f64
                        && y < r[3] as f64
                }
                _ => false,
            };
        }
        for action in actions {
            match action {
                Action::Compare(roi) => {
                    let draft = load_draft(&session.mosaic.config);
                    let groups = self.mosaic.members().to_vec();
                    self.previews.start_compare(roi, &groups, draft);
                }
                Action::Select(side) => self.previews.select_side(side),
                Action::UseA => {
                    if let Some(a) = self.previews.use_a() {
                        self.mosaic.edit_draft(session, a);
                    }
                }
                Action::Done => self.previews.end_compare(),
            }
            ctx.request_repaint();
        }
        let a = self.previews.set_peek(Peek::Button, button_held);
        let b = self.previews.set_peek(Peek::Preview, preview_held);
        if a || b {
            ctx.request_repaint();
        }
    }

    /// Draw ROI `roi`'s label at screen point `corner` over the canvas in
    /// `viewport`: the compare controls if it is compared (with the side
    /// selected), else **Compare**.
    fn roi_label(
        &self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        roi: &crate::rois::Roi,
        corner: egui::Pos2,
        viewport: egui::Rect,
    ) -> (mega_ui::widgets::LabelResponse, Option<Side>) {
        let name = roi.name.as_str();
        let accessible = format!("{name} on the canvas");
        let color = self.mosaic.roi_color(roi.id);
        let compared = self.previews.compare().filter(|c| c.roi == roi.id);
        let Some(c) = compared else {
            let compare = format!("Compare {name}");
            let state = PreviewState::UpToDate.text();
            let controls = [LabelControl::Button {
                text: "Compare",
                accessible: &compare,
                tooltip: Some(COMPARE_TIP),
                primary: false,
            }];
            let label = CanvasLabel {
                corner,
                clip: viewport,
                name,
                color,
                controls: &controls,
                text: state,
                text_below: false,
                accessible: &accessible,
                description: state,
            };
            return (canvas_label(ui, ("roi-label", roi.id), &label), None);
        };
        let shown = c.shown();
        let text = match shown {
            Side::A => settings_text(Side::A, &c.a_settings),
            Side::B => {
                let merging = self.previews.state(roi.id, self.mosaic.members(), session)
                    == PreviewState::Merging;
                if merging {
                    "B · merging…".to_string()
                } else {
                    settings_text(Side::B, &load_draft(&session.mosaic.config))
                }
            }
        };
        let description = if c.peeking() {
            format!("Peeking while held: {text}")
        } else {
            format!("Showing {text}")
        };
        let (show_a, show_b) = (format!("Show A of {name}"), format!("Show B of {name}"));
        let (use_a, done) = (
            format!("Use A for {name}"),
            format!("Done comparing {name}"),
        );
        let toggle_tip = keyed(
            "A: the preview as compare began. B: the Merge settings now. Hold the selected one, \
             or the preview, to see the other",
            PEEK,
        );
        let controls = [
            LabelControl::Toggle {
                options: ["A", "B"],
                accessible: [&show_a, &show_b],
                selected: index(c.selected),
                shown: index(shown),
                tooltip: Some(&toggle_tip),
            },
            LabelControl::Button {
                text: "Use A",
                accessible: &use_a,
                tooltip: Some(USE_A_TIP),
                primary: false,
            },
            LabelControl::Button {
                text: "Done",
                accessible: &done,
                tooltip: Some(DONE_TIP),
                primary: true,
            },
        ];
        let label = CanvasLabel {
            corner,
            clip: viewport,
            name,
            color,
            controls: &controls,
            text: &text,
            text_below: true,
            accessible: &accessible,
            description: &description,
        };
        let selected = c.selected;
        (
            canvas_label(ui, ("roi-label", roi.id), &label),
            Some(selected),
        )
    }
}
