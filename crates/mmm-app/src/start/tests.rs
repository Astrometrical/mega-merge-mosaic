//! Tests of the start screen, on synthetic sessions in temp folders (never
//! `test_data/`), with a stub folder picker. Also the shared start-screen
//! test helpers other modules use (`temp_dir`, `step_until`, ...).

use std::path::Path;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mega_ui::theme::Tone;
use mmm_core::mosaic::{CONFIG_FILE, MosaicConfig, lock_session};

use super::*;
use crate::session::{notes, save_notes_blocking, session_name};

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
    /// Frames the start screen has drawn.
    frames: usize,
    /// After the screen's first pass: the facts it knew, and the session
    /// it showed.
    first_pass: Option<(usize, Option<PathBuf>)>,
}

impl Screen {
    fn new(recent: Vec<RecentEntry>, picker: StubPicker) -> Self {
        Screen {
            screen: StartScreen::new(recent, Arc::new(picker)),
            actions: Vec::new(),
            frames: 0,
            first_pass: None,
        }
    }
}

fn harness(recent: Vec<RecentEntry>) -> Harness<'static, Screen> {
    harness_with(Screen::new(recent, StubPicker(None)))
}

fn harness_with(state: Screen) -> Harness<'static, Screen> {
    Harness::builder()
        .with_size(egui::vec2(1000.0, 800.0))
        // Short steps: a click takes three, and a double-click must fit
        // egui's 0.3 s window.
        .with_step_dt(0.02)
        // Background work (facts, hero, thumbnails) asks for a repaint as
        // each piece finishes, so `run` may take more than a few steps.
        .with_max_steps(64)
        .build_ui_state(
            |ui, s: &mut Screen| {
                if crate::testutil::themed(ui) {
                    s.frames += 1;
                    s.actions.extend(s.screen.ui(ui));
                    if s.first_pass.is_none() {
                        s.first_pass = Some((s.screen.facts_known(), s.screen.hero()));
                    }
                }
            },
            state,
        )
}

/// Facts as usual, except that a session named "Stuck…" never answers
/// (like a folder on a dead network share).
fn stuck_share(entry: &RecentEntry) -> facts::CardFacts {
    if session_name(&entry.path).starts_with("Stuck") {
        std::thread::sleep(Duration::from_secs(120));
    }
    facts::load_facts(entry)
}

/// [`harness`] whose facts come from [`stuck_share`].
fn stuck_harness(recent: Vec<RecentEntry>) -> Harness<'static, Screen> {
    let mut state = Screen::new(recent, StubPicker(None));
    state.screen = state.screen.with_facts_loader(stuck_share);
    harness_with(state)
}

/// A recent entry for `path`, opened at `t`.
fn entry_at(path: &Path, t: u64) -> RecentEntry {
    RecentEntry {
        path: path.to_path_buf(),
        last_opened: t,
        pinned: false,
    }
}

fn entry(path: &Path) -> RecentEntry {
    entry_at(path, 1_700_000_000)
}

/// An empty session `root/{name}.mmm-session` (created, then closed).
fn session(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(format!("{name}.mmm-session"));
    drop(crate::session::create_blocking(&dir).unwrap());
    dir
}

/// Step until every recent session's facts have arrived.
fn settle(h: &mut Harness<'static, Screen>) {
    step_until(h, |s| s.frames > 0 && !s.screen.checking_recent());
    h.run();
}

/// Let `secs` of harness time pass.
fn pause(h: &mut Harness<'static, Screen>, secs: f32) {
    h.run_steps((secs / 0.02).ceil() as usize);
}

fn double_click(h: &mut Harness<'static, Screen>, name: &str) {
    h.get_by_role_and_label(Role::Button, name).click();
    h.step();
    h.get_by_role_and_label(Role::Button, name).click();
    h.step();
}

/// Every text painted in the last frame (cards paint their status).
fn painted_text(h: &Harness<'static, Screen>) -> String {
    fn walk(shape: &egui::Shape, out: &mut String) {
        match shape {
            egui::Shape::Text(t) => out.push_str(t.galley.text()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// The left edge of `name`'s card.
fn card_left(h: &Harness<'static, Screen>, name: &str) -> f32 {
    h.get_by_role_and_label(Role::Button, name).rect().left()
}

#[test]
fn typed_path_opens_session() {
    let dir = temp_dir("typed").join("Orion.mmm-session");
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = harness(Vec::new());
    h.run();
    // The typed path lives behind More → Type a path….
    assert!(h.query_by_label("Session folder").is_none());
    h.get_by_label("More").click();
    h.run();
    h.get_by_label("Type a path…").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Session folder")
        .focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Session folder")
        .type_text(dir.to_str().unwrap());
    h.step();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().actions, vec![StartAction::Open(dir.clone())]);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn typed_path_keeps_focus_when_the_block_changes() {
    let root = temp_dir("typed-focus");
    let dir = session(&root, "Steady");
    let mut h = harness(vec![entry(&dir)]);
    settle(&mut h);
    h.get_by_label("More").click();
    h.run();
    h.get_by_label("Type a path…").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Session folder")
        .type_text("abc");
    h.run();
    // The rows above the field change (as when facts arrive or reload)...
    h.state_mut().screen.facts.clear();
    h.run();
    // ...and typing still goes into the field.
    h.get_by_role_and_label(Role::TextInput, "Session folder")
        .type_text("def");
    h.run();
    assert_eq!(h.state().screen.typed_path, "abcdef");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recent_list_marks_missing() {
    let root = temp_dir("recent");
    let present = session(&root, "Present");
    let gone = root.join("Gone.mmm-session");
    let mut h = harness(vec![entry(&present), entry(&gone)]);
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Present");
    h.get_by_role_and_label(Role::Button, "Gone");
    let missing = Some((Tone::Problem, "Missing".to_string()));
    assert_eq!(h.state().screen.card_look(&gone), (missing, true));
    assert!(
        !h.state().screen.card_look(&present).1,
        "present not dimmed"
    );
    assert!(painted_text(&h).contains("Missing"));
    // The missing entry is not openable; the present one is.
    double_click(&mut h, "Gone");
    h.run();
    assert!(h.state().actions.is_empty());
    pause(&mut h, 0.5);
    double_click(&mut h, "Present");
    h.run();
    assert_eq!(h.state().actions, vec![StartAction::Open(present.clone())]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn draws_before_any_facts_arrive() {
    let root = temp_dir("start-first-frame");
    let older = session(&root, "Andromeda");
    let newest = session(&root, "Bubble");
    let mut h = harness(vec![entry_at(&older, 100), entry_at(&newest, 200)]);
    while h.state().frames == 0 {
        h.step();
    }
    // The very first pass drew the newest session, knowing no facts...
    assert_eq!(h.state().first_pass, Some((0, Some(newest.clone()))));
    // ...with its name and Resume (disabled until its facts say it's free).
    h.get_by_label("Resume session");
    h.get_by_role_and_label(Role::Label, "Bubble");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hero_is_the_newest_available() {
    let root = temp_dir("start-hero-available");
    let gone = root.join("Gone.mmm-session");
    let next = session(&root, "Nebula");
    let mut h = harness(vec![entry_at(&gone, 300), entry_at(&next, 200)]);
    settle(&mut h);
    assert_eq!(h.state().screen.hero(), Some(next.clone()));
    h.get_by_role_and_label(Role::Label, "Nebula");
    assert!(h.query_by_role_and_label(Role::Label, "Gone").is_none());
    h.get_by_label("CONTINUE WHERE YOU LEFT OFF");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn busy_session_is_not_the_hero() {
    let root = temp_dir("start-hero-busy");
    let busy = session(&root, "Busy");
    let free = session(&root, "Free");
    let _lock = lock_session(&busy).unwrap();
    let mut h = harness(vec![entry_at(&busy, 300), entry_at(&free, 200)]);
    settle(&mut h);
    assert_eq!(h.state().screen.hero(), Some(free.clone()));
    h.get_by_role_and_label(Role::Label, "Free");
    let open_elsewhere = Some((Tone::Neutral, "Open in another window".to_string()));
    assert_eq!(h.state().screen.card_look(&busy), (open_elsewhere, false));
    assert!(painted_text(&h).contains("Open in another window"));
    // Selected, it cannot be resumed, and its notes are read-only.
    h.get_by_role_and_label(Role::Button, "Busy").click();
    h.run();
    h.get_by_role_and_label(Role::Label, "Busy");
    assert!(
        h.get_by_label("Resume session")
            .accesskit_node()
            .is_disabled()
    );
    let before = std::fs::read(busy.join(CONFIG_FILE)).unwrap();
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput)
        .type_text("mine now");
    pause(&mut h, 1.5);
    assert_eq!(h.state().screen.notes.text, "");
    assert_eq!(std::fs::read(busy.join(CONFIG_FILE)).unwrap(), before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn opened_session_stops_being_the_hero() {
    let root = temp_dir("start-reopen");
    let older = session(&root, "Older");
    let newest = session(&root, "Newest");
    let mut h = harness(vec![entry_at(&older, 100), entry_at(&newest, 200)]);
    settle(&mut h);
    assert_eq!(h.state().screen.hero(), Some(newest.clone()));
    // Opened (here, or in another process), then shown again ("Open
    // another…"): its facts are read again, and it is busy now.
    let _lock = lock_session(&older).unwrap();
    h.state_mut().screen.record_opened(&older, unix_now());
    settle(&mut h);
    assert_eq!(h.state().screen.hero(), Some(newest.clone()));
    assert_eq!(h.state().screen.recent()[0].path, older);
    let open_elsewhere = Some((Tone::Neutral, "Open in another window".to_string()));
    assert_eq!(h.state().screen.card_look(&older), (open_elsewhere, false));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unreachable_newest_is_not_the_hero() {
    let root = temp_dir("start-stuck");
    let stuck = session(&root, "Stuck share");
    let next = session(&root, "Next");
    let mut h = stuck_harness(vec![entry_at(&stuck, 300), entry_at(&next, 200)]);
    while h.state().frames == 0 {
        h.step();
    }
    // At first the newest is shown: its facts may come at any moment.
    assert_eq!(h.state().first_pass, Some((0, Some(stuck.clone()))));
    step_until(&mut h, |s| s.screen.facts_known() == 1);
    // A frame is due without any input: the hero is chosen again soon.
    let delay = h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay;
    assert!(delay <= Duration::from_secs(1), "{delay:?}");
    // After about a second, the next available session takes over.
    step_until(&mut h, |s| s.screen.hero() == Some(next.clone()));
    h.run();
    h.get_by_role_and_label(Role::Label, "Next");
    h.get_by_label("CONTINUE WHERE YOU LEFT OFF");
    assert!(
        !h.get_by_label("Resume session")
            .accesskit_node()
            .is_disabled()
    );
    pause(&mut h, 0.5);
    double_click(&mut h, "Next");
    h.run();
    assert_eq!(h.state().actions, vec![StartAction::Open(next.clone())]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn thumbnails_load_while_the_hero_is_stuck() {
    let root = temp_dir("start-stuck-thumbs");
    let stuck = session(&root, "Stuck share");
    let a = session(&root, "Alpha");
    let b = session(&root, "Beta");
    let recent = vec![entry_at(&stuck, 300), entry_at(&a, 200), entry_at(&b, 100)];
    let mut h = stuck_harness(recent);
    while h.state().frames == 0 {
        h.step();
    }
    // The user picks the stuck session: its hero never loads...
    h.get_by_role_and_label(Role::Button, "Stuck share").click();
    h.run();
    // ...and the other cards' thumbnails load anyway, a little later.
    step_until(&mut h, |s| {
        [&a, &b]
            .iter()
            .all(|p| matches!(s.screen.thumbs.get(*p), Some(Thumb::Ready(_))))
    });
    assert_eq!(h.state().screen.hero(), Some(stuck.clone()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn resume_opens_the_hero() {
    let root = temp_dir("start-resume");
    let older = session(&root, "Older");
    let newest = session(&root, "Newest");
    let mut h = harness(vec![entry_at(&older, 100), entry_at(&newest, 200)]);
    settle(&mut h);
    h.get_by_label("Resume session").click();
    h.run();
    assert_eq!(h.state().actions, vec![StartAction::Open(newest.clone())]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn card_click_selects_and_double_click_opens() {
    let root = temp_dir("start-select");
    let a = session(&root, "Alpha");
    let b = session(&root, "Beta");
    let mut h = harness(vec![entry_at(&a, 200), entry_at(&b, 100)]);
    settle(&mut h);
    h.get_by_role_and_label(Role::Label, "Alpha");
    h.get_by_role_and_label(Role::Button, "Beta").click();
    h.run();
    h.get_by_role_and_label(Role::Label, "Beta");
    h.get_by_label("SESSION");
    assert!(h.query_by_label("CONTINUE WHERE YOU LEFT OFF").is_none());
    assert!(h.state().actions.is_empty(), "a click only selects");
    pause(&mut h, 0.5);
    double_click(&mut h, "Beta");
    h.run();
    assert_eq!(h.state().actions, vec![StartAction::Open(b.clone())]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pinned_cards_lead() {
    let root = temp_dir("start-pins");
    let a = session(&root, "Alpha");
    let b = session(&root, "Beta");
    let c = session(&root, "Gamma");
    let mut h = harness(vec![
        entry_at(&a, 300),
        entry_at(&b, 200),
        entry_at(&c, 100),
    ]);
    settle(&mut h);
    assert!(card_left(&h, "Alpha") < card_left(&h, "Beta"));
    assert!(card_left(&h, "Beta") < card_left(&h, "Gamma"));
    h.get_by_role_and_label(Role::Button, "Gamma").click();
    h.run();
    h.get_by_label("Pin").click();
    h.run();
    assert!(
        h.state()
            .screen
            .recent()
            .iter()
            .any(|e| e.path == c && e.pinned)
    );
    assert!(card_left(&h, "Gamma") < card_left(&h, "Alpha"));
    assert!(card_left(&h, "Alpha") < card_left(&h, "Beta"));
    // Unpinned again, it goes back to its place by last opened.
    h.get_by_label("Unpin").click();
    h.run();
    assert!(card_left(&h, "Beta") < card_left(&h, "Gamma"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn notes_save_after_a_pause() {
    let root = temp_dir("start-notes");
    let dir = session(&root, "Notes");
    let before = std::fs::read(dir.join(CONFIG_FILE)).unwrap();
    let mut h = harness(vec![entry(&dir)]);
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput)
        .type_text("redo DBE");
    h.step();
    // Typing is not saved at once (0.2 s later, still nothing)...
    h.run_steps(10);
    assert_eq!(std::fs::read(dir.join(CONFIG_FILE)).unwrap(), before);
    // ...but after a pause.
    step_until(&mut h, |_| {
        MosaicConfig::load(&dir).is_ok_and(|c| notes(&c.ui) == "redo DBE")
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_card_can_be_removed() {
    let root = temp_dir("start-remove");
    let present = session(&root, "Present");
    let gone = root.join("Gone.mmm-session");
    let mut h = harness(vec![entry_at(&gone, 300), entry_at(&present, 200)]);
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Gone").click();
    h.run();
    h.get_by_role_and_label(Role::Label, "Gone");
    h.get_by_label("Locate…");
    h.get_by_label("Remove from list").click();
    h.run();
    assert!(h.query_by_role_and_label(Role::Button, "Gone").is_none());
    assert_eq!(h.state().screen.recent().len(), 1);
    h.get_by_role_and_label(Role::Label, "Present");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_card_can_be_located() {
    let root = temp_dir("start-locate");
    let moved = session(&root, "Moved");
    let gone = root.join("Gone.mmm-session");
    let picker = StubPicker(Some(moved.clone()));
    let mut h = harness_with(Screen::new(vec![entry(&gone)], picker));
    settle(&mut h);
    h.get_by_label("Locate…").click();
    step_until(&mut h, |s| s.screen.recent()[0].path == moved);
    settle(&mut h);
    h.get_by_role_and_label(Role::Label, "Moved");
    let found = Some((Tone::Neutral, "No panels yet".to_string()));
    assert_eq!(h.state().screen.card_look(&moved), (found, false));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn all_sessions_searches_names_and_notes() {
    let root = temp_dir("start-all");
    let a = session(&root, "Alpha");
    let b = session(&root, "Beta");
    let c = session(&root, "Gamma");
    save_notes_blocking(&a, "OIII 13–15: Gradient, redo DBE").unwrap();
    save_notes_blocking(&b, "fine").unwrap();
    let mut h = harness(vec![
        entry_at(&a, 300),
        entry_at(&b, 200),
        entry_at(&c, 100),
    ]);
    settle(&mut h);
    h.get_by_label("All sessions").click();
    h.run();
    assert!(h.query_by_label("RECENT SESSIONS").is_none());
    // The search box (its hint shows while it is empty).
    h.get_by(|n| n.placeholder() == Some("Search sessions and notes"));
    h.get_by_role(Role::TextInput).focus();
    h.step();
    h.get_by_role(Role::TextInput).type_text("gradient");
    h.run();
    h.get_by_role_and_label(Role::Button, "Alpha");
    assert!(h.query_by_role_and_label(Role::Button, "Beta").is_none());
    assert!(h.query_by_role_and_label(Role::Button, "Gamma").is_none());
    // Names match too, ignoring case.
    h.get_by_role(Role::TextInput).focus();
    h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
    h.step();
    h.get_by_role(Role::TextInput).type_text("GAM");
    h.run();
    h.get_by_role_and_label(Role::Button, "Gamma");
    assert!(h.query_by_role_and_label(Role::Button, "Alpha").is_none());
    // Back, by the button or Esc.
    h.get_by_label("Back").click();
    h.run();
    h.get_by_label("RECENT SESSIONS");
    h.get_by_label("All sessions").click();
    h.run();
    assert!(h.query_by_label("RECENT SESSIONS").is_none());
    h.key_press(egui::Key::Escape);
    h.run();
    h.get_by_label("RECENT SESSIONS");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn first_run_offers_new_and_open() {
    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label("New mosaic");
    h.get_by_label("Open…");
    h.get_by_label("Merge your stacked panels into one seamless mosaic.");
    assert!(h.query_by_label("RECENT SESSIONS").is_none());
    assert!(h.query_by_label("Resume session").is_none());
}

#[test]
fn ago_reads_naturally() {
    assert_eq!(ago(100, 90), "just now");
    assert_eq!(ago(10_000, 10_000 - 120), "2 minutes ago");
    assert_eq!(ago(200_000, 200_000 - 86_400), "1 day ago");
}
