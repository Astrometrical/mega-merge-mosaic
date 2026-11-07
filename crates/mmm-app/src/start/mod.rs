//! Start screen (spec §5.1): branding, **New mosaic…**, **Open…**, the
//! recent sessions, and a typed-path field that always works, even where
//! no file dialog is available (e.g. WSLg without a desktop portal).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::RichText;
use mega_ui::brand::{self, BrandImages, LockupSize};
use mega_ui::theme::palette;

use crate::session::{SESSION_EXT, session_name};

#[allow(dead_code)] // wired into the start screen in a later task
mod facts;
mod recent;

pub use recent::{Library, RecentEntry};

/// What the user asked the start screen for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartAction {
    /// Open the session folder.
    Open(PathBuf),
    /// Create a new session in this (not yet existing) folder.
    Create(PathBuf),
}

/// Picks a folder with a native dialog. Called on a background thread.
pub trait FolderPicker: Send + Sync {
    /// Show a folder dialog titled `title`; `None` when cancelled or when
    /// no dialog is available.
    fn pick_folder(&self, title: &str) -> Option<PathBuf>;
}

/// The native dialog through `rfd`.
pub struct RfdPicker;

impl FolderPicker for RfdPicker {
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }
}

/// What an open file dialog is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DialogPurpose {
    Open,
    NewParent,
}

/// A file dialog running on a background thread.
struct Dialog {
    purpose: DialogPurpose,
    rx: Receiver<Option<PathBuf>>,
}

/// The **New mosaic** form.
#[derive(Default)]
struct NewForm {
    parent: String,
    name: String,
}

/// The start screen's state.
pub struct StartScreen {
    picker: Arc<dyn FolderPicker>,
    library: Library,
    missing: HashSet<PathBuf>,
    check: Option<Receiver<HashSet<PathBuf>>>,
    checked: bool,
    typed_path: String,
    new_form: Option<NewForm>,
    dialog: Option<Dialog>,
    busy: Option<String>,
    error: Option<String>,
    notice: Option<String>,
    pending_open: Option<PathBuf>,
    brand: Option<BrandImages>,
}

impl StartScreen {
    /// A start screen listing `recent` (most recent first), with `picker`
    /// for the file dialogs.
    pub fn new(recent: Vec<RecentEntry>, picker: Arc<dyn FolderPicker>) -> Self {
        StartScreen {
            picker,
            library: Library::new(recent),
            missing: HashSet::new(),
            check: None,
            checked: false,
            typed_path: String::new(),
            new_form: None,
            dialog: None,
            busy: None,
            error: None,
            notice: None,
            pending_open: None,
            brand: None,
        }
    }

    /// The recent sessions, most recent first.
    pub fn recent(&self) -> &[RecentEntry] {
        self.library.entries()
    }

    /// Put `path` at the top of the recent sessions, opened at `now`
    /// (seconds since the Unix epoch).
    pub fn record_opened(&mut self, path: &Path, now: u64) {
        self.library.record_opened(path, now);
        self.missing.remove(path);
    }

    /// True while the background check of which recent paths exist runs.
    #[cfg(test)]
    pub fn checking_recent(&self) -> bool {
        !self.checked || self.check.is_some()
    }

    /// Show `label` with a spinner (or nothing, with `None`); actions are
    /// disabled meanwhile.
    pub fn set_busy(&mut self, label: Option<String>) {
        self.busy = label;
    }

    /// Close the New mosaic form (after a successful Create).
    pub fn clear_new_form(&mut self) {
        self.new_form = None;
    }

    /// The parent folder typed or picked in the New mosaic form, if open.
    #[cfg(test)]
    pub fn new_parent(&self) -> Option<&str> {
        self.new_form.as_ref().map(|f| f.parent.as_str())
    }

    /// The error shown, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Show an error message (or clear it, with `None`).
    pub fn set_error(&mut self, message: Option<String>) {
        self.error = message;
        if self.error.is_some() {
            self.notice = None;
        }
    }

    /// Draw the screen; returns what the user asked for, if anything.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<StartAction> {
        let ctx = ui.ctx().clone();
        self.poll_background(&ctx);
        if self.brand.is_none() {
            self.brand = Some(BrandImages::load(
                &ctx,
                brand::MEGA_MERGE_MOSAIC_PNG,
                "Mega Merge Mosaic",
            ));
        }
        // A folder picked in the Open… dialog.
        let mut action = self.pending_open.take().map(StartAction::Open);
        egui::ScrollArea::vertical().show(ui, |ui| {
            let width = ui.available_width().min(640.0);
            let pad = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(48.0);
                    self.header(ui);
                    ui.add_space(32.0);
                    let enabled = self.busy.is_none() && self.dialog.is_none();
                    ui.add_enabled_ui(enabled, |ui| {
                        if let Some(a) = self.actions(&ctx, ui) {
                            action = Some(a);
                        }
                    });
                    self.messages(ui);
                    ui.add_space(24.0);
                    if let Some(a) = self.recent_list(ui, enabled) {
                        action = Some(a);
                    }
                });
            });
        });
        if action.is_some() {
            self.notice = None;
            self.error = None;
        }
        action
    }

    /// Collect finished background work (file dialogs, the recent check).
    fn poll_background(&mut self, ctx: &egui::Context) {
        if !self.checked {
            self.checked = true;
            let paths: Vec<PathBuf> = self
                .library
                .entries()
                .iter()
                .map(|e| e.path.clone())
                .collect();
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let missing = paths.into_iter().filter(|p| !p.is_dir()).collect();
                let _ = tx.send(missing);
                ctx.request_repaint();
            });
            self.check = Some(rx);
        }
        if let Some(rx) = &self.check {
            match rx.try_recv() {
                Ok(missing) => {
                    self.missing = missing;
                    self.check = None;
                }
                Err(TryRecvError::Disconnected) => self.check = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(dialog) = &self.dialog {
            let picked = match dialog.rx.try_recv() {
                Ok(picked) => Some(Ok(picked)),
                Err(TryRecvError::Disconnected) => Some(Err(())),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(picked) = picked {
                let purpose = dialog.purpose;
                self.dialog = None;
                self.dialog_done(purpose, picked);
            }
        }
    }

    fn dialog_done(&mut self, purpose: DialogPurpose, picked: Result<Option<PathBuf>, ()>) {
        match (purpose, picked) {
            (DialogPurpose::Open, Ok(Some(path))) => {
                // Shown in the typed-path field too, for retrying.
                self.typed_path = path.display().to_string();
                self.notice = None;
                self.pending_open = Some(path);
            }
            (DialogPurpose::NewParent, Ok(Some(path))) => {
                self.new_form.get_or_insert_with(NewForm::default).parent =
                    path.display().to_string();
                self.notice = None;
            }
            (_, Ok(None)) => {
                self.notice =
                    Some("No folder chosen. If no dialog appeared, type the path instead.".into());
            }
            (_, Err(())) => {
                self.notice =
                    Some("The file dialog is unavailable here. Type the path instead.".into());
            }
        }
    }

    fn start_dialog(&mut self, ctx: &egui::Context, purpose: DialogPurpose) {
        let title = match purpose {
            DialogPurpose::Open => "Open a mosaic session (.mmm-session folder)",
            DialogPurpose::NewParent => "Choose where to create the new mosaic",
        };
        let (tx, rx) = mpsc::channel();
        let picker = Arc::clone(&self.picker);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = picker.pick_folder(title);
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.notice = None;
        self.dialog = Some(Dialog { purpose, rx });
    }

    fn header(&self, ui: &mut egui::Ui) {
        let Some(images) = &self.brand else { return };
        brand::lockup(ui, images, LockupSize::Large);
    }

    fn actions(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) -> Option<StartAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            if ui.button(RichText::new("New mosaic…").size(16.0)).clicked() {
                self.new_form.get_or_insert_with(NewForm::default);
                self.start_dialog(ctx, DialogPurpose::NewParent);
            }
            if ui.button(RichText::new("Open…").size(16.0)).clicked() {
                self.start_dialog(ctx, DialogPurpose::Open);
            }
        });
        ui.add_space(12.0);

        // Typed-path fallback: always present (Review Focus 5).
        ui.horizontal(|ui| {
            let label = ui.label(RichText::new("Session folder").color(palette::TEXT_BODY));
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.typed_path)
                        .hint_text("/path/to/Name.mmm-session")
                        .desired_width(ui.available_width() - 70.0),
                )
                .labelled_by(label.id);
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Open").clicked() || entered) && !self.typed_path.trim().is_empty() {
                action = Some(StartAction::Open(expand_home(self.typed_path.trim())));
            }
        });

        if let Some(form) = &mut self.new_form {
            let mut browse = false;
            let mut cancel = false;
            ui.add_space(16.0);
            egui::Frame::group(ui.style())
                .fill(palette::SURFACE_700)
                .show(ui, |ui| {
                    ui.label(RichText::new("New mosaic").color(palette::TEXT_STRONG));
                    egui::Grid::new("new-mosaic").num_columns(2).show(ui, |ui| {
                        let label = ui.label("Parent folder");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut form.parent).desired_width(360.0),
                            )
                            .labelled_by(label.id);
                            browse = ui.button("Browse…").clicked();
                        });
                        ui.end_row();
                        let label = ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.name)
                                .hint_text("e.g. Orion")
                                .desired_width(360.0),
                        )
                        .labelled_by(label.id);
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Create").clicked() {
                            match new_session_dir(&form.parent, &form.name) {
                                Ok(dir) => action = Some(StartAction::Create(dir)),
                                Err(e) => self.error = Some(e),
                            }
                        }
                        cancel = ui.button("Cancel").clicked();
                    });
                    ui.label(
                        RichText::new(format!(
                            "Creates NAME.{SESSION_EXT} in the parent folder; add panels \
                             with `mmm init` for now."
                        ))
                        .small()
                        .color(palette::TEXT_MUTED),
                    );
                });
            if browse {
                self.start_dialog(ctx, DialogPurpose::NewParent);
            }
            if cancel {
                self.new_form = None;
            }
        }
        action
    }

    fn messages(&self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if let Some(label) = &self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(label).color(palette::TEXT_BODY));
            });
        }
        if self.dialog.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Waiting for the file dialog…").color(palette::TEXT_MUTED));
            });
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(palette::HA_400));
        }
        if let Some(n) = &self.notice {
            ui.label(RichText::new(n).color(palette::TEXT_MUTED));
        }
    }

    fn recent_list(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<StartAction> {
        if self.library.entries().is_empty() {
            return None;
        }
        ui.label(RichText::new("Recent").color(palette::TEXT_STRONG));
        ui.separator();
        let now = unix_now();
        let mut action = None;
        let mut remove = None;
        for entry in self.library.entries().to_vec() {
            let missing = self.missing.contains(&entry.path);
            let (name_color, text_color) = if missing {
                (palette::TEXT_SUBTLE, palette::TEXT_SUBTLE)
            } else {
                (palette::ACCENT_400, palette::TEXT_MUTED)
            };
            ui.horizontal(|ui| {
                let name =
                    egui::Button::new(RichText::new(session_name(&entry.path)).color(name_color))
                        .frame(false);
                if ui.add_enabled(enabled && !missing, name).clicked() {
                    action = Some(StartAction::Open(entry.path.clone()));
                }
                if missing {
                    ui.label(RichText::new("missing").small().color(palette::HA_400));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if missing && ui.small_button("Remove").clicked() {
                        remove = Some(entry.path.clone());
                    }
                    ui.label(
                        RichText::new(ago(now, entry.last_opened))
                            .small()
                            .color(text_color),
                    );
                });
            });
            ui.label(
                RichText::new(entry.path.display().to_string())
                    .small()
                    .color(text_color),
            );
            ui.add_space(6.0);
        }
        if let Some(path) = remove {
            self.library.remove(&path);
            self.missing.remove(&path);
        }
        action
    }
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "5 minutes ago"-style age of `then` at `now` (both Unix seconds).
fn ago(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    let (n, unit) = match s {
        0..60 => return "just now".into(),
        60..3_600 => (s / 60, "minute"),
        3_600..86_400 => (s / 3_600, "hour"),
        86_400..2_592_000 => (s / 86_400, "day"),
        2_592_000..31_536_000 => (s / 2_592_000, "month"),
        _ => (s / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// Expand a leading `~` to the home directory.
fn expand_home(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => match home() {
            Some(h) => h.join(rest.trim_start_matches('/')),
            None => PathBuf::from(path),
        },
        _ => PathBuf::from(path),
    }
}

/// The folder for a new session called `name` under `parent`.
fn new_session_dir(parent: &str, name: &str) -> Result<PathBuf, String> {
    let parent = parent.trim();
    let name = name.trim();
    if parent.is_empty() {
        return Err("Choose a parent folder for the new mosaic.".into());
    }
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err("Give the new mosaic a name (no slashes).".into());
    }
    let suffix = format!(".{SESSION_EXT}");
    let file = if name.ends_with(&suffix) {
        name.to_string()
    } else {
        format!("{name}{suffix}")
    };
    Ok(expand_home(parent).join(file))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A picker that answers with a fixed folder, never a real dialog.
    pub(crate) struct StubPicker(pub Option<PathBuf>);

    impl FolderPicker for StubPicker {
        fn pick_folder(&self, _title: &str) -> Option<PathBuf> {
            self.0.clone()
        }
    }

    /// A fresh, canonical temp folder; `tag` must be unique per test.
    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        fresh_dir(&std::env::temp_dir(), tag)
    }

    /// [`temp_dir`] for [`relative_to_cwd`], in the workspace's `target`: no
    /// relative path crosses Windows drives, and the temp folder can be on
    /// another drive than the checkout (it is on GitHub's Windows runners).
    pub(crate) fn temp_dir_near_cwd(tag: &str) -> PathBuf {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp");
        fresh_dir(&target, tag)
    }

    fn fresh_dir(parent: &Path, tag: &str) -> PathBuf {
        let d = parent.join(format!("mmm-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    /// `path` (canonical, on the current directory's drive) as a path
    /// relative to the current directory.
    pub(crate) fn relative_to_cwd(path: &Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let shared = cwd
            .components()
            .zip(path.components())
            .take_while(|(a, b)| a == b)
            .count();
        let mut rel = PathBuf::new();
        for _ in cwd.components().skip(shared) {
            rel.push("..");
        }
        rel.extend(path.components().skip(shared));
        rel
    }

    /// Step `harness` until `done` holds (bounded: background threads).
    pub(crate) fn step_until<S>(harness: &mut Harness<'_, S>, done: impl Fn(&S) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(harness.state()) {
            assert!(Instant::now() < deadline, "timed out waiting for the UI");
            harness.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        harness.step();
    }

    struct Screen {
        screen: StartScreen,
        actions: Vec<StartAction>,
    }

    fn harness(recent: Vec<RecentEntry>) -> Harness<'static, Screen> {
        let state = Screen {
            screen: StartScreen::new(recent, Arc::new(StubPicker(None))),
            actions: Vec::new(),
        };
        Harness::builder()
            .with_size(egui::vec2(1000.0, 800.0))
            .build_ui_state(
                |ui, s: &mut Screen| {
                    if crate::testutil::themed(ui) {
                        s.actions.extend(s.screen.ui(ui));
                    }
                },
                state,
            )
    }

    fn entry(path: &Path) -> RecentEntry {
        RecentEntry {
            path: path.to_path_buf(),
            last_opened: 1_700_000_000,
            pinned: false,
        }
    }

    #[test]
    fn typed_path_opens_session() {
        let dir = temp_dir("typed").join("Orion.mmm-session");
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = harness(Vec::new());
        h.run();
        let field = h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Session folder");
        field.focus();
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Session folder")
            .type_text(dir.to_str().unwrap());
        h.step();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().actions, vec![StartAction::Open(dir.clone())]);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn recent_list_marks_missing() {
        let root = temp_dir("recent");
        let present = root.join("Present.mmm-session");
        std::fs::create_dir_all(&present).unwrap();
        let gone = root.join("Gone.mmm-session");
        let mut h = harness(vec![entry(&present), entry(&gone)]);
        h.step();
        step_until(&mut h, |s| !s.screen.checking_recent());
        h.run();
        h.get_by_label("Present");
        h.get_by_label("Gone");
        assert_eq!(h.query_all_by_label("missing").count(), 1);
        h.get_by_label_contains(&gone.display().to_string());
        // The missing entry is not openable; the present one is.
        h.get_by_label("Gone").click();
        h.run();
        assert!(h.state().actions.is_empty());
        h.get_by_label("Present").click();
        h.run();
        assert_eq!(h.state().actions, vec![StartAction::Open(present.clone())]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
