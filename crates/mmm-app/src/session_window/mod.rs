//! The session window (spec §5–§7.3): the top bar across the window, then
//! one canvas showing the plan ([`PlanView`]) or the merged mosaic
//! ([`MosaicView`]) of the group or composite chosen, with a view bar
//! across its top, and the inspector at its right.
//!
//! - **Top bar** ([`top_bar`]): ‹ (back to the start screen,
//!   [`SessionOutput::back`]; not while an export runs), the session name,
//!   the step strip ([`steps`]: Panels · Checks · Merge · Export, with the
//!   work in progress) and the main button (**Add panels…**, **Merge**
//!   with the merge settings' summary, or **Export…**).
//! - **Panel list** ([`PanelList`]): while open it is a resizable column
//!   at the left of the canvas (about half the window wide at first, never
//!   narrower than [`PANEL_LIST_MIN_WIDTH`], never so wide that the canvas
//!   gets narrower than [`CANVAS_MIN_WIDTH`]), so the view bar and the plan
//!   stay beside it; the Panels step opens and closes it, and switching
//!   between plan and mosaic leaves it as it is. A session without panels
//!   opens on it. Files dropped on the window are added whatever shows
//!   ([`SessionWindow::poll_panels`]). Rows and the plan are linked (spec
//!   §7.4): a row pointed at lights its panel's outline on the plan and a
//!   panel pointed at on the plan lights its row (the panels the view
//!   shows); a row clicked selects its panel on the plan (the inspector
//!   shows it), and a panel clicked on the plan selects its row, brought
//!   into view. Esc clears the selection in both.
//! - **One view, one camera.** The window keeps the view (a group or a
//!   composite) and pushes it into both canvases, so they always agree.
//!   Switching between plan and mosaic, or to another view, hands the shown
//!   canvas's [`Camera`] to both first, so nothing moves. Going to an ROI
//!   ([`SessionWindow::frame`]) hands both the camera that frames it.
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
//!   [`SessionWindow::poll_panels`], [`SessionWindow::poll_settings`],
//!   [`SessionWindow::track_analysis`] and [`SessionWindow::poll`] (the
//!   merged mosaic's work, the plan's texture release and the window's
//!   saves).
//! - **View state** ([`view_state`]): plan or mosaic, the view, the camera,
//!   the inspector's fold and open sections, the panel list and the
//!   Outlines and ROIs toggles are written into `ui` whenever they change.
//!   The first frame restores them: the mosaic only if it can show, else
//!   the plan; a view the session no longer has falls back to the first
//!   group (and the camera saved with it is dropped); the camera is handed
//!   to both canvases before either draws, so each paints its first frame
//!   through it (no fit is painted first). With nothing saved: the mosaic
//!   once something is merged, the first group, Problems open, Outlines and
//!   ROIs on.
//! - **Shortcuts** ([`shortcuts`]): `I`, `M`, `[` / `]`, `O`, `R`, `L`, `S`
//!   and `Ctrl+Enter`, each named in its control's tooltip, `Delete`
//!   (removes the panels selected in the open list) and `Esc` (clears the
//!   selection); none while a text field has the keyboard or the Export
//!   dialog is open.
//! - **Saves.** The view state, the display stretch toggles, the composites,
//!   the notes and the user's grouping choice (noted by the panel list,
//!   [`PanelList::take_ui_changed`]) live in the session's
//!   `MosaicConfig.ui`, changed at once
//!   in memory and written to `mosaic.json` on the UI thread at most once
//!   per [`SAVE_INTERVAL`](crate::util::SAVE_INTERVAL) ([`Debounce`]; the
//!   documented exception), and at once by
//!   [`SessionWindow::flush_save`] (the shell's
//!   [`flush_pending_saves`](crate::app::Shell::flush_pending_saves): on
//!   closing, eframe's periodic save and back to the start screen). The
//!   session holds the lock, so the notes are saved with it (the start
//!   screen reads them there).

mod canvas;
mod sections;
mod shortcuts;
mod steps;
mod top_bar;
pub mod view_state;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use mega_ui::inspector::{InspectorState, inspector_width};
use mega_ui::theme::palette;
use mega_ui::theme::tokens::{CANVAS_MIN_WIDTH, PANEL_LIST_MIN_WIDTH, space};
use mega_ui::widgets::NotesMode;
use mmm_core::checks::Check;
use mmm_core::mosaic::{Grouping, PanelEntry};
use mmm_core::session_summary::ResultState;
use mmm_core::settings::ChangeImpact;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::camera::Camera;
use crate::checks::ChecksEngine;
use crate::compositions::{Composition, ViewKind, composition_stretch_key, load_compositions};
use crate::jobs::{JobEvent, JobRunner, JobTarget};
use crate::mosaic_view::MosaicView;
use crate::panel_edits::FilesPicker;
use crate::panel_list::{PanelList, PanelListOutput};
use crate::pipeline::Pipeline;
use crate::plan_view::{CanvasLook, PlanView};
use crate::session::OpenSession;
use crate::util::{Debounce, group_names};
use view_state::ViewState;

/// What the canvas shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    /// ‹ was clicked: back to the start screen
    /// ([`crate::app::Shell::close_session`]).
    pub back: bool,
    /// Toasts to show, each with the undo its action button runs.
    pub toasts: Vec<WindowToast>,
}

/// What an Undo toast's action restores ([`SessionWindow::undo`]): only
/// what its action changed, into the inputs as they are by then (later,
/// unrelated edits stay).
#[derive(Clone, Debug)]
pub enum Undo {
    /// Put back removed panels, each as it was (its manual group too) at
    /// its index in the inputs before the removal (or the end, if fewer
    /// panels are left); one already back is left alone.
    Readd(Vec<(usize, PanelEntry)>),
    /// Put back the grouping from before the automatic grouping by FILTER
    /// (the panels as they are now), noting that the user has chosen how to
    /// group.
    Grouping(Grouping),
}

/// A toast the window asks the shell to show.
#[derive(Debug)]
pub struct WindowToast {
    /// What to show; its action (if any) runs `undo`.
    pub toast: mega_ui::widgets::Toast,
    /// What the toast's action restores.
    pub undo: Option<Undo>,
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
    /// The plan's selected panel as last seen: a new one opens Panels, and
    /// one picked on the plan selects its row in the panel list.
    selected: Option<PathBuf>,
    /// The view bar's choices' and toggles' one-row widths, as last drawn.
    bar_widths: Option<(f32, f32)>,
    save: Debounce,
    /// The view as last written into the session's `ui` (or found there).
    saved_view: Option<ViewState>,
    /// The panel list: the files and their grouping.
    panels: PanelList,
    /// Toasts raised since the last frame, handed to the shell by `ui`.
    toasts: Vec<WindowToast>,
    /// The panel list shows at the left of the canvas.
    pub(crate) panel_list_open: bool,
    /// A text field had the keyboard when the last frame ended: an Esc now
    /// left it (egui lets go of the field before the shortcuts see the key).
    was_typing: bool,
    /// The top bar's rect, as last drawn (tests).
    #[cfg(test)]
    pub(crate) top_bar_rect: Option<egui::Rect>,
    /// The panel list's rect, as last drawn (tests).
    #[cfg(test)]
    pub(crate) list_rect: Option<egui::Rect>,
}

/// Inspector section ids, in their fixed order.
const PROBLEMS: &str = "problems";
const PANELS: &str = "panels";
const ROIS: &str = "rois";
const MERGE_SETTINGS: &str = "merge-settings";
const COMPOSITES: &str = "composites";
const NOTES: &str = "notes";

impl SessionWindow {
    /// A window for a session just opened: the first group, Problems open;
    /// the panel list adds files picked with `picker`.
    pub fn new(picker: Arc<dyn FilesPicker>) -> Self {
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
            saved_view: None,
            panels: PanelList::new(picker),
            toasts: Vec::new(),
            panel_list_open: false,
            was_typing: false,
            #[cfg(test)]
            top_bar_rect: None,
            #[cfg(test)]
            list_rect: None,
        }
    }

    /// What the canvas shows (the plan until the first frame has looked at
    /// the session).
    pub fn mode(&self) -> CanvasMode {
        self.mode.unwrap_or(CanvasMode::Plan)
    }

    /// Show the plan or the mosaic, where the shown canvas looks (the
    /// panel list beside it stays as it is).
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

    /// The inspector's fold and open sections (tests; nothing outside the
    /// window reads them yet).
    #[cfg(test)]
    pub fn inspector(&self) -> &InspectorState {
        &self.inspector
    }

    /// The inspector, to open a section ([`InspectorState::reveal`]).
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

    /// React to a job event (from [`JobRunner::poll`]): the mosaic follows
    /// it, and a finished merge marks the session merged; one of the whole
    /// canvas leaves its group's result complete (until the summary writer
    /// classifies the results again, [`OpenSession::results`]).
    pub fn on_event(&mut self, session: &mut OpenSession, jobs: &JobRunner, event: &JobEvent) {
        self.mosaic.on_event(event);
        if let JobEvent::Finished { id, .. } = event
            && let Some((JobTarget::Blend, group)) = jobs.target(*id)
        {
            session.merged = true;
            if jobs.whole_canvas(*id) {
                session
                    .results
                    .insert(group.to_string(), ResultState::Complete);
            }
        }
    }

    /// See [`MosaicView::track_analysis`].
    pub fn track_analysis(&mut self, id: u64) {
        self.mosaic.track_analysis(id);
    }

    /// The panel list (tests).
    #[cfg(test)]
    pub(crate) fn panel_list(&self) -> &PanelList {
        &self.panels
    }

    /// The panel list's background work, shown or not ([`PanelList::poll`]):
    /// files picked, typed or dropped anywhere on the window are added, and
    /// a waiting edit applies. Call every frame after the runner's poll,
    /// before the pipeline's.
    pub fn poll_panels(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        self.panels.poll(ctx, session, jobs);
        self.save_panel_choices();
    }

    /// Save (rate-limited) what the panel list noted in the session's `ui`
    /// without an edit to carry it (the user's grouping choice).
    fn save_panel_choices(&mut self) {
        if self.panels.take_ui_changed() {
            self.save.touch();
        }
    }

    /// An edit of the panels waits for the jobs to stop (or a busy store):
    /// the automatic pipeline starts nothing meanwhile.
    pub fn panels_editing(&self) -> bool {
        self.panels.has_pending()
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
    /// view (on the first frame, as saved), the mosaic's work
    /// ([`MosaicView::poll`]), the plan's texture release, and the
    /// rate-limited save (with the view, if it changed). Call every frame
    /// after the runner's poll and [`SessionWindow::track_analysis`],
    /// before drawing.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.follow_session(session, jobs);
        self.mosaic.poll(ctx, session, jobs);
        self.plan.release_if_hidden(Instant::now());
        self.keep_view_state(session);
        self.save_if_due(ctx, session);
    }

    /// The shown view's key, for the summary and the saved view: a group
    /// name, or a composite's key (read from `ui`, the session's UI
    /// settings).
    pub fn view_key(&self, ui: &Value) -> Option<String> {
        match self.view.as_ref()? {
            ViewKind::Group(g) => Some(g.clone()),
            ViewKind::Composition(i) => load_compositions(ui)
                .get(*i)
                .map(|c| composition_stretch_key(&c.name)),
        }
    }

    /// An edit waits for the jobs to stop (panels, or applied merge
    /// settings).
    pub fn has_waiting_edits(&self) -> bool {
        self.panels.has_pending() || self.mosaic.has_waiting_settings()
    }

    /// Apply the edits waiting for the jobs to stop (panels, merge
    /// settings) now, if no job but an export runs: the session is being
    /// left, and nothing would apply them later. One refused because a
    /// store is held elsewhere stays waiting (logged).
    pub fn apply_waiting_edits(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if jobs.busy_except_exports() {
            return;
        }
        self.panels.apply_waiting(session, jobs);
        self.mosaic.apply_waiting_settings(session, jobs);
    }

    /// Raise a toast: the shell shows it with this frame's output (the
    /// next one's, raised after drawing).
    pub(crate) fn push_toast(&mut self, toast: WindowToast) {
        self.toasts.push(toast);
    }

    /// Run an Undo toast's action: an edit of the inputs as they are now,
    /// applied like any other (queued behind running jobs).
    pub fn undo(&mut self, undo: Undo, session: &mut OpenSession, jobs: &mut JobRunner) {
        match undo {
            Undo::Readd(removed) => self.panels.readd(removed, session, jobs),
            Undo::Grouping(grouping) => self.panels.regroup(grouping, session, jobs),
        }
        self.save_panel_choices();
    }

    /// Save a pending (rate-limited) change now, whatever the rate limit,
    /// the view as it is included: the window is closing, or the session is
    /// being left. A small `mosaic.json` write.
    pub fn flush_save(&mut self, session: &mut OpenSession) {
        self.keep_view_state(session);
        self.save_panel_choices();
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

    /// Draw the window filling `ui`: the top bar across it, the inspector
    /// at the right, the panel list (when open) at the left, the view bar
    /// and the canvas in the rest, and the export dialog when open.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        pipeline: &mut Pipeline,
        checks: &ChecksEngine,
    ) -> SessionOutput {
        let ctx = ui.ctx().clone();
        self.follow_session(session, jobs);
        self.shortcuts(&ctx, session, jobs, pipeline);
        let mut out = SessionOutput::default();
        let margin = egui::Margin::symmetric(space::M as i8, space::S as i8);
        let _top = egui::Panel::top("session-top-bar")
            .resizable(false)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(margin))
            .show(ui, |ui| {
                out.back = self.top_bar(ui, session, jobs, pipeline, checks);
            });
        #[cfg(test)]
        {
            self.top_bar_rect = Some(_top.response.rect);
        }
        egui::Panel::right("session-inspector")
            .resizable(false)
            .exact_size(inspector_width(&self.inspector))
            .show(ui, |ui| {
                self.inspector_ui(ui, session, jobs, pipeline, checks, &mut out);
            });
        if self.panel_list_open {
            // About half the window at first; the user resizes it, never
            // squeezing the canvas below its minimum (unless the window is
            // too small for both, when the list keeps its own).
            let widest = (ui.available_width() - CANVAS_MIN_WIDTH).max(PANEL_LIST_MIN_WIDTH);
            let width = (ctx.content_rect().width() / 2.0).clamp(PANEL_LIST_MIN_WIDTH, widest);
            let list = egui::Panel::left("panel-list")
                .resizable(true)
                .default_size(width)
                .min_size(PANEL_LIST_MIN_WIDTH)
                .max_size(widest)
                .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(margin))
                .show(ui, |ui| self.panels.ui(ui, session, jobs, checks.report()));
            #[cfg(test)]
            {
                self.list_rect = Some(list.response.rect);
            }
            self.follow_list(list.inner);
        } else {
            for toast in self.panels.take_toasts() {
                self.push_toast(toast);
            }
            self.plan.set_highlight(None);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| {
                self.view_bar(ui, session, jobs, pipeline);
                self.canvas_ui(ui, session, pipeline, checks);
            });
        self.mosaic.export_ui(&ctx, session, jobs);
        out.toasts.append(&mut self.toasts);
        self.save_panel_choices();
        // Files dropped anywhere are added, whatever shows.
        self.panels.drop_overlay(&ctx);
        // What this frame changed is saved from the next one.
        if self.keep_view_state(session) {
            ctx.request_repaint();
        }
        self.was_typing = ctx.text_edit_focused();
        out
    }

    /// Act on what the panel list reported this frame: its toasts; the row
    /// pointed at lights its panel on the plan; a row clicked (plainly)
    /// selects its panel there, opening the inspector's Panels section for
    /// a new one; a removed panel is no longer selected.
    fn follow_list(&mut self, out: PanelListOutput) {
        for toast in out.toasts {
            self.push_toast(toast);
        }
        self.plan.set_highlight(out.hovered);
        if let Some(path) = out.clicked {
            if self.selected.as_ref() != Some(&path) {
                self.inspector.reveal(PANELS);
            }
            self.plan.select(Some(path.clone()));
            self.selected = Some(path);
        }
        self.panels_removed(&out.removed);
    }

    /// `removed` left the inputs: the plan's selection goes with them (now,
    /// not once a queued edit applies).
    fn panels_removed(&mut self, removed: &[PathBuf]) {
        if self
            .plan
            .selected()
            .is_some_and(|p| removed.iter().any(|r| r == p))
        {
            self.plan.select(None);
            self.selected = None;
        }
    }

    /// Clear the selection, in the list and on the plan (Esc).
    fn clear_selection(&mut self) {
        self.panels.clear_selection();
        self.plan.select(None);
        self.selected = None;
    }

    /// On the first call, restore the view the session saved
    /// ([`SessionWindow::restore`]) or, with none saved, pick the mode;
    /// either way open the panel list for a session without panels (adding
    /// them comes first). Then keep the view one the session has (else its
    /// first group) in both canvases.
    fn follow_session(&mut self, session: &OpenSession, jobs: &JobRunner) {
        if self.mode.is_none() {
            let saved = ViewState::load(&session.mosaic.config.ui);
            match &saved {
                Some(s) => self.restore(s, session, jobs),
                None => {
                    self.mode = Some(if session.merged {
                        CanvasMode::Mosaic
                    } else {
                        CanvasMode::Plan
                    });
                }
            }
            self.saved_view = saved;
            self.panel_list_open |= session.mosaic.config.panels.is_empty();
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

    /// Show what `saved` says (a session just opened): the mosaic only if
    /// it can be shown (something merged, or a merge running), else the
    /// plan; its view if the session still has it, else (left to
    /// [`SessionWindow::follow_session`]) the first group; its camera with
    /// its view only, handed to both canvases before either draws (each
    /// paints its first frame through it); the inspector, the panel list
    /// and the toggles as saved.
    fn restore(&mut self, saved: &ViewState, session: &OpenSession, jobs: &JobRunner) {
        let mosaic_ok = session.merged || jobs.blending();
        self.mode = Some(match saved.mode {
            CanvasMode::Mosaic if mosaic_ok => CanvasMode::Mosaic,
            _ => CanvasMode::Plan,
        });
        let view = saved.view.as_deref().and_then(|k| view_of_key(session, k));
        if let Some(view) = view {
            self.view = Some(view);
            if let Some(c) = saved.camera {
                self.plan.set_camera(c);
                self.mosaic.set_camera(c);
            }
        }
        self.inspector.folded = saved.inspector_folded;
        self.inspector.open = saved.inspector_open.iter().cloned().collect();
        self.panel_list_open = saved.panel_list_open;
        self.look.outlines = saved.outlines;
        self.look.numbers = saved.outlines;
        self.look.rois = saved.rois;
    }

    /// The view as it is now (the stretch toggles are saved on their own).
    fn view_state(&self, ui: &Value) -> ViewState {
        ViewState {
            mode: self.mode(),
            view: self.view_key(ui),
            camera: self.shown_camera(),
            inspector_folded: self.inspector.folded,
            inspector_open: self.inspector.open.iter().cloned().collect(),
            panel_list_open: self.panel_list_open,
            outlines: self.look.outlines,
            rois: self.look.rois,
        }
    }

    /// Write the view into the session's `ui` if it changed since last
    /// written, and mark it for the rate-limited save; whether it did.
    /// Nothing before the first frame has looked at the session (the saved
    /// view is not restored yet).
    fn keep_view_state(&mut self, session: &mut OpenSession) -> bool {
        if self.mode.is_none() {
            return false;
        }
        let now = self.view_state(&session.mosaic.config.ui);
        if self.saved_view.as_ref() == Some(&now) {
            return false;
        }
        now.save(&mut session.mosaic.config.ui);
        self.saved_view = Some(now);
        self.save.touch();
        true
    }

    /// Where the shown canvas looks, if it has been drawn (or was handed a
    /// camera).
    fn shown_camera(&self) -> Option<Camera> {
        match self.mode() {
            CanvasMode::Plan => self.plan.camera(),
            CanvasMode::Mosaic => self.mosaic.camera(),
        }
    }

    /// Frame canvas rect `rect` (an ROI picked) on the canvas shown, as
    /// last drawn: the camera that frames it is handed to both canvases, so
    /// it shows from this frame and a later switch moves nothing. Nothing
    /// before either canvas has been drawn (no viewport to fit).
    pub(super) fn frame(&mut self, rect: [u64; 4]) {
        let (shown, other) = match self.mode() {
            CanvasMode::Plan => (self.plan.canvas_frame(), self.mosaic.viewer_frame()),
            CanvasMode::Mosaic => (self.mosaic.viewer_frame(), self.plan.canvas_frame()),
        };
        let Some((viewport, _)) = shown.or(other) else {
            return;
        };
        let c = Camera::framing(rect.map(|v| v as f64), viewport.size());
        self.plan.set_camera(c);
        self.mosaic.set_camera(c);
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

/// The view `key` ([`SessionWindow::view_key`]) names in `session`, if it
/// still has it: a group by its name, else a composite by its key.
fn view_of_key(session: &OpenSession, key: &str) -> Option<ViewKind> {
    if group_names(session).iter().any(|g| g == key) {
        return Some(ViewKind::Group(key.to_string()));
    }
    compositions_of(session)
        .iter()
        .position(|c| composition_stretch_key(&c.name) == key)
        .map(ViewKind::Composition)
}

#[cfg(test)]
mod tests;
