//! The merged mosaic canvas (spec §5–§7): the view's merged results drawn
//! tile by tile in the shared frame, with its regions of interest. Only the
//! canvas: the window around it picks the view ([`MosaicView::select_view`])
//! and the look ([`CanvasLook`]), starts merges and exports through its
//! methods, and shows the ROI list ([`MosaicView::rois_ui`]), the merge
//! settings ([`MosaicView::settings_ui`]) and the health
//! ([`MosaicView::health_ui`]) where it likes, whether or not the canvas
//! is drawn.
//!
//! - **Every frame.** The shell calls [`MosaicView::poll`] (and
//!   [`MosaicView::poll_settings`]) every frame, shown or not: it resolves
//!   the view's groups, opens and checks results, follows merges and
//!   analyses, releases given-up sources and runs the ROI and health work,
//!   so the merge guard, the ROI list and the health stay current while the
//!   window shows the plan. [`MosaicView::show`] only draws.
//! - **Views.** Each group, then each composition (the plan's
//!   compositions, [`crate::compositions`]). A composition view mixes its
//!   groups' results in a [`CompositeTileSource`], each through that
//!   group's own display stretch; the health is that of one member (the
//!   health section picks it). A group not merged yet adds nothing and is
//!   named over the image's bottom-left corner (an overlay: the image keeps
//!   the plan's place); groups on different canvases cannot be mixed: the
//!   view says why instead.
//! - **ROIs** ([`crate::rois`]): drawn on either canvas with its draw tool
//!   (the window arms it; the mosaic takes the rect through
//!   [`MosaicView::on_drawn`]), named, noted, reordered, selected (the
//!   window then frames it, [`MosaicView::rois_ui`]) and deleted (with an
//!   undo, [`MosaicView::take_roi_deleted`]); kept in `MosaicConfig.ui`.
//!   Reading the frame and reprojecting them (sky ↔ canvas) runs on a
//!   worker ([`RoiPanel`]). There is no ROI merge: an ROI's preview is
//!   transient, made by the window's preview runner ([`crate::previews`]),
//!   and the list and the canvas labels show its state
//!   ([`MosaicView::set_roi_states`]).
//!   [`MosaicView::merge_these`] merges the groups it is given over the whole
//!   canvas, committing the merge settings draft first.
//! - **Merge settings** ([`crate::blend_settings`]): the mosaic-wide
//!   merge settings (a draft kept in `ui`, edited at once) and analysis
//!   settings (applied at once, with an Undo toast). While an analysis edit
//!   waits to apply or the feather text is invalid, merging is blocked; an
//!   applied analysis edit reloads the view's (now stale) tiles.
//! - **Export** ([`crate::export`]) writes the view's groups' results to
//!   FITS or TIFF as jobs, after warning about stale or missing tiles.
//! - **Camera.** [`MosaicView::set_camera`] moves the canvas without
//!   animation from the next frame the viewer paints. The viewer is
//!   rebuilt when the view's results come and go (a view without any
//!   result has none); the camera is kept across that, and across a new
//!   canvas size: the next viewer's first painted frame is through it,
//!   never its home fit.
//! - **Stretch.** Always automatic: [`DisplayStretch::params`] of each
//!   shown group's coarse pyramid statistics, with the session's Linked and
//!   Strong ([`DisplayStretch`] in `MosaicConfig.ui`, what the look's
//!   [`CanvasLook::stretch`] shows), followed by `poll` whether or not the
//!   canvas is drawn. No pumping: while a merge of a shown group runs, the
//!   stretch in use is held (the previous result's; for a group's first
//!   result in a group view, the plan's stretch last handed to
//!   [`MosaicView::show`]). It is worked out again when the merges of the
//!   shown groups have ended, when the view changes and when the toggles
//!   change. The manual stretch earlier versions saved per group
//!   (`blend_stretch` in `MosaicConfig.ui`) is ignored.
//!
//! Everything slow runs on background threads ([`Task`]): opening each
//! result ([`open_result`]), the automatic stretch statistics, the health
//! metrics and the ROI sync. The one write on the UI
//! thread is the debounced `mosaic.json` save of the ROI list (at most once
//! per [`SAVE_INTERVAL`](crate::util::SAVE_INTERVAL)), the same documented
//! exception as the job runner's plan/apply saves.
//!
//! # No reader across a store recreation
//!
//! `run_blend` recreates (truncates) a group's store only when it is
//! missing, unparseable, or its geometry differs from the group's analysed
//! canvas; a live [`mmm_core::tiles::TileReader`] would then fault. This
//! view guards the geometry path (the other two have no reader to fault),
//! for every group it shows:
//!
//! - a source is kept only when [`open_result`] found the store's geometry
//!   equal to the group's analysed canvas, so a merge reopens it in place;
//! - the canvas changes only by analysis: after every analysis (finished,
//!   cancelled or failed) every shown group's result is checked again, and
//!   while an analysis is running or queued, or any shown group's check is
//!   pending, merging is blocked ([`MosaicView::merge_block_reason`]; a
//!   merge queued behind an analysis would start the moment it is applied,
//!   before the check);
//! - a source given up (out of date, view switched, replaced) is tracked by
//!   a `Weak` until the viewer's loader threads and any statistics task
//!   have dropped it; merging stays blocked until then. A
//!   composite holds every member's reader, so a member given up stays
//!   tracked until the composite itself is dropped: all member readers are
//!   released before any member's store may be recreated;
//! - with no source open, a merge's first progress update means it has
//!   created its store (progress is only reported once tiles are written),
//!   so the result is opened then and tiles appear while it runs; while a
//!   merge of a shown group is queued or has not reported yet (e.g. after
//!   switching to that group), no open of that group starts.
//!
//! Another program (the `mmm` CLI) recreating the store while it is shown
//! is not guarded against.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{radius, space};
use mega_ui::theme::{palette, scrim};
use mega_ui::viewer::{PyramidGeometry, TileSource, TiledViewer, Tool};
use mmm_core::health::{Health, health};
use mmm_core::mosaic::{FRAME_FILE, commit_blend_settings};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::settings::{AnalyzeSettings, ChangeImpact};
use mmm_core::stretch::StretchParams;
use mmm_core::surfaces::Surfaces;
use serde_json::Value;

use crate::blend_settings::{SettingsPanel, load_draft};
use crate::camera::Camera;
use crate::compositions::{Composition, ViewKind, composition_stretch_key, load_compositions};
use crate::display_stretch::DisplayStretch;
use crate::export::{ExportAction, ExportDialog, RfdSavePicker};
use crate::jobs::{JobEvent, JobRunner};
use crate::plan_view::CanvasLook;
use crate::rois::{Roi, RoiPanel};
use crate::session::OpenSession;
use crate::stretch_panel::channel_name;
use crate::tile_source::{CompositeTileSource, MosaicTileSource, Opened, open_result};
use crate::util::{Debounce, Poll, Task, group_names};

/// How often to look again while a given-up source is still being released.
const RELEASE_POLL: Duration = Duration::from_millis(20);

/// Health of the shown group, or why there is none.
type HealthResult = Result<Health, String>;

/// Compute `health` for the group analysed in `session_dir` (blocking).
fn load_health(session_dir: &std::path::Path, feather_px: f32) -> HealthResult {
    let run = || -> mmm_core::Result<Health> {
        let session = Session::open(session_dir)?;
        let graph = OverlapGraph::load(&session.overlap_graph_path())?;
        let phot = Photometry::load(&session.photometry_path())?;
        let surfaces = if session.surfaces_path().exists() {
            Some(Surfaces::load(&session.surfaces_path())?)
        } else {
            None
        };
        health(&session, &graph, &phot, surfaces.as_ref(), feather_px)
    };
    run().map_err(|e| e.to_string())
}

/// Where a group stands for merging; ordered worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Readiness {
    /// Never analysed.
    NotAnalyzed,
    /// Analysis settings changed since the last analysis.
    NeedsAnalyze,
    /// Can be merged.
    Analyzed,
}

/// The groups a view shows: the group itself, or a composition's mapped
/// groups (each once, in channel order). An unknown composition shows
/// nothing.
pub(crate) fn view_members(view: &ViewKind, compositions: &[Composition]) -> Vec<String> {
    match view {
        ViewKind::Group(g) => vec![g.clone()],
        ViewKind::Composition(i) => {
            let mut out: Vec<String> = Vec::new();
            for (g, _) in compositions
                .get(*i)
                .into_iter()
                .flat_map(|c| c.channels.iter().flatten())
            {
                if !out.contains(g) {
                    out.push(g.clone());
                }
            }
            out
        }
    }
}

/// When a group's stretch is to be worked out again; ordered by urgency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Refresh {
    /// Not asked for.
    None,
    /// Once no merge of a shown group is pending (the data has settled).
    WhenIdle,
    /// As soon as possible (the view or the look changed).
    Now,
}

/// One group of the view: its open result and its display stretch.
struct Member {
    /// The open result (geometry checked, see the module docs).
    source: Option<Arc<MosaicTileSource>>,
    /// Why there is no source.
    absent: Option<String>,
    /// A pending [`open_result`], with the look's stretch it was asked for.
    opening: Option<Task<(DisplayStretch, Opened)>>,
    /// Open (again) once no open is pending.
    reopen: bool,
    /// Last seen `(job, progress updates)` of this group's running merge.
    last_progress: Option<(u64, u64)>,
    /// A pending automatic stretch of `source`, with the look's stretch it
    /// was asked for.
    auto: Option<Task<(DisplayStretch, Option<Vec<StretchParams>>)>>,
    /// When to work the stretch out again.
    refresh: Refresh,
    /// The stretch last given to this group's result, kept while it is
    /// opened again (what a merge holds).
    stretch: Option<Vec<StretchParams>>,
}

impl Member {
    /// A member to be opened.
    fn new() -> Self {
        Member {
            source: None,
            absent: None,
            opening: None,
            reopen: true,
            last_progress: None,
            auto: None,
            refresh: Refresh::None,
            stretch: None,
        }
    }

    /// Nothing pending for this member.
    fn idle(&self) -> bool {
        self.opening.is_none()
            && !self.reopen
            && self.auto.is_none()
            && (self.refresh == Refresh::None || self.source.is_none())
    }

    /// Give up the source (the viewer's loaders let go of it after their
    /// current fetch), tracking it in `retired`.
    fn retire(&mut self, retired: &mut Vec<Weak<MosaicTileSource>>) {
        if let Some(src) = self.source.take() {
            retired.push(Arc::downgrade(&src));
        }
        self.auto = None;
    }
}

/// What the viewer shows: each member's source (by address, 0 for none)
/// and the composition mapping, so the viewer source is rebuilt only when
/// one of them changes.
type ShownKey = (Vec<usize>, Vec<Option<(String, u32)>>);

/// The merged mosaic canvas of the open session: what it draws, and the
/// work behind it (see the module docs).
pub struct MosaicView {
    /// The view shown: a group or a composition.
    view: Option<ViewKind>,
    /// The groups of the view, in order.
    members: Vec<String>,
    /// The member whose health is shown.
    group: Option<String>,
    /// Each member's result and stretch state.
    results: BTreeMap<String, Member>,
    viewer: Option<TiledViewer>,
    /// The geometry of `viewer`'s source.
    viewer_geometry: Option<PyramidGeometry>,
    /// What `viewer` was last given (see [`ShownKey`]).
    shown_key: Option<ShownKey>,
    /// Why the view cannot be drawn though its results are open (e.g. a
    /// composition of groups on different canvases).
    view_problem: Option<String>,
    /// Sources given up but maybe still mapped by loader or statistics
    /// threads (directly or inside a composite).
    retired: Vec<Weak<MosaicTileSource>>,
    /// A camera the viewer paints its next frame through: one set from
    /// outside, or the last one shown when the viewer was rebuilt.
    camera: Option<Camera>,
    /// The look's stretch the members' stretches follow.
    look: DisplayStretch,
    /// The view the members' stretches were last worked out for.
    stretch_view: Option<ViewKind>,
    /// The view the members were last resolved for (by `poll`).
    resolved: Option<ViewKind>,
    /// The plan's stretch of the view, as last handed to `show`.
    plan_stretch: Option<Vec<StretchParams>>,
    health: Option<HealthResult>,
    health_task: Option<Task<(String, HealthResult)>>,
    want_health: bool,
    /// Where the health's group stands (as of the last frame shown).
    readiness: Readiness,
    save: Debounce,
    /// The analysis this view tracks, until it ends.
    analyze_job: Option<u64>,
    /// The merges this view started, until they end: job id → group.
    merge_jobs: BTreeMap<u64, String>,
    loader_threads: usize,
    /// Drawn at least once.
    shown: bool,
    /// The session's ROIs and their workers.
    rois: RoiPanel,
    /// The export dialog.
    export: ExportDialog,
    /// The merge settings panel.
    settings: SettingsPanel,
    /// The tag over the image (why the shown groups are out of date, or
    /// that their settings changed), set by the window each frame.
    tag: Option<CanvasTag>,
    /// The tag was clicked: the window reveals Merge settings.
    reveal_settings: bool,
}

impl Default for MosaicView {
    fn default() -> Self {
        Self::new()
    }
}

impl MosaicView {
    /// A view showing nothing yet; the first group opens on the first frame.
    pub fn new() -> Self {
        MosaicView {
            view: None,
            members: Vec::new(),
            group: None,
            results: BTreeMap::new(),
            viewer: None,
            viewer_geometry: None,
            shown_key: None,
            view_problem: None,
            retired: Vec::new(),
            camera: None,
            look: DisplayStretch::default(),
            stretch_view: None,
            resolved: None,
            plan_stretch: None,
            health: None,
            health_task: None,
            want_health: false,
            readiness: Readiness::NotAnalyzed,
            save: Debounce::default(),
            analyze_job: None,
            merge_jobs: BTreeMap::new(),
            loader_threads: std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .clamp(2, 8),
            shown: false,
            rois: RoiPanel::default(),
            export: ExportDialog::new(Arc::new(RfdSavePicker)),
            tag: None,
            reveal_settings: false,
            settings: SettingsPanel::new(),
        }
    }

    /// Show `view` from the next frame on (its groups are resolved then; one
    /// that no longer exists falls back to the first group).
    pub fn select_view(&mut self, view: ViewKind) {
        self.view = Some(view);
    }

    /// The groups of the view, in order.
    pub fn members(&self) -> &[String] {
        &self.members
    }

    /// Set the tag drawn over the image's top-left corner (`None`: none).
    pub fn set_tag(&mut self, tag: Option<CanvasTag>) {
        self.tag = tag;
    }

    /// Whether the tag was clicked since the last call.
    pub fn take_reveal_settings(&mut self) -> bool {
        std::mem::take(&mut self.reveal_settings)
    }

    /// The view shown.
    pub fn view(&self) -> Option<&ViewKind> {
        self.view.as_ref()
    }

    /// The group whose health is shown (in a composition view, one of its
    /// groups).
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// Where the canvas looks: a camera set and not painted yet (or kept
    /// for the next viewer), else what the last frame showed; `None` with
    /// neither.
    pub fn camera(&self) -> Option<Camera> {
        self.camera.or_else(|| self.shown_camera())
    }

    /// Look through `camera` from the next frame the viewer paints, with no
    /// animation (the zoom is clamped to the viewer's limits). Kept until a
    /// viewer paints it, so it also applies to a viewer made later.
    pub fn set_camera(&mut self, camera: Camera) {
        self.camera = Some(camera);
    }

    /// The viewer's rect and time in the last frame it painted.
    pub fn viewer_frame(&self) -> Option<(egui::Rect, f64)> {
        self.viewer.as_ref()?.last_frame()
    }

    /// What the viewer showed in its last frame.
    fn shown_camera(&self) -> Option<Camera> {
        let viewer = self.viewer.as_ref()?;
        let (_, now) = viewer.last_frame()?;
        Some(Camera::of(viewer.view(), now))
    }

    /// The shown group's open result, if any.
    pub fn source(&self) -> Option<&Arc<MosaicTileSource>> {
        self.member()?.source.as_ref()
    }

    /// Why the shown group has no result, if it has none. `None` while its
    /// result is being opened (again): a reason from an earlier open may no
    /// longer hold, and the view shows "Opening the result…" meanwhile.
    pub fn absent(&self) -> Option<&str> {
        let m = self.member()?;
        if m.opening.is_some() || m.reopen {
            return None;
        }
        m.absent.as_deref()
    }

    /// The group's health, once computed.
    pub fn health(&self) -> Option<&HealthResult> {
        self.health.as_ref()
    }

    /// The session's ROIs.
    pub fn rois(&self) -> &[Roi] {
        self.rois.rois()
    }

    /// The ROIs' canvas rects follow the current frame: no reprojection is
    /// running or wanted ([`RoiPanel::synced`]).
    pub fn rois_synced(&self) -> bool {
        self.rois.synced()
    }

    /// Each ROI's preview state, by id ([`RoiPanel::set_states`]): the ROI
    /// list says it, and the canvas labels while a preview merges.
    pub fn set_roi_states(&mut self, states: BTreeMap<u64, crate::previews::PreviewState>) {
        self.rois.set_states(states);
    }

    /// Why the typed merge settings are refused, if they are
    /// ([`SettingsPanel::input_error`]): no merge, and no preview, meanwhile.
    pub fn input_error(&self) -> Option<&'static str> {
        self.settings.input_error()
    }

    /// Move ROI `id` to canvas rect `canvas`, as a reprojection does
    /// (tests).
    #[cfg(test)]
    pub(crate) fn move_roi(&mut self, id: u64, canvas: [u64; 4]) {
        if let Some(r) = self.rois.rois.iter_mut().find(|r| r.id == id) {
            r.canvas = canvas;
        }
    }

    fn member(&self) -> Option<&Member> {
        self.results.get(self.group.as_ref()?)
    }

    /// What the viewer's place says instead of an image, if it has no
    /// image to show.
    pub fn shown_text(&self) -> Option<String> {
        self.viewer.is_none().then(|| self.placeholder())
    }

    /// True once drawn with nothing pending: no open, statistics, health
    /// or ROI sync task, and the viewer (if any) has drawn and loaded
    /// its tiles. A session without groups settles at once.
    pub fn is_settled(&self) -> bool {
        self.shown
            && self.results.values().all(Member::idle)
            && self.health_task.is_none()
            && self.rois.idle()
            && self.export.idle()
            && !self.settings.has_pending()
            && self
                .viewer
                .as_ref()
                .is_none_or(|v| v.last_frame().is_some() && !v.is_loading())
    }

    /// The view's key, as the plan keys it: the group's name, or
    /// [`composition_stretch_key`] of the composition's name (read from
    /// `ui`, the session's UI settings). `None` before a view is chosen.
    pub fn view_key(&self, ui: &Value) -> Option<String> {
        match self.view.as_ref()? {
            ViewKind::Group(g) => Some(g.clone()),
            ViewKind::Composition(i) => load_compositions(ui)
                .get(*i)
                .map(|c| composition_stretch_key(&c.name)),
        }
    }

    /// Save a pending (debounced) ROI change now, whatever the rate limit:
    /// the window is closing, or the session state is being saved. A small
    /// `mosaic.json` write on the UI thread (the module's documented
    /// exception).
    pub fn flush_save(&mut self, session: &OpenSession) {
        if !self.save.pending() {
            return;
        }
        self.save.saved(Instant::now());
        if let Err(e) = session.mosaic.save() {
            tracing::warn!("ROIs not saved: {e}");
        }
    }

    /// Treat analysis job `id`, started elsewhere (the automatic
    /// pipeline), as this view's: merging is blocked until it ends, and the
    /// results and health are checked again then.
    pub fn track_analysis(&mut self, id: u64) {
        self.analyze_job = Some(id);
    }

    /// React to a job that ended (from [`JobRunner::poll`]).
    pub fn on_event(&mut self, event: &JobEvent) {
        let id = match event {
            JobEvent::Finished { id, .. }
            | JobEvent::Cancelled { id }
            | JobEvent::Failed { id, .. } => *id,
            JobEvent::Preview(_) => return,
        };
        if self.analyze_job == Some(id) {
            // The canvas (and frame) may have changed: check every result
            // and reproject the ROIs.
            self.analyze_job = None;
            for m in self.results.values_mut() {
                m.reopen = true;
            }
            self.want_health = true;
            self.rois.want_sync = true;
        } else if let Some(group) = self.merge_jobs.remove(&id) {
            if let Some(m) = self.results.get_mut(&group) {
                match &m.source {
                    // The data has settled (unless another merge of a shown
                    // group follows): its stretch is worked out again.
                    Some(src) => {
                        src.bump();
                        m.refresh = m.refresh.max(Refresh::WhenIdle);
                    }
                    None => m.reopen = true,
                }
            }
            if self.group.as_ref() == Some(&group) {
                self.want_health = true;
            }
        }
    }

    /// A global settings edit waits to apply, or jobs cancelled for one
    /// have not all been reported ([`SettingsPanel::editing`]): the
    /// automatic pipeline starts nothing meanwhile and restarts what was
    /// cancelled.
    pub fn settings_editing(&self, session: &OpenSession) -> bool {
        self.settings.editing(&session.mosaic.config)
    }

    /// The merge settings panel's background work and waiting edit (see
    /// [`SettingsPanel::poll`]). Call every frame after the runner's poll,
    /// whether or not the canvas is shown; the impact when an edit was
    /// applied.
    pub fn poll_settings(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        let applied = self.settings.poll(ctx, session, jobs);
        if let Some(impact) = applied {
            self.on_settings_applied(impact);
        }
        applied
    }

    /// An applied merge settings edit waits for the jobs to stop.
    pub fn has_waiting_settings(&self) -> bool {
        self.settings.has_pending()
    }

    /// Apply a merge settings edit waiting for the jobs to stop now
    /// ([`SettingsPanel::apply_waiting`]); call only while no job but an
    /// export runs.
    pub fn apply_waiting_settings(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if let Some(impact) = self.settings.apply_waiting(session, jobs) {
            self.on_settings_applied(impact);
        }
    }

    /// The view's every-frame work, whether or not the canvas is drawn:
    /// resolve the view's groups, poll and start the background work
    /// (opening results, stretches, health, ROI sync), follow
    /// merges and analyses, release given-up sources, and save. Call every
    /// frame after the runner's poll (and [`MosaicView::track_analysis`]),
    /// before anything is drawn. The stretch toggles followed are the
    /// session's ([`DisplayStretch`] in `MosaicConfig.ui`).
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        self.rois.load_once(&session.mosaic.config.ui);
        let groups = group_names(session);
        let compositions = load_compositions(&session.mosaic.config.ui);
        match self.resolve_view(&groups, &compositions) {
            Some(view) => {
                // A composition's mapping may change (in the plan): follow
                // it.
                let members = view_members(&view, &compositions);
                if self.view.as_ref() != Some(&view) || self.members != members {
                    self.apply_view(view, members);
                }
            }
            None => {
                self.retire_all();
                self.results.clear();
                self.members.clear();
                self.view = None;
                self.group = None;
            }
        }
        self.resolved = self.view.clone();
        let look = DisplayStretch::load(&session.mosaic.config.ui);
        let plan_stretch = self.plan_stretch.take();
        self.update(ctx, session, jobs, look, plan_stretch.as_deref());
        self.plan_stretch = plan_stretch;
        self.sync_viewer(&compositions);
        self.readiness = self
            .group
            .as_ref()
            .map_or(Readiness::NotAnalyzed, |g| group_readiness(session, g));
    }

    /// Settings were applied: the view's results went stale (their tiles
    /// load again to show it), and health is computed again (it depends on
    /// the feather).
    fn on_settings_applied(&mut self, impact: ChangeImpact) {
        if impact == ChangeImpact::None {
            return;
        }
        for m in self.results.values() {
            if let Some(src) = &m.source {
                src.bump();
            }
        }
        self.want_health = true;
    }

    /// Why no merge of every group may start now, if any (`None`: merging
    /// is allowed). The reason is for the user, e.g. on a disabled Merge
    /// button. See the module docs for the guard behind most reasons.
    pub fn merge_block_reason(&self, session: &OpenSession, jobs: &JobRunner) -> Option<String> {
        self.merge_guard(session, jobs, &group_names(session))
    }

    /// Why **Merge this group** cannot be used on `group` now, if it
    /// cannot: its whole-canvas merge is running or queued already, or
    /// [`MosaicView::merge_guard`] gives a reason for it.
    pub fn merge_group_block_reason(
        &self,
        session: &OpenSession,
        jobs: &JobRunner,
        group: &str,
    ) -> Option<String> {
        if jobs.whole_blend_pending(group) {
            return Some(format!("Merging {group}…"));
        }
        self.merge_guard(session, jobs, &[group.to_string()])
    }

    /// Why no merge of `groups` may start now, if any: the store
    /// recreation guard of the module docs, and every group analysed.
    fn merge_guard(
        &self,
        session: &OpenSession,
        jobs: &JobRunner,
        groups: &[String],
    ) -> Option<String> {
        if groups.is_empty() {
            return Some("There are no panels to merge yet.".into());
        }
        if self.analyze_job.is_some() || jobs.analyzing() {
            return Some("Waiting for the analysis to finish.".into());
        }
        let readiness = groups
            .iter()
            .map(|g| group_readiness(session, g))
            .min()
            .unwrap_or(Readiness::NotAnalyzed);
        match readiness {
            Readiness::NotAnalyzed => {
                return Some("The panels need an analysis before merging.".into());
            }
            Readiness::NeedsAnalyze => {
                return Some(
                    "The analysis settings changed: the panels need a new analysis before merging."
                        .into(),
                );
            }
            Readiness::Analyzed => {}
        }
        if !self.export.idle() {
            return Some("Waiting for the export check.".into());
        }
        if let Some(why) = self.settings.input_error() {
            return Some(why.into());
        }
        if self.settings.has_pending() {
            return Some("Waiting for the analysis settings change.".into());
        }
        if self
            .results
            .values()
            .any(|m| m.opening.is_some() || m.reopen)
        {
            return Some("Checking the merged result…".into());
        }
        if !self.retired.is_empty() {
            return Some("Releasing the previous result…".into());
        }
        None
    }

    /// Merge every group of the mosaic over the whole canvas (spec §5.4);
    /// nothing while [`MosaicView::merge_block_reason`] gives a reason.
    #[cfg(test)]
    pub fn merge_all(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        let groups = group_names(session);
        self.merge_these(session, jobs, groups);
    }

    /// Merge `groups` over the whole canvas with the merge settings draft
    /// (committed first, so the jobs queue with it); nothing while
    /// [`MosaicView::merge_guard`] gives a reason for them (a failed commit
    /// is raised as a problem). Groups running or queued already keep the
    /// settings they were queued with. Whether anything was queued.
    pub fn merge_these(
        &mut self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: Vec<String>,
    ) -> bool {
        if groups.is_empty() || self.merge_guard(session, jobs, &groups).is_some() {
            return false;
        }
        let draft = load_draft(&session.mosaic.config);
        if draft != session.mosaic.config.blend
            && let Err(e) = commit_blend_settings(&mut session.mosaic, draft)
        {
            self.settings
                .set_problem(format!("Merge settings not saved: {e}"));
            return false;
        }
        self.merge_groups(session, jobs, groups)
    }

    /// Merge each of `groups` over the whole canvas with the committed
    /// settings (a whole merge commits the draft first).
    fn merge_groups(
        &mut self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: Vec<String>,
    ) -> bool {
        // The export check maps result stores a merge may recreate; an
        // analysis edit waiting to apply would cancel the merge anyway.
        if !self.export.idle() || self.settings.has_pending() {
            return false;
        }
        let any = !groups.is_empty();
        for g in groups {
            let id = jobs.start_blend(session, &g, None);
            self.merge_jobs.insert(id, g);
        }
        any
    }

    /// Whether the export dialog may open now: the view has groups, no
    /// tracked analysis runs and the dialog is not open already
    /// ([`MosaicView::export_block_reason`] gives none).
    pub fn can_open_export(&self) -> bool {
        self.export_block_reason().is_none()
    }

    /// Why the export dialog cannot open now, if it cannot.
    pub fn export_block_reason(&self) -> Option<&'static str> {
        if self.export.is_open() {
            Some("The Export dialog is open.")
        } else if self.members.is_empty() {
            Some("There is nothing to export in this view.")
        } else if self.analyze_job.is_some() {
            Some("Waiting for the analysis to finish.")
        } else {
            None
        }
    }

    /// Whether the export dialog is open.
    pub fn export_open(&self) -> bool {
        self.export.is_open()
    }

    /// Open the export dialog for the view's groups.
    pub fn open_export(&mut self, session: &OpenSession) {
        self.export.open(session, self.members.clone());
    }

    /// Draw the export dialog (if open) and act on the user's decision:
    /// merge the chosen groups first, or start one export job per group.
    pub fn export_ui(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        let can_merge = self.merge_guard(session, jobs, &self.members).is_none();
        // Merges of the dialog's groups queued or running (whatever view is
        // showing now): the export check maps the stores they may recreate.
        let merging: BTreeSet<String> = self
            .export
            .groups()
            .into_iter()
            .filter(|g| jobs.blend_pending(g))
            .collect();
        match self.export.ui(ctx, session, can_merge, &merging) {
            Some(ExportAction::BlendFirst(groups)) => {
                self.merge_groups(session, jobs, groups);
            }
            Some(ExportAction::Export(dests, format)) => {
                for (group, out) in dests {
                    jobs.start_export(session, &group, out, format);
                }
            }
            None => {}
        }
    }

    /// Whether the viewer's ROI draw tool is on (a drag on the canvas
    /// draws an ROI).
    pub fn drawing(&self) -> bool {
        self.viewer
            .as_ref()
            .is_some_and(|v| v.tool() == Tool::DrawRect)
    }

    /// The selected ROI's id.
    #[cfg(test)]
    pub fn selected_roi(&self) -> Option<u64> {
        self.rois.selected
    }

    /// The ROIs as canvas overlays (the window gives them to the plan too).
    pub fn roi_overlays(&self) -> Vec<mega_ui::canvas_layers::Overlay> {
        self.rois.overlays()
    }

    /// Arm (or disarm) the viewer's ROI draw tool: the next drag draws a
    /// rectangle. Nothing without a viewer.
    pub fn set_drawing(&mut self, on: bool) {
        if let Some(v) = &mut self.viewer {
            v.set_tool(if on { Tool::DrawRect } else { Tool::Navigate });
        }
    }

    /// The ROI list: each ROI's name, size and state, and the note of the
    /// selected one; changes are saved (debounced). The canvas rect of an
    /// ROI just picked, to go to: the caller frames it on the canvas it
    /// shows (both canvases share the camera, so nothing here moves the
    /// viewer).
    pub fn rois_ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession) -> Option<[u64; 4]> {
        let out = self.rois.ui(ui, &mut session.mosaic.config.ui);
        if out.changed {
            self.save.touch();
        }
        out.go_to
    }

    /// The merge settings (a draft, edited at once) and the analysis
    /// settings (applied at once), marked against the view's groups.
    pub fn settings_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) {
        let groups = self.members.clone();
        if let Some(impact) = self.settings.ui(ui, session, jobs, &groups) {
            self.on_settings_applied(impact);
        }
    }

    /// Apply the analysis settings `new` ([`SettingsPanel::edit_analysis`]),
    /// queued behind the jobs in the way like any edit; the impact when it
    /// was applied now.
    pub fn edit_analysis(
        &mut self,
        new: AnalyzeSettings,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
    ) -> Option<ChangeImpact> {
        let applied = self.settings.edit_analysis(new, session, jobs);
        if let Some(impact) = applied {
            self.on_settings_applied(impact);
        }
        applied
    }

    /// The analysis settings from before the last applied change, once
    /// ([`SettingsPanel::take_analysis_undo`]).
    pub fn take_analysis_undo(&mut self) -> Option<AnalyzeSettings> {
        self.settings.take_analysis_undo()
    }

    /// Why the last analysis edit failed, or the merge settings could not
    /// be committed by a merge, once ([`SettingsPanel::take_problem`]).
    pub fn take_problem(&mut self) -> Option<String> {
        self.settings.take_problem()
    }

    /// Pretend the feather text is invalid (tests).
    #[cfg(test)]
    pub fn force_feather_error(&mut self) {
        self.settings.force_feather_error();
    }

    /// Take back [`MosaicView::force_feather_error`] (tests).
    #[cfg(test)]
    pub fn clear_feather_error(&mut self) {
        self.settings.clear_feather_error();
    }

    /// Ask for the ROIs to be reprojected, as an analysis ending does
    /// (tests).
    #[cfg(test)]
    pub(crate) fn want_roi_sync(&mut self) {
        self.rois.want_sync = true;
    }

    /// Whether the merge settings draft changed since the last call
    /// ([`SettingsPanel::take_draft_changed`]).
    pub fn take_draft_changed(&mut self) -> bool {
        self.settings.take_draft_changed()
    }

    /// The health of the view's group (in a composition view, of the group
    /// picked here): seam steps, flagged edges and gain ranges.
    pub fn health_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Health").color(palette::TEXT_STRONG));
        self.member_picker(ui);
        let muted = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).color(palette::TEXT_MUTED));
        };
        match (&self.health, self.readiness) {
            (_, Readiness::NotAnalyzed | Readiness::NeedsAnalyze) => {
                muted(ui, "Shown once the group is analysed.");
            }
            (None, _) => muted(ui, "Computing…"),
            (Some(Err(e)), _) => muted(ui, &format!("Not available: {e}")),
            (Some(Ok(h)), _) => health_grid(ui, h),
        }
    }

    /// In a composition view: which group's health is shown.
    fn member_picker(&mut self, ui: &mut egui::Ui) {
        if self.members.len() < 2 {
            return;
        }
        let mut chosen = self.group.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Health of")
                .selected_text(&chosen)
                .show_ui(ui, |ui| {
                    for g in &self.members {
                        ui.selectable_value(&mut chosen, g.clone(), g);
                    }
                });
        });
        if self.group.as_ref() != Some(&chosen) {
            self.group = Some(chosen);
            self.health = None;
            self.want_health = true;
        }
    }

    /// Draw the canvas filling `ui` and handle its input (a rectangle drawn
    /// with the ROI tool becomes an ROI). Drawing only: the state it draws
    /// is brought up to date by [`MosaicView::poll`]. The look's stretch
    /// toggles are the session's, which `poll` follows (see the module
    /// docs); the ROIs are drawn when the look has them. `plan_stretch` is
    /// the plan's stretch of the same view, kept for a group's first merge.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        look: &CanvasLook,
        plan_stretch: Option<&[StretchParams]>,
    ) {
        self.shown = true;
        self.plan_stretch = plan_stretch.map(<[StretchParams]>::to_vec);
        if self.resolved.as_ref() != self.view.as_ref() {
            // A view selected since the last poll: drawn from the next frame.
            ui.ctx().request_repaint();
        }
        let canvas = self.view_canvas();
        let overlays = if look.rois {
            self.rois.overlays()
        } else {
            Vec::new()
        };
        let notes = self.member_notes();
        let mut drawn = None;
        match &mut self.viewer {
            Some(viewer) => {
                // A held camera, in place of the home fit or the view
                // before: the viewer paints this frame through it.
                let size = ui.available_size();
                if size.x > 0.0
                    && size.y > 0.0
                    && let Some(c) = self.camera.take()
                {
                    viewer.set_camera(size, c.centre, c.zoom);
                }
                viewer.set_overlays(overlays);
                drawn = viewer.show(ui).drawn_rect;
                // A composition shows its merged groups; the others are
                // named over the image, which keeps the plan's place.
                if let Some((rect, _)) = viewer.last_frame() {
                    notes_overlay(ui, rect, &notes);
                    if let Some(tag) = &self.tag {
                        self.reveal_settings |= tag_overlay(ui, rect, tag);
                    }
                }
            }
            None => {
                let text = self.placeholder();
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(text).color(palette::TEXT_MUTED));
                });
                if let Some(tag) = &self.tag {
                    self.reveal_settings |= tag_overlay(ui, ui.max_rect(), tag);
                }
            }
        }
        if let (Some(r), Some(canvas)) = (drawn, canvas) {
            self.on_drawn(session, r, canvas);
        }
    }

    /// Give up every member's source and the viewer.
    fn retire_all(&mut self) {
        for m in self.results.values_mut() {
            m.retire(&mut self.retired);
        }
        self.drop_viewer();
        self.shown_key = None;
    }

    /// Drop the viewer, keeping where it looked for the next one.
    fn drop_viewer(&mut self) {
        if self.viewer.is_some() {
            self.keep_camera();
            self.viewer = None;
            self.viewer_geometry = None;
        }
    }

    /// Hold the camera the viewer shows now (unless one is held already),
    /// for a viewer about to be replaced or re-homed.
    fn keep_camera(&mut self) {
        if self.camera.is_none() {
            self.camera = self.shown_camera();
        }
    }

    /// Show `view` with these `members` (its groups). Members already open
    /// are kept; the others are given up, and new ones opened.
    fn apply_view(&mut self, view: ViewKind, members: Vec<String>) {
        let mut gone: Vec<String> = Vec::new();
        for g in self.results.keys() {
            if !members.contains(g) {
                gone.push(g.clone());
            }
        }
        for g in gone {
            if let Some(mut m) = self.results.remove(&g) {
                m.retire(&mut self.retired);
            }
        }
        for g in &members {
            self.results.entry(g.clone()).or_insert_with(Member::new);
        }
        if self.group.as_ref().is_none_or(|g| !members.contains(g)) {
            self.group = members.first().cloned();
            self.health = None;
            self.want_health = true;
        }
        self.view = Some(view);
        self.members = members;
    }

    /// Show `view` (a group, or a composition of `session`) now.
    #[cfg(test)]
    fn set_view(&mut self, session: &OpenSession, view: ViewKind) {
        let compositions = load_compositions(&session.mosaic.config.ui);
        let members = view_members(&view, &compositions);
        self.apply_view(view, members);
    }

    /// Show group `group` from scratch (its result is opened again).
    #[cfg(test)]
    fn select(&mut self, group: String) {
        if let Some(mut m) = self.results.remove(&group) {
            m.retire(&mut self.retired);
        }
        self.apply_view(ViewKind::Group(group.clone()), vec![group]);
    }

    /// The view to show this frame: the current one while it is still
    /// valid, else the first group.
    fn resolve_view(&self, groups: &[String], compositions: &[Composition]) -> Option<ViewKind> {
        match &self.view {
            Some(ViewKind::Group(g)) if groups.contains(g) => self.view.clone(),
            Some(ViewKind::Composition(i)) if *i < compositions.len() => self.view.clone(),
            _ => groups.first().map(|g| ViewKind::Group(g.clone())),
        }
    }

    /// What the viewer's place says when there is nothing to draw.
    fn placeholder(&self) -> String {
        if self.view.is_none() {
            return "Add panels to start a mosaic.".into();
        }
        if self.members.is_empty() {
            return "This composition maps no groups.".into();
        }
        if let Some(why) = &self.view_problem {
            return why.clone();
        }
        let single = self.members.len() == 1;
        for g in &self.members {
            let Some(m) = self.results.get(g) else {
                continue;
            };
            if m.opening.is_some() || m.reopen {
                return "Opening the result…".into();
            }
            if let (None, Some(why)) = (&m.source, &m.absent) {
                return if single {
                    why.clone()
                } else {
                    format!("{g}: {why}")
                };
            }
        }
        String::new()
    }

    /// In a composition view, a note for each group without an open
    /// result ("OIII: Not merged yet.").
    fn member_notes(&self) -> Vec<String> {
        if self.members.len() < 2 {
            return Vec::new();
        }
        self.members
            .iter()
            .filter_map(|g| {
                let m = self.results.get(g)?;
                if m.source.is_some() {
                    return None;
                }
                let why = match &m.absent {
                    Some(why) if m.opening.is_none() && !m.reopen => why.as_str(),
                    _ => "opening the result…",
                };
                Some(format!("{g}: {why}"))
            })
            .collect()
    }

    /// The view's canvas size (from its open results).
    fn view_canvas(&self) -> Option<(u64, u64)> {
        self.members
            .iter()
            .filter_map(|g| self.results.get(g)?.source.as_ref())
            .map(|s| {
                let g = s.tile_geometry();
                (g.width, g.height)
            })
            .next()
    }

    /// Poll tasks, follow merge progress and the look, start pending work,
    /// save.
    fn update(
        &mut self,
        ctx: &egui::Context,
        session: &mut OpenSession,
        jobs: &JobRunner,
        look: DisplayStretch,
        plan_stretch: Option<&[StretchParams]>,
    ) {
        self.follow_look(look);
        self.poll_opening();
        self.retired.retain(|w| w.strong_count() > 0);
        if !self.retired.is_empty() {
            ctx.request_repaint_after(RELEASE_POLL);
        }
        self.follow_merge(ctx, jobs);
        self.poll_auto();
        if self.rois.poll(&mut session.mosaic.config.ui) {
            self.save.touch();
        }
        if let Some(task) = &self.health_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done((g, h)) => {
                    self.health_task = None;
                    if self.group.as_ref() == Some(&g) {
                        self.health = Some(h);
                    }
                }
                Poll::Lost => {
                    self.health_task = None;
                    self.health = Some(Err("health computation failed (see the log)".into()));
                }
            }
        }
        // A merge of a shown group fills in the image: the stretches hold.
        let merging = self.members.iter().any(|g| jobs.blend_pending(g));
        // The plan's stretch stands in for a group's first result (a group
        // view only: a composition's plan stretch is per display channel).
        let plan = match &self.view {
            Some(ViewKind::Group(_)) => plan_stretch.map(<[StretchParams]>::to_vec),
            _ => None,
        };
        for (group, m) in &mut self.results {
            // A merge of this group that is queued, or running but has not
            // reported progress, may still create or recreate the store:
            // open only once it reports (`follow_merge`) or ends.
            if m.reopen && m.opening.is_none() && !jobs.blend_unstarted(group) {
                m.reopen = false;
                let result = session.mosaic.result_path(group);
                let dir = session.mosaic.group_session_dir(group);
                let held = if merging {
                    m.stretch.clone().or_else(|| plan.clone())
                } else {
                    None
                };
                m.opening = Task::spawn("mmm-open-result", ctx, move || {
                    (look, open_result(&result, &dir, held, look))
                });
            }
            let due = match m.refresh {
                Refresh::None => false,
                Refresh::WhenIdle => !merging,
                Refresh::Now => true,
            };
            if due
                && m.auto.is_none()
                && let Some(src) = m.source.clone()
            {
                m.refresh = Refresh::None;
                m.auto = Task::spawn("mmm-auto-stretch", ctx, move || (look, src.auto(look)));
            }
        }
        if self.want_health && self.health_task.is_none() {
            self.want_health = false;
            self.health = None;
            if let Some(g) = self.group.clone()
                && group_readiness(session, &g) == Readiness::Analyzed
            {
                let dir = session.mosaic.group_session_dir(&g);
                let feather = session.mosaic.config.settings_for(&g).1.feather_px;
                self.health_task = Task::spawn("mmm-health", ctx, move || {
                    let h = load_health(&dir, feather);
                    (g, h)
                });
            }
        }
        self.rois
            .start_sync(ctx, session.mosaic.dir.join(FRAME_FILE));
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            // Documented exception: a small mosaic.json write on the UI
            // thread, at most once per SAVE_INTERVAL.
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("ROIs not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }

    /// Ask for every shown result's stretch again when the look's stretch
    /// or the view has changed; a held stretch of a result not open is
    /// dropped with the look it was worked out for.
    fn follow_look(&mut self, look: DisplayStretch) {
        let look_changed = self.look != look;
        let view_changed = self.stretch_view != self.view;
        self.look = look;
        self.stretch_view = self.view.clone();
        if !look_changed && !view_changed {
            return;
        }
        for m in self.results.values_mut() {
            if m.source.is_some() {
                m.refresh = Refresh::Now;
            } else if look_changed {
                m.stretch = None;
            }
        }
    }

    fn poll_opening(&mut self) {
        let look = self.look;
        for m in self.results.values_mut() {
            let Some(task) = &m.opening else { continue };
            let (asked, opened) = match task.poll() {
                Poll::Pending => continue,
                Poll::Done(v) => v,
                Poll::Lost => {
                    m.opening = None;
                    m.retire(&mut self.retired);
                    m.absent = Some("Opening the result failed (see the log).".into());
                    m.refresh = Refresh::None;
                    continue;
                }
            };
            m.opening = None;
            match opened {
                Opened::Ready {
                    source,
                    stretch,
                    given,
                } => {
                    if let Some(old) = m.source.replace(Arc::new(source)) {
                        self.retired.push(Arc::downgrade(&old));
                    }
                    m.absent = None;
                    m.stretch = stretch;
                    if !given {
                        // Worked out from the data just now.
                        m.refresh = Refresh::None;
                    }
                    if asked != look {
                        m.refresh = Refresh::Now;
                    }
                }
                Opened::Absent(why) => {
                    m.retire(&mut self.retired);
                    m.absent = Some(why);
                    m.refresh = Refresh::None;
                }
            }
        }
    }

    /// Give the viewer what the view shows now: the group's result, or the
    /// composite of the composition's results. Rebuilt only when a member
    /// source or the mapping changes; with nothing to show the viewer (and
    /// its loaders' hold on the sources) is dropped. The camera survives
    /// both (see the module docs).
    fn sync_viewer(&mut self, compositions: &[Composition]) {
        let ptr = |g: &String| {
            self.results
                .get(g)
                .and_then(|m| m.source.as_ref())
                .map_or(0, |s| Arc::as_ptr(s) as usize)
        };
        let mapping: Vec<Option<(String, u32)>> = match &self.view {
            Some(ViewKind::Composition(i)) => compositions
                .get(*i)
                .map(|c| {
                    c.channels
                        .iter()
                        .map(|ch| ch.as_ref().map(|(g, w)| (g.clone(), w.to_bits())))
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let key: ShownKey = (self.members.iter().map(ptr).collect(), mapping);
        if self.shown_key.as_ref() == Some(&key) {
            return;
        }
        let source_of = |g: &String| self.results.get(g).and_then(|m| m.source.clone());
        self.view_problem = None;
        // A group view needs its result; a composition shows the members
        // that have one (a missing member adds nothing to a tile).
        let wanted: Option<Arc<dyn TileSource>> = match &self.view {
            _ if key.0.iter().all(|&p| p == 0) => None,
            Some(ViewKind::Group(g)) => source_of(g).map(|s| s as Arc<dyn TileSource>),
            Some(ViewKind::Composition(i)) => {
                let channels: Vec<Option<(Arc<MosaicTileSource>, f32)>> = compositions
                    .get(*i)
                    .map(|c| {
                        c.channels
                            .iter()
                            .map(|ch| ch.as_ref().and_then(|(g, w)| Some((source_of(g)?, *w))))
                            .collect()
                    })
                    .unwrap_or_default();
                match CompositeTileSource::new(&channels) {
                    Ok(c) => Some(Arc::new(c)),
                    Err(why) => {
                        self.view_problem = Some(why);
                        None
                    }
                }
            }
            None => None,
        };
        self.shown_key = Some(key);
        match wanted {
            None => self.drop_viewer(),
            Some(src) => {
                let geometry = src.geometry();
                // A new canvas size would re-home the viewer: keep where
                // it looks instead.
                if self.viewer_geometry != Some(geometry) {
                    self.keep_camera();
                }
                self.viewer_geometry = Some(geometry);
                match &mut self.viewer {
                    Some(v) => v.set_source(src),
                    None => self.viewer = Some(TiledViewer::new(src, self.loader_threads)),
                }
            }
        }
    }

    /// Follow the running merge of a group of the view: bump its source's
    /// generation on each progress update so new tiles load (through the
    /// held stretch); open its result once the merge has created its store.
    fn follow_merge(&mut self, ctx: &egui::Context, jobs: &JobRunner) {
        let Some((id, group, updates)) = jobs.blend_progress() else {
            return;
        };
        let Some(m) = self.results.get_mut(group) else {
            return;
        };
        if updates == 0 || m.last_progress == Some((id, updates)) {
            return;
        }
        m.last_progress = Some((id, updates));
        match &m.source {
            Some(src) => {
                src.bump();
                ctx.request_repaint();
            }
            None if m.opening.is_none() => m.reopen = true,
            None => {}
        }
    }

    /// Apply finished automatic stretches worked out for the current look.
    fn poll_auto(&mut self) {
        let look = self.look;
        for m in self.results.values_mut() {
            let Some(task) = &m.auto else { continue };
            let (asked, stretch) = match task.poll() {
                Poll::Pending => continue,
                Poll::Done(v) => v,
                Poll::Lost => (look, None),
            };
            m.auto = None;
            // Another look since: asked for again (`follow_look`). No
            // statistics (nothing merged yet): the stretch stays as it is
            // until the next merge ends.
            if asked != look {
                continue;
            }
            let (Some(s), Some(src)) = (stretch, &m.source) else {
                continue;
            };
            src.set_stretch(s.clone());
            m.stretch = Some(s);
        }
    }

    /// The ROI the list deleted since the last call, with its index, once:
    /// the window offers to undo it.
    pub fn take_roi_deleted(&mut self) -> Option<(usize, Roi)> {
        self.rois.take_deleted()
    }

    /// Put back a deleted ROI at its index, selected (saved, debounced).
    pub fn restore_roi(&mut self, session: &mut OpenSession, index: usize, roi: Roi) {
        self.rois.load_once(&session.mosaic.config.ui);
        if self.rois.restore(&mut session.mosaic.config.ui, index, roi) {
            self.save.touch();
        }
    }

    /// A rectangle drawn on a canvas (`r` in its image pixels, which is
    /// `canvas` pixels wide and high) becomes a new ROI, selected and saved
    /// (debounced).
    pub fn on_drawn(&mut self, session: &mut OpenSession, r: [f64; 4], canvas: (u64, u64)) {
        if self.rois.on_drawn(&mut session.mosaic.config.ui, r, canvas) {
            self.save.touch();
        }
    }
}

/// The tag over the top-left corner of the image: what it says, and whether
/// it is a quiet note (the settings changed since the merge) rather than a
/// warning (out of date).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasTag {
    /// What it says.
    pub text: String,
    /// Drawn quieter: muted text on a translucent panel.
    pub quiet: bool,
}

/// `tag` over the top-left corner of the image drawn in `rect`, on a panel
/// so it reads over any image: a warning's is opaque, a quiet tag's
/// translucent (the image shows through) yet its muted text keeps AA
/// contrast over a white image ([`scrim`]). Clicked, true.
fn tag_overlay(ui: &mut egui::Ui, rect: egui::Rect, tag: &CanvasTag) -> bool {
    let mut over = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(space::S))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let (fill, ink) = if tag.quiet {
        (
            scrim(palette::SURFACE_800, palette::TEXT_MUTED),
            palette::TEXT_MUTED,
        )
    } else {
        (palette::SURFACE_800, palette::TEXT_BODY)
    };
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(radius::SMALL)
        .inner_margin(egui::Margin::same(space::XS as i8))
        .show(&mut over, |ui| {
            let label = text(Type::Small, &tag.text).color(ink);
            let r = ui.add(
                egui::Label::new(label)
                    .selectable(false)
                    .sense(egui::Sense::click()),
            );
            r.on_hover_text("Merge re-merges it with the Merge settings")
                .clicked()
        })
        .inner
}

/// `notes` (the members of a composition not shown) over the bottom-left
/// corner of the image drawn in `rect`, on a panel so they read over any
/// image, first note on top. Labels only: a drag there still pans.
fn notes_overlay(ui: &mut egui::Ui, rect: egui::Rect, notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    let mut over = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(space::S))
            .layout(egui::Layout::bottom_up(egui::Align::Min)),
    );
    egui::Frame::NONE
        .fill(palette::SURFACE_800)
        .corner_radius(radius::SMALL)
        .inner_margin(egui::Margin::same(space::XS as i8))
        .show(&mut over, |ui| {
            // Bottom up: the last note first.
            for n in notes.iter().rev() {
                let note = text(Type::Small, n).color(palette::TEXT_MUTED);
                ui.add(egui::Label::new(note).selectable(false));
            }
        });
}

/// The health figures as a two-column grid.
fn health_grid(ui: &mut egui::Ui, h: &Health) {
    let flagged = h.seams.iter().filter(|s| s.flagged).count();
    egui::Grid::new("mosaic-health")
        .num_columns(2)
        .spacing([space::M, space::XS])
        .show(ui, |ui| {
            let row = |ui: &mut egui::Ui, k: &str, v: String, c: Color32| {
                ui.label(RichText::new(k).color(palette::TEXT_BODY));
                ui.label(RichText::new(v).color(c));
                ui.end_row();
            };
            row(
                ui,
                "Seam step median",
                format_step(h.seam_delta_median),
                palette::TEXT_STRONG,
            );
            row(
                ui,
                "Seam step max",
                format_step(h.seam_delta_max),
                palette::TEXT_STRONG,
            );
            row(
                ui,
                "Flagged edges",
                format!("{flagged} of {}", h.seams.len()),
                if flagged > 0 {
                    palette::HA_400
                } else {
                    palette::GREEN_300
                },
            );
            let n = h.gain_range.len();
            for (c, (lo, hi)) in h.gain_range.iter().enumerate() {
                let k = if n > 1 {
                    format!("Gain range {}", channel_name(c, n))
                } else {
                    "Gain range".to_string()
                };
                row(ui, &k, format!("{lo:.3} – {hi:.3}"), palette::TEXT_STRONG);
            }
        });
}

/// Where `group` stands for merging.
fn group_readiness(session: &OpenSession, group: &str) -> Readiness {
    let config = &session.mosaic.config;
    if !config.group_dirs.contains_key(group) {
        Readiness::NotAnalyzed
    } else if config.needs_analyze.contains(group) {
        Readiness::NeedsAnalyze
    } else {
        Readiness::Analyzed
    }
}

/// A seam step (linear data units) for display.
fn format_step(v: f64) -> String {
    if v == 0.0 {
        "0".into()
    } else if v.abs() < 1e-3 {
        format!("{v:.2e}")
    } else {
        format!("{v:.4}")
    }
}

#[cfg(test)]
#[path = "mosaic_view_tests.rs"]
mod tests;
