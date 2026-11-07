//! Tests of the Panels tab (a `#[path]` child of `panels_tab.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mega_ui::groups::SII_COLOR;
use mmm_core::mosaic::{MosaicConfig, PanelEntry};

use super::*;
use crate::jobs::JobRunner;
use crate::session::{OpenSession, create_blocking};
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::two_group_session;

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

/// What the shell hands the tab: the session and its job runner.
struct State {
    tab: PanelsTab,
    session: OpenSession,
    jobs: JobRunner,
}

impl State {
    fn panel_names(&self) -> Vec<String> {
        self.session
            .mosaic
            .config
            .panels
            .iter()
            .map(|p| name_of(&p.path))
            .collect()
    }

    fn manual(&self, name: &str) -> Option<String> {
        self.session
            .mosaic
            .config
            .panels
            .iter()
            .find(|p| name_of(&p.path) == name)
            .and_then(|p| p.manual_group.clone())
    }

    /// Nothing waiting: no edit pending, groups resolved for the inputs.
    fn settled(&self) -> bool {
        self.tab.settled(&self.session)
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

fn harness(session: OpenSession, picked: Vec<PathBuf>, jobs: JobRunner) -> Harness<'static, State> {
    let state = State {
        tab: PanelsTab::new(Arc::new(StubFiles(picked))),
        session,
        jobs,
    };
    Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(
            |ui, s: &mut State| {
                // As the shell does: jobs, then the tab's poll, then the tab.
                let ctx = ui.ctx().clone();
                s.jobs.poll(&mut s.session);
                s.tab.poll(&ctx, &mut s.session, &mut s.jobs);
                s.tab.ui(ui, &mut s.session, &mut s.jobs);
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

fn runner() -> JobRunner {
    JobRunner::new(egui::Context::default())
}

fn on_disk(s: &State) -> MosaicConfig {
    MosaicConfig::load(&s.session.mosaic.dir).unwrap()
}

#[test]
fn add_files_dedupes() {
    let root = temp_dir("panels-add");
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
    h.step();
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
    step_until(&mut h, |s| s.settled() && !s.tab.adding());
    assert_eq!(h.state().panel_names(), ["a.xisf", "b.xisf", "c.fits"]);

    // A typed pattern adds only the new match.
    let field = h.get_by_role_and_label(Role::TextInput, "Add path");
    field.focus();
    h.step();
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
    step_until(&mut h, |s| s.settled() && !s.tab.adding());
    assert_eq!(h.state().session.mosaic.config.panels.len(), 4);
    assert_eq!(
        on_disk(h.state()).panels,
        h.state().session.mosaic.config.panels
    );
    // Unreadable files still get a card, saying so.
    assert!(h.state().tab.card_text(&a).starts_with("Unreadable"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn grouping_off_shows_plain_grid() {
    let root = temp_dir("panels-plain");
    let mut session = two_group_session(&root);
    session.mosaic.config.grouping.enabled = false;
    session.mosaic.save().unwrap();
    let mut h = harness(session, vec![], runner());
    step_until(&mut h, |s| s.settled() && s.tab.filters_differ());
    for i in 0..4 {
        h.get_by_label(&format!("Ha_{i}.xisf"));
        h.get_by_label(&format!("OIII_{i}.xisf"));
    }
    // Dimensions and channels, once read.
    assert!(
        h.state()
            .tab
            .card_text(&h.state().session.mosaic.config.panels[0].path)
            .contains("600 × 420 · 3 channels")
    );
    // No mention of groups.
    assert_eq!(h.query_all_by_label_contains("to sort").count(), 0);
    assert_eq!(h.query_all_by_label("No group").count(), 0);
    assert!(
        h.query_by_role_and_label(Role::TextInput, "Header key")
            .is_none()
    );
    h.get_by_label("Ha_0.xisf").click();
    steps(&mut h);
    h.get_by_label("Remove");
    assert_eq!(h.query_all_by_label("Move to ▾").count(), 0);
    assert_eq!(h.query_all_by_label("Add to new group…").count(), 0);
    // The FILTER headers differ: the tab suggests grouping.
    h.get_by_label("Turn grouping on").click();
    step_until(&mut h, |s| s.session.mosaic.config.grouping.enabled);
    assert!(on_disk(h.state()).grouping.enabled);
    step_until(&mut h, |s| s.settled());
    h.get_by_label("Ha");
    h.get_by_label("OIII");
    assert_eq!(h.query_all_by_label("Turn grouping on").count(), 0);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_group_column_first() {
    let root = temp_dir("panels-nogroup");
    let mut session = two_group_session(&root);
    let stray = root.join("panels").join("stray.xisf");
    std::fs::write(&stray, b"no header").unwrap();
    session.mosaic.config.panels.push(PanelEntry {
        path: stray,
        fingerprint: None,
        manual_group: None,
    });
    session.mosaic.save().unwrap();
    let mut h = harness(session, vec![], runner());
    step_until(&mut h, |s| s.settled());
    let to_sort = h.get_by_label("1 to sort").rect();
    let ha = h.get_by_label("Ha").rect();
    let oiii = h.get_by_label("OIII").rect();
    assert!(to_sort.left() < ha.left(), "{to_sort:?} {ha:?}");
    assert!(ha.left() < oiii.left(), "{ha:?} {oiii:?}");
    // The stray card sits in the first column.
    let card = h.get_by_label("stray.xisf").rect();
    assert!(card.left() < ha.left());
    assert!(card.top() > to_sort.top());
    // The rule picker shows the header rule.
    assert_eq!(
        h.get_by_role_and_label(Role::TextInput, "Header key")
            .value()
            .as_deref(),
        Some("FILTER")
    );
    // A new key applies on Enter, not per keystroke: no panel has OBJECT.
    h.get_by_role_and_label(Role::TextInput, "Header key")
        .focus();
    h.step();
    h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Header key")
        .type_text("OBJECT");
    steps(&mut h);
    assert_eq!(
        h.state().session.mosaic.config.grouping.rule,
        GroupRule::default()
    );
    h.key_press(egui::Key::Enter);
    steps(&mut h);
    let rule = GroupRule::Header {
        key: "OBJECT".into(),
    };
    assert_eq!(h.state().session.mosaic.config.grouping.rule, rule);
    step_until(&mut h, |s| s.settled());
    h.get_by_label("9 to sort");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn move_to_sets_manual_group() {
    let root = temp_dir("panels-move");
    let session = two_group_session(&root);
    let mut h = harness(session, vec![], runner());
    step_until(&mut h, |s| s.settled());
    h.get_by_label("Ha_0.xisf").click();
    steps(&mut h);
    h.get_by_label("Ha_2.xisf")
        .click_modifiers(egui::Modifiers::SHIFT);
    h.step();
    assert_eq!(
        h.state().tab.selected().len(),
        3,
        "shift-click selects a range"
    );
    h.get_by_label("Move to ▾").click();
    steps(&mut h);
    h.get_by_role_and_label(Role::Button, "OIII").click();
    steps(&mut h);
    step_until(&mut h, |s| s.settled());
    let s = h.state();
    for n in ["Ha_0.xisf", "Ha_1.xisf", "Ha_2.xisf"] {
        assert_eq!(s.manual(n).as_deref(), Some("OIII"), "{n}");
    }
    assert_eq!(s.manual("Ha_3.xisf"), None);
    assert_eq!(on_disk(s).panels, s.session.mosaic.config.panels);
    let groups = s.session.groups.as_ref().unwrap();
    assert_eq!(groups.groups["OIII"].len(), 7);
    assert_eq!(groups.groups["Ha"].len(), 1);
    // The cards moved: Ha_0 now sits in the OIII column, right of Ha's.
    let ha = h.get_by_label("Ha").rect();
    assert!(h.get_by_label("Ha_0.xisf").rect().left() > ha.right());
    // Esc clears the selection.
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(h.state().tab.selected().is_empty());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_to_new_group_creates_group() {
    let root = temp_dir("panels-newgroup");
    let session = two_group_session(&root);
    let mut h = harness(session, vec![], runner());
    step_until(&mut h, |s| s.settled());
    h.get_by_label("Ha_0.xisf").click();
    steps(&mut h);
    h.get_by_label("OIII_3.xisf")
        .click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    assert_eq!(h.state().tab.selected().len(), 2, "ctrl-click adds");
    h.get_by_label("Add to new group…").click();
    steps(&mut h);
    h.get_by_role_and_label(Role::TextInput, "Group name")
        .focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Group name")
        .type_text("Red");
    h.step();
    h.get_by_label("Create").click();
    steps(&mut h);
    step_until(&mut h, |s| s.settled());
    let s = h.state();
    assert_eq!(s.manual("Ha_0.xisf").as_deref(), Some("Red"));
    assert_eq!(s.manual("OIII_3.xisf").as_deref(), Some("Red"));
    assert_eq!(s.manual("Ha_1.xisf"), None);
    assert_eq!(on_disk(s).panels, s.session.mosaic.config.panels);
    let names: Vec<_> = s
        .session
        .groups
        .as_ref()
        .unwrap()
        .groups
        .keys()
        .cloned()
        .collect();
    assert_eq!(names, ["Ha", "OIII", "Red"]);
    // Colours were assigned on resolve, and saved with the edit.
    let saved = load_group_colors(&on_disk(s).ui);
    assert_eq!(saved.keys().collect::<Vec<_>>(), ["Ha", "OIII"]);
    let now = load_group_colors(&s.session.mosaic.config.ui);
    assert_eq!(now.keys().collect::<Vec<_>>(), ["Ha", "OIII", "Red"]);
    let colors = group_colors(&now, ["Ha", "OIII", "Red"]);
    assert_eq!(colors["Ha"], palette::HA_500);
    assert_eq!(colors["OIII"], palette::ACCENT_400);
    h.get_by_label("Red");
    assert_eq!(
        h.query_all_by_label("Group name").count(),
        0,
        "prompt closed"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn remove_drops_entries() {
    let root = temp_dir("panels-remove");
    let session = two_group_session(&root);
    let mut h = harness(session, vec![], runner());
    step_until(&mut h, |s| s.settled());
    h.get_by_label("Ha_1.xisf").click();
    steps(&mut h);
    h.get_by_label("OIII_0.xisf")
        .click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.get_by_label("Remove").click();
    steps(&mut h);
    step_until(&mut h, |s| s.settled());
    let s = h.state();
    assert_eq!(s.panel_names().len(), 6);
    assert!(!s.panel_names().contains(&"Ha_1.xisf".to_string()));
    assert!(!s.panel_names().contains(&"OIII_0.xisf".to_string()));
    assert_eq!(on_disk(s).panels, s.session.mosaic.config.panels);
    assert!(s.tab.selected().is_empty());
    assert_eq!(h.query_all_by_label("Ha_1.xisf").count(), 0);
    let actions: Vec<_> = s
        .session
        .mosaic
        .config
        .provenance
        .iter()
        .map(|p| p.action.as_str())
        .collect();
    assert_eq!(actions, ["inputs"]);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn edit_while_job_running_is_queued_then_applied() {
    let root = temp_dir("panels-queued");
    let mut session = two_group_session(&root);
    let mut jobs = runner();
    let gate = jobs.hold();
    let job = jobs.start_analyze(&mut session);
    let mut h = harness(session, vec![], jobs);
    step_until(&mut h, |s| s.tab.resolved_current(&s.session));
    h.get_by_label("Ha_0.xisf").click();
    steps(&mut h);
    h.get_by_label("Remove").click();
    steps(&mut h);
    // Queued: the job is cancelling, nothing changed yet (memory or disk).
    {
        let s = h.state();
        assert!(s.tab.pending().is_some());
        assert_eq!(s.session.mosaic.config.panels.len(), 8);
        assert_eq!(on_disk(s).panels.len(), 8);
        let views = s.jobs.views();
        assert_eq!((views[0].id, views[0].stage.as_str()), (job, "Cancelling…"));
    }
    // Shown as it will be: the removed card is gone already.
    assert_eq!(h.query_all_by_label("Ha_0.xisf").count(), 0);
    h.get_by_label_contains("Waiting for the running job");
    // A second edit while queued replaces the first (newer work wins).
    h.get_by_label("Ha_1.xisf").click();
    steps(&mut h);
    h.get_by_label("Remove").click();
    steps(&mut h);
    assert_eq!(h.state().tab.pending().unwrap().panels.len(), 6);
    gate.open();
    step_until(&mut h, |s| !s.jobs.busy() && s.settled());
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
fn queued_edit_cancels_jobs_started_later() {
    let root = temp_dir("panels-requeue");
    let mut session = two_group_session(&root);
    let mut jobs = runner();
    let gate = jobs.hold();
    jobs.start_analyze(&mut session);
    let mut h = harness(session, vec![], jobs);
    step_until(&mut h, |s| s.tab.resolved_current(&s.session));
    h.get_by_label("Ha_0.xisf").click();
    steps(&mut h);
    h.get_by_label("Remove").click();
    steps(&mut h);
    assert!(h.state().tab.pending().is_some());
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
    step_until(&mut h, |s| !s.jobs.busy() && s.settled());
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

    let (root, mut s, mut jobs) = analyzed("panels-edit-export");
    jobs.start_blend(&mut s, "Ha", None);
    drain(&mut jobs, &mut s);
    // A blend runs (held) with an export queued behind it.
    let first = jobs.hold();
    let blend = jobs.start_blend(&mut s, "OIII", None);
    let out = root.join("ha.fits");
    let export = jobs.start_export(&s, "Ha", out.clone(), ExportFormat::Fits);
    let mut edit = PanelsEdit::of(&s);
    edit.panels.pop();
    // The edit cancels the blend but not the export, and waits for the
    // blend only.
    let Applied::Queued(edit) = edit.apply(&mut s, &mut jobs) else {
        panic!("the edit should wait for the blend");
    };
    let labels: Vec<String> = jobs.views().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["Blend OIII", "Export Ha"], "export kept");
    let second = jobs.hold();
    first.open();
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while jobs.views().first().map(|v| v.label.as_str()) != Some("Export Ha") {
        assert!(std::time::Instant::now() < deadline, "the export never ran");
        std::thread::sleep(std::time::Duration::from_millis(5));
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
