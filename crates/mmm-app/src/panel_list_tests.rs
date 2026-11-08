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
use crate::checks::ChecksEngine;
use crate::jobs::JobRunner;
use crate::panel_edits::{
    Applied, FilesPicker, GROUP_PALETTE, PHYSICAL_SLOTS, PanelsEdit, assign_group_colors,
    group_colors, load_group_colors, save_group_colors,
};
use crate::pipeline::Pipeline;
use crate::session::{OpenSession, create_blocking};
use crate::session_window::{SessionWindow, WindowToast};
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::{PATIENCE, open, two_group_session};

/// Answers the file dialog with fixed paths.
struct StubFiles(Vec<PathBuf>);

impl FilesPicker for StubFiles {
    fn pick_files(&self) -> Option<Vec<PathBuf>> {
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
        .with_size(egui::vec2(1400.0, 900.0))
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
    assert_eq!(group_by.value().as_deref(), Some("Header keyword…"));
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
    // The FILTER headers differ: the list suggests grouping again.
    h.get_by_label("Turn grouping on").click();
    wait(&mut h, |s| {
        s.settled() && s.session.mosaic.config.grouping.enabled
    });
    assert!(on_disk(h.state()).grouping.enabled);
    heading(&h, "Ha · 4");
    assert!(h.query_by_label("Turn grouping on").is_none());
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
    assert_eq!(cells(&h, "a.xisf"), ["Unreadable", "—", "—", "—"]);
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
    let (_, stopped) = jobs.stop_all(&mut session, Duration::from_secs(20));
    assert!(stopped);
    list.apply_waiting(&mut session, &mut jobs);
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
    // A merge runs (held) with an export queued behind it.
    let first = jobs.hold();
    let blend = jobs.start_blend(&mut s, "OIII", None);
    let out = root.join("ha.fits");
    let export = jobs.start_export(&s, "Ha", out.clone(), ExportFormat::Fits);
    let mut edit = PanelsEdit::of(&s);
    edit.panels.pop();
    // The edit cancels the merge but not the export, and waits for the
    // merge only.
    let Applied::Queued(edit) = edit.apply(&mut s, &mut jobs) else {
        panic!("the edit should wait for the merge");
    };
    let labels: Vec<String> = jobs.views().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["Merge OIII", "Export Ha"], "export kept");
    let second = jobs.hold();
    first.open();
    let mut events = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    while jobs.views().first().map(|v| v.label.as_str()) != Some("Export Ha") {
        assert!(Instant::now() < deadline, "the export never ran");
        std::thread::sleep(Duration::from_millis(5));
        events.extend(jobs.poll(&mut s));
    }
    // A running export does not hold the edit back.
    assert!(matches!(edit.apply(&mut s, &mut jobs), Applied::Done));
    assert_eq!(s.mosaic.config.panels.len(), 7);
    second.open();
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
