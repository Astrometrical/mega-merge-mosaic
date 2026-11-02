//! The application shell: the start screen until a session is open, then
//! the session view. [`Shell`] is plain egui (testable without eframe);
//! [`MmmApp`] wraps it for eframe with persistence and the self-test.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::RichText;
use mega_ui::theme::palette;

use crate::session::{OpenSession, Pending, Request, open_in_new_process};
use crate::start::{RecentEntry, RfdPicker, StartAction, StartScreen, unix_now};

/// eframe storage key of the recent sessions.
const RECENT_KEY: &str = "recent_sessions";

/// The window contents: start screen or open session.
pub struct Shell {
    /// The start screen (also shown over an open session to open another).
    pub start: StartScreen,
    session: Option<OpenSession>,
    pending: Option<Pending>,
    show_start: bool,
}

impl Shell {
    /// A shell showing `start`.
    pub fn new(start: StartScreen) -> Self {
        Shell {
            start,
            session: None,
            pending: None,
            show_start: false,
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
        self.pending = Some(Pending::spawn(request, ctx));
    }

    /// Draw one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll();
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
    fn poll(&mut self) {
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
                    self.session = Some(opened);
                }
            }
            Err(message) => {
                tracing::warn!("{message}");
                self.start.set_error(Some(message));
            }
        }
    }

    /// The open session (the Panels, Plan and Blend tabs build on this).
    fn session_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(session) = &self.session else { return };
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
        egui::CentralPanel::default().show(ui, |ui| {
            let n = session.mosaic.config.panels.len();
            ui.label(
                RichText::new(format!("{n} panel{}", if n == 1 { "" } else { "s" }))
                    .color(palette::TEXT_BODY),
            );
        });
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
            Some(_) => self.session_frames >= SELFTEST_SESSION_FRAMES,
        };
        if self.frames >= SELFTEST_FRAMES && session_done {
            return self.finish(ctx, Ok(()));
        }
        ctx.request_repaint();
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
        if self.selftest.is_none() {
            eframe::set_value(storage, RECENT_KEY, &self.shell.start.recent());
        }
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
