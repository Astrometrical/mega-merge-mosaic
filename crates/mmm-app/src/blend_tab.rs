//! The Blend tab: blend controls, the tiled result viewer with its display
//! stretch, regions of interest, and the shown group's health.
//!
//! - **Views.** Each group, then each composition (the Plan compositions,
//!   [`crate::compositions`]). A composition view mixes its groups' results
//!   in a [`CompositeTileSource`], each through that group's own Blend
//!   display stretch; the side panel's stretch and health are those of one
//!   member ("Stretch of"). A group not blended yet adds nothing and is
//!   named above the image; groups on different canvases cannot be mixed:
//!   the view says why instead.
//! - **ROIs** ([`crate::rois`]): drawn on the canvas with the viewer's draw
//!   tool ("+ new ROI"), named, noted, selected (the viewer zooms to it)
//!   and deleted; kept in `MosaicConfig.ui`. Reading the frame and
//!   reprojecting them (sky ↔ canvas) and counting how many of an ROI's
//!   level-0 tiles are fresh (per group of the view) run on workers
//!   ([`RoiPanel`]). **Blend ROI** starts one blend of the selected ROI per
//!   group of the view (the job runner queues them; newer work wins per
//!   group), once the ROIs follow the current frame; **Blend whole
//!   mosaic** blends every group over the whole canvas.
//! - **Blend settings** ([`crate::blend_settings`]): the mosaic-wide
//!   analyze and blend settings, staged and applied with their cost shown
//!   first. While its tile count runs or an edit waits to apply, Blend is
//!   disabled; an applied edit reloads the view's (now stale) tiles.
//! - **Export…** ([`crate::export`]) writes the view's groups' results to
//!   FITS or TIFF as jobs, after warning about stale or missing tiles.
//!
//! Everything slow runs on background threads ([`Task`]): opening each
//! result ([`open_result`]), the automatic display stretch statistics, the
//! health metrics, the ROI sync and freshness counts. The one write on the
//! UI thread is the debounced `mosaic.json` save of a manual display
//! stretch or the ROI list (at most once per [`SAVE_INTERVAL`]), the same
//! documented exception as the job runner's plan/apply saves.
//!
//! # No reader across a store recreation
//!
//! `run_blend` recreates (truncates) a group's store only when it is
//! missing, unparseable, or its geometry differs from the group's analysed
//! canvas; a live [`mmm_core::tiles::TileReader`] would then fault. This tab
//! guards the geometry path (the other two have no reader to fault), for
//! every group of the view:
//!
//! - a source is kept only when [`open_result`] found the store's geometry
//!   equal to the group's analysed canvas, so a blend reopens it in place;
//! - the canvas changes only by analysis: after every analysis (finished,
//!   cancelled or failed) every member's result is checked again, and while
//!   an analysis is running or queued, or any member's check is pending,
//!   **Blend** and **Blend ROI** are disabled (a blend queued behind an
//!   analysis would start the moment it is applied, before the check);
//! - a source given up (out of date, view switched, replaced) is tracked by
//!   a `Weak` until the viewer's loader threads and any statistics or
//!   freshness task have dropped it; **Blend** stays disabled until then.
//!   A composite holds every member's reader, so a member given up stays
//!   tracked until the composite itself is dropped: all member readers are
//!   released before any member's store may be recreated;
//! - with no source open, a blend's first progress update means it has
//!   created its store (progress is only reported once tiles are written),
//!   so the result is opened then and tiles appear while it runs; while a
//!   blend of a group of the view is queued or has not reported yet (e.g.
//!   after switching to that group), no open of that group starts.
//!
//! Another program (the `mmm` CLI) recreating the store while it is shown
//! is not guarded against.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use mega_ui::theme::palette;
use mega_ui::viewer::{TileSource, TiledViewer};
use mmm_core::health::{Health, health};
use mmm_core::mosaic::FRAME_FILE;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::settings::ChangeImpact;
use mmm_core::stretch::StretchParams;
use mmm_core::surfaces::Surfaces;
use serde_json::Value;

use crate::blend_settings::SettingsPanel;
use crate::compositions::{Composition, ViewKind, load_compositions};
use crate::export::{ExportAction, ExportDialog, RfdSavePicker};
use crate::jobs::{JobEvent, JobRunner};
use crate::rois::{Roi, RoiPanel};
use crate::session::OpenSession;
use crate::stretch_panel::{
    BLEND_STRETCH_KEY, StretchChange, StretchState, channel_name, save_stretch_at,
    saved_stretch_at, stretch_panel,
};
use crate::tile_source::{CompositeTileSource, MosaicTileSource, Opened, open_result};

/// Key in `MosaicConfig.ui` of the Blend view's display stretches, by group.
pub const STRETCH_KEY: &str = BLEND_STRETCH_KEY;
/// Least time between two saves of the display stretch.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// How often to look again while a given-up source is still being released.
const RELEASE_POLL: Duration = Duration::from_millis(20);
/// Width of the display stretch and health panel.
const SIDE_WIDTH: f32 = 260.0;

/// The saved display stretch of `group` in `ui` (`MosaicConfig.ui`), if
/// any: present means the user set it; absent means automatic.
pub fn saved_stretch(ui: &Value, group: &str) -> Option<Vec<StretchParams>> {
    saved_stretch_at(ui, STRETCH_KEY, group)
}

/// Save (`Some`) or clear (`None`, back to automatic) `group`'s display
/// stretch in `ui`, keeping everything else there.
pub fn save_stretch(ui: &mut Value, group: &str, stretch: Option<&[StretchParams]>) {
    save_stretch_at(ui, STRETCH_KEY, group, stretch);
}

/// Rate limit for saving: a change saves at once, then at most once per
/// [`SAVE_INTERVAL`].
#[derive(Debug, Default)]
pub struct Debounce {
    dirty: bool,
    last: Option<Instant>,
}

impl Debounce {
    /// Something changed that needs saving.
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    /// Whether a change is waiting to be saved (due or not).
    pub fn pending(&self) -> bool {
        self.dirty
    }

    /// Whether to save at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.dirty && self.wait(now).is_none()
    }

    /// Time left before a pending change may be saved (`None` if none is
    /// pending or it may be saved now).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        let last = self.last?;
        let left = SAVE_INTERVAL.saturating_sub(now.saturating_duration_since(last));
        (self.dirty && !left.is_zero()).then_some(left)
    }

    /// A save happened at `now`.
    pub fn saved(&mut self, now: Instant) {
        self.dirty = false;
        self.last = Some(now);
    }
}

/// A computation on its own thread; the UI is repainted when it ends.
pub(crate) struct Task<T> {
    rx: Receiver<T>,
}

/// What a [`Task`] has to say.
pub(crate) enum Poll<T> {
    Pending,
    Done(T),
    /// The thread ended without a result (it panicked).
    Lost,
}

impl<T: Send + 'static> Task<T> {
    pub(crate) fn spawn(
        name: &str,
        ctx: &egui::Context,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = tx.send(f());
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => Some(Task { rx }),
            Err(e) => {
                tracing::error!("cannot start thread {name}: {e}");
                None
            }
        }
    }

    pub(crate) fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(v) => Poll::Done(v),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }
}

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

/// The processing groups of `session`: resolved ones if known, else those
/// known from analysis.
pub fn group_names(session: &OpenSession) -> Vec<String> {
    match &session.groups {
        Some(g) => g.processing_groups().into_iter().map(|(n, _)| n).collect(),
        None => session.mosaic.config.group_dirs.keys().cloned().collect(),
    }
}

/// Where a group (or the whole view) stands for blending; ordered worst
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Readiness {
    /// Never analysed.
    NotAnalyzed,
    /// Analyse settings changed since the last analysis.
    NeedsAnalyze,
    /// Can be blended.
    Analyzed,
}

/// The groups a view shows: the group itself, or a composition's mapped
/// groups (each once, in channel order). An unknown composition shows
/// nothing.
fn view_members(view: &ViewKind, compositions: &[Composition]) -> Vec<String> {
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

/// One group of the view: its open result and its Blend display stretch.
struct Member {
    /// The open result (geometry checked, see the module docs).
    source: Option<Arc<MosaicTileSource>>,
    /// Why there is no source.
    absent: Option<String>,
    /// A pending [`open_result`].
    opening: Option<Task<Opened>>,
    /// Open (again) once no open is pending.
    reopen: bool,
    /// Last seen `(job, progress updates)` of this group's running blend.
    last_progress: Option<(u64, u64)>,
    /// A pending automatic stretch for `source`.
    auto: Option<Task<Option<Vec<StretchParams>>>>,
    /// Save the pending automatic stretch as a manual one (Auto pressed
    /// with Linked on).
    save_auto_result: bool,
    /// Compute the automatic stretch once possible (unless manual).
    want_auto: bool,
    /// One set of sliders for every channel.
    linked: bool,
    /// The stretch was set by the user (saved), so blends keep it.
    manual: bool,
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
            save_auto_result: false,
            want_auto: false,
            linked: false,
            manual: false,
        }
    }

    /// Nothing pending for this member.
    fn idle(&self) -> bool {
        self.opening.is_none() && !self.reopen && self.auto.is_none()
    }

    /// Give up the source (the viewer's loaders let go of it after their
    /// current fetch), tracking it in `retired`.
    fn retire(&mut self, retired: &mut Vec<Weak<MosaicTileSource>>) {
        if let Some(src) = self.source.take() {
            retired.push(Arc::downgrade(&src));
        }
        self.auto = None;
        self.save_auto_result = false;
    }
}

/// What the viewer shows: each member's source (by address, 0 for none)
/// and the composition mapping, so the viewer source is rebuilt only when
/// one of them changes.
type ShownKey = (Vec<usize>, Vec<Option<(String, u32)>>);

/// The Blend tab's state for the open session.
pub struct BlendTab {
    /// The view shown: a group or a composition.
    view: Option<ViewKind>,
    /// The groups of the view, in order.
    members: Vec<String>,
    /// The member whose display stretch and health are shown.
    group: Option<String>,
    /// Each member's result and stretch state.
    results: BTreeMap<String, Member>,
    viewer: Option<TiledViewer>,
    /// What `viewer` was last given (see [`ShownKey`]).
    shown_key: Option<ShownKey>,
    /// Why the view cannot be drawn though its results are open (e.g. a
    /// composition of groups on different canvases).
    view_problem: Option<String>,
    /// Sources given up but maybe still mapped by loader, statistics or
    /// freshness threads (directly or inside a composite).
    retired: Vec<Weak<MosaicTileSource>>,
    health: Option<HealthResult>,
    health_task: Option<Task<(String, HealthResult)>>,
    want_health: bool,
    save: Debounce,
    /// The analysis this tab started (or tracks), until it ends.
    analyze_job: Option<u64>,
    /// The blends this tab started, until they end: job id → group.
    blend_jobs: BTreeMap<u64, String>,
    loader_threads: usize,
    /// Drawn at least once.
    shown: bool,
    /// The session's ROIs and their workers.
    rois: RoiPanel,
    /// The export dialog.
    export: ExportDialog,
    /// The Blend settings panel.
    settings: SettingsPanel,
}

impl Default for BlendTab {
    fn default() -> Self {
        Self::new()
    }
}

impl BlendTab {
    /// A tab showing nothing yet; the first group opens on the first frame.
    pub fn new() -> Self {
        BlendTab {
            view: None,
            members: Vec::new(),
            group: None,
            results: BTreeMap::new(),
            viewer: None,
            shown_key: None,
            view_problem: None,
            retired: Vec::new(),
            health: None,
            health_task: None,
            want_health: false,
            save: Debounce::default(),
            analyze_job: None,
            blend_jobs: BTreeMap::new(),
            loader_threads: std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .clamp(2, 8),
            shown: false,
            rois: RoiPanel::default(),
            export: ExportDialog::new(Arc::new(RfdSavePicker)),
            settings: SettingsPanel::new(),
        }
    }

    /// Open the export dialog for the view's groups.
    pub fn open_export(&mut self, session: &OpenSession) {
        self.export.open(session, self.members.clone());
    }

    /// Draw the export dialog (if open) and act on the user's decision:
    /// blend the chosen groups first, or start one export job per group.
    fn export_ui(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &mut JobRunner) {
        let can_blend = self.can_blend(self.readiness(session));
        // Blends of the dialog's groups queued or running (whatever view is
        // showing now): the export check maps the stores they may recreate.
        let blending: BTreeSet<String> = self
            .export
            .groups()
            .into_iter()
            .filter(|g| jobs.blend_pending(g))
            .collect();
        match self.export.ui(ctx, session, can_blend, &blending) {
            Some(ExportAction::BlendFirst(groups)) => {
                self.blend_groups(session, jobs, groups, None);
            }
            Some(ExportAction::Export(dests, format)) => {
                for (group, out) in dests {
                    jobs.start_export(session, &group, out, format);
                }
            }
            None => {}
        }
    }

    /// The group whose display stretch and health are shown (in a
    /// composition view, one of its groups).
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The view shown.
    pub fn view(&self) -> Option<&ViewKind> {
        self.view.as_ref()
    }

    /// The shown group's open result, if any.
    pub fn source(&self) -> Option<&Arc<MosaicTileSource>> {
        self.member()?.source.as_ref()
    }

    /// Why the shown group has no result, if it has none. `None` while its
    /// result is being opened (again): a reason from an earlier open may no
    /// longer hold, and the tab shows "Opening the result…" meanwhile.
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

    fn member(&self) -> Option<&Member> {
        self.results.get(self.group.as_ref()?)
    }

    /// True once drawn with nothing pending: no open, statistics, health,
    /// ROI or freshness task, and the viewer (if any) has drawn and loaded
    /// its tiles. A session without groups settles at once.
    pub fn is_settled(&self) -> bool {
        self.shown
            && self.results.values().all(Member::idle)
            && self.health_task.is_none()
            && self.rois.idle()
            && self.export.idle()
            && self.settings.idle()
            && !self.settings.has_pending()
            && self
                .viewer
                .as_ref()
                .is_none_or(|v| v.last_frame().is_some() && !v.is_loading())
    }

    /// Save a pending (debounced) display stretch or ROI change now,
    /// whatever the rate limit: the window is closing, or the session state
    /// is being saved. A small `mosaic.json` write on the UI thread (the
    /// module's documented exception).
    pub fn flush_save(&mut self, session: &OpenSession) {
        if !self.save.pending() {
            return;
        }
        self.save.saved(Instant::now());
        if let Err(e) = session.mosaic.save() {
            tracing::warn!("display stretch and ROIs not saved: {e}");
        }
    }

    /// Mark the display stretch as changed and unsaved (tests).
    #[cfg(test)]
    pub(crate) fn touch_save(&mut self) {
        self.save.touch();
    }

    /// Whether **Blend** may start now (see the module docs for the store
    /// recreation guard): every group of the view is checked and open (or
    /// known absent), and no given-up source is still mapped.
    fn can_blend(&self, readiness: Readiness) -> bool {
        readiness == Readiness::Analyzed
            && self.export.idle()
            && self.settings.idle()
            && !self.settings.has_pending()
            && !self.members.is_empty()
            && self.analyze_job.is_none()
            && self
                .results
                .values()
                .all(|m| m.opening.is_none() && !m.reopen)
            && self.retired.is_empty()
    }

    /// Treat analysis job `id`, started elsewhere (the automatic
    /// pipeline), as this tab's: Analyze is disabled until it ends, and
    /// the result and health are checked again then.
    pub fn track_analysis(&mut self, id: u64) {
        self.analyze_job = Some(id);
    }

    /// React to a job that ended (from [`JobRunner::poll`]).
    pub fn on_event(&mut self, event: &JobEvent) {
        self.settings.on_event(event);
        let (id, finished) = match event {
            JobEvent::Finished { id, .. } => (*id, true),
            JobEvent::Cancelled { id } | JobEvent::Failed { id, .. } => (*id, false),
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
        } else if let Some(group) = self.blend_jobs.remove(&id) {
            if let Some(m) = self.results.get_mut(&group) {
                match &m.source {
                    Some(src) => {
                        src.bump();
                        m.want_auto |= finished;
                    }
                    None => m.reopen = true,
                }
            }
            if self.group.as_ref() == Some(&group) {
                self.want_health = true;
            }
            self.rois.want_fresh = true;
        }
    }

    /// A global settings edit waits to apply, or jobs cancelled for one
    /// have not all been reported ([`SettingsPanel::editing`]): the
    /// automatic pipeline starts nothing meanwhile and restarts what was
    /// cancelled.
    pub fn settings_editing(&self, session: &OpenSession) -> bool {
        self.settings.editing(&session.mosaic.config)
    }

    /// A settings edit waits for the jobs in its way to stop (or a busy
    /// store).
    #[cfg(test)]
    pub fn settings_pending(&self) -> bool {
        self.settings.has_pending()
    }

    /// The Blend settings panel's background work and waiting edit (see
    /// [`SettingsPanel::poll`]). Call every frame after the runner's poll,
    /// whichever tab is shown; the impact when an edit was applied.
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

    /// Settings were applied: the view's results went stale (their tiles
    /// load again to show it), and ROI freshness and health are computed
    /// again (health depends on the feather).
    fn on_settings_applied(&mut self, impact: ChangeImpact) {
        if impact == ChangeImpact::None {
            return;
        }
        for m in self.results.values() {
            if let Some(src) = &m.source {
                src.bump();
            }
        }
        self.rois.want_fresh = true;
        self.want_health = true;
    }

    /// Give up every member's source and the viewer.
    fn retire_all(&mut self) {
        for m in self.results.values_mut() {
            m.retire(&mut self.retired);
        }
        self.viewer = None;
        self.shown_key = None;
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
        if self.view.as_ref() != Some(&view) {
            self.rois.view_changed();
        }
        self.view = Some(view);
        self.members = members;
    }

    /// Show `view` from the next frame on (its groups are resolved then).
    pub fn select_view(&mut self, view: ViewKind) {
        if self.view.as_ref() != Some(&view) {
            self.rois.view_changed();
            self.view = Some(view);
        }
    }

    /// The view's key: the group's name, for a group view; `None` before a
    /// view is chosen, and for a composition (the Plan tab keys those).
    pub fn view_key(&self) -> Option<&str> {
        match self.view.as_ref()? {
            ViewKind::Group(g) => Some(g),
            ViewKind::Composition(_) => None,
        }
    }

    /// What the viewer's place says instead of an image, if it has no
    /// image to show.
    pub fn shown_text(&self) -> Option<String> {
        self.viewer.is_none().then(|| self.placeholder())
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

    /// Draw the tab.
    pub fn ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, jobs: &mut JobRunner) {
        let ctx = ui.ctx().clone();
        self.shown = true;
        self.rois.load_once(&session.mosaic.config.ui);
        let groups = group_names(session);
        let compositions = load_compositions(&session.mosaic.config.ui);
        match self.resolve_view(&groups, &compositions) {
            Some(view) => {
                // A composition's mapping may change (in Plan): follow it.
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
        self.update(&ctx, session, jobs);
        self.sync_viewer(&compositions);
        let readiness = self.readiness(session);

        egui::Panel::top("blend-toolbar").show(ui, |ui| {
            self.toolbar(ui, session, jobs, &groups, &compositions, readiness);
        });
        egui::Panel::right("blend-side")
            .resizable(false)
            .exact_size(SIDE_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(impact) = self.settings.ui(ui, session, jobs) {
                        self.on_settings_applied(impact);
                    }
                    ui.add_space(12.0);
                    self.member_picker(ui);
                    self.stretch_ui(ui, session);
                    ui.add_space(16.0);
                    if self
                        .rois
                        .ui(ui, &mut session.mosaic.config.ui, self.viewer.as_mut())
                    {
                        self.save.touch();
                    }
                    ui.add_space(16.0);
                    let r = self
                        .group
                        .as_ref()
                        .map_or(Readiness::NotAnalyzed, |g| group_readiness(session, g));
                    self.health_ui(ui, r);
                });
            });
        let canvas = self.view_canvas();
        let overlays = self.rois.overlays();
        let notes = self.member_notes();
        let mut drawn = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| match &mut self.viewer {
                Some(viewer) => {
                    // A composition shows its blended groups; the others
                    // are named here.
                    for n in &notes {
                        ui.label(RichText::new(n).small().color(palette::TEXT_MUTED));
                    }
                    viewer.set_overlays(overlays);
                    drawn = viewer.show(ui).drawn_rect;
                }
                None => {
                    let text = self.placeholder();
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new(text).color(palette::TEXT_MUTED));
                    });
                }
            });
        if let (Some(r), Some(canvas)) = (drawn, canvas) {
            self.on_drawn(session, r, canvas);
        }
        self.export_ui(&ctx, session, jobs);
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
    /// result ("OIII: Not blended yet.").
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

    fn readiness(&self, session: &OpenSession) -> Readiness {
        self.members
            .iter()
            .map(|g| group_readiness(session, g))
            .min()
            .unwrap_or(Readiness::NotAnalyzed)
    }

    /// Poll tasks, follow blend progress, start pending work, save.
    fn update(&mut self, ctx: &egui::Context, session: &mut OpenSession, jobs: &JobRunner) {
        self.poll_opening();
        self.retired.retain(|w| w.strong_count() > 0);
        if !self.retired.is_empty() {
            ctx.request_repaint_after(RELEASE_POLL);
        }
        self.follow_blend(ctx, jobs);
        self.poll_auto(session);
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
        for (group, m) in &mut self.results {
            // A blend of this group that is queued, or running but has not
            // reported progress, may still create or recreate the store:
            // open only once it reports (`follow_blend`) or ends.
            if m.reopen && m.opening.is_none() && !jobs.blend_unstarted(group) {
                m.reopen = false;
                let result = session.mosaic.result_path(group);
                let dir = session.mosaic.group_session_dir(group);
                let saved = saved_stretch(&session.mosaic.config.ui, group);
                m.opening = Task::spawn("mmm-open-result", ctx, move || {
                    open_result(&result, &dir, saved)
                });
            }
            if m.want_auto
                && !m.manual
                && m.auto.is_none()
                && let Some(src) = m.source.clone()
            {
                m.want_auto = false;
                m.auto = Task::spawn("mmm-auto-stretch", ctx, move || src.auto(false));
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
        let members = self
            .members
            .iter()
            .map(|g| {
                (
                    g.clone(),
                    self.results.get(g).and_then(|m| m.source.clone()),
                )
            })
            .collect();
        self.rois
            .start_tasks(ctx, session.mosaic.dir.join(FRAME_FILE), members);
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            // Documented exception: a small mosaic.json write on the UI
            // thread, at most once per SAVE_INTERVAL.
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("display stretch and ROIs not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }

    fn poll_opening(&mut self) {
        for m in self.results.values_mut() {
            let Some(task) = &m.opening else { continue };
            let opened = match task.poll() {
                Poll::Pending => continue,
                Poll::Done(v) => v,
                Poll::Lost => {
                    m.opening = None;
                    m.retire(&mut self.retired);
                    m.absent = Some("Opening the result failed (see the log).".into());
                    continue;
                }
            };
            m.opening = None;
            match opened {
                Opened::Ready { source, saved } => {
                    let src = Arc::new(source);
                    m.manual = saved;
                    m.linked = saved && src.tile_geometry().channels > 1 && {
                        let s = src.stretch();
                        s.iter().all(|p| *p == s[0])
                    };
                    if let Some(old) = m.source.replace(src) {
                        self.retired.push(Arc::downgrade(&old));
                    }
                    m.absent = None;
                }
                Opened::Absent(why) => {
                    m.retire(&mut self.retired);
                    m.absent = Some(why);
                }
            }
        }
    }

    /// Give the viewer what the view shows now: the group's result, or the
    /// composite of the composition's results. Rebuilt only when a member
    /// source or the mapping changes; with nothing to show the viewer (and
    /// its loaders' hold on the sources) is dropped.
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
        match (wanted, &mut self.viewer) {
            (None, _) => self.viewer = None,
            (Some(src), Some(v)) => v.set_source(src),
            (Some(src), None) => self.viewer = Some(TiledViewer::new(src, self.loader_threads)),
        }
        self.rois.want_fresh = true;
    }

    /// Follow the running blend of a group of the view: bump its source's
    /// generation on each progress update so new tiles load; open its
    /// result once the blend has created its store.
    fn follow_blend(&mut self, ctx: &egui::Context, jobs: &JobRunner) {
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
        self.rois.want_fresh = true;
    }

    /// Apply finished automatic stretches (unless the user has set one
    /// meanwhile).
    fn poll_auto(&mut self, session: &mut OpenSession) {
        for (g, m) in &mut self.results {
            let Some(task) = &m.auto else { continue };
            let stretch = match task.poll() {
                Poll::Pending => continue,
                Poll::Done(s) => s,
                Poll::Lost => None,
            };
            m.auto = None;
            let keep = std::mem::take(&mut m.save_auto_result);
            let Some(s) = stretch else {
                // No statistics (nothing blended yet). A Linked Auto saved
                // nothing, so the stretch is automatic again: the next
                // finished blend computes it (`on_event` sets `want_auto`).
                // Asking for it now would only spin on the same empty result.
                if keep {
                    m.manual = false;
                }
                continue;
            };
            let Some(src) = &m.source else { continue };
            if keep {
                save_stretch(&mut session.mosaic.config.ui, g, Some(&s));
                self.save.touch();
            } else if m.manual {
                continue;
            }
            src.set_stretch(s);
        }
    }

    /// A rectangle drawn on the canvas becomes a new ROI, selected and
    /// saved (debounced).
    fn on_drawn(&mut self, session: &mut OpenSession, r: [f64; 4], canvas: (u64, u64)) {
        if self.rois.on_drawn(&mut session.mosaic.config.ui, r, canvas) {
            self.save.touch();
        }
    }

    /// Blend the selected ROI for each group of the view (newer work wins
    /// per group in the job runner). Not while the ROIs are being
    /// reprojected: the rect may still be from an older frame.
    fn blend_roi(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        if !self.rois.synced() {
            return;
        }
        let Some(rect) = self.rois.selected_rect() else {
            return;
        };
        self.blend_groups(session, jobs, self.members.clone(), Some(rect));
    }

    /// Blend every group of the mosaic over the whole canvas (spec §5.4).
    fn blend_whole(&mut self, session: &mut OpenSession, jobs: &mut JobRunner) {
        let groups = group_names(session);
        self.blend_groups(session, jobs, groups, None);
    }

    /// Blend `rect` (`None`: the whole canvas) of each of `groups`.
    fn blend_groups(
        &mut self,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: Vec<String>,
        rect: Option<[u64; 4]>,
    ) {
        // The export check and the settings tile count map result stores
        // a blend may recreate; a settings edit waiting to apply would
        // cancel the blend anyway.
        if !self.export.idle() || !self.settings.idle() || self.settings.has_pending() {
            return;
        }
        for g in groups {
            let id = jobs.start_blend(session, &g, rect);
            self.blend_jobs.insert(id, g);
        }
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: &[String],
        compositions: &[Composition],
        readiness: Readiness,
    ) {
        ui.horizontal(|ui| {
            if groups.len() > 1 || !compositions.is_empty() {
                let shown = match &self.view {
                    Some(ViewKind::Group(g)) => g.clone(),
                    Some(ViewKind::Composition(i)) => compositions
                        .get(*i)
                        .map(|c| c.name.clone())
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let mut chosen = self.view.clone();
                egui::ComboBox::from_label("View")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for g in groups {
                            ui.selectable_value(&mut chosen, Some(ViewKind::Group(g.clone())), g);
                        }
                        if !compositions.is_empty() {
                            ui.separator();
                        }
                        for (i, c) in compositions.iter().enumerate() {
                            let v = Some(ViewKind::Composition(i));
                            ui.selectable_value(&mut chosen, v, &c.name);
                        }
                    });
                if chosen != self.view
                    && let Some(v) = chosen
                {
                    let members = view_members(&v, compositions);
                    self.apply_view(v, members);
                }
                ui.separator();
            }
            let analyzing = self.analyze_job.is_some();
            let has_group = !self.members.is_empty();
            if ui
                .add_enabled(has_group && !analyzing, egui::Button::new("Analyze"))
                .on_hover_text("Analyze every group's panels")
                .clicked()
            {
                self.analyze_job = Some(jobs.start_analyze(session));
            }
            let can_blend = self.can_blend(readiness);
            let all_ready = groups
                .iter()
                .all(|g| group_readiness(session, g) == Readiness::Analyzed);
            if ui
                .add_enabled(
                    can_blend && all_ready,
                    egui::Button::new("Blend whole mosaic"),
                )
                .on_hover_text("Blend every group over the whole canvas")
                .on_disabled_hover_text(if self.export.idle() {
                    "Blending is not possible right now"
                } else {
                    "Waiting for the export check"
                })
                .clicked()
            {
                self.blend_whole(session, jobs);
            }
            let has_roi = self.rois.selected_rect().is_some();
            if ui
                .add_enabled(
                    can_blend && has_roi && self.rois.synced(),
                    egui::Button::new("Blend ROI"),
                )
                .on_hover_text("Blend the selected region of interest for this view's groups")
                .on_disabled_hover_text(if !self.export.idle() {
                    "Waiting for the export check"
                } else if has_roi {
                    "Blending is not possible right now"
                } else {
                    "Select or draw a region of interest first"
                })
                .clicked()
            {
                self.blend_roi(session, jobs);
            }
            if ui
                .add_enabled(
                    has_group && !analyzing && !self.export.is_open(),
                    egui::Button::new("Export…"),
                )
                .on_hover_text("Export this view's blended groups to FITS or TIFF")
                .clicked()
            {
                self.open_export(session);
            }
            let views = jobs.views();
            if ui
                .add_enabled(jobs.busy(), egui::Button::new("Cancel"))
                .clicked()
            {
                for v in &views {
                    jobs.cancel(v.id);
                }
            }
            if let Some(v) = views.first() {
                ui.spinner();
                let pct = (v.done * 100)
                    .checked_div(v.total)
                    .map(|p| format!(" {p}%"))
                    .unwrap_or_default();
                ui.label(
                    RichText::new(format!("{} — {}{pct}", v.label, v.stage))
                        .color(palette::TEXT_BODY),
                );
            }
            let note = match readiness {
                _ if !has_group => None,
                Readiness::NotAnalyzed => Some("Analyze the panels before blending."),
                Readiness::NeedsAnalyze => Some(
                    "Analyze settings changed since the last analysis: analyze before blending.",
                ),
                Readiness::Analyzed => None,
            };
            if let Some(note) = note {
                ui.label(RichText::new(note).color(palette::TEXT_MUTED));
            }
        });
    }

    /// In a composition view: which group's display stretch and health the
    /// side panel shows.
    fn member_picker(&mut self, ui: &mut egui::Ui) {
        if self.members.len() < 2 {
            return;
        }
        let mut chosen = self.group.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Stretch of")
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
        ui.add_space(8.0);
    }

    /// The shared display stretch panel, wired to the shown group's source,
    /// Auto task and the debounced save.
    fn stretch_ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession) {
        let Some(g) = self.group.clone() else {
            stretch_panel(ui, &mut StretchState::default());
            return;
        };
        let Some(m) = self.results.get_mut(&g) else {
            return;
        };
        let src = m.source.clone();
        let mut state = StretchState {
            stretch: src.as_ref().map_or_else(Vec::new, |s| s.stretch()),
            channels: src
                .as_ref()
                .map_or(0, |s| s.tile_geometry().channels as usize),
            linked: m.linked,
            manual: m.manual,
            auto_busy: m.auto.is_some(),
        };
        let change = stretch_panel(ui, &mut state);
        m.linked = state.linked;
        match (change, src) {
            (StretchChange::Auto, _) => self.auto_now(ui.ctx(), session),
            (StretchChange::Manual(stretch), Some(src)) => {
                src.set_stretch(stretch.clone());
                m.manual = true;
                m.auto = None;
                m.save_auto_result = false;
                m.want_auto = false;
                save_stretch(&mut session.mosaic.config.ui, &g, Some(&stretch));
                self.save.touch();
                ui.ctx().request_repaint();
            }
            _ => {}
        }
    }

    /// Auto button: the shown group back to the automatic stretch
    /// (unlinked; with Linked on, the linked automatic stretch, kept as a
    /// manual one).
    fn auto_now(&mut self, ctx: &egui::Context, session: &mut OpenSession) {
        let Some(g) = self.group.clone() else { return };
        let Some(m) = self.results.get_mut(&g) else {
            return;
        };
        let Some(src) = m.source.clone() else {
            return;
        };
        let linked = m.linked;
        m.manual = linked;
        m.want_auto = false;
        save_stretch(&mut session.mosaic.config.ui, &g, None);
        self.save.touch();
        m.auto = Task::spawn("mmm-auto-stretch", ctx, move || src.auto(linked));
        // A linked result is saved when it arrives (see `poll_auto`).
        m.save_auto_result = linked && m.auto.is_some();
    }

    fn health_ui(&self, ui: &mut egui::Ui, readiness: Readiness) {
        ui.label(RichText::new("Health").color(palette::TEXT_STRONG));
        let muted = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).color(palette::TEXT_MUTED));
        };
        match (&self.health, readiness) {
            (_, Readiness::NotAnalyzed | Readiness::NeedsAnalyze) => {
                muted(ui, "Shown once the group is analyzed.");
            }
            (None, _) => muted(ui, "Computing…"),
            (Some(Err(e)), _) => muted(ui, &format!("Not available: {e}")),
            (Some(Ok(h)), _) => {
                let flagged = h.seams.iter().filter(|s| s.flagged).count();
                egui::Grid::new("blend-health")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
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
        }
    }
}

/// Where `group` stands for blending.
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
#[path = "blend_tab_tests.rs"]
mod tests;
