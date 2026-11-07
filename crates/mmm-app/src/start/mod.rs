//! Start screen (spec §5): the most recently opened available session as
//! a full-window hero, its details and **Resume session** on the left, a
//! shelf of recent sessions along the bottom, an "All sessions" grid with
//! a search, and a first-run screen. A typed path (behind "More") always
//! works, even where no file dialog is available (e.g. WSLg without a
//! desktop portal).
//!
//! The screen never opens a session, and the UI thread does no file I/O:
//! card facts ([`FactsLoader`]), the hero's views
//! ([`hero_views::build_hero`]), card thumbnails, notes saves and the
//! reduced-motion probe run on background threads that this module polls
//! every frame. Load order (spec §5.4):
//! the hero candidate's facts first, its views as soon as they arrive,
//! and card thumbnails only once the hero has drawn ([`hero`]). Notes are
//! saved a pause after typing ([`notes`]); drawing is in [`layout`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use mega_ui::brand::{self, BrandImages};
use mega_ui::hero::HeroStage;
use mega_ui::theme::Tone;

use crate::session::{SESSION_EXT, session_name};

mod facts;
mod hero;
mod hero_views;
mod layout;
mod notes;
mod recent;

use facts::{Availability, CardFacts, CardState, FactsLoader, LoadFacts};
use hero::{HeroBuild, Shown, Thumb, ThumbResults};
use notes::{NotesEditor, NotesSave};
pub use recent::{Library, RecentEntry};

/// Seconds after the screen starts during which a session whose facts
/// haven't arrived may still be the default hero; after that, one whose
/// folder doesn't answer (e.g. on a dead network share) gives way to the
/// most recent available one.
const FACTS_WAIT_S: f64 = 1.0;
/// Status and hero line of a session that cannot be read.
const UNREADABLE: &str = "Can't read this session";

/// What the user asked the start screen for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartAction {
    /// Open the session folder.
    Open(PathBuf),
    /// Create a new session in this (not yet existing) folder.
    Create(PathBuf),
}

/// The native dialogs the start screen uses. Called on a background
/// thread.
pub trait SessionDialogs: Send + Sync {
    /// Show a folder dialog titled `title`; `None` when cancelled or when
    /// no dialog is available.
    fn pick_folder(&self, title: &str) -> Option<PathBuf>;

    /// Show the save dialog of a new mosaic: the path chosen (the folder
    /// to create, with or without the `.mmm-session` extension); `None`
    /// when cancelled or when no dialog is available.
    fn pick_new_session(&self) -> Option<PathBuf>;
}

/// The native dialogs through `rfd`.
pub struct RfdDialogs;

impl SessionDialogs for RfdDialogs {
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    fn pick_new_session(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("New mosaic")
            .set_file_name("Untitled")
            .save_file()
    }
}

/// What an open file dialog is for.
#[derive(Clone, PartialEq, Eq)]
enum DialogPurpose {
    Open,
    /// The folder of a new session.
    NewSession,
    /// The new folder of the missing session listed at this path.
    Locate(PathBuf),
}

/// A file dialog running on a background thread.
struct Dialog {
    purpose: DialogPurpose,
    rx: Receiver<Option<PathBuf>>,
}

/// A facts loader and how many results it still owes.
struct Loading {
    loader: FactsLoader,
    left: usize,
}

/// What the user did while the screen was drawn, applied afterwards.
enum Cmd {
    Select(PathBuf),
    Open(PathBuf),
    Create(PathBuf),
    TogglePin(PathBuf),
    Remove(PathBuf),
    Dialog(DialogPurpose),
    NewMosaic,
    TypePath,
    ShowAll(bool),
}

/// What drawing one frame collected.
#[derive(Default)]
struct Drawn {
    cmds: Vec<Cmd>,
    /// Cards on screen, which want thumbnails.
    thumbs: Vec<PathBuf>,
}

/// The start screen's state.
pub struct StartScreen {
    dialogs: Arc<dyn SessionDialogs>,
    /// How sessions' facts are read in tests (else [`facts::load_facts`]).
    load: Option<LoadFacts>,
    library: Library,
    facts: HashMap<PathBuf, CardFacts>,
    loading: Vec<Loading>,
    /// The first frame started the facts loader and the motion probe.
    started: bool,
    /// Sessions whose facts must be read again.
    reload: Vec<PathBuf>,
    /// The card the user clicked (else the default hero is shown).
    selected: Option<PathBuf>,
    /// When the screen first drew (egui time).
    started_at: Option<f64>,
    /// The time of the frame being drawn (egui time).
    clock: f64,
    stage: HeroStage,
    shown: Option<Shown>,
    hero_build: Option<HeroBuild>,
    thumbs: HashMap<PathBuf, Thumb>,
    thumb_rx: Option<ThumbResults>,
    notes: NotesEditor,
    saves: Vec<NotesSave>,
    /// Saves waiting for an earlier save of the same session.
    queued_saves: Vec<(PathBuf, String)>,
    /// An open waiting for that session's notes to be saved first.
    held: Option<StartAction>,
    motion: Option<Receiver<bool>>,
    all_sessions: bool,
    search: String,
    typed_open: bool,
    /// The typed-path row is shown because a dialog gave no answer.
    no_dialog: bool,
    focus_typed: bool,
    /// Layout memory: where the session block's contents ended.
    block_bottom: f32,
    /// Layout memory: the height of the session block's contents.
    block_height: f32,
    /// Layout memory: the first-run button row's width.
    first_run_row: f32,
    typed_path: String,
    dialog: Option<Dialog>,
    busy: Option<String>,
    error: Option<String>,
    notice: Option<String>,
    /// An open or create chosen in a dialog, taken by the next frame.
    pending_open: Option<StartAction>,
    brand: Option<BrandImages>,
}

impl StartScreen {
    /// A start screen listing `recent` (most recent first), with `dialogs`
    /// for the file dialogs.
    pub fn new(recent: Vec<RecentEntry>, dialogs: Arc<dyn SessionDialogs>) -> Self {
        let loader_threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(2, 4);
        StartScreen {
            dialogs,
            load: None,
            library: Library::new(recent),
            facts: HashMap::new(),
            loading: Vec::new(),
            started: false,
            reload: Vec::new(),
            selected: None,
            started_at: None,
            clock: 0.0,
            stage: HeroStage::new(loader_threads),
            shown: None,
            hero_build: None,
            thumbs: HashMap::new(),
            thumb_rx: None,
            notes: NotesEditor::default(),
            saves: Vec::new(),
            queued_saves: Vec::new(),
            held: None,
            motion: None,
            all_sessions: false,
            search: String::new(),
            typed_open: false,
            no_dialog: false,
            focus_typed: false,
            block_bottom: 0.0,
            block_height: 0.0,
            first_run_row: 0.0,
            typed_path: String::new(),
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
    /// (seconds since the Unix epoch). The hero lets go of its tiles (the
    /// session now open may write them), and the session's facts are read
    /// again: until they say otherwise, it is not known to be available.
    pub fn record_opened(&mut self, path: &Path, now: u64) {
        self.library.record_opened(path, now);
        self.facts.remove(path);
        self.reload.push(path.to_path_buf());
        self.selected = None;
        self.drop_hero();
        self.stop_thumbnails();
    }

    /// True while recent sessions' facts are still loading.
    #[cfg(test)]
    pub fn checking_recent(&self) -> bool {
        !self.started || !self.loading.is_empty() || !self.reload.is_empty()
    }

    /// Show `label` with a spinner (or nothing, with `None`); actions are
    /// disabled meanwhile.
    pub fn set_busy(&mut self, label: Option<String>) {
        self.busy = label;
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
        let now = ui.input(|i| i.time);
        self.clock = now;
        self.started_at.get_or_insert(now);
        // Polled before anything starts: the first pass draws without facts.
        self.poll_background(&ctx);
        self.start_background(&ctx);
        if self.brand.is_none() {
            self.brand = Some(BrandImages::load(
                &ctx,
                brand::MEGA_MERGE_MOSAIC_PNG,
                "Mega Merge Mosaic",
            ));
        }
        let target = self.target();
        if let Some(wait) = self.facts_wait_left() {
            // The default hero may change then, with no input to wake us.
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
        self.update_hero(&ctx, target.as_deref(), now);
        self.sync_notes(&ctx, target.as_deref());

        let mut drawn = Drawn::default();
        self.draw(ui, target.as_deref(), now, &mut drawn);

        // A folder picked in the Open… or New mosaic dialog.
        let mut action = self.pending_open.take();
        for cmd in drawn.cmds {
            if let Some(a) = self.apply(&ctx, cmd) {
                action = Some(a);
            }
        }
        // Opening a session whose notes are being saved waits for the save:
        // both take the session lock. A newer request replaces a waiting one.
        if action.is_some() {
            self.held = None;
        }
        if let Some(StartAction::Open(path)) = &action
            && self.notes_pending(path)
        {
            self.notes.save_now = true;
            self.held = action.take();
        }
        self.save_notes_when_due(&ctx, now);
        if action.is_none()
            && let Some(StartAction::Open(path)) = &self.held
            && !self.notes_pending(path)
        {
            action = self.held.take();
        }
        self.request_thumbnails(&ctx, &drawn.thumbs, target.as_deref(), now);
        if action.is_some() {
            self.notice = None;
            self.error = None;
        }
        action
    }

    /// The session the hero and the block show: the selected card, else
    /// the default hero.
    fn target(&self) -> Option<PathBuf> {
        let listed = |p: &PathBuf| self.library.entries().iter().any(|e| e.path == *p);
        match &self.selected {
            Some(s) if listed(s) => Some(s.clone()),
            _ => self.default_hero(),
        }
    }

    /// The most recently opened session whose facts show it available. For
    /// [`FACTS_WAIT_S`] after the screen starts, one whose facts haven't
    /// arrived may be it too (they usually come at once); after that it
    /// gives way, so a folder that never answers is never the hero while
    /// another session is available. With none available, the most recent
    /// session whose facts are pending, else the most recent one, so the
    /// block can say why.
    fn default_hero(&self) -> Option<PathBuf> {
        let by_recency = self.library.by_recency();
        // Some(available), or None while the facts are pending.
        let available = |e: &RecentEntry| {
            self.facts
                .get(&e.path)
                .map(|f| f.availability == Availability::Available)
        };
        let pick = if self.facts_wait_left().is_some() {
            by_recency.iter().find(|e| available(e) != Some(false))
        } else {
            by_recency
                .iter()
                .find(|e| available(e) == Some(true))
                .or_else(|| by_recency.iter().find(|e| available(e).is_none()))
        };
        pick.or(by_recency.first()).map(|e| e.path.clone())
    }

    /// While sessions whose facts are pending may still be the default hero
    /// (see [`FACTS_WAIT_S`]), the seconds left; `None` once that is over or
    /// when every session's facts are in.
    fn facts_wait_left(&self) -> Option<f64> {
        let left = FACTS_WAIT_S - (self.clock - self.started_at?);
        let pending = self
            .library
            .entries()
            .iter()
            .any(|e| !self.facts.contains_key(&e.path));
        (left > 0.0 && pending).then_some(left)
    }

    /// The display name of `path`'s session.
    fn name_of(&self, path: &Path) -> String {
        self.facts
            .get(path)
            .map_or_else(|| session_name(path), |f| f.name.clone())
    }

    /// Whether actions are allowed (no open under way, no dialog up).
    fn enabled(&self) -> bool {
        self.busy.is_none() && self.dialog.is_none()
    }

    /// Apply one thing the user did; an action for the app, if it is one.
    fn apply(&mut self, ctx: &egui::Context, cmd: Cmd) -> Option<StartAction> {
        ctx.request_repaint();
        match cmd {
            Cmd::Select(path) => self.selected = Some(path),
            Cmd::Open(path) => return Some(StartAction::Open(path)),
            Cmd::Create(path) => return Some(StartAction::Create(with_session_ext(path))),
            Cmd::TogglePin(path) => {
                let pinned = self
                    .library
                    .entries()
                    .iter()
                    .any(|e| e.path == path && e.pinned);
                self.library.set_pinned(&path, !pinned);
            }
            Cmd::Remove(path) => {
                self.library.remove(&path);
                self.facts.remove(&path);
                self.thumbs.remove(&path);
                if self.selected.as_ref() == Some(&path) {
                    self.selected = None;
                }
            }
            Cmd::Dialog(purpose) => self.start_dialog(ctx, purpose),
            Cmd::NewMosaic => self.start_dialog(ctx, DialogPurpose::NewSession),
            Cmd::TypePath => {
                self.typed_open = true;
                self.no_dialog = false;
                self.focus_typed = true;
            }
            Cmd::ShowAll(on) => self.all_sessions = on,
        }
        None
    }

    /// On the first frame, start loading every session's facts (the hero
    /// candidate first) and probing reduced motion; later, reload facts
    /// asked for.
    fn start_background(&mut self, ctx: &egui::Context) {
        if !self.started {
            self.started = true;
            self.reload.clear();
            let entries: Vec<RecentEntry> =
                self.library.by_recency().into_iter().cloned().collect();
            let first = entries.first().map(|e| e.path.clone());
            self.spawn_facts(entries, first, ctx);
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(mega_ui::motion::reduced_motion());
                ctx.request_repaint();
            });
            self.motion = Some(rx);
        } else if !self.reload.is_empty() {
            let paths = std::mem::take(&mut self.reload);
            let entries = self
                .library
                .entries()
                .iter()
                .filter(|e| paths.contains(&e.path))
                .cloned()
                .collect();
            self.spawn_facts(entries, None, ctx);
        }
    }

    fn spawn_facts(
        &mut self,
        entries: Vec<RecentEntry>,
        first: Option<PathBuf>,
        ctx: &egui::Context,
    ) {
        if entries.is_empty() {
            return;
        }
        let left = entries.len();
        let loader = match self.load {
            Some(load) => FactsLoader::spawn_with(entries, first, ctx, load),
            None => FactsLoader::spawn(entries, first, ctx),
        };
        self.loading.push(Loading { loader, left });
    }

    /// Collect finished background work.
    fn poll_background(&mut self, ctx: &egui::Context) {
        let mut arrived = Vec::new();
        for l in &mut self.loading {
            let got = l.loader.poll();
            l.left = l.left.saturating_sub(got.len());
            arrived.extend(got);
        }
        self.loading.retain(|l| l.left > 0);
        for f in arrived {
            self.facts_arrived(f);
        }
        if let Some(rx) = &self.motion {
            match rx.try_recv() {
                Ok(on) => {
                    self.stage.set_reduced_motion(on);
                    self.motion = None;
                }
                Err(TryRecvError::Disconnected) => self.motion = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        self.poll_thumbnails(ctx);
        self.poll_saves(ctx);
        if let Some(dialog) = &self.dialog {
            let picked = match dialog.rx.try_recv() {
                Ok(picked) => Some(Ok(picked)),
                Err(TryRecvError::Disconnected) => Some(Err(())),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(picked) = picked {
                let purpose = dialog.purpose.clone();
                self.dialog = None;
                self.dialog_done(purpose, picked);
            }
        }
    }

    fn facts_arrived(&mut self, f: CardFacts) {
        // Forgotten while loading.
        if !self.library.entries().iter().any(|e| e.path == f.path) {
            return;
        }
        self.hero_outdated(&f.path);
        self.facts.insert(f.path.clone(), f);
    }

    fn dialog_done(&mut self, purpose: DialogPurpose, picked: Result<Option<PathBuf>, ()>) {
        match (purpose, picked) {
            (DialogPurpose::Open, Ok(Some(path))) => {
                // Shown in the typed-path field too, for retrying.
                self.typed_path = path.display().to_string();
                self.no_dialog = false;
                self.notice = None;
                self.pending_open = Some(StartAction::Open(path));
            }
            (DialogPurpose::NewSession, Ok(Some(path))) => {
                self.notice = None;
                self.no_dialog = false;
                self.pending_open = Some(StartAction::Create(with_session_ext(path)));
            }
            (DialogPurpose::Locate(old), Ok(Some(new))) => {
                self.library.relocate(&old, &new);
                for p in [&old, &new] {
                    self.facts.remove(p);
                    self.thumbs.remove(p);
                }
                self.reload.push(new.clone());
                if self.selected.as_ref() == Some(&old) {
                    self.selected = Some(new);
                }
                self.notice = None;
            }
            // Cancelled, or no dialog here: the typed path always works.
            (_, Ok(None) | Err(())) => {
                self.notice = None;
                self.typed_open = true;
                self.no_dialog = true;
                self.focus_typed = true;
            }
        }
    }

    fn start_dialog(&mut self, ctx: &egui::Context, purpose: DialogPurpose) {
        let title = match purpose {
            DialogPurpose::Open => Some("Open a mosaic session (.mmm-session folder)"),
            DialogPurpose::NewSession => None,
            DialogPurpose::Locate(_) => Some("Locate the session (.mmm-session folder)"),
        };
        let (tx, rx) = mpsc::channel();
        let dialogs = Arc::clone(&self.dialogs);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = match title {
                Some(title) => dialogs.pick_folder(title),
                None => dialogs.pick_new_session(),
            };
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.notice = None;
        self.dialog = Some(Dialog { purpose, rx });
    }
}

/// Test accessors.
#[cfg(test)]
impl StartScreen {
    /// Read sessions' facts with `load` instead of [`facts::load_facts`].
    pub(crate) fn with_facts_loader(mut self, load: LoadFacts) -> Self {
        self.load = Some(load);
        self
    }

    /// How many sessions' facts have arrived.
    pub(crate) fn facts_known(&self) -> usize {
        self.facts.len()
    }

    /// The session the hero and the session block show.
    pub(crate) fn hero(&self) -> Option<PathBuf> {
        self.target()
    }

    /// The status line and dimming of `path`'s card.
    pub(crate) fn card_look(&self, path: &Path) -> (Option<(Tone, String)>, bool) {
        card_look(self.facts.get(path))
    }
}

/// The status line of a session: its availability, else its state
/// (`None` when nothing is known).
fn status_line(f: &CardFacts) -> Option<(Tone, &'static str)> {
    Some(match &f.availability {
        Availability::Missing => (Tone::Problem, "Missing"),
        Availability::Busy => (Tone::Neutral, "Open in another window"),
        Availability::Unreadable(_) => (Tone::Problem, UNREADABLE),
        Availability::Available => match f.state {
            CardState::Merged => (Tone::Success, "Merged"),
            CardState::MergedExported => (Tone::Success, "Merged · exported"),
            CardState::ReMergeNeeded => (
                Tone::Warning,
                "Panels changed since the last merge — re-merge needed",
            ),
            CardState::PlanReady => (Tone::Info, "Plan ready"),
            CardState::NoPanels => (Tone::Neutral, "No panels yet"),
            CardState::Unknown => return None,
        },
    })
}

/// A card's status (the block's, shortened to fit) and whether it is
/// dimmed (a missing session).
fn card_look(facts: Option<&CardFacts>) -> (Option<(Tone, String)>, bool) {
    let Some(f) = facts else {
        return (None, false);
    };
    let status = status_line(f).map(|(tone, label)| {
        let stale =
            f.availability == Availability::Available && f.state == CardState::ReMergeNeeded;
        let label = if stale { "Re-merge needed" } else { label };
        (tone, label.to_string())
    });
    (status, f.availability == Availability::Missing)
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

/// "1 panel", "3 panels".
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// `path` for display: without Windows' `\\?\` verbatim prefix.
fn display_path(path: &Path) -> String {
    let s = path.display().to_string();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = s.strip_prefix(r"\\?\") {
        local.to_string()
    } else {
        s
    }
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

/// `path` with the session extension appended when it lacks it.
fn with_session_ext(path: PathBuf) -> PathBuf {
    let suffix = format!(".{SESSION_EXT}");
    let named = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(&suffix));
    if named {
        return path;
    }
    let mut full = path.into_os_string();
    full.push(suffix);
    PathBuf::from(full)
}

/// The tests and the shared start-screen test helpers live in `tests.rs`.
#[cfg(test)]
pub(crate) mod tests;

/// An image snapshot of the start screen (ignored: needs a GPU).
#[cfg(test)]
mod snapshot_tests;
