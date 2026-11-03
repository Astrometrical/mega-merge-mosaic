//! The application shell: the start screen until a session is open, then
//! the session view. [`Shell`] is plain egui (testable without eframe);
//! [`MmmApp`] wraps it for eframe with persistence and the self-test.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use mega_ui::theme::palette;

use crate::blend_tab::BlendTab;
use crate::checks::ChecksEngine;
use crate::compositions::{ViewKind, load_compositions};
use crate::jobs::{JobEvent, JobRunner, PREVIEWS_LABEL};
use crate::panels_tab::{FilesPicker, PanelsTab, RfdFilesPicker};
use crate::pipeline::{Pipeline, Step, auto_analyze, set_auto_analyze};
use crate::plan_tab::{PlanFeed, PlanTab};
use crate::session::{OpenSession, Pending, Request, open_in_new_process};
use crate::start::{RecentEntry, RfdPicker, StartAction, StartScreen, unix_now};
use crate::status::{self, SysMonitor};

/// Directory, inside the session, where compressed panels are decoded.
const DECODED_DIR: &str = "decoded";
/// eframe storage key of the recent sessions.
const RECENT_KEY: &str = "recent_sessions";
/// How long a job notice stays on screen.
const NOTICE_TIME: Duration = Duration::from_secs(6);

/// The session view's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// The panel files and their grouping.
    Panels,
    /// Every panel in place, from its preview.
    Plan,
    /// Blend controls and the result viewer.
    Blend,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Panels, Tab::Plan, Tab::Blend];

    fn label(self) -> &'static str {
        match self {
            Tab::Panels => "Panels",
            Tab::Plan => "Plan",
            Tab::Blend => "Blend",
        }
    }
}

/// A toast-style notice about a job that ended.
struct Notice {
    text: String,
    color: Color32,
    until: Instant,
}

/// The tab a session opens on: Panels while it has no panels (adding
/// them is the first step), else Plan.
fn default_tab(session: &OpenSession) -> Tab {
    if session.mosaic.config.panels.is_empty() {
        Tab::Panels
    } else {
        Tab::Plan
    }
}

/// The window contents: start screen or open session.
pub struct Shell {
    /// The start screen (also shown over an open session to open another).
    pub start: StartScreen,
    session: Option<OpenSession>,
    pending: Option<Pending>,
    show_start: bool,
    /// Background jobs of the open session (made with the session).
    jobs: Option<JobRunner>,
    sys: SysMonitor,
    notices: Vec<Notice>,
    tab: Tab,
    /// The Blend tab (made afresh with each session).
    blend: BlendTab,
    /// The Panels tab (made afresh with each session).
    panels: PanelsTab,
    /// The Plan tab (made afresh with each session).
    plan: PlanTab,
    /// The panel file dialog used by the Panels tab.
    files_picker: Arc<dyn FilesPicker>,
    /// The automatic previews and analysis (made afresh with each session).
    pipeline: Pipeline,
    /// The checks (made afresh with each session).
    checks: ChecksEngine,
}

impl Shell {
    /// A shell showing `start`.
    pub fn new(start: StartScreen) -> Self {
        Shell {
            start,
            session: None,
            pending: None,
            show_start: false,
            jobs: None,
            sys: SysMonitor::start(),
            notices: Vec::new(),
            // Set when a session opens (`default_tab`).
            tab: Tab::Plan,
            blend: BlendTab::new(),
            panels: PanelsTab::new(Arc::new(RfdFilesPicker)),
            plan: PlanTab::new(),
            files_picker: Arc::new(RfdFilesPicker),
            pipeline: Pipeline::new(),
            checks: ChecksEngine::new(),
        }
    }

    /// The session open in this window, if any.
    pub fn session(&self) -> Option<&OpenSession> {
        self.session.as_ref()
    }

    /// True while a session is being opened or created.
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The tab shown in the session view.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The Blend tab.
    pub fn blend(&self) -> &BlendTab {
        &self.blend
    }

    /// The Blend tab, mutably.
    pub fn blend_mut(&mut self) -> &mut BlendTab {
        &mut self.blend
    }

    /// Show `tab`.
    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    /// The Plan tab.
    pub fn plan(&self) -> &PlanTab {
        &self.plan
    }

    /// The Plan tab, to change its view.
    pub fn plan_mut(&mut self) -> &mut PlanTab {
        &mut self.plan
    }

    /// The automatic pipeline (its previews feed Plan).
    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    /// The checks engine.
    pub fn checks(&self) -> &ChecksEngine {
        &self.checks
    }

    /// Save anything held back by a rate limit (the display stretch), e.g.
    /// when the window closes.
    pub fn flush_pending_saves(&mut self) {
        if let Some(session) = &self.session {
            self.blend.flush_save(session);
            self.plan.flush_save(session);
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
        if self.session.is_none() {
            egui::CentralPanel::default().show(ui, |ui| {
                if let Some(action) = self.start.ui(ui) {
                    self.request(to_request(action), &ctx);
                }
            });
        } else {
            self.session_ui(ui);
        }
    }

    /// Apply a finished open/create.
    fn poll(&mut self, ctx: &egui::Context) {
        let Some(outcome) = self.pending.as_mut().and_then(Pending::poll) else {
            return;
        };
        let created = self
            .pending
            .take()
            .is_some_and(|p| matches!(p.request, Request::Create(_)));
        self.start.set_busy(None);
        match outcome {
            Ok(opened) => {
                // Both paths are normalised (absolute, canonical) by
                // `session::open_blocking`, so this compares like with like.
                let dir = opened.mosaic.dir.clone();
                if created {
                    self.start.clear_new_form();
                }
                if self
                    .session
                    .as_ref()
                    .is_some_and(|open| open.mosaic.dir == dir)
                {
                    self.start.set_error(Some(format!(
                        "{} is already open in this window.",
                        dir.display()
                    )));
                } else if self.session.is_some() {
                    // One session per window: the other one gets its own.
                    // A created session holds the lock; release it so the
                    // child can take it.
                    drop(opened);
                    match open_in_new_process(&dir) {
                        Ok(()) => {
                            tracing::info!("opened {} in a new window", dir.display());
                            self.start.record_opened(&dir, unix_now());
                            self.show_start = false;
                        }
                        Err(e) => self.start.set_error(Some(format!(
                            "Cannot open {} in a new window: {e}",
                            dir.display()
                        ))),
                    }
                } else {
                    tracing::info!("opened session {}", dir.display());
                    self.start.record_opened(&dir, unix_now());
                    // Compressed panels decode to disk once, shared by
                    // previews and analysis (one session per process).
                    mmm_core::decoded::set_cache_dir(dir.join(DECODED_DIR));
                    self.tab = default_tab(&opened);
                    self.session = Some(opened);
                    self.jobs = Some(JobRunner::new(ctx.clone()));
                    self.blend = BlendTab::new();
                    self.panels = PanelsTab::new(self.files_picker.clone());
                    self.plan = PlanTab::new();
                    self.pipeline = Pipeline::new();
                    self.checks = ChecksEngine::new();
                }
            }
            Err(message) => {
                tracing::warn!("{message}");
                self.start.set_error(Some(message));
            }
        }
    }

    /// Apply finished jobs and post a notice for each, let the Panels tab
    /// take its background work and apply a waiting edit, then run the
    /// automatic pipeline and the checks.
    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let Some((session, jobs)) = self.session.as_mut().zip(self.jobs.as_mut()) else {
            return;
        };
        let now = Instant::now();
        let until = now + NOTICE_TIME;
        let events = jobs.poll(session);
        // An edit (panels, or global Blend settings) waiting or having
        // cancelled jobs when the events were taken: jobs it cancelled are
        // restarted by the pipeline, not given up.
        let editing = self.panels.has_pending() || self.blend.settings_editing(session);
        for event in &events {
            self.blend.on_event(event);
            let name = |id: &u64| jobs.label(*id).unwrap_or("Job").to_string();
            let (text, color) = match event {
                // Previews run by themselves and often: no notices but
                // failures.
                JobEvent::Preview(_) => continue,
                JobEvent::Finished { label, .. } if label == PREVIEWS_LABEL => continue,
                JobEvent::Cancelled { id } if name(id) == PREVIEWS_LABEL => continue,
                JobEvent::Finished { label, .. } => {
                    (format!("{label} finished"), palette::GREEN_300)
                }
                JobEvent::Cancelled { id } => {
                    (format!("{} cancelled", name(id)), palette::TEXT_BODY)
                }
                JobEvent::Failed { message, .. } => (message.clone(), palette::HA_500),
            };
            self.notices.push(Notice { text, color, until });
        }
        self.panels.poll(ctx, session, jobs);
        self.blend.poll_settings(ctx, session, jobs);
        let editing = editing || self.panels.has_pending() || self.blend.settings_editing(session);
        self.pipeline
            .poll(ctx, now, session, jobs, &events, editing);
        if let Some(id) = self.pipeline.take_started_analysis() {
            self.blend.track_analysis(id);
        }
        self.checks.poll(ctx, session, jobs, &events);
        self.plan.release_if_hidden(now);
    }

    /// The open session (the Panels, Plan and Blend tabs build on this).
    fn session_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(session) = &self.session else { return };
        let mut toggled = None;
        egui::Panel::top("session-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(session.name())
                        .size(16.0)
                        .color(palette::TEXT_STRONG),
                );
                ui.label(
                    RichText::new(session.mosaic.dir.display().to_string())
                        .small()
                        .color(palette::TEXT_MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Open another…").clicked() {
                        self.show_start = true;
                    }
                    let mut auto = auto_analyze(&session.mosaic.config);
                    if ui
                        .checkbox(&mut auto, "Auto-analyze")
                        .on_hover_text(
                            "Analyze by itself when the panels, grouping or settings change",
                        )
                        .changed()
                    {
                        toggled = Some(auto);
                    }
                });
            });
            // Opening another session reports here too: the start window
            // may be closed meanwhile.
            if let Some(p) = &self.pending {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!(
                            "{} {}…",
                            verb(&p.request),
                            p.request.path().display()
                        ))
                        .color(palette::TEXT_BODY),
                    );
                });
            }
            if let Some(e) = self.start.error().map(str::to_string) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(e).color(palette::HA_500));
                    if ui.small_button("Dismiss").clicked() {
                        self.start.set_error(None);
                    }
                });
            }
        });
        let views = self.jobs.as_ref().map(JobRunner::views).unwrap_or_default();
        egui::Panel::bottom("status-bar").show(ui, |ui| {
            status::show(
                ui,
                &views,
                self.checks.counts(&session.mosaic.config),
                // Plan reads the frame as soon as previews derive it.
                session.canvas().or(self.plan.frame_canvas()),
                session.group_count(),
                self.sys.latest(),
            );
        });
        if let Some(on) = toggled
            && let Some(session) = self.session.as_mut()
            && let Err(message) = set_auto_analyze(session, on)
        {
            tracing::warn!("{message}");
            self.notices.push(Notice {
                text: message,
                color: palette::HA_500,
                until: Instant::now() + NOTICE_TIME,
            });
        }
        egui::Panel::top("session-tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, tab.label());
                }
            });
        });
        match (self.tab, self.session.as_mut(), self.jobs.as_mut()) {
            (Tab::Blend, Some(session), Some(jobs)) => self.blend.ui(ui, session, jobs),
            (Tab::Panels, Some(session), Some(jobs)) => self.panels.ui(ui, session, jobs),
            (Tab::Plan, Some(session), Some(_)) => {
                let now = Instant::now();
                let feed = PlanFeed {
                    previews: self.pipeline.previews(),
                    generation: self.pipeline.generation(),
                    complete: self.pipeline.previews_complete(),
                    previews_busy: self.pipeline.step() == Some(Step::Previews)
                        || self.pipeline.waiting(now),
                    checks: self.checks.report(),
                };
                let out = self.plan.ui(ui, session, &feed);
                if out.resume_previews {
                    self.pipeline.request_previews();
                }
                for check in &out.accept {
                    if let Err(message) = self.checks.accept(session, check) {
                        tracing::warn!("{message}");
                        self.notices.push(Notice {
                            text: message,
                            color: palette::HA_500,
                            until: now + NOTICE_TIME,
                        });
                    }
                }
            }
            _ => {}
        }
        self.notices_ui(&ctx);
        let mut open = self.show_start;
        let mut action = None;
        egui::Window::new("Open another session")
            .open(&mut open)
            .collapsible(false)
            .default_width(680.0)
            .show(&ctx, |ui| action = self.start.ui(ui));
        self.show_start = open;
        if let Some(action) = action {
            self.request(to_request(action), &ctx);
        }
    }
}

impl Shell {
    /// Draw the live notices (newest at the bottom) above the status bar.
    fn notices_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.notices.retain(|n| n.until > now);
        let Some(next) = self.notices.iter().map(|n| n.until).min() else {
            return;
        };
        ctx.request_repaint_after(next - now);
        egui::Area::new(egui::Id::new("job-notices"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0))
            .show(ctx, |ui| {
                for n in &self.notices {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(&n.text).color(n.color));
                    });
                }
            });
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
/// Frames rendered with the session open, at least.
const SELFTEST_SESSION_FRAMES: u32 = 10;
/// Give up on a self-test after this long.
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Exit outright if no frame follows the timeout within this long.
const SELFTEST_BACKSTOP: Duration = Duration::from_secs(10);

/// `--selftest` state: render a fixed number of frames, then close.
struct SelfTest {
    frames: u32,
    session_frames: u32,
    session: Option<PathBuf>,
    session_requested: bool,
    timed_out: Arc<AtomicBool>,
    outcome: SelfTestOutcome,
    done: bool,
    /// Where the session part of the test is.
    phase: Phase,
}

/// The self-test's walk through the session's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Waiting for the Blend tab to settle.
    Blend,
    /// Waiting for the Blend tab's first composition to settle.
    BlendComposition,
    /// Waiting for the Plan tab's first view to settle.
    Plan,
    /// Waiting for the Plan tab's first composition to settle.
    PlanComposition,
    /// Every tab visited.
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
            if !self.session_requested && self.frames >= SELFTEST_START_FRAMES {
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

    /// Visit Blend, then Plan (the first group, then the first
    /// composition, if any), each once it has settled.
    fn advance(&mut self, shell: &mut Shell) {
        if self.phase == Phase::Blend && shell.tab() != Tab::Blend {
            // A session opens on Plan (or Panels); the walk starts at Blend.
            shell.set_tab(Tab::Blend);
        }
        match self.phase {
            Phase::Blend
                if self.session_frames >= SELFTEST_SESSION_FRAMES
                    && shell.tab() == Tab::Blend
                    && shell.blend().is_settled() =>
            {
                log_blend_tab(shell.blend());
                if has_composition(shell) {
                    shell.blend_mut().select_view(ViewKind::Composition(0));
                    self.phase = Phase::BlendComposition;
                } else {
                    shell.set_tab(Tab::Plan);
                    self.phase = Phase::Plan;
                }
            }
            Phase::BlendComposition
                if matches!(shell.blend().view(), Some(ViewKind::Composition(_)))
                    && shell.blend().is_settled() =>
            {
                log_blend_tab(shell.blend());
                shell.set_tab(Tab::Plan);
                self.phase = Phase::Plan;
            }
            Phase::Plan if shell.tab() == Tab::Plan && shell.plan().is_settled() => {
                log_plan_tab(shell);
                self.phase = if has_composition(shell) {
                    shell.plan_mut().select_view(ViewKind::Composition(0));
                    Phase::PlanComposition
                } else {
                    Phase::Done
                };
            }
            Phase::PlanComposition
                if matches!(shell.plan().view(), Some(ViewKind::Composition(_)))
                    && shell.plan().is_settled() =>
            {
                log_plan_tab(shell);
                self.phase = Phase::Done;
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

/// Whether the open session has a composition.
fn has_composition(shell: &Shell) -> bool {
    shell
        .session()
        .is_some_and(|s| !load_compositions(&s.mosaic.config.ui).is_empty())
}

/// Log what the self-test's Blend tab shows.
fn log_blend_tab(blend: &BlendTab) {
    tracing::info!(
        "self-test: Blend tab view {:?}: {}; {} ROIs",
        blend.view(),
        blend
            .shown_text()
            .unwrap_or_else(|| "viewer shown".to_string()),
        blend.rois().len()
    );
    let group = blend.group().unwrap_or("(none)");
    match (blend.source(), blend.absent()) {
        (Some(src), _) => {
            let g = src.tile_geometry();
            let stretch = src.stretch();
            tracing::info!(
                "self-test: Blend tab shows group {group}: {} × {} × {} result, display stretch {stretch:?}",
                g.width,
                g.height,
                g.channels
            );
        }
        (None, why) => tracing::info!(
            "self-test: Blend tab, group {group}: no result ({})",
            why.unwrap_or("none")
        ),
    }
    match blend.health() {
        Some(Ok(h)) => tracing::info!(
            "self-test: health: seam step median {:.3e}, max {:.3e}, {} of {} edges flagged",
            h.seam_delta_median,
            h.seam_delta_max,
            h.seams.iter().filter(|s| s.flagged).count(),
            h.seams.len()
        ),
        Some(Err(e)) => tracing::info!("self-test: health unavailable: {e}"),
        None => tracing::info!("self-test: no health (group not analyzed)"),
    }
}

/// Log what the self-test's Plan tab shows.
fn log_plan_tab(shell: &Shell) {
    let plan = shell.plan();
    let items = plan.items();
    let textured = items.iter().filter(|i| !i.layers.is_empty()).count();
    let layers: usize = items.iter().map(|i| i.layers.len()).sum();
    tracing::info!(
        "self-test: Plan tab shows {:?}: {} panels placed, {textured} with previews ({layers} layers), \
         frame {:?}, display stretch {:?}",
        plan.view(),
        items.len(),
        plan.frame_canvas(),
        plan.stretch()
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
            _ => Vec::new(),
        };
        let shell = Shell::new(StartScreen::new(recent, Arc::new(RfdPicker)));
        let selftest = selftest.map(|outcome| SelfTest {
            frames: 0,
            session_frames: 0,
            session: initial.clone(),
            session_requested: false,
            timed_out: selftest_watchdog(&cc.egui_ctx, SELFTEST_TIMEOUT, Some(SELFTEST_BACKSTOP)),
            outcome,
            done: false,
            phase: Phase::Blend,
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
        self.shell.flush_pending_saves();
        if self.selftest.is_none() {
            eframe::set_value(storage, RECENT_KEY, &self.shell.start.recent());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // `save` runs first only with persistence storage available.
        self.shell.flush_pending_saves();
    }

    fn persist_egui_memory(&self) -> bool {
        self.selftest.is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::start::tests::{StubPicker, relative_to_cwd, step_until, temp_dir};

    const CANVAS_W: u64 = crate::testutil::CANVAS.0;

    fn harness(picker: StubPicker) -> Harness<'static, Shell> {
        let shell = Shell::new(StartScreen::new(Vec::new(), Arc::new(picker)));
        Harness::builder()
            .with_size(egui::vec2(1000.0, 800.0))
            .build_ui_state(|ui, s: &mut Shell| s.ui(ui), shell)
    }

    fn type_into(h: &mut Harness<'static, Shell>, field: &str, text: &str) {
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, field)
            .focus();
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, field)
            .type_text(text);
        h.step();
    }

    #[test]
    fn new_mosaic_creates_session() {
        let parent = temp_dir("new");
        let mut h = harness(StubPicker(Some(parent.clone())));
        h.run();
        h.get_by_label("New mosaic…").click();
        // The (stub) dialog answers on a background thread.
        step_until(&mut h, |s| {
            s.start.new_parent() == Some(parent.display().to_string().as_str())
        });
        type_into(&mut h, "Name", "Orion");
        h.get_by_label("Create").click();
        step_until(&mut h, |s| s.session().is_some());
        let dir = parent.join("Orion.mmm-session");
        assert_eq!(h.state().session().unwrap().mosaic.dir, dir);
        assert!(dir.join("mosaic.json").is_file());
        assert_eq!(h.state().start.recent()[0].path, dir);
        // The form is cleared for the next New mosaic….
        assert_eq!(h.state().start.new_parent(), None);
        // A session without panels opens on the Panels tab.
        assert_eq!(h.state().tab(), Tab::Panels);
        let _ = std::fs::remove_dir_all(&parent);
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
        let root = temp_dir("shell-relative");
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

        // Opening it again (relative) from this window is refused, and the
        // error shows in the session bar, not only in the start window.
        h.get_by_label("Open another…").click();
        h.run();
        type_into(&mut h, "Session folder", "");
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| !s.is_busy());
        h.run();
        let shown = h
            .get_all_by_label_contains("already open in this window")
            .count();
        assert!(shown >= 1);
        h.get_by_label("Dismiss");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn jobs_show_in_status_bar_and_notices() {
        let root = temp_dir("shell-jobs");
        let dir = crate::testutil::two_group_session(&root).mosaic.dir;
        let mut h = harness(StubPicker(None));
        h.run();
        type_into(&mut h, "Session folder", dir.to_str().unwrap());
        h.key_press(egui::Key::Enter);
        h.step();
        step_until(&mut h, |s| s.session().is_some());
        h.get_by_label("2 groups");
        h.get_by_label("Auto-analyze");
        // The pipeline previews the panels, then analyses them by itself.
        step_until(&mut h, |s| {
            s.session().unwrap().mosaic.config.group_dirs.len() == 2
                && !s.jobs.as_ref().unwrap().busy()
        });
        h.run();
        assert_eq!(h.state().pipeline().previews().len(), 8);
        h.get_by_label("Analyze finished");
        h.get_by_label("Ready");
        h.get_by_label("Canvas 600 × 420 (0.3 MP)");
        h.get_by_label("2 groups");
        // The checks (unsolved registered panels: a note each).
        step_until(&mut h, |s| {
            s.checks().report().is_some() && !s.checks().running()
        });
        h.run();
        h.get_by_label("Checks:");
        h.get_by_label("0 problems");
        h.get_by_label("8 notes");
        h.get_by_label("0 passed");
        // A session with panels opens on the Plan tab.
        assert_eq!(h.state().tab(), Tab::Plan);
        // The Blend tab shows the first group, Ha; it was never blended.
        h.get_by_label("Blend").click();
        h.step();
        step_until(&mut h, |s| s.tab() == Tab::Blend);
        assert_eq!(h.state().blend().group(), Some("Ha"));
        step_until(&mut h, |s| s.blend().absent().is_some());
        h.get_by_label("Not blended yet.");
        step_until(&mut h, |s| s.blend().health().is_some());
        assert!(matches!(h.state().blend().health(), Some(Ok(_))));
        h.get_by_label("Seam step median");
        h.get_by_label("Blend whole mosaic").click();
        h.step();
        step_until(&mut h, |s| !s.jobs.as_ref().unwrap().busy());
        h.run();
        h.get_by_label("Blend Ha finished");
        h.get_by_label("Blend OIII finished");
        // The result opens with an automatic display stretch, and the
        // viewer settles with its tiles loaded.
        step_until(&mut h, |s| {
            s.blend().source().is_some() && s.blend().is_settled()
        });
        let src = h.state().blend().source().unwrap().clone();
        assert_eq!(src.tile_geometry().width, CANVAS_W);
        assert_ne!(
            src.stretch()[0],
            crate::tile_source::LINEAR,
            "auto stretch applied"
        );
        h.get_by_label("Automatic");
        // The Panels tab shows the panels in their groups.
        h.get_by_label("Panels").click();
        h.step();
        step_until(&mut h, |s| s.tab() == Tab::Panels);
        h.get_by_label("Ha_0.xisf");
        let deadline = Instant::now() + Duration::from_secs(10);
        while h.query_by_label("OIII").is_none() {
            assert!(Instant::now() < deadline, "no OIII column");
            h.step();
        }
        // Plan shows the first group's previews in place.
        h.get_by_label("Plan").click();
        h.step();
        step_until(&mut h, |s| s.tab() == Tab::Plan && s.plan().is_settled());
        assert_eq!(h.state().plan().items().len(), 4);
        h.get_by_label("View");
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
        // "Blend whole mosaic" blends every group.
        assert_eq!(actions, ["analyze", "blend", "blend"]);
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
        shell.blend.touch_save();
        shell.flush_pending_saves();
        let back = mmm_core::mosaic::MosaicConfig::load(&dir).unwrap();
        assert_eq!(back.ui["marker"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watchdog_fires_after_deadline() {
        let ctx = egui::Context::default();
        let fired = selftest_watchdog(&ctx, std::time::Duration::from_millis(10), None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !fired.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
