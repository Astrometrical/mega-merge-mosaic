//! The Blend tab: blend controls, the tiled result viewer with its display
//! stretch, and the group's health.
//!
//! Everything slow runs on background threads ([`Task`]): opening the
//! result ([`open_result`]), the automatic display stretch statistics, and
//! the health metrics. The one write on the UI thread is the debounced
//! `mosaic.json` save of a manual display stretch (at most once per
//! [`SAVE_INTERVAL`]), the same documented exception as the job runner's
//! plan/apply saves.
//!
//! # No reader across a store recreation
//!
//! `run_blend` recreates (truncates) a group's store only when it is
//! missing, unparseable, or its geometry differs from the group's analysed
//! canvas; a live [`mmm_core::tiles::TileReader`] would then fault. This tab
//! guards the geometry path (the other two have no reader to fault):
//!
//! - a source is kept only when [`open_result`] found the store's geometry
//!   equal to the group's analysed canvas, so a blend reopens it in place;
//! - the canvas changes only by analysis: after every analysis (finished,
//!   cancelled or failed) the result is checked again, and while an
//!   analysis is running or queued, or a check is pending, **Blend** is
//!   disabled (a blend queued behind an analysis would start the moment it
//!   is applied, before the check);
//! - a source given up (out of date, group switched, replaced) is tracked
//!   by a `Weak` until the viewer's loader threads and any statistics task
//!   have dropped it; **Blend** stays disabled until then;
//! - with no source open, a blend's first progress update means it has
//!   created its store (progress is only reported once tiles are written),
//!   so the result is opened then and tiles appear while it runs; while a
//!   blend of the shown group is queued or has not reported yet (e.g.
//!   after switching to that group), no open starts.
//!
//! Another program (the `mmm` CLI) recreating the store while it is shown
//! is not guarded against.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use mega_ui::theme::palette;
use mega_ui::viewer::{TileSource, TiledViewer};
use mmm_core::health::{Health, health};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::stretch::StretchParams;
use mmm_core::surfaces::Surfaces;
use serde_json::Value;

use crate::jobs::{JobEvent, JobRunner};
use crate::session::OpenSession;
use crate::stretch_panel::{
    BLEND_STRETCH_KEY, StretchChange, StretchState, channel_name, save_stretch_at,
    saved_stretch_at, stretch_panel,
};
use crate::tile_source::{MosaicTileSource, Opened, open_result};

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

/// Where the shown group stands for blending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readiness {
    /// Never analysed.
    NotAnalyzed,
    /// Analyse settings changed since the last analysis.
    NeedsAnalyze,
    /// Can be blended.
    Analyzed,
}

/// The Blend tab's state for the open session.
pub struct BlendTab {
    /// The group shown.
    group: Option<String>,
    viewer: Option<TiledViewer>,
    /// The open result of `group` (geometry checked, see the module docs).
    source: Option<Arc<MosaicTileSource>>,
    /// Why there is no source (shown in the viewer's place).
    absent: Option<String>,
    /// A pending [`open_result`] for the named group.
    opening: Option<Task<(String, Opened)>>,
    /// Open (again) once no open is pending.
    reopen: bool,
    /// Sources given up but maybe still mapped by loader or stats threads.
    retired: Vec<Weak<MosaicTileSource>>,
    /// A pending automatic stretch for `source`.
    auto: Option<Task<Option<Vec<StretchParams>>>>,
    /// Save the pending automatic stretch as a manual one (Auto pressed
    /// with Linked on).
    save_auto_result: bool,
    /// Compute the automatic stretch once possible (unless manual).
    want_auto: bool,
    health: Option<HealthResult>,
    health_task: Option<Task<(String, HealthResult)>>,
    want_health: bool,
    /// One set of sliders for every channel.
    linked: bool,
    /// The stretch was set by the user (saved), so blends keep it.
    manual: bool,
    save: Debounce,
    /// The analysis this tab started, until it ends.
    analyze_job: Option<u64>,
    /// The blend this tab started, until it ends.
    blend_job: Option<u64>,
    /// Last seen `(job, progress updates)` of the running blend.
    last_progress: Option<(u64, u64)>,
    loader_threads: usize,
    /// Drawn at least once.
    shown: bool,
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
            group: None,
            viewer: None,
            source: None,
            absent: None,
            opening: None,
            reopen: false,
            retired: Vec::new(),
            auto: None,
            save_auto_result: false,
            want_auto: false,
            health: None,
            health_task: None,
            want_health: false,
            linked: false,
            manual: false,
            save: Debounce::default(),
            analyze_job: None,
            blend_job: None,
            last_progress: None,
            loader_threads: std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .clamp(2, 8),
            shown: false,
        }
    }

    /// The group shown.
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The open result, if any.
    pub fn source(&self) -> Option<&Arc<MosaicTileSource>> {
        self.source.as_ref()
    }

    /// Why no result is shown, if none is.
    pub fn absent(&self) -> Option<&str> {
        self.absent.as_deref()
    }

    /// The group's health, once computed.
    pub fn health(&self) -> Option<&HealthResult> {
        self.health.as_ref()
    }

    /// True once drawn with nothing pending: no open, statistics or health
    /// task, and the viewer (if any) has drawn and loaded its tiles. A
    /// session without groups settles at once.
    pub fn is_settled(&self) -> bool {
        self.shown
            && self.opening.is_none()
            && !self.reopen
            && self.auto.is_none()
            && self.health_task.is_none()
            && self
                .viewer
                .as_ref()
                .is_none_or(|v| v.last_frame().is_some() && !v.is_loading())
    }

    /// Save a pending (debounced) display stretch change now, whatever the
    /// rate limit: the window is closing, or the session state is being
    /// saved. A small `mosaic.json` write on the UI thread (the module's
    /// documented exception).
    pub fn flush_save(&mut self, session: &OpenSession) {
        if !self.save.pending() {
            return;
        }
        self.save.saved(Instant::now());
        if let Err(e) = session.mosaic.save() {
            tracing::warn!("display stretch not saved: {e}");
        }
    }

    /// Mark the display stretch as changed and unsaved (tests).
    #[cfg(test)]
    pub(crate) fn touch_save(&mut self) {
        self.save.touch();
    }

    /// Whether **Blend** may start now (see the module docs for the store
    /// recreation guard).
    fn can_blend(&self, readiness: Readiness) -> bool {
        readiness == Readiness::Analyzed
            && self.analyze_job.is_none()
            && self.opening.is_none()
            && !self.reopen
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
        let (id, finished) = match event {
            JobEvent::Finished { id, .. } => (*id, true),
            JobEvent::Cancelled { id } | JobEvent::Failed { id, .. } => (*id, false),
            JobEvent::Preview(_) => return,
        };
        if self.analyze_job == Some(id) {
            // The canvas may have changed: check the result again.
            self.analyze_job = None;
            self.reopen = true;
            self.want_health = true;
        } else if self.blend_job == Some(id) {
            self.blend_job = None;
            match &self.source {
                Some(src) => {
                    src.bump();
                    self.want_auto |= finished;
                }
                None => self.reopen = true,
            }
            self.want_health = true;
        }
    }

    /// Give up the current source and viewer (the loaders let go of it
    /// after their current fetch).
    fn retire(&mut self) {
        if let Some(src) = self.source.take() {
            self.retired.push(Arc::downgrade(&src));
        }
        self.viewer = None;
        self.auto = None;
        self.save_auto_result = false;
    }

    /// Show `group` (from scratch).
    fn select(&mut self, group: String) {
        self.retire();
        self.group = Some(group);
        self.absent = None;
        self.health = None;
        self.reopen = true;
        self.want_health = true;
        self.want_auto = false;
        self.last_progress = None;
    }

    /// Draw the tab.
    pub fn ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession, jobs: &mut JobRunner) {
        let ctx = ui.ctx().clone();
        self.shown = true;
        let groups = group_names(session);
        if self.group.as_ref().is_none_or(|g| !groups.contains(g))
            && let Some(first) = groups.first()
        {
            self.select(first.clone());
        }
        self.update(&ctx, session, jobs);
        let readiness = self.readiness(session);

        egui::Panel::top("blend-toolbar").show(ui, |ui| {
            self.toolbar(ui, session, jobs, &groups, readiness);
        });
        egui::Panel::right("blend-side")
            .resizable(false)
            .exact_size(SIDE_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.stretch_ui(ui, session);
                    ui.add_space(16.0);
                    self.health_ui(ui, readiness);
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| match &mut self.viewer {
                Some(viewer) => {
                    viewer.show(ui);
                }
                None => {
                    let text = if self.group.is_none() {
                        "Add panels to start a mosaic."
                    } else if self.opening.is_some() || self.reopen {
                        "Opening the result…"
                    } else {
                        self.absent.as_deref().unwrap_or("")
                    };
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new(text).color(palette::TEXT_MUTED));
                    });
                }
            });
    }

    fn readiness(&self, session: &OpenSession) -> Readiness {
        let Some(g) = &self.group else {
            return Readiness::NotAnalyzed;
        };
        let config = &session.mosaic.config;
        if !config.group_dirs.contains_key(g) {
            Readiness::NotAnalyzed
        } else if config.needs_analyze.contains(g) {
            Readiness::NeedsAnalyze
        } else {
            Readiness::Analyzed
        }
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
        let Some(group) = self.group.clone() else {
            return;
        };
        // A blend of this group that is queued, or running but has not
        // reported progress, may still create or recreate the store: open
        // only once it reports (`follow_blend`) or ends.
        if self.reopen && self.opening.is_none() && !jobs.blend_unstarted(&group) {
            self.reopen = false;
            let result = session.mosaic.result_path(&group);
            let dir = session.mosaic.group_session_dir(&group);
            let saved = saved_stretch(&session.mosaic.config.ui, &group);
            self.opening = Task::spawn("mmm-open-result", ctx, move || {
                let opened = open_result(&result, &dir, saved);
                (group.clone(), opened)
            });
        }
        if self.want_auto
            && !self.manual
            && self.auto.is_none()
            && let Some(src) = self.source.clone()
        {
            self.want_auto = false;
            self.auto = Task::spawn("mmm-auto-stretch", ctx, move || src.auto(false));
        }
        if self.want_health && self.health_task.is_none() {
            self.want_health = false;
            self.health = None;
            if self.readiness(session) == Readiness::Analyzed {
                let g = self.group.clone().unwrap_or_default();
                let dir = session.mosaic.group_session_dir(&g);
                let feather = session.mosaic.config.settings_for(&g).1.feather_px;
                self.health_task = Task::spawn("mmm-health", ctx, move || {
                    let h = load_health(&dir, feather);
                    (g, h)
                });
            }
        }
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            // Documented exception: a small mosaic.json write on the UI
            // thread, at most once per SAVE_INTERVAL.
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("display stretch not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }

    fn poll_opening(&mut self) {
        let Some(task) = &self.opening else { return };
        let (group, opened) = match task.poll() {
            Poll::Pending => return,
            Poll::Done(v) => v,
            Poll::Lost => {
                self.opening = None;
                self.retire();
                self.absent = Some("Opening the result failed (see the log).".into());
                return;
            }
        };
        self.opening = None;
        if self.group.as_ref() != Some(&group) {
            return; // switched meanwhile; `select` asked for a new open
        }
        match opened {
            Opened::Ready { source, saved } => {
                let src = Arc::new(source);
                self.manual = saved;
                self.linked = saved && src.tile_geometry().channels > 1 && {
                    let s = src.stretch();
                    s.iter().all(|p| *p == s[0])
                };
                if let Some(old) = self.source.replace(src.clone()) {
                    self.retired.push(Arc::downgrade(&old));
                }
                let dyn_src: Arc<dyn TileSource> = src;
                match &mut self.viewer {
                    Some(v) => v.set_source(dyn_src),
                    None => self.viewer = Some(TiledViewer::new(dyn_src, self.loader_threads)),
                }
                self.absent = None;
            }
            Opened::Absent(why) => {
                self.retire();
                self.absent = Some(why);
            }
        }
    }

    /// Follow the running blend of the shown group: bump the generation on
    /// each progress update so new tiles load; open the result once the
    /// blend has created its store.
    fn follow_blend(&mut self, ctx: &egui::Context, jobs: &JobRunner) {
        let Some((id, group, updates)) = jobs.blend_progress() else {
            return;
        };
        if self.group.as_deref() != Some(group) || updates == 0 {
            return;
        }
        if self.last_progress == Some((id, updates)) {
            return;
        }
        self.last_progress = Some((id, updates));
        match &self.source {
            Some(src) => {
                src.bump();
                ctx.request_repaint();
            }
            None if self.opening.is_none() => self.reopen = true,
            None => {}
        }
    }

    /// Apply a finished automatic stretch (unless the user has set one
    /// meanwhile).
    fn poll_auto(&mut self, session: &mut OpenSession) {
        let Some(task) = &self.auto else { return };
        let stretch = match task.poll() {
            Poll::Pending => return,
            Poll::Done(s) => s,
            Poll::Lost => None,
        };
        self.auto = None;
        let keep = std::mem::take(&mut self.save_auto_result);
        let Some(s) = stretch else {
            // No statistics (nothing blended yet). A Linked Auto saved
            // nothing, so the stretch is automatic again: the next finished
            // blend computes it (`on_event` sets `want_auto`). Asking for it
            // now would only spin on the same empty result.
            if keep {
                self.manual = false;
            }
            return;
        };
        let Some(src) = &self.source else {
            return;
        };
        if keep {
            if let Some(g) = &self.group {
                save_stretch(&mut session.mosaic.config.ui, g, Some(&s));
                self.save.touch();
            }
        } else if self.manual {
            return;
        }
        src.set_stretch(s);
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        groups: &[String],
        readiness: Readiness,
    ) {
        ui.horizontal(|ui| {
            if groups.len() > 1 {
                let shown = self.group.clone().unwrap_or_default();
                let mut chosen = shown.clone();
                egui::ComboBox::from_label("Group")
                    .selected_text(&shown)
                    .show_ui(ui, |ui| {
                        for g in groups {
                            ui.selectable_value(&mut chosen, g.clone(), g);
                        }
                    });
                if chosen != shown {
                    self.select(chosen);
                }
                ui.separator();
            }
            let analyzing = self.analyze_job.is_some();
            let has_group = self.group.is_some();
            if ui
                .add_enabled(has_group && !analyzing, egui::Button::new("Analyze"))
                .on_hover_text("Analyze every group's panels")
                .clicked()
            {
                self.analyze_job = Some(jobs.start_analyze(session));
            }
            let can_blend = self.can_blend(readiness);
            if ui
                .add_enabled(can_blend, egui::Button::new("Blend whole mosaic"))
                .on_hover_text("Blend this group over the whole canvas")
                .clicked()
                && let Some(g) = self.group.clone()
            {
                self.blend_job = Some(jobs.start_blend(session, &g, None));
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

    /// The shared display stretch panel, wired to this tab's source, Auto
    /// task and debounced save.
    fn stretch_ui(&mut self, ui: &mut egui::Ui, session: &mut OpenSession) {
        let src = self.source.clone();
        let mut state = StretchState {
            stretch: src.as_ref().map_or_else(Vec::new, |s| s.stretch()),
            channels: src
                .as_ref()
                .map_or(0, |s| s.tile_geometry().channels as usize),
            linked: self.linked,
            manual: self.manual,
            auto_busy: self.auto.is_some(),
        };
        let change = stretch_panel(ui, &mut state);
        self.linked = state.linked;
        match (change, src) {
            (StretchChange::Auto, _) => self.auto_now(ui.ctx(), session),
            (StretchChange::Manual(stretch), Some(src)) => {
                src.set_stretch(stretch.clone());
                self.manual = true;
                self.auto = None;
                self.save_auto_result = false;
                self.want_auto = false;
                if let Some(g) = &self.group {
                    save_stretch(&mut session.mosaic.config.ui, g, Some(&stretch));
                    self.save.touch();
                }
                ui.ctx().request_repaint();
            }
            _ => {}
        }
    }

    /// Auto button: back to the automatic stretch (unlinked; with Linked on,
    /// the linked automatic stretch, kept as a manual one).
    fn auto_now(&mut self, ctx: &egui::Context, session: &mut OpenSession) {
        let Some(src) = self.source.clone() else {
            return;
        };
        let linked = self.linked;
        self.manual = linked;
        self.want_auto = false;
        if let Some(g) = &self.group {
            save_stretch(&mut session.mosaic.config.ui, g, None);
            self.save.touch();
        }
        self.auto = Task::spawn("mmm-auto-stretch", ctx, move || src.auto(linked));
        // A linked result is saved when it arrives (see `poll_auto`).
        self.save_auto_result = linked && self.auto.is_some();
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
                                palette::HA_500
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
