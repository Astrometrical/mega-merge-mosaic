//! The application shell: the start screen until a session is open, then
//! the session window, until its ‹ goes back to the start screen
//! ([`Shell::close_session`]). [`Shell`] is plain egui (testable without eframe);
//! [`MmmApp`] wraps it for eframe with persistence and the self-test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mega_ui::theme::{Tone, palette};
use mega_ui::widgets::{Toast, ToastEvent, ToastId, Toasts};
use mmm_core::mosaic::SessionLock;

use crate::checks::ChecksEngine;
use crate::compositions::{ViewKind, load_compositions};
use crate::jobs::{JobEvent, JobRunner, PREVIEWS_LABEL};
use crate::mosaic_view::MosaicView;
use crate::panel_edits::{FilesPicker, RfdFilesPicker};
use crate::pipeline::Pipeline;
use crate::session::{OpenSession, Pending, Request, open_in_new_process};
use crate::session_window::{CanvasMode, SessionWindow, Undo, WindowToast};
use crate::start::{RecentEntry, RfdDialogs, StartAction, StartScreen, unix_now};
use crate::summary_writer::{FinalWrite, SummaryWriter};
use crate::util::group_names;

/// Directory, inside the session, where compressed panels are decoded.
const DECODED_DIR: &str = "decoded";
/// eframe storage key of the recent sessions.
const RECENT_KEY: &str = "recent_sessions";
/// The window title when no session is open.
const APP_TITLE: &str = "Mega Merge Mosaic";
/// Longest leaving the session waits for its jobs to stop.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// The window title for a session named `name`.
fn session_title(name: &str) -> String {
    format!("{name} — {APP_TITLE}")
}

/// A toast with just a message.
fn toast(tone: Tone, title: String) -> Toast {
    Toast {
        tone,
        title,
        detail: None,
        action: None,
    }
}

/// Opens another application window.
pub trait Launcher: Send + Sync {
    /// Open a window on `session`, or on the start screen with `None`;
    /// `Err` says why no window opened.
    fn open_window(&self, session: Option<&Path>) -> Result<(), String>;
}

/// Opens windows as new processes of this executable.
pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn open_window(&self, session: Option<&Path>) -> Result<(), String> {
        open_in_new_process(session).map_err(|e| format!("Cannot open a new window: {e}"))?;
        tracing::info!("opened a new window");
        Ok(())
    }
}

/// Open a window through `launcher`; a failure becomes a toast at time
/// `now`. Whether a window opened.
fn launch(launcher: &dyn Launcher, toasts: &mut Toasts, now: f64, session: Option<&Path>) -> bool {
    match launcher.open_window(session) {
        Ok(()) => true,
        Err(text) => {
            tracing::warn!("{text}");
            toasts.push(toast(Tone::Problem, text), now);
            false
        }
    }
}

/// The window contents: start screen or open session.
pub struct Shell {
    /// The start screen (the window's content until a session is open).
    pub start: StartScreen,
    session: Option<OpenSession>,
    pending: Option<Pending>,
    /// Opens a session asked for while one is open in a window of its own.
    launcher: Arc<dyn Launcher>,
    /// Background jobs of the open session (made with the session).
    jobs: Option<JobRunner>,
    /// Job outcomes and failures to open windows.
    toasts: Toasts,
    /// What each toast with an action undoes (dropped with the toast; the
    /// toasts go with the session, [`Shell::drop_undos`]).
    undos: BTreeMap<ToastId, Undo>,
    /// The analysis settings toast shown, which the next one replaces.
    analysis_toast: Option<ToastId>,
    /// Every toast's title pushed, in order (tests: a toast expires).
    #[cfg(test)]
    toasted: Vec<String>,
    /// The window title last sent.
    title: String,
    /// The session window (made afresh with each session).
    window: SessionWindow,
    /// The panel file dialog used by the session window's panel list.
    files_picker: Arc<dyn FilesPicker>,
    /// The automatic previews and analysis (made afresh with each session).
    pipeline: Pipeline,
    /// The checks (made afresh with each session).
    checks: ChecksEngine,
    /// Keeps the session's `summary.json` current (made afresh with each
    /// session).
    summary: SummaryWriter,
    /// Longest leaving the session waits for its jobs ([`STOP_WAIT`]).
    stop_wait: Duration,
    /// Sessions left while a job still ran, sent by their reaper once it
    /// has released the lock ([`reap`]).
    reaped_tx: Sender<PathBuf>,
    reaped_rx: Receiver<PathBuf>,
}

impl Shell {
    /// A shell showing `start`.
    pub fn new(start: StartScreen) -> Self {
        let (reaped_tx, reaped_rx) = mpsc::channel();
        Shell {
            start,
            session: None,
            pending: None,
            launcher: Arc::new(ProcessLauncher),
            jobs: None,
            toasts: Toasts::default(),
            undos: BTreeMap::new(),
            analysis_toast: None,
            #[cfg(test)]
            toasted: Vec::new(),
            title: APP_TITLE.to_string(),
            window: SessionWindow::new(Arc::new(RfdFilesPicker)),
            files_picker: Arc::new(RfdFilesPicker),
            pipeline: Pipeline::new(),
            checks: ChecksEngine::new(),
            summary: SummaryWriter::new(None),
            stop_wait: STOP_WAIT,
            reaped_tx,
            reaped_rx,
        }
    }

    /// Open other windows with `launcher` (default: new processes).
    pub fn with_launcher(mut self, launcher: Arc<dyn Launcher>) -> Self {
        self.launcher = launcher;
        self
    }

    /// The session open in this window, if any.
    pub fn session(&self) -> Option<&OpenSession> {
        self.session.as_ref()
    }

    /// True while a session is being opened or created.
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The session window.
    pub fn window(&self) -> &SessionWindow {
        &self.window
    }

    /// The session window, to change what it shows.
    pub fn window_mut(&mut self) -> &mut SessionWindow {
        &mut self.window
    }

    /// Self-test: start an export of the first group and one of the first
    /// composition (if any) into `dir`, as FITS, the way the Export dialog
    /// does. Returns the files they will write.
    fn selftest_exports(&mut self, dir: &Path) -> Vec<PathBuf> {
        let Some((session, jobs)) = self.session.as_ref().zip(self.jobs.as_mut()) else {
            return Vec::new();
        };
        let fmt = mmm_core::export::ExportFormat::Fits;
        let mut files = Vec::new();
        if let Some(group) = group_names(session).into_iter().next() {
            let out = dir.join(format!("group.{}", fmt.extension()));
            jobs.start_export(
                session,
                crate::jobs::ExportTarget::Group(group),
                out.clone(),
                fmt,
                false,
            );
            files.push(out);
        }
        if let Some(c) = load_compositions(&session.mosaic.config.ui)
            .into_iter()
            .next()
        {
            let out = dir.join(format!("composite.{}", fmt.extension()));
            let [r, g, b] = c.channels;
            let target = crate::jobs::ExportTarget::Composite {
                id: c.id,
                name: c.name,
                channels: [r.map(|c| c.0), g.map(|c| c.0), b.map(|c| c.0)],
            };
            jobs.start_export(session, target, out.clone(), fmt, false);
            files.push(out);
        }
        files
    }

    /// The automatic pipeline (its previews feed the plan).
    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    /// The checks engine.
    pub fn checks(&self) -> &ChecksEngine {
        &self.checks
    }

    /// Every toast's title pushed so far, in order, whether or not it is
    /// still on screen.
    #[cfg(test)]
    pub fn toasted(&self) -> &[String] {
        &self.toasted
    }

    /// Save anything held back: edits waiting for the jobs to stop (panels,
    /// merge settings) when none runs, then what a rate limit holds back
    /// (the session window's view, the display stretch, ROIs, composites,
    /// notes). When the window closes, and on leaving the session
    /// ([`Shell::close_session`]); also from eframe's periodic save.
    pub fn flush_pending_saves(&mut self) {
        self.flush_saves(true);
    }

    /// [`Shell::flush_pending_saves`] from eframe's periodic save: edits still
    /// working out their scope are left to apply themselves.
    fn flush_periodic_saves(&mut self) {
        self.flush_saves(false);
    }

    fn flush_saves(&mut self, leaving: bool) {
        let Some(session) = &mut self.session else {
            return;
        };
        if let Some(jobs) = &mut self.jobs {
            self.window.apply_waiting_edits(session, jobs, leaving);
        }
        self.window.flush_save(session);
    }

    /// Leave the open session for the start screen, in this window (‹,
    /// spec §15). Refused while an export runs or waits (`Err` says why).
    /// Otherwise every other job is cancelled (all can be run again) and
    /// waited for, on both of the runner's lanes ([`JobRunner::stop_all`]),
    /// at most [`STOP_WAIT`]; pending saves are flushed
    /// ([`Shell::flush_pending_saves`]); the session is dropped with its
    /// window, pipeline, checks and summary writer, and its Undo toasts
    /// are taken down; the decode cache is
    /// forgotten; the start screen reads the session's facts again; the
    /// title names no session.
    ///
    /// The ROI previews stop with the jobs: the one in flight is cancelled
    /// and its thread ([`SessionWindow::stop_previews`]) gets the same
    /// wait as the jobs.
    ///
    /// With every job and the preview thread stopped, the summary is
    /// written on this thread and the lock released at once. A job or a
    /// preview that ignores the wait (a decode cannot be interrupted) is let
    /// go of without hanging the window: a reaper thread keeps the session's
    /// lock until its thread, and every job worker of either lane
    /// ([`JobRunner::detach_workers`]), has ended, then writes the summary
    /// and releases
    /// the lock (the start screen shows the session open in another window
    /// meanwhile, and reads it again after). An edit that was waiting for
    /// the jobs is then dropped (logged) if a job did not stop. Either way
    /// nothing writes into the session (its decoded-panel cache included)
    /// once its lock is released. Without a session open, nothing to do.
    pub fn close_session(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        if let Some(why) = self
            .jobs
            .as_ref()
            .and_then(SessionWindow::back_block_reason)
        {
            return Err(why.to_string());
        }
        let deadline = Instant::now() + self.stop_wait;
        let previews = self.window.stop_previews();
        let mut stopped = true;
        if let Some(jobs) = &mut self.jobs {
            let (events, done) = jobs.stop_all(session, self.stop_wait);
            stopped = done;
            for event in &events {
                let target = match event {
                    JobEvent::Finished { id, .. } => jobs.target(*id),
                    _ => None,
                };
                self.summary
                    .on_job_event(session, event, target, unix_now());
            }
        }
        // The preview thread gets what is left of the same wait.
        let previews = previews.and_then(|worker| join_by(worker, deadline));
        if previews.is_some() {
            tracing::warn!(
                "an ROI preview did not stop within {:?} of leaving the session; the session \
                 stays locked until it has",
                self.stop_wait
            );
        }
        // With a job still running, waiting edits stay unapplied.
        self.flush_pending_saves();
        let mut leftover = match &mut self.jobs {
            Some(jobs) if !stopped => {
                tracing::warn!(
                    "a job did not stop within {:?} of leaving the session; the session \
                     stays locked until it has",
                    self.stop_wait
                );
                if self.window.has_waiting_edits() {
                    tracing::warn!(
                        "an edit waiting for the jobs to stop is dropped: a job did not stop"
                    );
                }
                jobs.detach_workers()
            }
            _ => Vec::new(),
        };
        leftover.extend(previews);
        let Some(mut session) = self.session.take() else {
            return Ok(());
        };
        let checks = self.checks.counts(&session.mosaic.config);
        let last_view = self.window.view_key(&session.mosaic.config.ui);
        let dir = session.mosaic.dir.clone();
        if leftover.is_empty() {
            self.summary.finish(&session, checks, last_view);
        } else {
            let summary = self.summary.close(&session, checks, last_view);
            let reaped = self.reaped_tx.clone();
            reap(leftover, summary, session.lock.take(), &dir, reaped, ctx);
        }
        self.jobs = None;
        self.drop_undos();
        drop(session);
        self.window = SessionWindow::new(self.files_picker.clone());
        self.pipeline = Pipeline::new();
        self.checks = ChecksEngine::new();
        self.summary = SummaryWriter::new(None);
        mmm_core::decoded::clear_cache_dir();
        tracing::info!("closed session {}", dir.display());
        self.start.session_closed(&dir);
        self.update_title(ctx);
        ctx.request_repaint();
        Ok(())
    }

    /// Take down the toasts whose Undo would change the session being left
    /// (or one no longer open), and forget what they undo.
    fn drop_undos(&mut self) {
        self.analysis_toast = None;
        for id in std::mem::take(&mut self.undos).into_keys() {
            self.toasts.dismiss(id);
        }
    }

    /// Open or create a session on a background thread. With a session
    /// already open, the new one goes to a new process (spec §3).
    pub fn request(&mut self, request: Request, ctx: &egui::Context) {
        if self.pending.is_some() {
            return;
        }
        self.start.set_error(None);
        self.start.set_busy(Some(format!(
            "{} {}…",
            verb(&request),
            request.path().display()
        )));
        self.pending = Some(Pending::spawn(request, self.session.is_none(), ctx));
    }

    /// Draw one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.poll_jobs(&ctx);
        self.update_title(&ctx);
        if self.session.is_none() {
            // The start screen's hero fills the window: no panel margin.
            let frame = egui::Frame::NONE.fill(palette::SURFACE_900);
            egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                if let Some(action) = self.start.ui(ui) {
                    self.request(to_request(action), &ctx);
                }
            });
        } else {
            self.session_ui(ui);
        }
        // Toasts float above either screen.
        let now = ctx.input(|i| i.time);
        for event in self.toasts.show(&ctx, now) {
            match event {
                ToastEvent::Action(id) => {
                    if let Some(undo) = self.undos.remove(&id)
                        && let Some((session, jobs)) = self.session.as_mut().zip(self.jobs.as_mut())
                    {
                        self.window.undo(undo, session, jobs);
                    }
                }
                ToastEvent::Dismissed(id) => {
                    self.undos.remove(&id);
                }
            }
        }
    }

    /// Name the session in the window title (sent when it changes).
    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.session {
            Some(open) => session_title(&open.name()),
            None => APP_TITLE.to_string(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// Apply a finished open/create; the start screen reads a session again
    /// once its reaper has released it.
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(dir) = self.reaped_rx.try_recv() {
            self.start.session_closed(&dir);
        }
        let Some(outcome) = self.pending.as_mut().and_then(Pending::poll) else {
            return;
        };
        self.pending = None;
        self.start.set_busy(None);
        match outcome {
            Ok(opened) => {
                // Both paths are normalised (absolute, canonical) by
                // `session::open_blocking`, so this compares like with like.
                let dir = opened.mosaic.dir.clone();
                if self
                    .session
                    .as_ref()
                    .is_some_and(|open| open.mosaic.dir == dir)
                {
                    self.start.set_error(Some(format!(
                        "{} is already open in this window.",
                        dir.display()
                    )));
                } else if self.session.is_some() || opened.lock.is_none() {
                    // One session per window: the other one gets its own
                    // (also when this window's session was closed since it
                    // was asked for: it was only checked, without the
                    // lock). A created session holds the lock; release it
                    // so the child can take it.
                    drop(opened);
                    let now = ctx.input(|i| i.time);
                    if launch(&*self.launcher, &mut self.toasts, now, Some(&dir)) {
                        self.start.record_opened(&dir, unix_now());
                    }
                } else {
                    self.open_here(opened, ctx);
                }
            }
            Err(message) => {
                tracing::warn!("{message}");
                self.start.set_error(Some(message));
            }
        }
    }

    /// Show `opened` (its lock taken) in this window: recorded as opened,
    /// its decode cache set, and a fresh job runner, window, pipeline,
    /// checks and summary writer made for it.
    fn open_here(&mut self, opened: OpenSession, ctx: &egui::Context) {
        let dir = opened.mosaic.dir.clone();
        tracing::info!("opened session {}", dir.display());
        self.start.record_opened(&dir, unix_now());
        // Compressed panels decode to disk once, shared by previews and
        // analysis (one session per process).
        mmm_core::decoded::set_cache_dir(dir.join(DECODED_DIR));
        self.summary = SummaryWriter::new(opened.summary.clone());
        self.session = Some(opened);
        self.jobs = Some(JobRunner::new(ctx.clone()));
        self.drop_undos();
        // A session without panels opens on its panel list.
        self.window = SessionWindow::new(self.files_picker.clone());
        self.pipeline = Pipeline::new();
        self.checks = ChecksEngine::new();
    }

    /// Apply finished jobs and toast each outcome, let the panel list
    /// take its background work and apply a waiting edit, run the
    /// automatic pipeline, let the session window follow (shown or not),
    /// run the checks, and keep the summary (and the session's result
    /// states) current.
    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let Some((session, jobs)) = self.session.as_mut().zip(self.jobs.as_mut()) else {
            return;
        };
        let now = Instant::now();
        let clock = ctx.input(|i| i.time);
        let events = jobs.poll(session);
        // An edit (panels, or the merge settings) waiting or having
        // cancelled jobs when the events were taken: jobs it cancelled are
        // restarted by the pipeline, not given up.
        let editing = self.window.panels_editing() || self.window.settings_editing(session);
        for event in &events {
            self.window.on_event(session, jobs, event);
            let target = match event {
                JobEvent::Finished { id, .. } => jobs.target(*id),
                _ => None,
            };
            self.summary
                .on_job_event(session, event, target, unix_now());
            let name = |id: &u64| jobs.label(*id).unwrap_or("Job").to_string();
            let (tone, text) = match event {
                // Previews run by themselves and often: no toasts but
                // failures.
                JobEvent::Preview(_) => continue,
                JobEvent::Finished { label, .. } if label == PREVIEWS_LABEL => continue,
                JobEvent::Cancelled { id } if name(id) == PREVIEWS_LABEL => continue,
                // A panel edit's one toast says why (below).
                JobEvent::Cancelled { id } if jobs.stopped_by_edit(*id) => continue,
                JobEvent::Finished { label, .. } => (Tone::Success, format!("{label} finished")),
                JobEvent::Cancelled { id } => (Tone::Neutral, format!("{} cancelled", name(id))),
                JobEvent::Failed { message, .. } => (Tone::Problem, message.clone()),
            };
            #[cfg(test)]
            self.toasted.push(text.clone());
            self.toasts.push(toast(tone, text), clock);
        }
        if jobs.take_merge_stopped() {
            let text = "Merge stopped: panels changed".to_string();
            #[cfg(test)]
            self.toasted.push(text.clone());
            self.toasts.push(toast(Tone::Neutral, text), clock);
        }
        self.window.poll_panels(ctx, session, jobs);
        self.window.poll_settings(ctx, session, jobs);
        let editing =
            editing || self.window.panels_editing() || self.window.settings_editing(session);
        self.pipeline
            .poll(ctx, now, session, jobs, &events, editing);
        if let Some(id) = self.pipeline.take_started_analysis() {
            self.window.track_analysis(id);
        }
        // The session window (the merged mosaic's state above all) follows
        // jobs and analyses even while the panel list is shown.
        self.window.poll(ctx, session, jobs);
        self.checks.poll(ctx, session, jobs, &events);
        let last_view = self.window.view_key(&session.mosaic.config.ui);
        let counts = self.checks.counts(&session.mosaic.config);
        self.summary.poll(ctx, session, counts, last_view);
        if let Some(results) = self.summary.take_results() {
            session.results = results;
        }
    }

    /// The open session: the session window (its top bar, canvas or panel
    /// list, and inspector).
    fn session_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some((session, jobs)) = self.session.as_mut().zip(self.jobs.as_mut()) else {
            return;
        };
        let out = self
            .window
            .ui(ui, session, jobs, &mut self.pipeline, &self.checks);
        for check in &out.accept {
            if let Err(message) = self.checks.accept(session, check) {
                tracing::warn!("{message}");
                #[cfg(test)]
                self.toasted.push(message.clone());
                self.toasts
                    .push(toast(Tone::Problem, message), ctx.input(|i| i.time));
            }
        }
        for check in &out.unaccept {
            if let Err(message) = self.checks.unaccept(session, check) {
                tracing::warn!("{message}");
                #[cfg(test)]
                self.toasted.push(message.clone());
                self.toasts
                    .push(toast(Tone::Problem, message), ctx.input(|i| i.time));
            }
        }
        let now = ctx.input(|i| i.time);
        for WindowToast { toast, undo } in out.toasts {
            let has_action = toast.action.is_some();
            #[cfg(test)]
            self.toasted.push(toast.title.clone());
            let id = self.toasts.push(toast, now);
            if let (true, Some(undo)) = (has_action, undo) {
                // A new analysis toast replaces the previous one.
                if matches!(undo, Undo::Analyze(_))
                    && let Some(old) = self.analysis_toast.replace(id)
                {
                    self.undos.remove(&old);
                    self.toasts.dismiss(old);
                }
                self.undos.insert(id, undo);
            }
        }
        if out.back
            && let Err(why) = self.close_session(&ctx)
        {
            #[cfg(test)]
            self.toasted.push(why.clone());
            self.toasts
                .push(toast(Tone::Neutral, why), ctx.input(|i| i.time));
        }
    }
}

/// `worker` joined once it has ended (`None`), or still running at
/// `deadline` (`Some`: the caller waits for it elsewhere).
fn join_by(worker: JoinHandle<()>, deadline: Instant) -> Option<JoinHandle<()>> {
    while !worker.is_finished() {
        if Instant::now() >= deadline {
            return Some(worker);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = worker.join();
    None
}

/// Keep a session left while `workers` (its jobs' workers, its ROI
/// previews' thread) still run locked until
/// they have ended: on a thread of its own, join them, run the summary's
/// last write, release `lock`, then send `dir` to `reaped` and repaint
/// `ctx`. Nothing of the session's is written after the lock is released.
fn reap(
    workers: Vec<JoinHandle<()>>,
    summary: FinalWrite,
    lock: Option<SessionLock>,
    dir: &Path,
    reaped: Sender<PathBuf>,
    ctx: &egui::Context,
) {
    let dir = dir.to_path_buf();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("mmm-session-reaper".into())
        .spawn(move || {
            for w in workers {
                let _ = w.join();
            }
            summary.write();
            drop(lock);
            tracing::info!("released session {} once its last job ended", dir.display());
            let _ = reaped.send(dir);
            ctx.request_repaint();
        });
    // Without a thread, the lock goes with the closure: released at once.
    if let Err(e) = spawned {
        tracing::error!("cannot start the session reaper: {e}; the session is released now");
    }
}

/// "Opening" or "Creating", for progress labels.
fn verb(request: &Request) -> &'static str {
    match request {
        Request::Open(_) => "Opening",
        Request::Create(_) => "Creating",
    }
}

fn to_request(action: StartAction) -> Request {
    match action {
        StartAction::Open(p) => Request::Open(p),
        StartAction::Create(p) => Request::Create(p),
    }
}

/// How a self-test run ended: `Ok` or the reason it failed.
pub type SelfTestOutcome = Arc<Mutex<Option<Result<(), String>>>>;

/// Frames the self-test renders.
const SELFTEST_FRAMES: u32 = 30;
/// Frames rendered with the start screen before a given session is opened.
const SELFTEST_START_FRAMES: u32 = 5;
/// Most frames the start screen waits for its hero's first image.
const SELFTEST_HERO_FRAMES: u32 = 600;
/// Frames rendered with the session open, at least.
const SELFTEST_SESSION_FRAMES: u32 = 10;
/// Give up on a self-test after this long.
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Exit outright if no frame follows the timeout within this long.
const SELFTEST_BACKSTOP: Duration = Duration::from_secs(10);

/// `--selftest` state: render a fixed number of frames, then close.
struct SelfTest {
    /// When the app started.
    started: Instant,
    frames: u32,
    session_frames: u32,
    session: Option<PathBuf>,
    session_requested: bool,
    timed_out: Arc<AtomicBool>,
    outcome: SelfTestOutcome,
    done: bool,
    /// Where the session part of the test is.
    phase: Phase,
    /// The temporary folder the export phase writes into, and the files it
    /// waits for.
    exports: Option<(PathBuf, Vec<PathBuf>)>,
}

/// The self-test's walk through the session window's canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Waiting for the mosaic of the first group to settle.
    Mosaic,
    /// Waiting for the mosaic of the first composite to settle.
    MosaicComposition,
    /// Waiting for the plan of the view shown (the composite, if any) to
    /// settle.
    Plan,
    /// Waiting for the plan of the first group to settle (after a
    /// composite's).
    PlanGroup,
    /// Every view visited: export a group and a composite.
    StartExports,
    /// Waiting for the exported files to appear.
    Exporting,
    /// Everything done.
    Done,
}

impl SelfTest {
    /// Called after each frame; closes the window when finished.
    fn after_frame(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        shell: &mut Shell,
        panicked: bool,
    ) {
        if self.done {
            return;
        }
        if self.frames == 0
            && let Err(e) = log_gl(frame)
        {
            return self.finish(ctx, Err(e));
        }
        self.frames += 1;
        if panicked {
            return self.finish(ctx, Err(format!("frame {} panicked", self.frames)));
        }
        if self.timed_out.load(Ordering::SeqCst) {
            return self.finish(ctx, Err("timed out".into()));
        }
        if let Some(dir) = &self.session {
            // The session (the start screen's hero) opens once the hero
            // has loaded, or after a while.
            let loaded = shell.start.hero_loaded();
            let waited = self.frames >= SELFTEST_HERO_FRAMES;
            if !self.session_requested && self.frames >= SELFTEST_START_FRAMES && (loaded || waited)
            {
                let ms = self.started.elapsed().as_millis();
                match shell.start.hero_image() {
                    Some(label) => {
                        tracing::info!(
                            "self-test: start screen hero shows {label:?}, {ms} ms after launch"
                        )
                    }
                    None if loaded => tracing::info!("self-test: start screen hero has no image"),
                    None => tracing::warn!("self-test: start screen hero drew nothing in {ms} ms"),
                }
                self.session_requested = true;
                shell.request(Request::Open(dir.clone()), ctx);
            }
            if shell.session().is_some() {
                self.session_frames += 1;
            }
        }
        let session_done = match &self.session {
            None => true,
            Some(_) if !self.session_requested || shell.is_busy() => false,
            Some(dir) if shell.session().is_none() => {
                let why = shell.start.error().unwrap_or("unknown error").to_string();
                return self.finish(
                    ctx,
                    Err(format!("session {} did not open: {why}", dir.display())),
                );
            }
            Some(_) => {
                self.advance(shell);
                self.phase == Phase::Done
            }
        };
        if self.frames >= SELFTEST_FRAMES && session_done {
            return self.finish(ctx, Ok(()));
        }
        ctx.request_repaint();
    }

    /// Visit the mosaic (the first group, then the first composite, if
    /// any), then the plan (that view, then the first group), each once it
    /// has settled.
    fn advance(&mut self, shell: &mut Shell) {
        if self.phase == Phase::Mosaic {
            // A session opens on the plan until something is merged (on
            // its panel list without panels); the walk starts at the
            // mosaic, which closes the list.
            shell.window_mut().set_mode(CanvasMode::Mosaic);
        }
        match self.phase {
            Phase::StartExports => {
                let dir = std::env::temp_dir().join(format!("mmm-selftest-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                let files = match std::fs::create_dir_all(&dir) {
                    Ok(()) => shell.selftest_exports(&dir),
                    Err(e) => {
                        tracing::error!("self-test: cannot create {}: {e}", dir.display());
                        Vec::new()
                    }
                };
                self.exports = Some((dir, files));
                self.phase = Phase::Exporting;
                return;
            }
            Phase::Exporting => {
                if let Some((dir, files)) = &self.exports {
                    // A file appears only when its export is complete (the
                    // export writes `.part` and renames). The watchdog ends
                    // a test whose export never finishes.
                    if files.iter().all(|f| f.is_file()) {
                        tracing::info!("self-test: exported {} files", files.len());
                        let _ = std::fs::remove_dir_all(dir);
                        self.phase = Phase::Done;
                    }
                }
                return;
            }
            _ => {}
        }
        let w = shell.window();
        match self.phase {
            Phase::Mosaic
                if self.session_frames >= SELFTEST_SESSION_FRAMES && w.mosaic().is_settled() =>
            {
                log_mosaic(w.mosaic());
                if let Some(id) = first_composition(shell) {
                    shell.window_mut().select_view(ViewKind::Composition(id));
                    self.phase = Phase::MosaicComposition;
                } else {
                    shell.window_mut().set_mode(CanvasMode::Plan);
                    self.phase = Phase::Plan;
                }
            }
            Phase::MosaicComposition
                if matches!(w.mosaic().view(), Some(ViewKind::Composition(_)))
                    && w.mosaic().is_settled() =>
            {
                log_mosaic(w.mosaic());
                shell.window_mut().set_mode(CanvasMode::Plan);
                self.phase = Phase::Plan;
            }
            Phase::Plan if w.mode() == CanvasMode::Plan && w.plan().is_settled() => {
                log_plan(shell);
                let first = shell
                    .session()
                    .and_then(|s| group_names(s).into_iter().next());
                self.phase = match first {
                    Some(g) if matches!(w.view(), Some(ViewKind::Composition(_))) => {
                        shell.window_mut().select_view(ViewKind::Group(g));
                        Phase::PlanGroup
                    }
                    _ => Phase::StartExports,
                };
            }
            Phase::PlanGroup
                if matches!(w.plan().view(), Some(ViewKind::Group(_))) && w.plan().is_settled() =>
            {
                log_plan(shell);
                self.phase = Phase::StartExports;
            }
            _ => {}
        }
    }

    fn finish(&mut self, ctx: &egui::Context, outcome: Result<(), String>) {
        match &outcome {
            Ok(()) => tracing::info!("self-test passed after {} frames", self.frames),
            Err(e) => tracing::error!("self-test failed: {e}"),
        }
        *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
        self.done = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The id of the open session's first composition, if it has one.
fn first_composition(shell: &Shell) -> Option<u64> {
    shell
        .session()
        .and_then(|s| load_compositions(&s.mosaic.config.ui).first().map(|c| c.id))
}

/// Log what the self-test's mosaic shows.
fn log_mosaic(mosaic: &MosaicView) {
    tracing::info!(
        "self-test: Mosaic of {:?}: {}; {} ROIs",
        mosaic.view(),
        mosaic
            .shown_text()
            .unwrap_or_else(|| "viewer shown".to_string()),
        mosaic.rois().len()
    );
    let group = mosaic.group().unwrap_or("(none)");
    match (mosaic.source(), mosaic.absent()) {
        (Some(src), _) => {
            let g = src.tile_geometry();
            let stretch = src.stretch();
            tracing::info!(
                "self-test: Mosaic shows group {group}: {} × {} × {} result, display stretch {stretch:?}",
                g.width,
                g.height,
                g.channels
            );
        }
        (None, why) => tracing::info!(
            "self-test: Mosaic, group {group}: no result ({})",
            why.unwrap_or("none")
        ),
    }
    match mosaic.health() {
        Some(Ok(h)) => tracing::info!(
            "self-test: health: seam step median {:.3e}, max {:.3e}, {} of {} edges flagged",
            h.seam_delta_median,
            h.seam_delta_max,
            h.seams.iter().filter(|s| s.flagged).count(),
            h.seams.len()
        ),
        Some(Err(e)) => tracing::info!("self-test: health unavailable: {e}"),
        None => tracing::info!("self-test: no health (group not analysed)"),
    }
}

/// Log what the self-test's plan shows.
fn log_plan(shell: &Shell) {
    let plan = shell.window().plan();
    let items = plan.items();
    let textured = items.iter().filter(|i| !i.layers.is_empty()).count();
    let layers: usize = items.iter().map(|i| i.layers.len()).sum();
    tracing::info!(
        "self-test: Plan shows {:?}: {} panels placed, {textured} with previews ({layers} layers), \
         frame {:?}, display stretch {:?}",
        plan.view(),
        items.len(),
        plan.frame_canvas(),
        plan.stretch_for_view()
    );
    tracing::info!(
        "self-test: {} previews in memory; checks {}",
        shell.pipeline().previews().len(),
        if shell.checks().running() {
            "running"
        } else {
            "idle"
        }
    );
}

/// Log the GL renderer and version; an error without a GL context.
fn log_gl(frame: &eframe::Frame) -> Result<(), String> {
    use eframe::glow::{self, HasContext};
    let gl = frame
        .gl()
        .ok_or("no GL context (the glow renderer is not running)")?;
    // SAFETY: plain string queries on the current, live context.
    let (vendor, renderer, version) = unsafe {
        (
            gl.get_parameter_string(glow::VENDOR),
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VERSION),
        )
    };
    tracing::info!("GL vendor: {vendor}");
    tracing::info!("GL renderer: {renderer}");
    tracing::info!("GL version: {version}");
    Ok(())
}

/// Self-test watchdog: after `timeout` it raises the returned flag and
/// wakes the UI, so the next frame fails the test and closes the window.
/// If no frame comes (an occluded or minimised window gets none), the
/// process exits with 1 after a further `backstop`.
pub fn selftest_watchdog(
    ctx: &egui::Context,
    timeout: Duration,
    backstop: Option<Duration>,
) -> Arc<AtomicBool> {
    let fired = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&fired);
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("mmm-selftest-watchdog".into())
        .spawn(move || {
            std::thread::sleep(timeout);
            flag.store(true, Ordering::SeqCst);
            ctx.request_repaint();
            if let Some(backstop) = backstop {
                std::thread::sleep(backstop);
                tracing::error!("self-test failed: no frame after the timeout; exiting");
                std::process::exit(1);
            }
        })
        .expect("spawn the self-test watchdog");
    fired
}

/// The eframe application.
pub struct MmmApp {
    shell: Shell,
    initial: Option<PathBuf>,
    selftest: Option<SelfTest>,
}

impl MmmApp {
    /// Set up the app: theme, recent sessions from storage, and the session
    /// to open (`initial`). With `selftest`, run the self-test and report
    /// into it instead of persisting anything.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
        selftest: Option<SelfTestOutcome>,
    ) -> Self {
        mega_ui::theme::apply(&cc.egui_ctx);
        let recent: Vec<RecentEntry> = match (&selftest, cc.storage) {
            (None, Some(storage)) => eframe::get_value(storage, RECENT_KEY).unwrap_or_default(),
            // The self-test's session is the start screen's hero first.
            (Some(_), _) => initial
                .iter()
                .map(|path| RecentEntry {
                    path: path.clone(),
                    last_opened: unix_now(),
                    pinned: false,
                })
                .collect(),
            _ => Vec::new(),
        };
        let shell = Shell::new(StartScreen::new(recent, Arc::new(RfdDialogs)))
            .with_launcher(Arc::new(ProcessLauncher));
        let selftest = selftest.map(|outcome| SelfTest {
            started: Instant::now(),
            frames: 0,
            session_frames: 0,
            session: initial.clone(),
            session_requested: false,
            timed_out: selftest_watchdog(&cc.egui_ctx, SELFTEST_TIMEOUT, Some(SELFTEST_BACKSTOP)),
            outcome,
            done: false,
            phase: Phase::Mosaic,
            exports: None,
        });
        MmmApp {
            shell,
            // The self-test opens the session itself, after the start screen.
            initial: if selftest.is_some() { None } else { initial },
            selftest,
        }
    }
}

impl eframe::App for MmmApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(dir) = self.initial.take() {
            self.shell.request(Request::Open(dir), ui.ctx());
        }
        let Some(test) = &mut self.selftest else {
            self.shell.ui(ui);
            return;
        };
        let shell = &mut self.shell;
        let panicked =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shell.ui(ui))).is_err();
        test.after_frame(ui.ctx(), frame, shell, panicked);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.shell.flush_periodic_saves();
        if self.selftest.is_none() {
            eframe::set_value(storage, RECENT_KEY, &self.shell.start.recent());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // `save` runs first only with persistence storage available.
        self.shell.flush_pending_saves();
        // Notes typed on the start screen less than a pause ago.
        self.shell.start.flush_notes();
    }

    fn persist_egui_memory(&self) -> bool {
        self.selftest.is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::start::tests::{
        RecordingLauncher, StubPicker, relative_to_cwd, step_until, temp_dir, temp_dir_near_cwd,
    };

    const CANVAS_W: u64 = crate::testutil::CANVAS.0;

    fn harness(picker: StubPicker) -> Harness<'static, Shell> {
        harness_launching(picker, Arc::new(RecordingLauncher::default()))
    }

    fn harness_launching(
        picker: StubPicker,
        launcher: Arc<RecordingLauncher>,
    ) -> Harness<'static, Shell> {
        let shell =
            Shell::new(StartScreen::new(Vec::new(), Arc::new(picker))).with_launcher(launcher);
        Harness::builder()
            .with_size(egui::vec2(1000.0, 800.0))
            // The start screen's background work (facts, hero, thumbnails)
            // asks for a repaint as each piece finishes.
            .with_step_dt(0.01)
            .with_max_steps(64)
            .build_ui_state(
                |ui, s: &mut Shell| {
                    if crate::testutil::themed(ui) {
                        s.ui(ui);
                    }
                },
                shell,
            )
    }

    fn type_into(h: &mut Harness<'static, Shell>, field: &str, text: &str) {
        let role = egui::accesskit::Role::TextInput;
        // The typed path lives behind More → Type a path….
        if field == "Session folder" && h.query_by_role_and_label(role, field).is_none() {
            h.get_by_label("More").click();
            h.run();
            h.get_by_label("Type a path…").click();
            h.run();
        }
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, field)
            .focus();
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, field)
            .type_text(text);
        h.step();
    }

    #[test]
    fn new_mosaic_creates_from_one_dialog() {
        let parent = temp_dir("new");
        // The save dialog answers a name without the extension.
        let mut h = harness(StubPicker(Some(parent.join("Orion"))));
        h.run();
        h.get_by_label("New mosaic").click();
        // The (stub) dialog answers on a background thread.
        step_until(&mut h, |s| s.session().is_some());
        let dir = parent.join("Orion.mmm-session");
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        assert!(dir.join("mosaic.json").is_file());
        assert_eq!(h.state().start.recent()[0].path, dir);
        // A session without panels opens on its panel list.
        assert!(h.state().window().panel_list_open);
        h.get_by_label("Add panels…");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn cancelled_dialog_reveals_typed_path() {
        let mut h = harness(StubPicker(None));
        h.run();
        let role = egui::accesskit::Role::TextInput;
        assert!(
            h.query_by_role_and_label(role, "No dialog? Type a path")
                .is_none()
        );
        h.get_by_label("New mosaic").click();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while h
            .query_by_role_and_label(role, "No dialog? Type a path")
            .is_none()
        {
            assert!(Instant::now() < deadline, "no typed-path row");
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(h.state().session().is_none());
    }

    #[test]
    fn typed_path_creates_after_a_cancelled_dialog() {
        for use_enter in [false, true] {
            let parent = temp_dir(if use_enter {
                "typed-enter"
            } else {
                "typed-button"
            });
            let mut h = harness(StubPicker(None));
            h.run();
            h.get_by_label("New mosaic").click();
            let role = egui::accesskit::Role::TextInput;
            let deadline = Instant::now() + crate::testutil::PATIENCE;
            while h
                .query_by_role_and_label(role, "No dialog? Type a path")
                .is_none()
            {
                assert!(Instant::now() < deadline, "no typed-path row");
                h.step();
                std::thread::sleep(Duration::from_millis(5));
            }
            let typed = parent.join("Typed");
            type_into(&mut h, "No dialog? Type a path", typed.to_str().unwrap());
            if use_enter {
                h.key_press(egui::Key::Enter);
                h.step();
            } else {
                h.get_by_label("Create").click();
            }
            step_until(&mut h, |s| s.session().is_some());
            let dir = parent.join("Typed.mmm-session");
            assert!(dir.join("mosaic.json").is_file());
            assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
            assert!(h.state().window().panel_list_open);
            let _ = std::fs::remove_dir_all(&parent);
        }
    }

    #[test]
    fn failed_launch_shows_a_toast_and_records_nothing() {
        struct Failing;
        impl Launcher for Failing {
            fn open_window(&self, _: Option<&Path>) -> Result<(), String> {
                Err("Cannot open a new window: no process".into())
            }
        }
        let root = temp_dir("shell-launch-fail");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let other = root.join("Other.mmm-session");
        drop(crate::session::create_blocking(&other).unwrap());
        let shell = Shell::new(StartScreen::new(Vec::new(), Arc::new(StubPicker(None))))
            .with_launcher(Arc::new(Failing));
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 800.0))
            .with_step_dt(0.01)
            .with_max_steps(64)
            .build_ui_state(
                |ui, s: &mut Shell| {
                    if crate::testutil::themed(ui) {
                        s.ui(ui);
                    }
                },
                shell,
            );
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        let recent = h.state().start.recent().to_vec();
        // Another session goes to a window of its own, which cannot open:
        // a toast says so, and the session is not recorded.
        let ctx = h.ctx.clone();
        h.state_mut().request(Request::Open(other), &ctx);
        step_until(&mut h, |s| !s.is_busy());
        h.step();
        h.get_by_label_contains("Cannot open a new window");
        assert_eq!(h.state().start.recent(), recent.as_slice());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn another_session_opens_in_a_new_window() {
        let root = temp_dir("shell-another");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let other = root.join("Other.mmm-session");
        drop(crate::session::create_blocking(&other).unwrap());
        let launcher = Arc::new(RecordingLauncher::default());
        let mut h = harness_launching(StubPicker(None), launcher.clone());
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        let ctx = h.ctx.clone();
        h.state_mut().request(Request::Open(other.clone()), &ctx);
        step_until(&mut h, |s| !s.is_busy());
        assert_eq!(*launcher.0.lock().unwrap(), [Some(other)]);
        // No floating start window over the session, which stays open.
        assert!(h.query_by_label("Open another session").is_none());
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shell_writes_summary_after_opening() {
        use mmm_core::session_summary::{SUMMARY_FILE, read_summary};
        let root = temp_dir("shell-summary");
        let opened = crate::testutil::two_group_session(&root);
        let dir = opened.mosaic.dir.clone();
        let panels = opened.mosaic.config.panels.len();
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        step_until(&mut h, |_| dir.join(SUMMARY_FILE).is_file());
        let summary = read_summary(&dir).unwrap();
        assert_eq!(summary.panels, panels);
        let names: Vec<_> = summary.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Ha", "OIII"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn last_view_follows_the_window() {
        use mmm_core::session_summary::read_summary;
        let root = temp_dir("shell-last-view");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        let last = |dir: &Path| read_summary(dir).and_then(|s| s.last_view);
        step_until(&mut h, |_| last(&dir).as_deref() == Some("Ha"));
        // The view picked in the window, whichever canvas shows it.
        h.state_mut()
            .window_mut()
            .select_view(ViewKind::Group("OIII".into()));
        step_until(&mut h, |_| last(&dir).as_deref() == Some("OIII"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_mosaic_folder_shows_error() {
        let root = temp_dir("shell-legacy");
        std::fs::write(root.join("session.json"), "{}").unwrap();
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", root.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| !s.is_busy());
        h.run();
        assert!(h.state().session().is_none());
        h.get_by_label_contains("mmm init");

        // A folder that is no session of any kind.
        let plain = temp_dir("shell-plain");
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Session folder")
            .focus();
        h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Session folder")
            .type_text(plain.to_str().unwrap());
        h.step();
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| !s.is_busy());
        h.run();
        assert!(h.state().session().is_none());
        h.get_by_label_contains("not a Mega Merge Mosaic session");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn relative_path_is_absolute_and_reopen_refused() {
        let root = temp_dir_near_cwd("shell-relative");
        let dir = root.join("Rel.mmm-session");
        crate::session::create_blocking(&dir).unwrap();
        let rel = relative_to_cwd(&dir);
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", rel.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        assert_eq!(h.state().start.recent()[0].path, dir);

        // Opening it again (relative) in this window is refused (the start
        // screen says why).
        let ctx = h.ctx.clone();
        h.state_mut().request(Request::Open(rel), &ctx);
        h.step();
        step_until(&mut h, |s| !s.is_busy());
        h.run_steps(2);
        let error = h.state().start.error().unwrap_or_default();
        assert!(error.contains("already open in this window"), "{error}");
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn job_outcomes_show_as_toasts() {
        let root = temp_dir("shell-jobs");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        // The pipeline previews the panels, then analyses them by itself.
        step_until(&mut h, |s| {
            s.session().unwrap().mosaic.config.group_dirs.len() == 2
                && !s.jobs.as_ref().unwrap().busy()
        });
        h.run_steps(2);
        assert_eq!(h.state().pipeline().previews().len(), 8);
        assert_eq!(h.state().toasted(), ["Analyse finished"]);
        // No status bar: its items are gone.
        assert!(h.query_by_label("Ready").is_none());
        assert!(h.query_by_label("Canvas 600 × 420 (0.3 MP)").is_none());
        // The checks (unsolved registered panels: a note each).
        step_until(&mut h, |s| {
            s.checks().report().is_some() && !s.checks().running()
        });
        h.run_steps(2);
        // A session with panels opens on the canvas, on the plan while
        // nothing is merged.
        assert!(!h.state().window().panel_list_open);
        assert_eq!(h.state().window().mode(), CanvasMode::Plan);
        // The plan shows the first group's previews in place.
        step_until(&mut h, |s| s.window().plan().is_settled());
        assert_eq!(h.state().window().plan().items().len(), 4);
        h.get_by_label("Ha · 4");
        h.get_by_label("OIII · 4");
        // Behind it, the mosaic of Ha: never merged, but analysed (health).
        assert_eq!(h.state().window().mosaic().group(), Some("Ha"));
        step_until(&mut h, |s| s.window().mosaic().absent().is_some());
        step_until(&mut h, |s| s.window().mosaic().health().is_some());
        assert!(matches!(h.state().window().mosaic().health(), Some(Ok(_))));
        // The health ends the Merge settings section.
        h.state_mut()
            .window_mut()
            .inspector_mut()
            .reveal("merge-settings");
        h.run_steps(2);
        h.get_by_label("Seam step median");
        // The main button (at the right of the Merge step) merges every
        // group and shows the mosaic.
        let right = h
            .query_all_by_label("Merge")
            .map(|n| n.rect().center().x)
            .fold(f32::MIN, f32::max);
        h.query_all_by_label("Merge")
            .find(|n| n.rect().center().x == right)
            .expect("the main button")
            .click();
        h.step();
        assert_eq!(h.state().window().mode(), CanvasMode::Mosaic);
        step_until(&mut h, |s| !s.jobs.as_ref().unwrap().busy());
        h.run_steps(2);
        let toasted = h.state().toasted();
        for t in ["Merge Ha finished", "Merge OIII finished"] {
            assert!(toasted.iter().any(|x| x == t), "{t}: {toasted:?}");
        }
        assert!(h.state().session().unwrap().merged);
        // Merged: Export… is the main button now, and stays so once the
        // summary has classified the results.
        h.get_by_label("Merged");
        h.get_by_label("Export…");
        step_until(&mut h, |s| !s.summary.busy());
        h.run_steps(2);
        h.get_by_label("Export…");
        // The result opens with an automatic display stretch, and the
        // viewer settles with its tiles loaded.
        step_until(&mut h, |s| {
            s.window().mosaic().source().is_some() && s.window().mosaic().is_settled()
        });
        let src = h.state().window().mosaic().source().unwrap().clone();
        assert_eq!(src.tile_geometry().width, CANVAS_W);
        assert_ne!(
            src.stretch()[0],
            crate::tile_source::LINEAR,
            "auto stretch applied"
        );
        // The panel list shows the panels in their groups.
        h.get_by_label("8 panels").click();
        h.step();
        assert!(h.state().window().panel_list_open);
        h.get_by_label("Ha_0.xisf");
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        // The OIII section's heading reads like its chip in the view bar.
        while h.query_all_by_label("OIII · 4").count() < 2 {
            assert!(Instant::now() < deadline, "no OIII section");
            h.step();
        }
        h.get_by_label("OIII_0.xisf");
        let actions: Vec<_> = h
            .state()
            .session()
            .unwrap()
            .mosaic
            .config
            .provenance
            .iter()
            .map(|p| p.action.clone())
            .collect();
        // Merge merged every group.
        assert_eq!(actions, ["analyze", "blend", "blend"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_status_bar() {
        let root = temp_dir("shell-no-status");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        step_until(&mut h, |s| {
            s.session().unwrap().mosaic.config.group_dirs.len() == 2
                && !s.jobs.as_ref().unwrap().busy()
        });
        h.run_steps(2);
        for label in ["Ready", "Checks:", "RAM", "CPU"] {
            assert!(h.query_by_label_contains(label).is_none(), "{label}");
        }
        assert!(h.query_by_label("2 groups").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn title_names_the_session() {
        let root = temp_dir("shell-title");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let mut h = harness(StubPicker(None));
        h.run();
        let titles = |h: &Harness<'static, Shell>| -> Vec<String> {
            h.output().viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::ViewportCommand::Title(t) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(titles(&h).is_empty(), "no session, no title change");
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        // Collect from every frame, the first included: the open may land
        // (and the title go out) in the very step that takes the Enter.
        let mut seen = Vec::new();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        loop {
            h.step();
            seen.extend(titles(&h));
            if h.state().session().is_some() && !seen.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "no session, or no title for it");
            std::thread::sleep(Duration::from_millis(5));
        }
        // A few frames more (the pipeline is at work: no `run`): once.
        for _ in 0..3 {
            h.step();
            seen.extend(titles(&h));
        }
        assert_eq!(seen, ["m — Mega Merge Mosaic"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pending_saves_flushed_by_shell() {
        let root = temp_dir("shell-flush");
        let mut session = crate::testutil::two_group_session(&root);
        session.mosaic.save().unwrap();
        let mut shell = Shell::new(StartScreen::new(Vec::new(), Arc::new(StubPicker(None))));
        // Nothing open: nothing to do.
        shell.flush_pending_saves();
        session.mosaic.config.ui = serde_json::json!({ "marker": 1 });
        let dir = session.mosaic.dir.clone();
        shell.session = Some(session);
        shell.window.touch_save();
        shell.flush_pending_saves();
        let back = mmm_core::mosaic::MosaicConfig::load(&dir).unwrap();
        assert_eq!(back.ui["marker"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn view_state_flushed_by_shell() {
        use crate::session_window::view_state::ViewState;
        use mmm_core::mosaic::MosaicConfig;
        let root = temp_dir("shell-view-state");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        gate.open();
        h.run_steps(3);
        let saved = |dir: &Path| ViewState::load(&MosaicConfig::load(dir).unwrap().ui);
        step_until(&mut h, |_| {
            saved(&dir).is_some_and(|v| v.view.as_deref() == Some("Ha"))
        });
        // Changed with no frame since: the rate limit holds it back, the
        // flush writes it.
        h.state_mut()
            .window_mut()
            .select_view(ViewKind::Group("OIII".into()));
        h.state_mut().flush_pending_saves();
        assert_eq!(saved(&dir).unwrap().view.as_deref(), Some("OIII"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The ‹ button's label.
    const BACK: &str = "Back to the start screen";

    /// The window titles sent in the last frame.
    fn titles(h: &Harness<'static, Shell>) -> Vec<String> {
        h.output().viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::ViewportCommand::Title(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    /// A shell (asking `launcher` for other windows) with the session in
    /// `dir` open, as the start screen opens one; its job runner holds every
    /// worker at the gate returned until it opens.
    fn harness_open(
        dir: &Path,
        launcher: Arc<RecordingLauncher>,
    ) -> (Harness<'static, Shell>, Arc<crate::jobs::tests::Gate>) {
        let mut h = harness_launching(StubPicker(None), launcher);
        let opened = crate::session::open_blocking(dir).unwrap();
        let ctx = h.ctx.clone();
        let shell = h.state_mut();
        shell.open_here(opened, &ctx);
        let gate = shell.jobs.as_mut().unwrap().hold();
        (h, gate)
    }

    #[test]
    fn back_returns_to_the_start_screen() {
        use crate::session::{notes, set_notes};
        use mmm_core::mosaic::MosaicConfig;
        let root = temp_dir("shell-back");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let launcher = Arc::new(RecordingLauncher::default());
        let (mut h, gate) = harness_open(&dir, launcher.clone());
        gate.open();
        h.run_steps(3);
        assert!(h.query_by_label("Resume session").is_none());
        // Notes typed in the session window: saved, then a later edit held
        // back by the rate limit.
        let type_notes = |h: &mut Harness<'static, Shell>, text: &str| {
            let shell = h.state_mut();
            set_notes(&mut shell.session.as_mut().unwrap().mosaic.config.ui, text);
            shell.window.touch_save();
        };
        let on_disk = |dir: &Path| notes(&MosaicConfig::load(dir).unwrap().ui);
        type_notes(&mut h, "Draft");
        step_until(&mut h, |_| on_disk(&dir) == "Draft");
        type_notes(&mut h, "Redo DBE");
        h.get_by_role_and_label(egui::accesskit::Role::Button, BACK)
            .click();
        h.step();
        assert!(h.state().session().is_none());
        assert_eq!(on_disk(&dir), "Redo DBE", "flushed on leaving");
        assert_eq!(titles(&h), ["Mega Merge Mosaic"]);
        // The start screen, the session's card read again (its notes as
        // the window saved them).
        step_until(&mut h, |s| !s.start.checking_recent());
        h.run();
        h.get_by_label("Resume session");
        assert_eq!(h.state().start.notes_text(), "Redo DBE");
        // It opens again in this window.
        h.get_by_label("Resume session").click();
        step_until(&mut h, |s| s.session().is_some());
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        assert!(launcher.0.lock().unwrap().is_empty(), "no other window");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_undo_toast_restores_the_edit() {
        use crate::panel_edits::{Applied, PanelsEdit};
        use mmm_core::mosaic::MosaicConfig;
        let root = temp_dir("shell-undo");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        gate.open();
        step_until(&mut h, |s| {
            s.session().unwrap().mosaic.config.group_dirs.len() == 2
                && !s.jobs.as_ref().unwrap().busy()
        });
        h.run_steps(2);
        let on_disk = |dir: &Path| MosaicConfig::load(dir).unwrap().panels.len();
        let previous = PanelsEdit::of(h.state().session().unwrap());
        assert_eq!(previous.panels.len(), 8);
        // Remove two panels, and offer to put them back.
        let mut fewer = previous.clone();
        let removed: Vec<_> = fewer
            .panels
            .drain(6..)
            .enumerate()
            .map(|(i, p)| (6 + i, p))
            .collect();
        {
            let shell = h.state_mut();
            let (session, jobs) = shell.session.as_mut().zip(shell.jobs.as_mut()).unwrap();
            assert!(matches!(fewer.apply(session, jobs), Applied::Done));
            shell.window.push_toast(WindowToast {
                toast: Toast {
                    tone: Tone::Neutral,
                    title: "Removed 2 panels".into(),
                    detail: None,
                    action: Some("Undo".into()),
                },
                undo: Some(Undo::Readd(removed)),
            });
        }
        assert_eq!(on_disk(&dir), 6);
        h.run_steps(2);
        h.get_by_label("Removed 2 panels");
        assert_eq!(h.state().undos.len(), 1);
        h.get_by_label("Undo").click();
        step_until(&mut h, |_| on_disk(&dir) == 8);
        assert_eq!(MosaicConfig::load(&dir).unwrap().panels, previous.panels);
        assert!(h.state().undos.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_panel_edit_stops_the_merge_with_one_toast() {
        use crate::panel_edits::{Applied, PanelsEdit};
        let root = temp_dir("shell-edit-stops-merge");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        gate.open();
        step_until(&mut h, |s| {
            s.session().unwrap().mosaic.config.group_dirs.len() == 2
                && !s.jobs.as_ref().unwrap().busy()
        });
        h.run_steps(2);
        let before = h.state().toasted().len();
        // Two merges, one running (held) and one queued; a panel is removed.
        let held = {
            let shell = h.state_mut();
            let (session, jobs) = shell.session.as_mut().zip(shell.jobs.as_mut()).unwrap();
            let held = jobs.hold();
            jobs.start_blend(session, "Ha", None);
            jobs.start_blend(session, "OIII", None);
            let mut edit = PanelsEdit::of(session);
            edit.panels.pop();
            assert!(matches!(edit.apply(session, jobs), Applied::Queued(_)));
            held
        };
        held.open();
        step_until(&mut h, |s| !s.jobs.as_ref().unwrap().busy());
        h.run_steps(2);
        let toasted = &h.state().toasted()[before..];
        assert_eq!(toasted, ["Merge stopped: panels changed"], "{toasted:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_analysis_toast_replaces_the_previous_one() {
        let root = temp_dir("shell-undo-analysis");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        gate.open();
        step_until(&mut h, |s| !s.jobs.as_ref().unwrap().busy());
        h.run_steps(2);
        let analysis = h.state().session().unwrap().mosaic.config.analyze.clone();
        for _ in 0..2 {
            h.state_mut().window.push_toast(WindowToast {
                toast: Toast {
                    tone: Tone::Neutral,
                    title: "Analysis settings changed".into(),
                    detail: None,
                    action: Some("Undo".into()),
                },
                undo: Some(Undo::Analyze(analysis.clone())),
            });
            h.run_steps(2);
        }
        assert_eq!(h.state().undos.len(), 1, "the first one was taken down");
        assert_eq!(h.query_all_by_label("Analysis settings changed").count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leaving_takes_down_the_undo_toasts() {
        let root = temp_dir("shell-undo-leave");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        gate.open();
        step_until(&mut h, |s| !s.jobs.as_ref().unwrap().busy());
        h.run_steps(2);
        let panel = h.state().session().unwrap().mosaic.config.panels[7].clone();
        let removed = vec![(7, panel)];
        h.state_mut().window.push_toast(WindowToast {
            toast: Toast {
                tone: Tone::Neutral,
                title: "Removed 1 panel".into(),
                detail: None,
                action: Some("Undo".into()),
            },
            undo: Some(Undo::Readd(removed)),
        });
        h.run_steps(2);
        h.get_by_label("Removed 1 panel");
        // Its Undo would change a session no longer open: it goes too.
        h.get_by_role_and_label(egui::accesskit::Role::Button, BACK)
            .click();
        h.step();
        assert!(h.state().session().is_none());
        h.run_steps(2);
        assert!(h.query_by_label("Removed 1 panel").is_none());
        assert!(h.query_by_label("Undo").is_none());
        assert!(h.state().undos.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn back_cancels_running_work_and_releases_the_lock() {
        use mmm_core::mosaic::{CONFIG_FILE, MosaicConfig, lock_session};
        use mmm_core::session_summary::SUMMARY_FILE;
        let root = temp_dir("shell-back-cancel");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, previews) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        // The previews pass; the analysis after them is held before it
        // starts.
        step_until(&mut h, |s| s.jobs.as_ref().unwrap().busy());
        let analysis = h.state_mut().jobs.as_mut().unwrap().hold();
        previews.open();
        let running = |s: &Shell| {
            let views = s.jobs.as_ref().unwrap().views();
            views
                .first()
                .is_some_and(|v| v.label == crate::jobs::ANALYZE_LABEL)
        };
        step_until(&mut h, running);
        assert!(running(h.state()), "analysing");
        let before = MosaicConfig::load(&dir).unwrap();
        // It gets going once ‹ has cancelled it, while ‹ waits for it.
        h.state_mut()
            .jobs
            .as_mut()
            .unwrap()
            .on_stop(move || analysis.open());
        h.get_by_role_and_label(egui::accesskit::Role::Button, BACK)
            .click();
        h.step();
        assert!(h.state().session().is_none());
        assert!(h.state().jobs.is_none(), "no jobs");
        drop(lock_session(&dir).expect("the session's lock is released"));
        // Cancelled, not run to the end: nothing analysed.
        let after = MosaicConfig::load(&dir).unwrap();
        assert_eq!(after.group_dirs, before.group_dirs);
        assert_eq!(after.provenance, before.provenance);
        let config = std::fs::read(dir.join(CONFIG_FILE)).unwrap();
        let summary = std::fs::read(dir.join(SUMMARY_FILE)).expect("a summary");
        // A moment later, nothing has written into the session.
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read(dir.join(CONFIG_FILE)).unwrap(), config);
        assert_eq!(std::fs::read(dir.join(SUMMARY_FILE)).unwrap(), summary);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn back_keeps_the_lock_until_a_job_that_ignores_cancelling_stops() {
        use egui::accesskit::Role;
        use mmm_core::mosaic::lock_session;
        use mmm_core::session_summary::SUMMARY_FILE;
        let root = temp_dir("shell-back-stuck");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        h.state_mut().stop_wait = Duration::from_millis(50);
        // The previews' worker is held at the gate: it cannot see a cancel
        // (like a decode, which nothing interrupts).
        step_until(&mut h, |s| s.jobs.as_ref().unwrap().busy());
        h.get_by_role_and_label(Role::Button, BACK).click();
        h.step();
        assert!(h.state().session().is_none(), "back without waiting");
        assert!(lock_session(&dir).is_err(), "locked while the job runs");
        // The card says so, and opening it meanwhile is refused like any
        // busy session's.
        let busy = |s: &Shell| {
            s.start
                .card_look(&dir)
                .0
                .is_some_and(|(_, t)| t == "Open in another window")
        };
        step_until(&mut h, |s| !s.start.checking_recent() && busy(s));
        let ctx = h.ctx.clone();
        h.state_mut().request(Request::Open(dir.clone()), &ctx);
        step_until(&mut h, |s| !s.is_busy());
        assert!(h.state().session().is_none());
        let error = h.state().start.error().unwrap_or_default().to_string();
        assert!(
            error.contains("open in another window or process"),
            "{error}"
        );
        // Once the job has stopped, the summary is written, then the lock
        // released, and the card is read again.
        gate.open();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while lock_session(&dir).is_err() {
            assert!(Instant::now() < deadline, "the lock is never released");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(dir.join(SUMMARY_FILE).is_file());
        step_until(&mut h, |s| !s.start.checking_recent() && !busy(s));
        h.run();
        h.get_by_label("Resume session").click();
        step_until(&mut h, |s| s.session().is_some());
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A merged two-group session in a temporary root with ROI "Core" in
    /// view, closed: its root and its directory.
    fn merged_with_an_roi(tag: &str) -> (PathBuf, PathBuf) {
        let (root, mut s, mut jobs) = crate::jobs::tests::analyzed(tag);
        jobs.start_blend(&mut s, "Ha", None);
        let _ = crate::jobs::tests::drain(&mut jobs, &mut s);
        let core = crate::rois::Roi {
            id: 7,
            name: "Core".into(),
            note: String::new(),
            sky: None,
            canvas: [100, 100, 200, 160],
        };
        crate::rois::save_rois(&mut s.mosaic.config.ui, &[core]);
        s.mosaic.save().unwrap();
        let dir = s.mosaic.dir.clone();
        drop((s, jobs));
        (root, dir)
    }

    /// Whether the session window has started a preview of an ROI.
    fn previewing(s: &Shell) -> bool {
        s.window
            .previews()
            .log()
            .iter()
            .any(|e| matches!(e, crate::previews::Logged::Started(..)))
    }

    #[test]
    fn leaving_a_session_ends_its_preview_before_the_lock_goes() {
        use mmm_core::mosaic::lock_session;
        use std::sync::atomic::Ordering::SeqCst;
        let (root, dir) = merged_with_an_roi("shell-back-preview");
        // A preview a cancel cannot stop (like a decode, which nothing
        // interrupts) outlives the wait: leaving does not hang, and the
        // session stays locked until the preview's thread has ended.
        let (mut h, jobs) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        jobs.open();
        h.state_mut().stop_wait = Duration::from_millis(50);
        let gate = h.state_mut().window.previews_mut().hold_through_cancel();
        let probe = h.state().window.previews().probe();
        step_until(&mut h, previewing);
        let ctx = h.ctx.clone();
        assert_eq!(h.state_mut().close_session(&ctx), Ok(()));
        assert!(h.state().session().is_none(), "left without waiting");
        assert!(probe.alive.load(SeqCst), "the preview still runs");
        assert!(lock_session(&dir).is_err(), "locked while the preview runs");
        gate.open();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while lock_session(&dir).is_err() {
            assert!(Instant::now() < deadline, "the lock is never released");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!probe.alive.load(SeqCst), "released once the preview ended");
        // A preview that stops when cancelled has ended by the time leaving
        // returns, and the lock goes at once.
        let (mut h, jobs) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        jobs.open();
        let gate = h.state_mut().window.previews_mut().hold();
        let probe = h.state().window.previews().probe();
        step_until(&mut h, previewing);
        let ctx = h.ctx.clone();
        assert_eq!(h.state_mut().close_session(&ctx), Ok(()));
        assert!(!probe.alive.load(SeqCst), "the preview ended first");
        drop(lock_session(&dir).expect("the session's lock is released"));
        gate.open();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn back_is_disabled_while_exporting() {
        use egui::accesskit::Role;
        use mmm_core::export::ExportFormat;
        let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("shell-back-export");
        jobs.start_blend(&mut s, "Ha", None);
        let _ = crate::jobs::tests::drain(&mut jobs, &mut s);
        let dir = s.mosaic.dir.clone();
        drop((s, jobs));
        let (mut h, gate) = harness_open(&dir, Arc::new(RecordingLauncher::default()));
        {
            let shell = h.state_mut();
            let (session, jobs) = shell.session.as_mut().zip(shell.jobs.as_mut()).unwrap();
            let ha = crate::jobs::ExportTarget::Group("Ha".into());
            jobs.start_export(session, ha, root.join("Ha.fits"), ExportFormat::Fits, false);
        }
        h.run_steps(3);
        let back = |h: &Harness<'static, Shell>| {
            h.get_by_role_and_label(Role::Button, BACK)
                .accesskit_node()
                .is_disabled()
        };
        assert!(back(&h), "disabled while exporting");
        // Its tooltip says why.
        let why = "Exporting: cancel it or wait to go back";
        h.get_by_role_and_label(Role::Button, BACK).hover();
        for _ in 0..200 {
            if h.query_by_label(why).is_some() {
                break;
            }
            h.step();
        }
        h.get_by_label(why);
        let ctx = h.ctx.clone();
        assert_eq!(h.state_mut().close_session(&ctx), Err(why.to_string()));
        assert!(h.state().session().is_some());
        // Once the export has ended, ‹ goes back.
        gate.open();
        step_until(&mut h, |s| !s.jobs.as_ref().unwrap().busy());
        h.run_steps(2);
        assert!(!back(&h));
        h.get_by_role_and_label(Role::Button, BACK).click();
        h.step();
        assert!(h.state().session().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watchdog_fires_after_deadline() {
        let ctx = egui::Context::default();
        let fired = selftest_watchdog(&ctx, std::time::Duration::from_millis(10), None);
        let deadline = std::time::Instant::now() + crate::testutil::PATIENCE;
        while !fired.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
