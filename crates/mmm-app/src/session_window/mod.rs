//! The session window (spec §5, §7.1–§7.3): one canvas showing the plan
//! ([`PlanView`]) or the merged mosaic ([`MosaicView`]) of the group or
//! composite chosen, with a view bar across its top, and the inspector at
//! its right.
//!
//! - **One view, one camera.** The window keeps the view (a group or a
//!   composite) and pushes it into both canvases, so they always agree.
//!   Switching between plan and mosaic, or to another view, hands the shown
//!   canvas's [`Camera`] to both first, so nothing moves.
//! - **Mode.** The window opens on the mosaic once something is merged
//!   ([`OpenSession::merged`]), else on the plan; **Mosaic** cannot be
//!   chosen before something is merged or a merge has started.
//!   [`SessionWindow::merge`] merges every group and shows the mosaic.
//! - **View bar** ([`canvas`]): Plan / Mosaic, a chip per group and per
//!   composite then **+** (with two or more groups), and the Outlines, ROIs,
//!   Linked and Strong toggles.
//! - **Inspector** ([`sections`]): Problems, Panels, ROIs, Merge settings,
//!   Composites and Notes, in that order.
//! - **Every frame.** The shell drives the window's hooks whether or not the
//!   window is drawn: [`SessionWindow::on_event`] for each job event,
//!   [`SessionWindow::poll_settings`], [`SessionWindow::track_analysis`]
//!   and [`SessionWindow::poll`] (the merged mosaic's work, the plan's
//!   texture release and the window's saves).
//! - **Saves.** The display stretch toggles, the composites and the notes
//!   live in the session's `MosaicConfig.ui`, changed at once in memory and
//!   written to `mosaic.json` on the UI thread at most once per
//!   [`SAVE_INTERVAL`](crate::util::SAVE_INTERVAL) ([`Debounce`]; the
//!   documented exception). The session holds the lock, so the notes are
//!   saved with it (the start screen reads them there).

mod canvas;
mod sections;

use std::path::PathBuf;
use std::time::Instant;

use mega_ui::inspector::{InspectorState, inspector_width};
use mega_ui::theme::palette;
use mega_ui::widgets::NotesMode;
use mmm_core::checks::Check;
use mmm_core::settings::ChangeImpact;
use serde_json::Value;

use crate::camera::Camera;
use crate::checks::ChecksEngine;
use crate::compositions::{Composition, ViewKind, load_compositions};
use crate::jobs::{JobEvent, JobRunner, JobTarget};
use crate::mosaic_view::MosaicView;
use crate::pipeline::Pipeline;
use crate::plan_view::{CanvasLook, PlanView};
use crate::session::OpenSession;
use crate::util::{Debounce, group_names};

/// What the canvas shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasMode {
    /// The panels in place, from their previews.
    Plan,
    /// The merged mosaic.
    Mosaic,
}

/// What the window asks of the shell after a frame.
#[derive(Debug, Default)]
pub struct SessionOutput {
    /// Accept these checks ([`ChecksEngine::accept`]).
    pub accept: Vec<Check>,
}

/// The open session's window: canvas, view bar and inspector (see the
/// module docs).
pub struct SessionWindow {
    plan: PlanView,
    mosaic: MosaicView,
    inspector: InspectorState,
    /// `None` until the first frame picks it from the session.
    mode: Option<CanvasMode>,
    /// The view both canvases show.
    view: Option<ViewKind>,
    /// Outlines and ROIs; the stretch is the session's, read each frame.
    look: CanvasLook,
    /// Problems lists every check, notes and passed ones too.
    show_all_checks: bool,
    notes_mode: NotesMode,
    /// The panel selected on the plan last frame (a new one opens Panels).
    selected: Option<PathBuf>,
    /// The view bar's choices' and toggles' one-row widths, as last drawn.
    bar_widths: Option<(f32, f32)>,
    save: Debounce,
}

impl Default for SessionWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// Inspector section ids, in their fixed order.
const PROBLEMS: &str = "problems";
const PANELS: &str = "panels";
const ROIS: &str = "rois";
const MERGE_SETTINGS: &str = "merge-settings";
const COMPOSITES: &str = "composites";
const NOTES: &str = "notes";

impl SessionWindow {
    /// A window for a session just opened: the first group, Problems open.
    pub fn new() -> Self {
        let mut inspector = InspectorState::default();
        inspector.toggle(PROBLEMS);
        SessionWindow {
            plan: PlanView::new(),
            mosaic: MosaicView::new(),
            inspector,
            mode: None,
            view: None,
            look: CanvasLook::default(),
            show_all_checks: false,
            notes_mode: NotesMode::default(),
            selected: None,
            bar_widths: None,
            save: Debounce::default(),
        }
    }

    /// What the canvas shows (the plan until the first frame has looked at
    /// the session).
    pub fn mode(&self) -> CanvasMode {
        self.mode.unwrap_or(CanvasMode::Plan)
    }

    /// Show the plan or the mosaic, where the shown canvas looks.
    pub fn set_mode(&mut self, mode: CanvasMode) {
        if self.mode == Some(mode) {
            return;
        }
        self.carry_camera();
        self.mode = Some(mode);
    }

    /// The group or composite shown (once the first frame has picked one).
    pub fn view(&self) -> Option<&ViewKind> {
        self.view.as_ref()
    }

    /// Show `view` on both canvases, where the shown canvas looks (one that
    /// no longer exists falls back to the first group).
    pub fn select_view(&mut self, view: ViewKind) {
        if self.view.as_ref() == Some(&view) {
            return;
        }
        self.carry_camera();
        self.plan.select_view(view.clone());
        self.mosaic.select_view(view.clone());
        self.view = Some(view);
    }

    /// Merge every group over the whole canvas and show the mosaic, where
    /// the merge goes; nothing while
    /// [`MosaicView::merge_block_reason`] gives a reason.
    pub fn merge(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if self.mosaic.merge_block_reason(session, jobs).is_some() {
            return;
        }
        self.mosaic.merge_all(session, jobs);
        self.set_mode(CanvasMode::Mosaic);
    }

    /// The inspector's fold and open sections.
    #[allow(dead_code)] // used by the top bar from Task 8 on
    pub fn inspector(&self) -> &InspectorState {
        &self.inspector
    }

    /// The inspector, to open a section ([`InspectorState::reveal`]).
    #[allow(dead_code)] // used by the top bar from Task 8 on
    pub fn inspector_mut(&mut self) -> &mut InspectorState {
        &mut self.inspector
    }

    /// The plan canvas.
    pub fn plan(&self) -> &PlanView {
        &self.plan
    }

    /// The merged mosaic canvas.
    pub fn mosaic(&self) -> &MosaicView {
        &self.mosaic
    }

    /// The merged mosaic canvas, to start merges and exports.
    pub fn mosaic_mut(&mut self) -> &mut MosaicView {
        &mut self.mosaic
    }

    /// React to a job event (from [`JobRunner::poll`]): the mosaic follows
    /// it, and a finished merge marks the session merged.
    pub fn on_event(&mut self, session: &mut OpenSession, jobs: &JobRunner, event: &JobEvent) {
        self.mosaic.on_event(event);
        if let JobEvent::Finished { id, .. } = event
            && matches!(jobs.target(*id), Some((JobTarget::Blend, _)))
        {
            session.merged = true;
        }
    }

    /// See [`MosaicView::track_analysis`].
    pub fn track_analysis(&mut self, id: u64) {
        self.mosaic.track_analysis(id);
    }

    /// See [`MosaicView::settings_editing`].
    pub fn settings_editing(&self, session: &OpenSession) -> bool {
        self.mosaic.settings_editing(session)
    }

    /// See [`MosaicView::poll_settings`]. Call every frame after the
    /// runner's poll, whether or not the window is drawn.
    pub fn poll_settings(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        self.mosaic.poll_settings(ctx, session, jobs)
    }

    /// The window's every-frame work, drawn or not: pick the mode and the
    /// view, the mosaic's work ([`MosaicView::poll`]), the plan's texture
    /// release, and the rate-limited save. Call every frame after the
    /// runner's poll and [`SessionWindow::track_analysis`], before drawing.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.follow_session(session);
        self.mosaic.poll(ctx, session, jobs);
        self.plan.release_if_hidden(Instant::now());
        self.save_if_due(ctx, session);
    }

    /// The shown view's key for the summary: a group name, or a
    /// composite's key (read from `ui`, the session's UI settings).
    pub fn view_key(&self, ui: &Value) -> Option<String> {
        self.mosaic.view_key(ui)
    }

    /// Save a pending (rate-limited) change now, whatever the rate limit:
    /// the window is closing. A small `mosaic.json` write.
    pub fn flush_save(&mut self, session: &OpenSession) {
        if self.save.pending() {
            self.save.saved(Instant::now());
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("session settings not saved: {e}");
            }
        }
        self.mosaic.flush_save(session);
    }

    /// Mark a change as unsaved (tests).
    #[cfg(test)]
    pub(crate) fn touch_save(&mut self) {
        self.save.touch();
    }

    /// Draw the window filling `ui`: the inspector at the right, the view
    /// bar and the canvas in the rest, and the export dialog when open.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &mut Pipeline,
        checks: &ChecksEngine,
    ) -> SessionOutput {
        let ctx = ui.ctx().clone();
        self.follow_session(session);
        let mut out = SessionOutput::default();
        egui::Panel::right("session-inspector")
            .resizable(false)
            .exact_size(inspector_width(&self.inspector))
            .show(ui, |ui| {
                self.inspector_ui(ui, session, jobs, pipeline, checks, &mut out);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| {
                self.view_bar(ui, session, jobs, pipeline);
                self.canvas_ui(ui, session, pipeline, checks);
            });
        self.mosaic.export_ui(&ctx, session, jobs);
        out
    }

    /// Pick the mode on the first call, and keep the view one the session
    /// has (else its first group) in both canvases.
    fn follow_session(&mut self, session: &OpenSession) {
        if self.mode.is_none() {
            self.mode = Some(if session.merged {
                CanvasMode::Mosaic
            } else {
                CanvasMode::Plan
            });
        }
        let groups = group_names(session);
        let compositions = compositions_of(session);
        let valid = match &self.view {
            Some(ViewKind::Group(g)) => groups.contains(g),
            Some(ViewKind::Composition(i)) => *i < compositions.len(),
            None => false,
        };
        if !valid {
            self.view = groups.first().cloned().map(ViewKind::Group);
        }
        if let Some(view) = &self.view {
            if self.plan.view() != Some(view) {
                self.plan.select_view(view.clone());
            }
            if self.mosaic.view() != Some(view) {
                self.mosaic.select_view(view.clone());
            }
        }
    }

    /// Where the shown canvas looks, if it has been drawn (or was handed a
    /// camera).
    fn shown_camera(&self) -> Option<Camera> {
        match self.mode() {
            CanvasMode::Plan => self.plan.camera(),
            CanvasMode::Mosaic => self.mosaic.camera(),
        }
    }

    /// Hand the shown canvas's camera to both canvases, before the mode or
    /// the view changes: whichever is drawn next looks at the same place.
    fn carry_camera(&mut self) {
        if let Some(c) = self.shown_camera() {
            self.plan.set_camera(c);
            self.mosaic.set_camera(c);
        }
    }

    /// Save a pending change once the rate limit allows (documented
    /// exception: a small `mosaic.json` write on the UI thread).
    fn save_if_due(&mut self, ctx: &egui::Context, session: &OpenSession) {
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("session settings not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }
}

/// The session's composites (none with grouping off: one group to show).
fn compositions_of(session: &OpenSession) -> Vec<Composition> {
    if session.mosaic.config.grouping.enabled {
        load_compositions(&session.mosaic.config.ui)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
