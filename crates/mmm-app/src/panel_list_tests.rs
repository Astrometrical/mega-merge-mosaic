//! Tests of the panel list (a `#[path]` child of `panel_list.rs`), driven
//! inside the session window as the shell drives it, and of the edit model
//! behind it (`panel_edits.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Rect;
use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use mega_ui::groups::SII_COLOR;
use mega_ui::table::ROLL_UP_ROWS;
use mega_ui::theme::palette;
use mmm_core::mosaic::{GroupRule, Grouping, Mosaic, MosaicConfig, PanelEntry};
use mmm_core::synth::{SynthWcs, write_xisf_solved, write_xisf_with_header_xml};

use super::*;
use crate::checks::{ChecksEngine, ChecksReport};
use crate::jobs::JobRunner;
use crate::panel_edits::{
    Applied, FilesPicker, Found, GROUP_PALETTE, PHYSICAL_SLOTS, PanelsEdit, assign_group_colors,
    group_colors, grouping_chosen, load_group_colors, save_group_colors,
};
use crate::pipeline::Pipeline;
use crate::session::{OpenSession, create_blocking};
use crate::session_window::{SessionWindow, Undo, WindowToast};
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::{PATIENCE, open, two_group_session};

/// Answers the file dialog with fixed paths.
struct StubFiles(Vec<PathBuf>);

impl FilesPicker for StubFiles {
    fn pick_files(&self) -> Option<Vec<PathBuf>> {
        Some(self.0.clone())
    }

    fn pick_folders(&self) -> Option<Vec<PathBuf>> {
        Some(self.0.clone())
    }
}

/// A file dropped on the window (native: just a path).
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// The window and what the shell hands it each frame.
struct State {
    w: SessionWindow,
    session: OpenSession,
    jobs: JobRunner,
    pipeline: Pipeline,
    checks: ChecksEngine,
    /// Every toast the window raised, in order.
    toasts: Vec<WindowToast>,
}

impl State {
    fn list(&self) -> &PanelList {
        self.w.panel_list()
    }

    /// Nothing waiting: no edit pending, groups resolved for the inputs.
    fn settled(&self) -> bool {
        self.list().settled(&self.session)
    }

    fn panel_names(&self) -> Vec<String> {
        self.session
            .mosaic
            .config
            .panels
            .iter()
            .map(|p| name_of(&p.path))
            .collect()
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

fn runner() -> JobRunner {
    JobRunner::new(egui::Context::default())
}

/// The session window over `session` with the panel list open, its file
/// dialog answering `picked`.
fn harness(session: OpenSession, picked: Vec<PathBuf>, jobs: JobRunner) -> Harness<'static, State> {
    harness_sized(session, picked, jobs, WINDOW)
}

/// The window size of [`harness`].
const WINDOW: egui::Vec2 = egui::vec2(1400.0, 900.0);

/// [`harness`] in a window of `size`.
fn harness_sized(
    session: OpenSession,
    picked: Vec<PathBuf>,
    jobs: JobRunner,
    size: egui::Vec2,
) -> Harness<'static, State> {
    let mut w = SessionWindow::new(Arc::new(StubFiles(picked)));
    w.panel_list_open = true;
    let state = State {
        w,
        session,
        jobs,
        pipeline: Pipeline::new(),
        checks: ChecksEngine::new(),
        toasts: Vec::new(),
    };
    Harness::builder()
        .with_size(size)
        .with_step_dt(0.01)
        .with_max_steps(64)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                // As the shell does: jobs, the window's hooks, then the
                // window.
                let ctx = ui.ctx().clone();
                let events = s.jobs.poll(&mut s.session);
                for e in &events {
                    s.w.on_event(&mut s.session, &s.jobs, e);
                }
                s.w.poll_panels(&ctx, &mut s.session, &mut s.jobs);
                s.w.poll(&ctx, &mut s.session, &mut s.jobs);
                let out =
                    s.w.ui(ui, &mut s.session, &mut s.jobs, &mut s.pipeline, &s.checks);
                s.toasts.extend(out.toasts);
            },
            state,
        )
}

/// A few frames: a click shows its effect a frame later.
fn steps(h: &mut Harness<'static, State>) {
    for _ in 0..3 {
        h.step();
    }
}

/// Step until `done` holds (bounded by [`PATIENCE`]: background work).
fn wait(h: &mut Harness<'static, State>, done: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for the list");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    steps(h);
}

fn on_disk(s: &State) -> MosaicConfig {
    MosaicConfig::load(&s.session.mosaic.dir).unwrap()
}

/// Every text painted last frame, with where.
fn painted(h: &Harness<'_, State>) -> Vec<(String, Rect)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
        match shape {
            egui::Shape::Text(t) => out.push((
                t.galley.text().to_owned(),
                Rect::from_min_size(t.pos, t.galley.size()),
            )),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// Whether `s` is one icon glyph (the icon font's private-use range).
fn is_icon(s: &str) -> bool {
    let mut chars = s.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if ('\u{e000}'..='\u{f8ff}').contains(&c))
}

/// The texts painted on the row of panel `name` after its name, left to
/// right (icons left out).
fn cells(h: &Harness<'_, State>, name: &str) -> Vec<String> {
    let row = h.get_by_role_and_label(Role::ListItem, name).rect();
    let mut on_row: Vec<(String, Rect)> = painted(h)
        .into_iter()
        .filter(|(t, r)| t != name && !is_icon(t) && row.contains(r.center()))
        .collect();
    on_row.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
    on_row.into_iter().map(|(t, _)| t).collect()
}

/// The rows (panels) the table lays out.
fn rows(h: &Harness<'_, State>) -> usize {
    h.query_all_by_role(Role::ListItem).count()
}

/// The list's section heading titled `title` (the view bar's chips read
/// the same).
fn heading<'h>(h: &'h Harness<'static, State>, title: &'h str) -> egui_kittest::Node<'h> {
    let list = h.state().w.list_rect.expect("the list was drawn");
    let mut found = h
        .query_all_by_role_and_label(Role::Button, title)
        .filter(|n| list.contains(n.rect().center()));
    let node = found.next().unwrap_or_else(|| panic!("no heading {title}"));
    assert!(found.next().is_none(), "two headings {title}");
    node
}

/// Whether the list has a heading titled `title`.
fn has_heading(h: &Harness<'static, State>, title: &str) -> bool {
    let list = h.state().w.list_rect.expect("the list was drawn");
    h.query_all_by_role_and_label(Role::Button, title)
        .any(|n| list.contains(n.rect().center()))
}

/// The warning `heading` carries, if any.
fn warning(h: &Harness<'static, State>, title: &str) -> Option<String> {
    heading(h, title).accesskit_node().description()
}

/// Remove panel `name` through its row's actions menu.
fn remove_row(h: &mut Harness<'static, State>, name: &str) {
    h.get_by_role_and_label(Role::ListItem, name).hover();
    steps(h);
    h.get_by_label("Panel actions").click();
    steps(h);
    h.get_by_role_and_label(Role::Button, "Remove").click();
    steps(h);
}

/// A small mono XISF panel `name` in `dir` with these FITS header cards.
fn panel(dir: &Path, name: &str, cards: &[(&str, &str)]) -> PathBuf {
    let path = dir.join(name);
    let xml: String = cards
        .iter()
        .map(|(k, v)| format!(r#"<FITSKeyword name="{k}" value="{v}" comment=""/>"#))
        .collect();
    write_xisf_with_header_xml(&path, 64, 48, 1, &vec![0.1; 64 * 48], &xml).unwrap();
    path
}

/// A session in `root` of `paths`, grouped by the FILTER header.
fn session_of(root: &Path, paths: Vec<PathBuf>) -> OpenSession {
    let mut cfg = MosaicConfig::new(paths);
    cfg.grouping = Grouping {
        enabled: true,
        rule: GroupRule::Header {
            key: "FILTER".into(),
        },
    };
    open(Mosaic::create(&root.join("p.mmm-session"), cfg).unwrap())
}

#[test]
fn the_list_sits_beside_the_plan() {
    let (root, session, jobs) = crate::jobs::tests::analyzed("list-beside");
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| {
        let plan = s.w.plan();
        s.settled() && plan.canvas_frame().is_some() && !plan.items().is_empty()
    });
    assert!(h.state().w.panel_list_open);
    // The list, and the view bar and the plan beside it.
    h.get_by_label("Add files…");
    h.get_by_role_and_label(Role::ListItem, "Ha_0.xisf");
    h.get_by_label("Plan");
    let list = h.state().w.list_rect.expect("the list was drawn");
    let (canvas, _) = h.state().w.plan().canvas_frame().unwrap();
    assert!(list.right() <= canvas.left(), "{list:?} {canvas:?}");
    assert!(list.width() >= mega_ui::theme::tokens::PANEL_LIST_MIN_WIDTH);
    // Canvas shortcuts work with the list open: O toggles the outlines.
    let outlines = |h: &Harness<'static, State>| {
        let toggle = By::new()
            .label("Outlines")
            .predicate(|n| n.toggled().is_some());
        h.get(toggle).accesskit_node().toggled() == Some(Toggled::True)
    };
    assert!(outlines(&h));
    h.key_press(egui::Key::O);
    steps(&mut h);
    assert!(!outlines(&h), "the outlines toggled");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rows_show_header_facts() {
    let root = temp_dir("list-facts");
    let dir = root.join("panels");
    std::fs::create_dir_all(&dir).unwrap();
    let ha = |name, frames| {
        panel(
            &dir,
            name,
            &[("FILTER", "'Ha'"), ("EXPTIME", "300"), ("NCOMBINE", frames)],
        )
    };
    let a = ha("Ha_a.xisf", "24");
    let b = ha("Ha_b.xisf", "37");
    let c = panel(
        &dir,
        "Ha_c.xisf",
        &[("FILTER", "'Ha'"), ("EXPTIME", "45"), ("NCOMBINE", "1")],
    );
    // Solved at 1.21″/px, without FILTER or exposure cards.
    let s = 1.21 / 3600.0;
    let solved = dir.join("solved.xisf");
    let wcs = SynthWcs {
        crval: [83.8, -5.4],
        refimg: [32.0, 24.0],
        cd: [[-s, 0.0], [0.0, s]],
    };
    write_xisf_solved(&solved, 64, 48, 1, &vec![0.1; 64 * 48], &wcs).unwrap();
    let mut h = harness(session_of(&root, vec![a, b, c, solved]), vec![], runner());
    wait(&mut h, State::settled);
    assert_eq!(cells(&h, "Ha_a.xisf"), ["Ha", "2h 00m", "—", "64 × 48"]);
    assert_eq!(cells(&h, "Ha_b.xisf"), ["Ha", "3h 05m", "—", "64 × 48"]);
    assert_eq!(cells(&h, "Ha_c.xisf"), ["Ha", "45s", "—", "64 × 48"]);
    assert_eq!(cells(&h, "solved.xisf"), ["—", "—", "1.21″", "64 × 48"]);
    // The group's total integration, when every member's is known.
    heading(&h, "Ha · 3 · 5h 06m");
    let texts: Vec<String> = painted(&h).into_iter().map(|(t, _)| t).collect();
    for column in ["PANEL", "FILTER", "INTEGRATION", "SCALE", "SIZE"] {
        assert!(texts.iter().any(|t| t == column), "{column}: {texts:?}");
    }
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sections_head_each_group() {
    let root = temp_dir("list-sections");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    // No integration cards: counts only, and no mismatch.
    assert_eq!(warning(&h, "Ha · 4"), None);
    assert_eq!(warning(&h, "OIII · 4"), None);
    let (ha, oiii) = (heading(&h, "Ha · 4").rect(), heading(&h, "OIII · 4").rect());
    assert!(ha.top() < oiii.top(), "groups in name order");
    // Each section holds its group's rows, in the group's colour.
    let row = h
        .get_by_role_and_label(Role::ListItem, "OIII_0.xisf")
        .rect();
    assert!(row.top() > oiii.top());
    let colors = load_group_colors(&h.state().session.mosaic.config.ui);
    assert_eq!(colors.keys().collect::<Vec<_>>(), ["Ha", "OIII"]);
    // One fewer: the mismatch shows against the largest group.
    remove_row(&mut h, "OIII_0.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    assert_eq!(warning(&h, "OIII · 3").as_deref(), Some("1 fewer than Ha"));
    assert_eq!(warning(&h, "Ha · 4"), None);
    let texts: Vec<String> = painted(&h).into_iter().map(|(t, _)| t).collect();
    assert!(texts.iter().any(|t| t == "1 fewer than Ha"), "{texts:?}");
    // Removing offered an Undo.
    let s = h.state();
    let toast = s.toasts.last().expect("a toast");
    assert_eq!(toast.toast.title, "Removed 1 panel");
    assert_eq!(toast.toast.action.as_deref(), Some("Undo"));
    assert_eq!(on_disk(s).panels.len(), 7);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_group_comes_first_with_its_reason() {
    let root = temp_dir("list-nogroup");
    let mut session = two_group_session(&root);
    let stray = panel(&root.join("panels"), "stray.xisf", &[("OBJECT", "'M42'")]);
    session.mosaic.config.panels.push(PanelEntry {
        path: stray,
        fingerprint: None,
        manual_group: None,
    });
    session.mosaic.save().unwrap();
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    let none = heading(&h, "No group · 1").rect();
    assert_eq!(
        warning(&h, "No group · 1").as_deref(),
        Some("FILTER missing")
    );
    let ha = heading(&h, "Ha · 4").rect();
    assert!(none.top() < ha.top(), "{none:?} {ha:?}");
    let stray = h.get_by_role_and_label(Role::ListItem, "stray.xisf").rect();
    assert!(none.top() < stray.top() && stray.top() < ha.top());
    assert_eq!(cells(&h, "stray.xisf"), ["—", "—", "—", "64 × 48"]);
    // A header rule on another keyword: only the stray one has OBJECT.
    choose(&mut h, "Header keyword…");
    let field = h.get_by_role_and_label(Role::TextInput, "Header key");
    assert_eq!(field.value().as_deref(), Some("FILTER"));
    field.focus();
    h.step();
    h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Header key")
        .type_text("OBJECT");
    steps(&mut h);
    assert_eq!(
        h.state().session.mosaic.config.grouping.rule,
        GroupRule::default(),
        "applies on Enter, not per keystroke"
    );
    h.key_press(egui::Key::Enter);
    wait(&mut h, |s| {
        s.settled()
            && s.session.mosaic.config.grouping.rule
                == GroupRule::Header {
                    key: "OBJECT".into(),
                }
    });
    assert_eq!(
        warning(&h, "No group · 8").as_deref(),
        Some("OBJECT missing")
    );
    heading(&h, "M42 · 1");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn groups_roll_up() {
    let root = temp_dir("list-rollup");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    assert_eq!(h.query_all_by_label("… 1 more").count(), 2);
    assert_eq!(rows(&h), 2 * ROLL_UP_ROWS);
    assert!(
        h.query_by_role_and_label(Role::ListItem, "Ha_3.xisf")
            .is_none()
    );
    // "… 1 more" opens the whole group.
    h.query_all_by_label("… 1 more").next().unwrap().click();
    steps(&mut h);
    h.get_by_role_and_label(Role::ListItem, "Ha_3.xisf");
    assert_eq!(h.query_all_by_label("… 1 more").count(), 1);
    // A heading folds its section.
    heading(&h, "OIII · 4").click();
    steps(&mut h);
    assert!(
        h.query_by_role_and_label(Role::ListItem, "OIII_0.xisf")
            .is_none()
    );
    // Grouping off: one flat list, nothing rolled up.
    let group_by = h.get_by_role_and_label(Role::ComboBox, "Group by");
    assert_eq!(group_by.value().as_deref(), Some("FILTER header"));
    group_by.click();
    steps(&mut h);
    h.get_by_label("Don't group").click();
    wait(&mut h, |s| {
        s.settled() && !s.session.mosaic.config.grouping.enabled
    });
    assert_eq!(rows(&h), 8);
    assert_eq!(h.query_all_by_label_contains("more").count(), 0);
    assert!(!has_heading(&h, "Ha · 4"));
    assert!(
        h.query_by_role_and_label(Role::TextInput, "Header key")
            .is_none()
    );
    // The user chose: the differing FILTER headers don't group the panels
    // again by themselves.
    assert!(grouping_chosen(&on_disk(h.state()).ui));
    steps(&mut h);
    assert!(!h.state().session.mosaic.config.grouping.enabled);
    assert!(h.state().toasts.is_empty(), "no automatic grouping");
    // FILTER header groups them again.
    choose(&mut h, "FILTER header");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.grouping.enabled
    });
    assert!(on_disk(h.state()).grouping.enabled);
    heading(&h, "Ha · 4");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unreadable_panel_shows_a_row() {
    let root = temp_dir("list-unreadable");
    let mut session = two_group_session(&root);
    let broken = root.join("panels").join("broken.xisf");
    std::fs::write(&broken, b"not an image").unwrap();
    session.mosaic.config.panels.insert(
        0,
        PanelEntry {
            path: broken,
            fingerprint: None,
            manual_group: None,
        },
    );
    session.mosaic.save().unwrap();
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    assert_eq!(cells(&h, "broken.xisf"), ["Unreadable", "—", "—", "—"]);
    // Its problem says why (the ⚠'s tooltip).
    h.get_by_label_contains("Unreadable: ");
    // The rest still group.
    assert_eq!(
        warning(&h, "No group · 1").as_deref(),
        Some("FILTER missing")
    );
    heading(&h, "Ha · 4");
    heading(&h, "OIII · 4");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_large_session_draws_a_bounded_list() {
    let root = temp_dir("list-large");
    let dir = root.join("panels");
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for filter in ["Ha", "OIII", "SII"] {
        for i in 0..50 {
            let name = format!("{filter}_{i:02}.xisf");
            let card = format!("'{filter}'");
            paths.push(panel(&dir, &name, &[("FILTER", &card)]));
        }
    }
    let mut h = harness(session_of(&root, paths), vec![], runner());
    wait(&mut h, State::settled);
    for g in ["Ha", "OIII", "SII"] {
        heading(&h, &format!("{g} · 50"));
    }
    assert_eq!(h.query_all_by_label("… 47 more").count(), 3);
    let drawn = rows(&h);
    assert!(drawn <= 3 * (ROLL_UP_ROWS + 1), "{drawn} rows laid out");
    // Frames keep completing.
    steps(&mut h);
    assert_eq!(h.state().session.mosaic.config.panels.len(), 150);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rule_change_keeps_the_rows_where_they_were_until_resolved() {
    let root = temp_dir("list-rule-change");
    let dir = root.join("panels");
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for filter in ["Ha", "OIII", "SII"] {
        for i in 0..50 {
            let name = format!("M42_FILTER_{filter}_{i:02}.xisf");
            let card = format!("'{filter}'");
            paths.push(panel(&dir, &name, &[("FILTER", &card)]));
        }
    }
    let mut h = harness(session_of(&root, paths), vec![], runner());
    wait(&mut h, State::settled);
    heading(&h, "Ha · 50");
    // The same groups by path keyword: every frame until they are
    // resolved shows the groups as they were, never 150 rows in No group.
    h.get_by_role_and_label(Role::ComboBox, "Group by").click();
    steps(&mut h);
    h.get_by_role_and_label(Role::Button, "Path keyword…")
        .click();
    let bound = 3 * (ROLL_UP_ROWS + 1);
    let deadline = Instant::now() + PATIENCE;
    let mut frames = 0;
    loop {
        h.step();
        frames += 1;
        let drawn = rows(&h);
        assert!(drawn <= bound, "{drawn} rows laid out in frame {frames}");
        assert!(!has_heading(&h, "No group · 150"), "frame {frames}");
        let s = h.state();
        let rule = &s.session.mosaic.config.grouping.rule;
        if s.settled() && matches!(rule, GroupRule::PathKeyword { .. }) {
            break;
        }
        assert!(Instant::now() < deadline, "timed out waiting for the list");
        std::thread::sleep(Duration::from_millis(5));
    }
    steps(&mut h);
    for g in ["Ha", "OIII", "SII"] {
        heading(&h, &format!("{g} · 50"));
    }
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_files_dedupes() {
    let root = temp_dir("list-add");
    let files = root.join("files");
    std::fs::create_dir_all(&files).unwrap();
    for f in ["a.xisf", "b.xisf", "c.fits", "d.xisf", "notes.txt"] {
        std::fs::write(files.join(f), b"not an image").unwrap();
    }
    let session = create_blocking(&root.join("s.mmm-session")).unwrap();
    let (a, b, c) = (
        files.join("a.xisf"),
        files.join("b.xisf"),
        files.join("c.fits"),
    );
    let mut h = harness(session, vec![a.clone(), b.clone(), a.clone()], runner());
    steps(&mut h);
    h.get_by_label("Add files…").click();
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 2);
    assert_eq!(h.state().panel_names(), ["a.xisf", "b.xisf"]);

    // Dropped files: duplicates (also through a symlink) and non-panel
    // files are ignored.
    #[cfg(unix)]
    let link = {
        let link = root.join("link-to-a.xisf");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        link
    };
    #[cfg(not(unix))]
    let link = a.clone();
    h.input_mut().dropped_files = [b.clone(), link, c.clone(), files.join("notes.txt")]
        .into_iter()
        .map(|path| Arc::new(Dropped(path)) as egui::DroppedFileHandle)
        .collect();
    h.step();
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 3);
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    assert_eq!(h.state().panel_names(), ["a.xisf", "b.xisf", "c.fits"]);

    // A typed pattern (More → Type a path…) adds only the new match.
    h.get_by_label("More").click();
    steps(&mut h);
    h.get_by_label("Type a path…").click();
    steps(&mut h);
    h.get_by_role_and_label(Role::TextInput, "Add path")
        .type_text(files.join("*.xisf").to_str().unwrap());
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 4);
    assert_eq!(
        h.state().panel_names(),
        ["a.xisf", "b.xisf", "c.fits", "d.xisf"]
    );
    // A plain path that is already in is ignored too.
    h.get_by_role_and_label(Role::TextInput, "Add path").focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Add path")
        .type_text(a.to_str().unwrap());
    h.step();
    h.get_by_label("Add").click();
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    assert_eq!(h.state().session.mosaic.config.panels.len(), 4);
    assert_eq!(
        on_disk(h.state()).panels,
        h.state().session.mosaic.config.panels
    );
    // Unreadable files still get a row, saying so (no grouping: one flat
    // list).
    steps(&mut h);
    // The list kept its width while it was empty (a narrow hint shrinks
    // no panel).
    let list = h.state().w.list_rect.expect("the list was drawn");
    assert!(list.width() >= WINDOW.x / 2.0 - 1.0, "{list:?}");
    assert_eq!(cells(&h, "a.xisf"), ["Unreadable", "—", "—", "—"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder of empty panel files (names `names`), plus noise `panel_files_in`
/// must skip: a subfolder with two panels and a `.txt`.
fn folder_with_panels(root: &Path, names: &[&str]) -> PathBuf {
    let dir = root.join("folder");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for n in names {
        std::fs::write(dir.join(n), b"not an image").unwrap();
    }
    for n in ["s1.xisf", "s2.xisf"] {
        std::fs::write(dir.join("sub").join(n), b"not an image").unwrap();
    }
    std::fs::write(dir.join("notes.txt"), b"text").unwrap();
    dir
}

/// Drop `paths` on the window for one frame.
fn drop_paths(h: &mut Harness<'static, State>, paths: Vec<PathBuf>) {
    h.input_mut().dropped_files = paths
        .into_iter()
        .map(|path| Arc::new(Dropped(path)) as egui::DroppedFileHandle)
        .collect();
    h.step();
    h.input_mut().dropped_files.clear();
}

#[test]
fn folders_add_only_new_top_level_panels() {
    let root = temp_dir("list-folders");
    let dir = folder_with_panels(&root, &["a.xisf", "b.fits", "c.XISF"]);
    let session = create_blocking(&root.join("s.mmm-session")).unwrap();
    let mut h = harness(session, vec![dir.clone()], runner());
    steps(&mut h);
    // One of the folder's panels is in already.
    drop_paths(&mut h, vec![dir.join("a.xisf")]);
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 1);
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    h.get_by_label("Add folder…").click();
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 3);
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    assert_eq!(h.state().panel_names(), ["a.xisf", "b.fits", "c.XISF"]);
    // The same folder dropped again adds nothing.
    drop_paths(&mut h, vec![dir.clone()]);
    steps(&mut h);
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    assert_eq!(h.state().session.mosaic.config.panels.len(), 3);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dropped_files_and_folders_mix() {
    let root = temp_dir("list-mix");
    let dir = folder_with_panels(&root, &["a.xisf", "b.xisf"]);
    let lone = root.join("lone.fits");
    std::fs::write(&lone, b"not an image").unwrap();
    let missing = root.join("gone.xisf");
    let session = create_blocking(&root.join("s.mmm-session")).unwrap();
    let mut h = harness(session, vec![], runner());
    steps(&mut h);
    drop_paths(
        &mut h,
        vec![lone, dir.clone(), dir.join("notes.txt"), missing],
    );
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 3);
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    let mut names = h.state().panel_names();
    names.sort();
    assert_eq!(names, ["a.xisf", "b.xisf", "lone.fits"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_path_still_works() {
    let root = temp_dir("list-typed");
    let dir = folder_with_panels(&root, &["a.xisf", "b.xisf"]);
    let session = create_blocking(&root.join("s.mmm-session")).unwrap();
    let mut h = harness(session, vec![], runner());
    steps(&mut h);
    h.get_by_label("More").click();
    steps(&mut h);
    h.get_by_label("Type a path…").click();
    steps(&mut h);
    h.get_by_role_and_label(Role::TextInput, "Add path")
        .type_text(dir.join("*.xisf").to_str().unwrap());
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 2);
    assert_eq!(h.state().panel_names(), ["a.xisf", "b.xisf"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn edit_while_job_running_is_queued_then_applied() {
    let root = temp_dir("list-queued");
    let mut session = two_group_session(&root);
    let mut jobs = runner();
    let gate = jobs.hold();
    let job = jobs.start_analyze(&mut session);
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| s.list().resolved_current(&s.session));
    remove_row(&mut h, "Ha_0.xisf");
    // Queued: the job is cancelling, nothing changed yet (memory or disk).
    {
        let s = h.state();
        assert!(s.list().pending().is_some());
        assert_eq!(s.session.mosaic.config.panels.len(), 8);
        assert_eq!(on_disk(s).panels.len(), 8);
        let views = s.jobs.views();
        assert_eq!((views[0].id, views[0].stage.as_str()), (job, "Cancelling…"));
    }
    // Shown as it will be: the removed row is gone already.
    assert!(
        h.query_by_role_and_label(Role::ListItem, "Ha_0.xisf")
            .is_none()
    );
    h.get_by_label_contains("Waiting for the running job");
    // A second edit while queued replaces the first (newer work wins).
    remove_row(&mut h, "Ha_1.xisf");
    assert_eq!(h.state().list().pending().unwrap().panels.len(), 6);
    gate.open();
    wait(&mut h, |s| !s.jobs.busy() && s.settled());
    let s = h.state();
    assert_eq!(s.session.mosaic.config.panels.len(), 6);
    assert_eq!(on_disk(s).panels, s.session.mosaic.config.panels);
    let actions: Vec<_> = s
        .session
        .mosaic
        .config
        .provenance
        .iter()
        .map(|p| p.action.as_str())
        .collect();
    assert_eq!(actions, ["inputs"], "applied once, after the job stopped");
    assert!(
        s.session.mosaic.config.needs_analyze.is_empty(),
        "no analysis started here"
    );
    assert!(!s.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_waiting_edit_applies_when_the_session_is_left() {
    let root = temp_dir("list-apply-waiting");
    let mut session = two_group_session(&root);
    let mut jobs = runner();
    let gate = jobs.hold();
    jobs.start_analyze(&mut session);
    let mut list = PanelList::new(Arc::new(StubFiles(vec![])));
    let mut edit = PanelsEdit::of(&session);
    edit.panels.remove(0);
    list.submit_edit(edit, &mut session, &mut jobs);
    assert!(list.has_pending(), "waits for the analysis to stop");
    // Leaving the session: the jobs stop, then the edit applies at once.
    gate.open();
    let (_, stopped) = jobs.stop_all(&mut session, crate::testutil::PATIENCE);
    assert!(stopped);
    list.apply_waiting(&mut session, &mut jobs, true);
    assert!(!list.has_pending());
    assert_eq!(
        MosaicConfig::load(&session.mosaic.dir)
            .unwrap()
            .panels
            .len(),
        7
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn queued_edit_cancels_jobs_started_later() {
    let root = temp_dir("list-requeue");
    let mut session = two_group_session(&root);
    let mut jobs = runner();
    let gate = jobs.hold();
    jobs.start_analyze(&mut session);
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| s.list().resolved_current(&s.session));
    remove_row(&mut h, "Ha_0.xisf");
    assert!(h.state().list().pending().is_some());
    // Another job starts before the queued edit could apply...
    let later = {
        let s = h.state_mut();
        let gate = s.jobs.hold();
        let id = s.jobs.start_analyze(&mut s.session);
        (id, gate)
    };
    gate.open();
    later.1.open();
    // ...and is cancelled too: the edit applies, no analysis ran.
    wait(&mut h, |s| !s.jobs.busy() && s.settled());
    let s = h.state();
    assert_eq!(s.session.mosaic.config.panels.len(), 7);
    let actions: Vec<_> = s
        .session
        .mosaic
        .config
        .provenance
        .iter()
        .map(|p| p.action.as_str())
        .collect();
    assert_eq!(actions, ["inputs"]);
    assert!(
        s.session.mosaic.config.group_dirs.is_empty(),
        "never analysed"
    );
    let _ = later.0;
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn panels_edit_spares_exports() {
    use crate::jobs::JobEvent;
    use crate::jobs::tests::{analyzed, drain};
    use mmm_core::export::ExportFormat;

    let (root, mut s, mut jobs) = analyzed("list-edit-export");
    jobs.start_blend(&mut s, "Ha", None);
    drain(&mut jobs, &mut s);
    // A merge runs with an export beside it, each held at its own gate.
    let merge_gate = jobs.hold();
    let blend = jobs.start_blend(&mut s, "OIII", None);
    let export_gate = jobs.hold_exports();
    let out = root.join("ha.fits");
    let ha = crate::jobs::ExportTarget::Group("Ha".into());
    let export = jobs.start_export(&s, ha, out.clone(), ExportFormat::Fits, false);
    let mut edit = PanelsEdit::of(&s);
    edit.panels.pop();
    // The edit cancels the merge but not the export, and waits for the
    // merge only.
    let Applied::Queued(edit) = edit.apply(&mut s, &mut jobs) else {
        panic!("the edit should wait for the merge");
    };
    let labels: Vec<String> = jobs.views().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["Merge OIII", "Export Ha"], "export kept");
    merge_gate.open();
    let mut events = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    while jobs.views().first().map(|v| v.label.as_str()) != Some("Export Ha") {
        assert!(Instant::now() < deadline, "the merge never ended");
        std::thread::sleep(Duration::from_millis(5));
        events.extend(jobs.poll(&mut s));
    }
    // A running export does not hold the edit back.
    assert!(matches!(edit.apply(&mut s, &mut jobs), Applied::Done));
    assert_eq!(s.mosaic.config.panels.len(), 7);
    export_gate.open();
    events.extend(drain(&mut jobs, &mut s));
    assert!(events.contains(&JobEvent::Cancelled { id: blend }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { id, .. } if *id == export)),
        "{events:?}"
    );
    assert!(out.is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn group_colors_are_stable_and_physical() {
    let mut map = BTreeMap::new();
    assert!(assign_group_colors(&mut map, ["Ha", "Foo"]));
    assert!(!assign_group_colors(&mut map, ["Foo", "Ha"]), "unchanged");
    let before = group_colors(&map, ["Ha", "Foo"]);
    assert_eq!(before["Ha"], palette::HA_500);
    // A group that sorts first changes no colour already given.
    assert!(assign_group_colors(&mut map, ["Aaa", "Ha", "Foo"]));
    let after = group_colors(&map, ["Aaa", "Ha", "Foo"]);
    assert_eq!(after["Ha"], before["Ha"]);
    assert_eq!(after["Foo"], before["Foo"]);
    assert_ne!(after["Aaa"], after["Foo"]);
    // Physical colours for well-known names, whatever the order.
    let c = group_colors(
        &BTreeMap::new(),
        ["Blue", "Green", "Red", "oiii", "SII", "L", "No group"],
    );
    assert_eq!(c["Blue"], palette::ACCENT_600);
    assert_eq!(c["Green"], palette::GREEN_400);
    assert_eq!(c["Red"], palette::HA_500);
    assert_eq!(c["oiii"], palette::ACCENT_400);
    assert_eq!(c["SII"], SII_COLOR);
    assert_ne!(SII_COLOR, palette::HA_500);
    assert_eq!(c["L"], palette::TEXT_STRONG);
    assert!(!c.contains_key("No group"));
    // An unknown name seen first takes no physical colour.
    let mut map = BTreeMap::new();
    assign_group_colors(&mut map, ["Foo"]);
    assign_group_colors(&mut map, ["Ha"]);
    let c = group_colors(&map, ["Foo", "Ha"]);
    assert_eq!(c["Ha"], palette::HA_500);
    assert_ne!(c["Foo"], c["Ha"]);
    // Unknown names cycle through the non-physical colours only.
    let many: Vec<String> = (0..12).map(|i| format!("X{i:02}")).collect();
    let c = group_colors(&BTreeMap::new(), many.iter().map(String::as_str));
    for name in &many {
        assert!(
            GROUP_PALETTE[PHYSICAL_SLOTS..].contains(&c[name]),
            "{name} took a physical colour"
        );
    }
    assert_ne!(c["X00"], c["X01"]);
    let mut map = BTreeMap::new();
    assert!(assign_group_colors(&mut map, ["Ha", "Foo"]));
    // Persisted in `ui`, next to whatever else is there.
    let mut ui = serde_json::json!({ "other": 1 });
    save_group_colors(&mut ui, &map);
    assert_eq!(load_group_colors(&ui), map);
    assert_eq!(ui["other"], 1);
}

#[test]
fn integration_reads_hours_and_minutes() {
    assert_eq!(integration_text(7200.0), "2h 00m");
    assert_eq!(integration_text(11_100.0), "3h 05m");
    assert_eq!(integration_text(45.0), "45s");
    assert_eq!(integration_text(59.4), "59s");
    assert_eq!(integration_text(90.0), "0h 02m");
    assert_eq!(integration_text(138_000.0), "38h 20m");
    assert_eq!(scale_text(1.2149), "1.21″");
}

/// The panels' rows, top to bottom, as drawn.
fn row_order(h: &Harness<'_, State>) -> Vec<String> {
    let mut rows: Vec<(String, f32)> = h
        .query_all_by_role(Role::ListItem)
        .map(|n| {
            (
                n.accesskit_node().label().unwrap_or_default(),
                n.rect().top(),
            )
        })
        .collect();
    rows.sort_by(|a, b| a.1.total_cmp(&b.1));
    rows.into_iter().map(|(l, _)| l).collect()
}

#[test]
fn integration_sorts_within_sections() {
    let root = temp_dir("list-sort");
    let dir = root.join("panels");
    std::fs::create_dir_all(&dir).unwrap();
    let p = |name, filter: &str, cards: &[(&'static str, &'static str)]| {
        let filter = format!("'{filter}'");
        let mut all = vec![("FILTER", filter.as_str())];
        all.extend_from_slice(cards);
        panel(&dir, name, &all)
    };
    let paths = vec![
        p("Ha_a.xisf", "Ha", &[("EXPTIME", "300"), ("NCOMBINE", "24")]),
        p("Ha_b.xisf", "Ha", &[("EXPTIME", "45"), ("NCOMBINE", "1")]),
        p("Ha_c.xisf", "Ha", &[("EXPTIME", "300")]),
        p(
            "OIII_x.xisf",
            "OIII",
            &[("EXPTIME", "300"), ("NCOMBINE", "37")],
        ),
        p(
            "OIII_y.xisf",
            "OIII",
            &[("EXPTIME", "300"), ("NCOMBINE", "12")],
        ),
    ];
    let mut h = harness(session_of(&root, paths), vec![], runner());
    wait(&mut h, State::settled);
    let caller = [
        "Ha_a.xisf",
        "Ha_b.xisf",
        "Ha_c.xisf",
        "OIII_x.xisf",
        "OIII_y.xisf",
    ];
    assert_eq!(row_order(&h), caller);
    // Shortest first, as numbers ("45s" before "2h 00m"), the unknown last;
    // each group keeps its own rows.
    h.get_by_role_and_label(Role::Button, "INTEGRATION").click();
    steps(&mut h);
    assert_eq!(
        row_order(&h),
        [
            "Ha_b.xisf",
            "Ha_a.xisf",
            "Ha_c.xisf",
            "OIII_y.xisf",
            "OIII_x.xisf"
        ]
    );
    let ha = heading(&h, "Ha · 3").rect();
    let oiii = heading(&h, "OIII · 2 · 4h 05m").rect();
    assert!(ha.top() < oiii.top());
    // Longest first, the unknown still last.
    h.get_by_role_and_label(Role::Button, "INTEGRATION").click();
    steps(&mut h);
    assert_eq!(
        row_order(&h),
        [
            "Ha_a.xisf",
            "Ha_b.xisf",
            "Ha_c.xisf",
            "OIII_x.xisf",
            "OIII_y.xisf"
        ]
    );
    // The sort stays as the panels change: a panel removed.
    remove_row(&mut h, "Ha_a.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 4
    });
    assert_eq!(
        row_order(&h),
        ["Ha_b.xisf", "Ha_c.xisf", "OIII_x.xisf", "OIII_y.xisf"]
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// The texts painted on the row of panel `name` (its name included, icons
/// left out) and where, left to right.
fn cell_rects(h: &Harness<'_, State>, name: &str) -> Vec<(String, Rect)> {
    let row = h.get_by_role_and_label(Role::ListItem, name).rect();
    let mut on_row: Vec<(String, Rect)> = painted(h)
        .into_iter()
        .filter(|(t, r)| !is_icon(t) && row.contains(r.center()))
        .collect();
    on_row.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
    on_row
}

#[test]
fn the_list_fits_its_narrowest_width() {
    use mega_ui::theme::tokens::PANEL_LIST_MIN_WIDTH;
    let root = temp_dir("list-narrow");
    // Half this window is less than the narrowest list.
    let size = egui::vec2(2.0 * PANEL_LIST_MIN_WIDTH - 20.0, 900.0);
    let mut h = harness_sized(two_group_session(&root), vec![], runner(), size);
    wait(&mut h, State::settled);
    let list = h.state().w.list_rect.expect("the list was drawn");
    assert!(
        (list.width() - PANEL_LIST_MIN_WIDTH).abs() < 1.0,
        "{list:?}"
    );
    // The toolbar fits, with a keyword field.
    choose(&mut h, "Header keyword…");
    for (role, label) in [
        (Role::Button, "Add files…"),
        (Role::Button, "More"),
        (Role::ComboBox, "Group by"),
        (Role::TextInput, "Header key"),
    ] {
        let r = h.get_by_role_and_label(role, label).rect();
        assert!(list.contains_rect(r), "{label} at {r:?} not in {list:?}");
    }
    let row = h.get_by_role_and_label(Role::ListItem, "Ha_0.xisf").rect();
    assert!(list.contains_rect(row), "{row:?} not in {list:?}");
    // What shows lines up inside the row, nothing overlapping.
    let cells = cell_rects(&h, "Ha_0.xisf");
    assert!(cells.len() >= 2, "{cells:?}");
    for (t, r) in &cells {
        assert!(
            r.left() >= row.left() && r.right() <= row.right() + 0.5,
            "{t:?} at {r:?} outside {row:?}"
        );
    }
    for pair in cells.windows(2) {
        assert!(pair[0].1.right() <= pair[1].1.left(), "{pair:?} overlap");
    }
    // The actions button is in reach, inside its row.
    h.get_by_role_and_label(Role::ListItem, "Ha_0.xisf").hover();
    steps(&mut h);
    let button = h.get_by_label("Panel actions").rect();
    assert!(row.contains_rect(button), "{button:?} not in {row:?}");
    assert!(list.contains_rect(button), "{button:?} not in {list:?}");
    remove_row(&mut h, "Ha_0.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn row_ids_stay_with_their_panels() {
    let root = temp_dir("list-ids");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    h.get_by_role_and_label(Role::ListItem, "Ha_2.xisf").focus();
    steps(&mut h);
    assert!(
        h.get_by_role_and_label(Role::ListItem, "Ha_2.xisf")
            .is_focused()
    );
    // An earlier panel goes (from elsewhere: the list only shows it).
    {
        let s = h.state_mut();
        let mut edit = PanelsEdit::of(&s.session);
        edit.panels.remove(0);
        assert!(matches!(
            edit.apply(&mut s.session, &mut s.jobs),
            Applied::Done
        ));
    }
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    assert!(
        h.get_by_role_and_label(Role::ListItem, "Ha_2.xisf")
            .is_focused(),
        "the focus stays with its panel"
    );
    assert!(
        !h.get_by_role_and_label(Role::ListItem, "Ha_3.xisf")
            .is_focused()
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_canvas_keeps_its_room() {
    use mega_ui::inspector::inspector_width;
    use mega_ui::theme::tokens::CANVAS_MIN_WIDTH;
    let root = temp_dir("list-max");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    let before = h.state().w.list_rect.expect("the list was drawn");
    // Drag the list's edge almost to the window's right edge.
    // The resize handle straddles the edge; grab it just inside.
    let edge = egui::pos2(before.right() - 2.0, before.center().y);
    let far = egui::pos2(WINDOW.x - 10.0, edge.y);
    h.hover_at(edge);
    steps(&mut h);
    h.drag_at(edge);
    steps(&mut h);
    h.hover_at(far);
    steps(&mut h);
    h.drop_at(far);
    steps(&mut h);
    let after = h.state().w.list_rect.expect("the list was drawn");
    assert!(after.width() > before.width() + 1.0, "{before:?} {after:?}");
    let inspector = inspector_width(h.state().w.inspector());
    let limit = WINDOW.x - inspector - CANVAS_MIN_WIDTH;
    assert!(after.right() <= limit + 1.0, "{after:?} past {limit}");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rows_read_dashes_until_their_header_is_read() {
    let list = PanelList::new(Arc::new(StubFiles(vec![])));
    let row = list.row(Path::new("panels/p.xisf"), None);
    assert_eq!(row.cells, ["p.xisf", "Reading…", "—", "—", "—"]);
    assert_eq!(row.problem, None);
}

#[test]
fn rows_show_their_problems() {
    use mmm_core::checks::{Check, CheckCode, Severity};
    let root = temp_dir("list-problems");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    assert!(
        h.query_by_role_and_label(Role::ListItem, "Ha_3.xisf")
            .is_none(),
        "rolled up"
    );
    let ha_3 = h.state().session.mosaic.config.panels[3].path.clone();
    assert_eq!(name_of(&ha_3), "Ha_3.xisf");
    let check = Check {
        code: CheckCode::SolutionPresent,
        severity: Severity::Problem,
        panels: vec![ha_3],
        groups: vec!["Ha".into()],
        message: "Ha_3.xisf has no astrometric solution.".into(),
        remedy: Some("Plate-solve it, then add it again.".into()),
        target: mmm_core::checks::CheckTarget::Session,
        rect: None,
    };
    h.state_mut().checks.set_report(ChecksReport {
        checks: vec![check],
        errors: Vec::new(),
        seams: Default::default(),
    });
    steps(&mut h);
    // The rolled-up section shows the row with the problem, its ⚠ telling
    // what it is.
    let row = h.get_by_role_and_label(Role::ListItem, "Ha_3.xisf").rect();
    let tip = h
        .get_by_label("Ha_3.xisf has no astrometric solution.\nPlate-solve it, then add it again.");
    assert!(
        row.contains_rect(tip.rect()),
        "{:?} not in {row:?}",
        tip.rect()
    );
    assert_eq!(rows(&h), 2 * ROLL_UP_ROWS + 1);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// Click panel `name`'s row with `modifiers` held.
fn click_row(h: &mut Harness<'static, State>, name: &str, modifiers: egui::Modifiers) {
    h.get_by_role_and_label(Role::ListItem, name)
        .click_modifiers(modifiers);
    steps(h);
}

/// Open panel `name`'s actions menu.
fn open_menu(h: &mut Harness<'static, State>, name: &str) {
    h.get_by_role_and_label(Role::ListItem, name).hover();
    steps(h);
    h.get_by_label("Panel actions").click();
    steps(h);
}

/// Move panel `name` (with the selection, if it is selected) to `target`
/// through its menu: a group, "No group" or "Let the rule decide".
fn move_row(h: &mut Harness<'static, State>, name: &str, target: &str) {
    open_menu(h, name);
    h.get_by_role_and_label(Role::Button, "Move to").click();
    steps(h);
    h.get_by_role_and_label(Role::Button, target).click();
    steps(h);
}

/// The selected panels' names, sorted.
fn selection(h: &Harness<'static, State>) -> Vec<String> {
    let list = h.state().list();
    let mut names: Vec<String> = list.selection().iter().map(|p| name_of(p)).collect();
    names.sort();
    names
}

/// Whether panel `name`'s row reads as selected.
fn is_selected(h: &Harness<'static, State>, name: &str) -> bool {
    h.get_by_role_and_label(Role::ListItem, name)
        .accesskit_node()
        .is_selected()
        == Some(true)
}

/// Panel `name`'s manual group in the session.
fn manual(s: &State, name: &str) -> Option<String> {
    let panels = &s.session.mosaic.config.panels;
    let p = panels.iter().find(|p| name_of(&p.path) == name);
    p.unwrap_or_else(|| panic!("no panel {name}"))
        .manual_group
        .clone()
}

/// Drag from `from` to `to` (screen), as a pointer does.
fn drag(h: &mut Harness<'static, State>, from: egui::Pos2, to: egui::Pos2) {
    h.drag_at(from);
    steps(h);
    h.hover_at(from + egui::vec2(0.0, mega_ui::theme::tokens::space::L));
    steps(h);
    h.hover_at(to);
    steps(h);
    h.drop_at(to);
    steps(h);
}

/// Panel `path`'s centre on the plan, as last drawn.
fn on_plan(h: &Harness<'static, State>, path: &Path) -> egui::Pos2 {
    let plan = h.state().w.plan();
    let items = plan.items();
    let it = items.iter().find(|i| i.path == path).expect("drawn").rect;
    plan.to_screen(((it[0] + it[2]) / 2.0, (it[1] + it[3]) / 2.0))
        .unwrap()
}

/// Click panel `path` on the plan.
fn click_plan(h: &mut Harness<'static, State>, path: &Path) {
    let p = on_plan(h, path);
    h.hover_at(p);
    h.drag_at(p);
    h.event(egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    steps(h);
}

/// The fill painted behind panel `name`'s row, if any.
fn row_fill(h: &Harness<'static, State>, name: &str) -> Option<egui::Color32> {
    let row = h.get_by_role_and_label(Role::ListItem, name).rect();
    let near = |r: Rect| (r.min - row.min).length() < 0.5 && (r.max - row.max).length() < 0.5;
    h.output().shapes.iter().find_map(|c| match &c.shape {
        egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT && near(r.rect) => {
            Some(r.fill)
        }
        _ => None,
    })
}

/// Run the undo of the `i`th toast raised.
fn undo_toast(h: &mut Harness<'static, State>, i: usize) {
    let s = h.state_mut();
    let undo = s.toasts[i].undo.clone().expect("an undo");
    s.w.undo(undo, &mut s.session, &mut s.jobs);
}

#[test]
fn shift_and_ctrl_extend_the_selection() {
    use egui::Modifiers;
    let root = temp_dir("list-select");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "Ha_1.xisf", Modifiers::NONE);
    assert_eq!(selection(&h), ["Ha_1.xisf"]);
    assert!(is_selected(&h, "Ha_1.xisf"));
    // Shift: the rows on screen from the anchor (Ha_3 is rolled up).
    click_row(&mut h, "OIII_1.xisf", Modifiers::SHIFT);
    assert_eq!(
        selection(&h),
        ["Ha_1.xisf", "Ha_2.xisf", "OIII_0.xisf", "OIII_1.xisf"]
    );
    assert!(is_selected(&h, "OIII_0.xisf") && !is_selected(&h, "Ha_0.xisf"));
    // Ctrl (Cmd): one row in or out.
    click_row(&mut h, "Ha_2.xisf", Modifiers::COMMAND);
    click_row(&mut h, "Ha_0.xisf", Modifiers::CTRL);
    assert_eq!(
        selection(&h),
        ["Ha_0.xisf", "Ha_1.xisf", "OIII_0.xisf", "OIII_1.xisf"]
    );
    assert!(!is_selected(&h, "Ha_2.xisf"));
    // Shift from the new anchor, adding with Ctrl held.
    click_row(&mut h, "OIII_2.xisf", Modifiers::SHIFT | Modifiers::COMMAND);
    assert_eq!(selection(&h).len(), 6, "{:?}", selection(&h));
    // A plain click selects that row only (and the panel on the plan).
    click_row(&mut h, "OIII_2.xisf", Modifiers::NONE);
    assert_eq!(selection(&h), ["OIII_2.xisf"]);
    let oiii_2 = h.state().session.mosaic.config.panels[6].path.clone();
    assert_eq!(h.state().w.plan().selected(), Some(oiii_2.as_path()));
    // Esc clears it.
    h.key_press(egui::Key::Escape);
    steps(&mut h);
    assert!(selection(&h).is_empty());
    assert!(!is_selected(&h, "OIII_2.xisf"));
    assert_eq!(h.state().w.plan().selected(), None);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn move_to_puts_rows_in_a_group_by_hand() {
    use egui::Modifiers;
    use mmm_core::mosaic::UNGROUPED;
    let root = temp_dir("list-move");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "Ha_0.xisf", Modifiers::NONE);
    click_row(&mut h, "Ha_1.xisf", Modifiers::COMMAND);
    // The menu of a selected row moves the selection.
    move_row(&mut h, "Ha_0.xisf", "OIII");
    wait(&mut h, |s| s.settled() && manual(s, "Ha_1.xisf").is_some());
    let s = h.state();
    assert_eq!(manual(s, "Ha_0.xisf").as_deref(), Some("OIII"));
    assert_eq!(manual(s, "Ha_1.xisf").as_deref(), Some("OIII"));
    assert_eq!(on_disk(s).panels, s.session.mosaic.config.panels, "saved");
    heading(&h, "OIII · 6");
    heading(&h, "Ha · 2");
    // Tagged by hand; the FILTER cell keeps the header's value.
    assert_eq!(cells(&h, "Ha_0.xisf")[..2], ["by hand", "Ha"]);
    assert_eq!(cells(&h, "OIII_0.xisf")[0], "OIII", "no tag");
    let ha_0 = h.get_by_role_and_label(Role::ListItem, "Ha_0.xisf").rect();
    assert!(ha_0.top() > heading(&h, "OIII · 6").rect().top());
    // The selection moved with them.
    assert_eq!(selection(&h), ["Ha_0.xisf", "Ha_1.xisf"]);
    // An unselected row's menu moves that row only: to No group.
    move_row(&mut h, "OIII_0.xisf", "No group");
    wait(&mut h, |s| {
        s.settled() && manual(s, "OIII_0.xisf").is_some()
    });
    assert_eq!(manual(h.state(), "OIII_0.xisf").as_deref(), Some(UNGROUPED));
    assert_eq!(manual(h.state(), "Ha_0.xisf").as_deref(), Some("OIII"));
    heading(&h, "No group · 1");
    assert_eq!(warning(&h, "No group · 1"), None, "put there by hand");
    assert_eq!(cells(&h, "OIII_0.xisf")[..2], ["by hand", "OIII"]);
    // Let the rule decide: back where the FILTER header puts it.
    move_row(&mut h, "Ha_0.xisf", "Let the rule decide");
    wait(&mut h, |s| s.settled() && manual(s, "Ha_1.xisf").is_none());
    assert_eq!(manual(h.state(), "Ha_0.xisf"), None);
    heading(&h, "Ha · 4");
    assert_eq!(cells(&h, "Ha_0.xisf")[0], "Ha", "no tag");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_group_takes_the_selection() {
    use egui::Modifiers;
    let root = temp_dir("list-new-group");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "OIII_0.xisf", Modifiers::NONE);
    click_row(&mut h, "OIII_1.xisf", Modifiers::COMMAND);
    open_menu(&mut h, "OIII_1.xisf");
    h.get_by_role_and_label(Role::Button, "Move to").click();
    steps(&mut h);
    h.get_by_role_and_label(Role::Button, "New group…").click();
    steps(&mut h);
    // A name typed in the menu itself.
    h.get_by_role_and_label(Role::TextInput, "New group name")
        .focus();
    steps(&mut h);
    h.get_by_role_and_label(Role::TextInput, "New group name")
        .type_text("Red");
    steps(&mut h);
    assert!(manual(h.state(), "OIII_0.xisf").is_none(), "on Enter only");
    h.key_press(egui::Key::Enter);
    wait(&mut h, |s| {
        s.settled() && manual(s, "OIII_1.xisf").is_some()
    });
    let s = h.state();
    assert_eq!(manual(s, "OIII_0.xisf").as_deref(), Some("Red"));
    assert_eq!(manual(s, "OIII_1.xisf").as_deref(), Some("Red"));
    assert_eq!(manual(s, "OIII_2.xisf"), None);
    assert_eq!(on_disk(s).panels, s.session.mosaic.config.panels);
    heading(&h, "Red · 2");
    heading(&h, "OIII · 2");
    assert!(h.query_by_label("New group name").is_none(), "menu closed");
    // The new group has its colour.
    let colors = load_group_colors(&h.state().session.mosaic.config.ui);
    assert!(colors.contains_key("Red"), "{colors:?}");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dropping_rows_on_a_heading_moves_them() {
    use egui::Modifiers;
    let root = temp_dir("list-drop");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "Ha_0.xisf", Modifiers::NONE);
    click_row(&mut h, "Ha_1.xisf", Modifiers::COMMAND);
    let row = |h: &Harness<'static, State>, name: &str| {
        h.get_by_role_and_label(Role::ListItem, name)
            .rect()
            .center()
    };
    // A selected row carries the selection.
    let (from, to) = (
        row(&h, "Ha_0.xisf"),
        heading(&h, "OIII · 4").rect().center(),
    );
    drag(&mut h, from, to);
    wait(&mut h, |s| s.settled() && manual(s, "Ha_1.xisf").is_some());
    assert_eq!(manual(h.state(), "Ha_0.xisf").as_deref(), Some("OIII"));
    assert_eq!(manual(h.state(), "Ha_1.xisf").as_deref(), Some("OIII"));
    heading(&h, "OIII · 6");
    assert_eq!(cells(&h, "Ha_1.xisf")[..2], ["by hand", "Ha"]);
    // A row dropped on its own section does not move.
    let edits = h.state().session.mosaic.config.provenance.len();
    let (from, to) = (
        row(&h, "OIII_0.xisf"),
        heading(&h, "OIII · 6").rect().center(),
    );
    drag(&mut h, from, to);
    wait(&mut h, State::settled);
    assert_eq!(manual(h.state(), "OIII_0.xisf"), None);
    assert_eq!(h.state().session.mosaic.config.provenance.len(), edits);
    // An unselected row carries itself.
    let (from, to) = (
        row(&h, "OIII_0.xisf"),
        heading(&h, "Ha · 2").rect().center(),
    );
    drag(&mut h, from, to);
    wait(&mut h, |s| {
        s.settled() && manual(s, "OIII_0.xisf").is_some()
    });
    assert_eq!(manual(h.state(), "OIII_0.xisf").as_deref(), Some("Ha"));
    assert_eq!(manual(h.state(), "OIII_1.xisf"), None);
    heading(&h, "Ha · 3");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn remove_offers_undo() {
    use egui::Modifiers;
    use mmm_core::mosaic::UNGROUPED;
    let root = temp_dir("list-remove-undo");
    let mut session = two_group_session(&root);
    // OIII_0 is pinned to its group by hand.
    session.mosaic.config.panels[4].manual_group = Some("OIII".into());
    session.mosaic.save().unwrap();
    let before = session.mosaic.config.panels.clone();
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "Ha_0.xisf", Modifiers::NONE);
    click_row(&mut h, "OIII_0.xisf", Modifiers::COMMAND);
    remove_row(&mut h, "OIII_0.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 6
    });
    for name in ["Ha_0.xisf", "OIII_0.xisf"] {
        let row = h.query_by_role_and_label(Role::ListItem, name);
        assert!(row.is_none(), "{name} shows");
    }
    assert_eq!(on_disk(h.state()).panels.len(), 6);
    let toast = &h.state().toasts[0];
    assert_eq!(toast.toast.title, "Removed 2 panels");
    assert_eq!(toast.toast.action.as_deref(), Some("Undo"));
    assert!(matches!(&toast.undo, Some(Undo::Readd(r)) if r.len() == 2));
    // A later edit of another panel...
    move_row(&mut h, "Ha_1.xisf", "No group");
    wait(&mut h, |s| s.settled() && manual(s, "Ha_1.xisf").is_some());
    // ...survives the Undo, which puts back only what it removed: where
    // they were, as they were.
    undo_toast(&mut h, 0);
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 8
    });
    let mut want = before;
    want[1].manual_group = Some(UNGROUPED.into());
    assert_eq!(h.state().session.mosaic.config.panels, want);
    assert_eq!(on_disk(h.state()).panels, want);
    h.get_by_role_and_label(Role::ListItem, "Ha_0.xisf");
    assert_eq!(cells(&h, "OIII_0.xisf")[..2], ["by hand", "OIII"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn folding_a_section_deselects_its_rows() {
    use egui::Modifiers;
    let root = temp_dir("list-fold-select");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "Ha_0.xisf", Modifiers::NONE);
    click_row(&mut h, "Ha_1.xisf", Modifiers::COMMAND);
    click_row(&mut h, "OIII_1.xisf", Modifiers::COMMAND);
    assert_eq!(selection(&h), ["Ha_0.xisf", "Ha_1.xisf", "OIII_1.xisf"]);
    // Rows folded away are no longer selected: Delete acts on what shows.
    heading(&h, "Ha · 4").click();
    steps(&mut h);
    assert_eq!(selection(&h), ["OIII_1.xisf"]);
    h.key_press(egui::Key::Delete);
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    let names = h.state().panel_names();
    assert!(names.iter().any(|n| n == "Ha_0.xisf"), "{names:?}");
    assert!(!names.iter().any(|n| n == "OIII_1.xisf"), "{names:?}");
    // Unfolding leaves the selection as it is.
    heading(&h, "Ha · 4").click();
    steps(&mut h);
    assert!(selection(&h).is_empty());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn delete_removes_the_selection() {
    use egui::Modifiers;
    let root = temp_dir("list-delete");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    click_row(&mut h, "Ha_0.xisf", Modifiers::NONE);
    click_row(&mut h, "Ha_2.xisf", Modifiers::COMMAND);
    // Not while a text field has the keyboard.
    choose(&mut h, "Header keyword…");
    h.get_by_role_and_label(Role::TextInput, "Header key")
        .focus();
    steps(&mut h);
    h.key_press(egui::Key::Delete);
    steps(&mut h);
    h.key_press(egui::Key::Escape);
    steps(&mut h);
    assert_eq!(h.state().session.mosaic.config.panels.len(), 8);
    assert_eq!(selection(&h), ["Ha_0.xisf", "Ha_2.xisf"]);
    h.key_press(egui::Key::Delete);
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 6
    });
    let left = ["Ha_1", "Ha_3", "OIII_0", "OIII_1", "OIII_2", "OIII_3"];
    let left: Vec<String> = left.iter().map(|n| format!("{n}.xisf")).collect();
    assert_eq!(h.state().panel_names(), left);
    let toast = h.state().toasts.last().expect("a toast");
    assert_eq!(toast.toast.title, "Removed 2 panels");
    assert!(selection(&h).is_empty());
    // Nothing selected: Delete does nothing.
    h.key_press(egui::Key::Delete);
    steps(&mut h);
    assert_eq!(h.state().session.mosaic.config.panels.len(), 6);
    assert_eq!(h.state().toasts.len(), 1);
    // Nor while the list is closed.
    click_row(&mut h, "Ha_1.xisf", Modifiers::NONE);
    h.state_mut().w.panel_list_open = false;
    steps(&mut h);
    h.key_press(egui::Key::Delete);
    steps(&mut h);
    assert_eq!(h.state().session.mosaic.config.panels.len(), 6);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_row_click_selects_the_panel_in_the_plan() {
    let (root, session, jobs) = crate::jobs::tests::analyzed("list-row-plan");
    let ha_1 = session.mosaic.config.panels[1].path.clone();
    let ha_3 = session.mosaic.config.panels[3].path.clone();
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| s.settled() && !s.w.plan().items().is_empty());
    click_row(&mut h, "Ha_1.xisf", egui::Modifiers::NONE);
    assert_eq!(h.state().w.plan().selected(), Some(ha_1.as_path()));
    // Its details show in the inspector's Panels section.
    assert!(h.state().w.inspector().is_open("panels"));
    h.get_by_label(&ha_1.display().to_string());
    // A click on the plan selects its row, brought into view (Ha_3 was
    // rolled up).
    let hidden = h.query_by_role_and_label(Role::ListItem, "Ha_3.xisf");
    assert!(hidden.is_none());
    click_plan(&mut h, &ha_3);
    assert_eq!(h.state().w.plan().selected(), Some(ha_3.as_path()));
    assert_eq!(selection(&h), ["Ha_3.xisf"]);
    assert!(is_selected(&h, "Ha_3.xisf"));
    assert!(!is_selected(&h, "Ha_1.xisf"));
    let list = h.state().w.list_rect.unwrap();
    let row = h.get_by_role_and_label(Role::ListItem, "Ha_3.xisf").rect();
    assert!(list.contains_rect(row), "{row:?} not in {list:?}");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_row_click_shows_its_group_on_the_plan() {
    use crate::compositions::ViewKind;
    let (root, session, jobs) = crate::jobs::tests::analyzed("list-row-group");
    let oiii_1 = session.mosaic.config.panels[5].path.clone();
    assert_eq!(name_of(&oiii_1), "OIII_1.xisf");
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| s.settled() && !s.w.plan().items().is_empty());
    let ha = ViewKind::Group("Ha".into());
    assert_eq!(h.state().w.view(), Some(&ha));
    // Pointing at a row of another group changes nothing.
    h.get_by_role_and_label(Role::ListItem, "OIII_1.xisf")
        .hover();
    steps(&mut h);
    assert_eq!(h.state().w.view(), Some(&ha));
    assert!(h.state().w.plan().items().iter().all(|i| !i.highlighted));
    // A click shows its group, the panel selected there.
    click_row(&mut h, "OIII_1.xisf", egui::Modifiers::NONE);
    let oiii = ViewKind::Group("OIII".into());
    assert_eq!(h.state().w.view(), Some(&oiii));
    assert_eq!(h.state().w.plan().selected(), Some(oiii_1.as_path()));
    wait(&mut h, |s| {
        s.w.plan().items().iter().any(|i| i.path == oiii_1)
    });
    assert_eq!(selection(&h), ["OIII_1.xisf"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hover_links_rows_and_outlines() {
    let (root, session, jobs) = crate::jobs::tests::analyzed("list-hover");
    let ha_2 = session.mosaic.config.panels[2].path.clone();
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| s.settled() && !s.w.plan().items().is_empty());
    let lit = |h: &Harness<'static, State>| -> Vec<String> {
        let items = h.state().w.plan().items();
        let lit = items.iter().filter(|i| i.highlighted);
        lit.map(|i| name_of(&i.path)).collect()
    };
    assert!(lit(&h).is_empty());
    // A row's panel lights up on the plan.
    h.get_by_role_and_label(Role::ListItem, "Ha_1.xisf").hover();
    steps(&mut h);
    assert_eq!(lit(&h), ["Ha_1.xisf"]);
    // Only panels the plan shows (it shows Ha).
    h.get_by_role_and_label(Role::ListItem, "OIII_1.xisf")
        .hover();
    steps(&mut h);
    assert!(lit(&h).is_empty());
    // A panel pointed at on the plan lights up its row.
    let p = on_plan(&h, &ha_2);
    h.hover_at(p);
    steps(&mut h);
    assert!(lit(&h).is_empty(), "no row is hovered");
    assert!(row_fill(&h, "Ha_2.xisf").is_some(), "Ha_2's row lit");
    for name in ["Ha_0.xisf", "Ha_1.xisf", "OIII_0.xisf"] {
        assert_eq!(row_fill(&h, name), None, "{name} lit");
    }
    // Pointing elsewhere: nothing lit.
    h.hover_at(egui::pos2(WINDOW.x - 10.0, WINDOW.y - 10.0));
    steps(&mut h);
    assert_eq!(row_fill(&h, "Ha_2.xisf"), None);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn removing_the_selected_panel_clears_the_selection() {
    use egui::Modifiers;
    let (root, session, jobs) = crate::jobs::tests::analyzed("list-remove-selected");
    let before = session.mosaic.config.panels.clone();
    let ha_1 = before[1].path.clone();
    let mut h = harness(session, vec![], jobs);
    wait(&mut h, |s| s.settled() && !s.w.plan().items().is_empty());
    click_row(&mut h, "Ha_1.xisf", Modifiers::NONE);
    assert_eq!(h.state().w.plan().selected(), Some(ha_1.as_path()));
    remove_row(&mut h, "Ha_1.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    assert!(selection(&h).is_empty());
    assert_eq!(h.state().w.plan().selected(), None);
    let shown = ha_1.display().to_string();
    assert!(h.query_by_label(&shown).is_none(), "its details are gone");
    // Every panel: open OIII in full, select from first to last, Delete.
    h.get_by_label("… 1 more").click();
    steps(&mut h);
    click_row(&mut h, "Ha_0.xisf", Modifiers::NONE);
    click_row(&mut h, "OIII_3.xisf", Modifiers::SHIFT);
    assert_eq!(selection(&h).len(), 7);
    h.key_press(egui::Key::Delete);
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.is_empty()
    });
    assert!(selection(&h).is_empty());
    assert_eq!(h.state().w.plan().selected(), None);
    assert_eq!(h.state().toasts[1].toast.title, "Removed 7 panels");
    h.get_by_label("No panels yet: add files or folders, or drop them anywhere on the window.");
    // Undo puts them back, then the first removal's Undo puts Ha_1 back
    // in its place.
    undo_toast(&mut h, 1);
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    assert!(selection(&h).is_empty());
    undo_toast(&mut h, 0);
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 8
    });
    assert_eq!(h.state().session.mosaic.config.panels, before);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// Pick `choice` in the **Group by** dropdown.
fn choose(h: &mut Harness<'static, State>, choice: &str) {
    h.get_by_role_and_label(Role::ComboBox, "Group by").click();
    steps(h);
    h.get_by_role_and_label(Role::Button, choice).click();
    steps(h);
}

/// The **Group by** dropdown's choice as shown.
fn group_by_shows(h: &Harness<'static, State>) -> String {
    let combo = h.get_by_role_and_label(Role::ComboBox, "Group by");
    combo.value().unwrap_or_default()
}

/// Wait until the session's grouping is `want`, applied and resolved, and
/// check it was saved, with the user's choice noted.
fn wait_grouping(h: &mut Harness<'static, State>, want: Grouping) {
    wait(h, |s| {
        s.settled() && s.session.mosaic.config.grouping == want
    });
    let saved = on_disk(h.state());
    assert_eq!(saved.grouping, want);
    assert!(grouping_chosen(&saved.ui), "the choice is noted");
}

/// Replace the text of the toolbar's keyword field `label` with `typed`
/// and press Enter.
fn type_keyword(h: &mut Harness<'static, State>, label: &str, typed: &str) {
    h.get_by_role_and_label(Role::TextInput, label).focus();
    h.step();
    h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
    h.step();
    h.get_by_role_and_label(Role::TextInput, label)
        .type_text(typed);
    h.step();
    h.key_press(egui::Key::Enter);
    steps(h);
}

fn header(key: &str) -> Grouping {
    Grouping {
        enabled: true,
        rule: GroupRule::Header { key: key.into() },
    }
}

#[test]
fn group_by_switches_rules() {
    let root = temp_dir("list-rules");
    let data = root.join("data");
    let mut paths = Vec::new();
    // The folders name the filters one way, the headers another.
    for (folder, filter, names) in [
        ("FILTER_Ha", "'Halpha'", ["p1.xisf", "p2.xisf"]),
        ("FILTER_OIII", "'O3'", ["p3.xisf", "p4.xisf"]),
    ] {
        let dir = data.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        for n in names {
            paths.push(panel(&dir, n, &[("FILTER", filter), ("OBJECT", "'M42'")]));
        }
    }
    let mut h = harness(session_of(&root, paths), vec![], runner());
    wait(&mut h, State::settled);
    assert_eq!(group_by_shows(&h), "FILTER header");
    heading(&h, "Halpha · 2");
    heading(&h, "O3 · 2");
    assert_eq!(h.query_all_by_role(Role::TextInput).count(), 0, "no field");
    // Path keyword: the token after FILTER in the path.
    choose(&mut h, "Path keyword…");
    wait_grouping(
        &mut h,
        Grouping {
            enabled: true,
            rule: GroupRule::PathKeyword {
                keyword: "FILTER".into(),
            },
        },
    );
    assert_eq!(group_by_shows(&h), "Path keyword…");
    let field = h.get_by_role_and_label(Role::TextInput, "Path keyword");
    assert_eq!(field.value().as_deref(), Some("FILTER"));
    heading(&h, "Ha · 2");
    heading(&h, "OIII · 2");
    // Header keyword…: a header key to type, FILTER at first.
    choose(&mut h, "Header keyword…");
    wait_grouping(&mut h, header("FILTER"));
    assert_eq!(group_by_shows(&h), "Header keyword…");
    let field = h.get_by_role_and_label(Role::TextInput, "Header key");
    assert_eq!(field.value().as_deref(), Some("FILTER"));
    type_keyword(&mut h, "Header key", "OBJECT");
    wait_grouping(&mut h, header("OBJECT"));
    heading(&h, "M42 · 4");
    // By hand: nothing placed until panels are moved.
    choose(&mut h, "By hand");
    wait_grouping(
        &mut h,
        Grouping {
            enabled: true,
            rule: GroupRule::Manual,
        },
    );
    assert_eq!(
        warning(&h, "No group · 4").as_deref(),
        Some("not placed by hand")
    );
    assert_eq!(h.query_all_by_role(Role::TextInput).count(), 0, "no field");
    // Don't group: one flat list.
    choose(&mut h, "Don't group");
    wait(&mut h, |s| {
        s.settled() && !s.session.mosaic.config.grouping.enabled
    });
    assert!(!on_disk(h.state()).grouping.enabled);
    assert_eq!(group_by_shows(&h), "Don't group");
    assert_eq!(rows(&h), 4);
    assert!(!has_heading(&h, "No group · 4"));
    // FILTER header again.
    choose(&mut h, "FILTER header");
    wait_grouping(&mut h, header("FILTER"));
    heading(&h, "Halpha · 2");
    heading(&h, "O3 · 2");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_legacy_filename_token_session_keeps_its_groups() {
    let root = temp_dir("list-legacy");
    let dir = root.join("panels");
    std::fs::create_dir_all(&dir).unwrap();
    let paths = [
        "M42_filter_Ha-300s_1.xisf",
        "M42_filter_Ha-300s_2.xisf",
        "M42_filter_OIII-300s_1.xisf",
    ]
    .map(|n| panel(&dir, n, &[("FILTER", "'L'")]))
    .to_vec();
    let mut cfg = MosaicConfig::new(paths);
    let legacy = Grouping {
        enabled: true,
        rule: GroupRule::FilenameToken {
            keyword: "filter".into(),
        },
    };
    cfg.grouping = legacy.clone();
    let session = open(Mosaic::create(&root.join("p.mmm-session"), cfg).unwrap());
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    // Grouped as before: the file name's `_` token (not the path keyword's
    // "Ha").
    heading(&h, "Ha-300s · 2");
    heading(&h, "OIII-300s · 1");
    // Shown as a path keyword, its keyword kept; nothing rewritten.
    assert_eq!(group_by_shows(&h), "Path keyword…");
    let field = h.get_by_role_and_label(Role::TextInput, "Path keyword");
    assert_eq!(field.value().as_deref(), Some("filter"));
    let saved = on_disk(h.state());
    assert_eq!(saved.grouping, legacy);
    assert!(saved.provenance.is_empty(), "no edit");
    // Picking the choice shown changes nothing, but notes the choice.
    choose(&mut h, "Path keyword…");
    wait(&mut h, |s| s.settled() && grouping_chosen(&on_disk(s).ui));
    let s = h.state();
    assert_eq!(s.session.mosaic.config.grouping, legacy);
    let saved = on_disk(s);
    assert_eq!(saved.grouping, legacy);
    assert!(saved.provenance.is_empty(), "no edit");
    heading(&h, "Ha-300s · 2");
    // Editing the keyword stores a path keyword rule.
    type_keyword(&mut h, "Path keyword", "FILTER");
    wait_grouping(
        &mut h,
        Grouping {
            enabled: true,
            rule: GroupRule::PathKeyword {
                keyword: "FILTER".into(),
            },
        },
    );
    heading(&h, "Ha · 2");
    heading(&h, "OIII · 1");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_header_key_in_another_case_is_kept() {
    let root = temp_dir("list-key-case");
    let mut session = two_group_session(&root);
    // A legacy session's lower-case key.
    let legacy = header("filter");
    session.mosaic.config.grouping = legacy.clone();
    session.mosaic.save().unwrap();
    session.groups = session.mosaic.groups().ok();
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    assert_eq!(group_by_shows(&h), "FILTER header");
    let edits = on_disk(h.state()).provenance.len();
    let unchanged = |h: &Harness<'static, State>| {
        let s = h.state();
        assert_eq!(s.session.mosaic.config.grouping, legacy);
        let saved = on_disk(s);
        assert_eq!(saved.grouping, legacy);
        assert_eq!(saved.provenance.len(), edits, "no edit");
    };
    // The same rule whatever the case: picking Header keyword… changes
    // nothing (the analysis stays).
    choose(&mut h, "Header keyword…");
    wait(&mut h, |s| s.settled() && grouping_chosen(&on_disk(s).ui));
    assert_eq!(group_by_shows(&h), "Header keyword…");
    unchanged(&h);
    // Nor does typing it in capitals, nor FILTER header.
    type_keyword(&mut h, "Header key", "FILTER");
    wait(&mut h, State::settled);
    unchanged(&h);
    choose(&mut h, "FILTER header");
    wait(&mut h, State::settled);
    assert_eq!(group_by_shows(&h), "FILTER header");
    unchanged(&h);
    heading(&h, "Ha · 4");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn filters_group_themselves_once() {
    let root = temp_dir("list-auto");
    let mut session = two_group_session(&root);
    session.mosaic.config.grouping.enabled = false;
    session.mosaic.save().unwrap();
    session.groups = session.mosaic.groups().ok();
    let mut h = harness(session, vec![], runner());
    // Two FILTER values, grouping off: grouped by FILTER, in one edit.
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.grouping.enabled
    });
    let s = h.state();
    assert_eq!(s.session.mosaic.config.grouping, header("FILTER"));
    let saved = on_disk(s);
    assert_eq!(saved.grouping, header("FILTER"));
    assert_eq!(saved.provenance.len(), 1, "one edit");
    assert!(!grouping_chosen(&saved.ui), "not the user's choice");
    assert_eq!(s.toasts.len(), 1);
    let toast = &s.toasts[0];
    assert_eq!(toast.toast.title, "Grouped by FILTER: 2 filters");
    assert_eq!(toast.toast.action.as_deref(), Some("Undo"));
    assert!(matches!(&toast.undo, Some(Undo::Grouping(g)) if !g.enabled));
    assert_eq!(group_by_shows(&h), "FILTER header");
    heading(&h, "Ha · 4");
    heading(&h, "OIII · 4");
    // A later edit of the panels...
    remove_row(&mut h, "OIII_0.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 7
    });
    // ...survives the Undo, which restores only the grouping, and notes the
    // choice.
    undo_toast(&mut h, 0);
    wait(&mut h, |s| {
        s.settled() && !s.session.mosaic.config.grouping.enabled
    });
    let saved = on_disk(h.state());
    assert!(!saved.grouping.enabled);
    assert_eq!(saved.panels.len(), 7);
    assert!(grouping_chosen(&saved.ui));
    assert_eq!(group_by_shows(&h), "Don't group");
    // The next resolution (another panel removed) doesn't group again.
    remove_row(&mut h, "OIII_1.xisf");
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.panels.len() == 6
    });
    steps(&mut h);
    assert!(!h.state().session.mosaic.config.grouping.enabled);
    let titles: Vec<&str> = h
        .state()
        .toasts
        .iter()
        .map(|t| t.toast.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Grouped by FILTER: 2 filters",
            "Removed 1 panel",
            "Removed 1 panel"
        ]
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rule_that_places_nothing_explains_why() {
    let root = temp_dir("list-nothing");
    let mut h = harness(two_group_session(&root), vec![], runner());
    wait(&mut h, State::settled);
    // No FILTER in these paths: every panel in No group, saying why.
    choose(&mut h, "Path keyword…");
    wait_grouping(
        &mut h,
        Grouping {
            enabled: true,
            rule: GroupRule::PathKeyword {
                keyword: "FILTER".into(),
            },
        },
    );
    assert_eq!(
        warning(&h, "No group · 8").as_deref(),
        Some("no FILTER in the path")
    );
    assert!(!has_heading(&h, "Ha · 4"));
    // FILTER header puts them back in their groups.
    choose(&mut h, "FILTER header");
    wait_grouping(&mut h, header("FILTER"));
    heading(&h, "Ha · 4");
    heading(&h, "OIII · 4");
    assert!(!has_heading(&h, "No group · 8"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn conflicting_path_values_say_so() {
    let root = temp_dir("list-conflict");
    let data = root.join("data");
    let dir = data.join("FILTER_Ha");
    std::fs::create_dir_all(&dir).unwrap();
    // The folder says Ha, the names say otherwise.
    let paths = vec![
        panel(&dir, "M42_FILTER_OIII_1.xisf", &[("FILTER", "'Ha'")]),
        panel(&dir, "M42_FILTER_SII_2.xisf", &[("FILTER", "'Ha'")]),
    ];
    let mut h = harness(session_of(&root, paths), vec![], runner());
    wait(&mut h, State::settled);
    choose(&mut h, "Path keyword…");
    wait_grouping(
        &mut h,
        Grouping {
            enabled: true,
            rule: GroupRule::PathKeyword {
                keyword: "FILTER".into(),
            },
        },
    );
    assert_eq!(
        warning(&h, "No group · 2").as_deref(),
        Some("no single FILTER value in the path")
    );
    // A panel with no FILTER in its path too: both reasons.
    let plain = panel(&root, "M42_3.xisf", &[("FILTER", "'Ha'")]);
    let reason = |paths: &[&Path]| {
        no_group_reason(
            &GroupRule::PathKeyword {
                keyword: "FILTER".into(),
            },
            paths.iter().copied(),
        )
    };
    assert_eq!(reason(&[&plain]), "no FILTER in the path");
    let conflict = data.join("FILTER_Ha/M42_FILTER_OIII_1.xisf");
    assert_eq!(
        reason(&[&conflict, &plain]),
        "no FILTER in the path, or no single value"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_folder_without_panels_says_so() {
    let root = temp_dir("list-folder-says");
    // Per-filter subfolders, nothing at the top level.
    let parent = root.join("Orion");
    for f in ["FILTER_Ha", "FILTER_OIII"] {
        std::fs::create_dir_all(parent.join(f)).unwrap();
        std::fs::write(parent.join(f).join("p.xisf"), b"not an image").unwrap();
    }
    std::fs::write(parent.join("notes.txt"), b"text").unwrap();
    let session = create_blocking(&root.join("s.mmm-session")).unwrap();
    let mut h = harness(session, vec![parent.clone()], runner());
    steps(&mut h);
    let last = |s: &State| s.toasts.last().map(|t| t.toast.title.clone());
    drop_paths(&mut h, vec![parent.clone()]);
    step_until(&mut h, |s| !s.list().adding() && !s.toasts.is_empty());
    let toast = &h.state().toasts[0];
    assert_eq!(toast.toast.title, "No panels at the top level of Orion");
    assert!(toast.toast.action.is_none() && toast.undo.is_none());
    assert!(h.state().session.mosaic.config.panels.is_empty());
    // The subfolders dropped together add their panels, saying nothing.
    drop_paths(
        &mut h,
        vec![parent.join("FILTER_Ha"), parent.join("FILTER_OIII")],
    );
    step_until(&mut h, |s| s.session.mosaic.config.panels.len() == 2);
    step_until(&mut h, |s| s.settled() && !s.list().adding());
    assert_eq!(h.state().toasts.len(), 1);
    // Add folder… picks the parent: the same toast.
    h.get_by_label("Add folder…").click();
    step_until(&mut h, |s| !s.list().adding() && s.toasts.len() == 2);
    assert_eq!(
        last(h.state()).as_deref(),
        Some("No panels at the top level of Orion")
    );
    // Every panel found already in: one toast for the batch.
    drop_paths(
        &mut h,
        vec![parent.join("FILTER_Ha"), parent.join("FILTER_OIII")],
    );
    step_until(&mut h, |s| !s.list().adding() && s.toasts.len() == 3);
    assert_eq!(last(h.state()).as_deref(), Some("Already in the session"));
    steps(&mut h);
    assert_eq!(h.state().toasts.len(), 3);
    assert_eq!(h.state().session.mosaic.config.panels.len(), 2);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nothing_added_names_each_reason() {
    let found = |files: &[&str], empty: &[&str], unreadable: &[&str]| Found {
        files: files.iter().map(PathBuf::from).collect(),
        empty: empty.iter().map(PathBuf::from).collect(),
        unreadable: unreadable.iter().map(PathBuf::from).collect(),
        skipped: Vec::new(),
    };
    let toast = |f: Found| nothing_added(&f).map(|t| (t.toast.title, t.toast.detail));
    // Nothing to say: nothing in the batch.
    assert_eq!(toast(found(&[], &[], &[])), None);
    // Only files that are not panels (or not there).
    let strays = Found {
        skipped: vec![PathBuf::from("d/notes.txt"), PathBuf::from("d/gone.xisf")],
        ..Found::default()
    };
    assert_eq!(
        toast(strays),
        Some(("Only XISF and FITS panel files can be added".into(), None))
    );
    let with_known = Found {
        skipped: vec![PathBuf::from("d/notes.txt")],
        ..found(&["d/a.xisf"], &[], &[])
    };
    assert_eq!(
        toast(with_known),
        Some((
            "Already in the session".into(),
            Some("Only XISF and FITS panel files can be added".into())
        ))
    );
    assert_eq!(
        toast(found(&["d/a.xisf"], &[], &[])),
        Some(("Already in the session".into(), None))
    );
    assert_eq!(
        toast(found(&[], &["d/Orion"], &[])),
        Some(("No panels at the top level of Orion".into(), None))
    );
    // Every reason, in one toast: the worst first.
    let all = nothing_added(&found(&["d/a.xisf"], &["d/Orion", "d/M31"], &["d/Locked"]))
        .expect("a toast");
    assert_eq!(all.toast.title, "Couldn't read Locked");
    assert_eq!(
        all.toast.detail.as_deref(),
        Some("No panels at the top level of Orion, M31\nAlready in the session")
    );
    assert_eq!(all.toast.tone, Tone::Problem);
    assert!(all.toast.action.is_none() && all.undo.is_none());
}

/// [`two_group_session`] with grouping off (the rule left as it is).
fn ungrouped_session(root: &Path) -> OpenSession {
    let mut session = two_group_session(root);
    session.mosaic.config.grouping.enabled = false;
    session.mosaic.save().unwrap();
    session.groups = session.mosaic.groups().ok();
    session
}

#[test]
fn an_analysed_session_keeps_its_results() {
    let root = temp_dir("list-analysed");
    let mut session = ungrouped_session(&root);
    let mut jobs = runner();
    jobs.start_analyze(&mut session);
    crate::jobs::tests::drain(&mut jobs, &mut session);
    let before = session.mosaic.config.clone();
    assert!(!before.group_dirs.is_empty(), "analysed");
    assert!(before.needs_analyze.is_empty());
    let extra = panel(&root.join("panels"), "Ha_extra.xisf", &[("FILTER", "'Ha'")]);
    // Reopened with its two filters ungrouped: left as it is, results and
    // all.
    let mut h = harness(session, vec![extra], jobs);
    wait(&mut h, State::settled);
    steps(&mut h);
    let s = h.state();
    assert!(!s.session.mosaic.config.grouping.enabled);
    assert!(s.toasts.is_empty(), "no automatic grouping");
    let saved = on_disk(s);
    assert!(saved.needs_analyze.is_empty(), "results kept");
    assert_eq!(saved.provenance, before.provenance, "no edit");
    assert_eq!(saved.group_dirs, before.group_dirs);
    // A panel added (its edit invalidates the results anyway): grouped by
    // FILTER then.
    h.get_by_label("Add files…").click();
    wait(&mut h, |s| {
        s.settled()
            && s.session.mosaic.config.panels.len() == 9
            && s.session.mosaic.config.grouping.enabled
    });
    assert_eq!(h.state().session.mosaic.config.grouping, header("FILTER"));
    let titles: Vec<&str> = h
        .state()
        .toasts
        .iter()
        .map(|t| t.toast.title.as_str())
        .collect();
    assert_eq!(titles, ["Grouped by FILTER: 2 filters"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// An ungrouped two-filter session changed by `setup` stays ungrouped when
/// opened.
fn stays_ungrouped(tag: &str, setup: fn(&mut OpenSession)) {
    let root = temp_dir(tag);
    let mut session = ungrouped_session(&root);
    setup(&mut session);
    session.mosaic.save().unwrap();
    let before = session.mosaic.config.clone();
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    steps(&mut h);
    let s = h.state();
    assert!(!s.session.mosaic.config.grouping.enabled, "{tag}");
    assert!(s.toasts.is_empty(), "{tag}: no automatic grouping");
    assert_eq!(on_disk(s).grouping, before.grouping, "{tag}");
    assert_eq!(rows(&h), 8, "{tag}: one flat list");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_prior_choice_keeps_grouping_off() {
    use mmm_core::checks::{CheckCode, CheckKey};
    // The "filters differ" note accepted.
    stays_ungrouped("list-prior-accepted", |s| {
        s.mosaic.config.accepted_checks.push(CheckKey {
            code: CheckCode::FiltersDifferUngrouped,
            panel: None,
            other: None,
            group: None,
        });
    });
    // A rule other than FILTER header chosen before grouping was turned off.
    stays_ungrouped("list-prior-token", |s| {
        s.mosaic.config.grouping.rule = GroupRule::FilenameToken {
            keyword: "filter".into(),
        };
    });
}

#[test]
fn a_stale_undo_keeps_a_later_choice() {
    let root = temp_dir("list-stale-undo");
    let mut h = harness(ungrouped_session(&root), vec![], runner());
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.grouping.enabled
    });
    assert_eq!(
        h.state().toasts[0].toast.title,
        "Grouped by FILTER: 2 filters"
    );
    // The user picks another rule while the toast still shows...
    choose(&mut h, "By hand");
    let manual = Grouping {
        enabled: true,
        rule: GroupRule::Manual,
    };
    wait_grouping(&mut h, manual.clone());
    let edits = h.state().session.mosaic.config.provenance.len();
    // ...then clicks its Undo: their choice stays.
    undo_toast(&mut h, 0);
    steps(&mut h);
    wait(&mut h, State::settled);
    let s = h.state();
    assert_eq!(s.session.mosaic.config.grouping, manual);
    assert_eq!(on_disk(s).grouping, manual);
    assert_eq!(s.session.mosaic.config.provenance.len(), edits, "no edit");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dropping_no_panel_files_says_so() {
    let root = temp_dir("list-strays");
    let notes = root.join("notes.txt");
    std::fs::write(&notes, b"text").unwrap();
    let session = create_blocking(&root.join("s.mmm-session")).unwrap();
    let mut h = harness(session, vec![], runner());
    steps(&mut h);
    drop_paths(&mut h, vec![notes, root.join("gone.xisf")]);
    step_until(&mut h, |s| !s.list().adding() && !s.toasts.is_empty());
    let toast = &h.state().toasts[0];
    assert_eq!(
        toast.toast.title,
        "Only XISF and FITS panel files can be added"
    );
    assert!(toast.toast.action.is_none() && toast.undo.is_none());
    assert!(h.state().session.mosaic.config.panels.is_empty());
    steps(&mut h);
    assert_eq!(h.state().toasts.len(), 1, "one toast for the batch");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_panicking_header_read_shows_an_unreadable_row() {
    let root = temp_dir("list-panic-read");
    let session = two_group_session(&root);
    let bad = session.mosaic.config.panels[0].path.clone();
    assert_eq!(name_of(&bad), "Ha_0.xisf");
    read_hooks::panic_reading(&bad);
    let mut h = harness(session, vec![], runner());
    wait(&mut h, State::settled);
    assert_eq!(cells(&h, "Ha_0.xisf"), ["Unreadable", "—", "—", "—"]);
    h.get_by_label_contains("Unreadable: ");
    // The rest still group, and nothing is read again.
    heading(&h, "Ha · 3");
    heading(&h, "OIII · 4");
    assert_eq!(
        warning(&h, "No group · 1").as_deref(),
        Some("FILTER missing")
    );
    steps(&mut h);
    assert_eq!(read_hooks::resolves(&bad), 1);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_lost_resolution_is_retried_later() {
    let root = temp_dir("list-panic-resolve");
    let session = two_group_session(&root);
    let p = session.mosaic.config.panels[0].path.clone();
    read_hooks::panic_resolving(&p, true);
    let mut h = harness(session, vec![], runner());
    let start = Instant::now();
    step_until(&mut h, |_| read_hooks::resolves(&p) >= 1);
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    // Not again at once: at most once per RESOLVE_RETRY (more apart each
    // time).
    let tries = read_hooks::resolves(&p);
    let allowed = 1 + (start.elapsed().as_secs_f64() / RESOLVE_RETRY.as_secs_f64()) as usize;
    assert!(tries <= allowed, "{tries} tries in {:?}", start.elapsed());
    // Once it works, a later try resolves the panels.
    read_hooks::panic_resolving(&p, false);
    wait(&mut h, State::settled);
    heading(&h, "Ha · 4");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}
