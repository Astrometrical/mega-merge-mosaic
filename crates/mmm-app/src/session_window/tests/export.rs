//! The Export dialog (spec §14) as the window shows it: the session's
//! groups and composites with their readiness, the folder, format and file
//! name pattern, the preview of the exact files with their marks, the
//! explicit Overwrite, the remembered choices, and the keyboard.

use egui::accesskit::Toggled;
use egui::{Key, Modifiers};
use mmm_core::export::ExportFormat;
use mmm_core::session_summary::ResultState;
use serde_json::json;

use super::*;
use crate::export_dialog::{EXPORT_KEY, FolderPicker, Pick, Readiness, readiness_of};

/// Answers Browse… with a fixed folder.
struct FixedFolder(PathBuf);

impl FolderPicker for FixedFolder {
    fn pick_folder(&self, _start: Option<&std::path::Path>) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

/// A two-group session ("m": Ha and OIII) whose groups read as merged and
/// complete. No result stores: the dialog reads the session's
/// classification of them.
fn ready_session(tag: &str) -> (PathBuf, OpenSession) {
    let root = temp_dir(tag);
    let mut s = two_group_session(&root);
    for g in ["Ha", "OIII"] {
        s.results.insert(g.into(), ResultState::Complete);
    }
    s.merged = true;
    (root, s)
}

/// The window showing `s`, its view picked.
fn shown(s: OpenSession, jobs: JobRunner) -> Harness<'static, State> {
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    h
}

/// A composite with `id`, `name` and a group (or none) per channel.
fn comp(id: u64, name: &str, channels: [Option<&str>; 3]) -> Composition {
    let mut c = Composition::new(name);
    c.id = id;
    c.channels = channels.map(|g| g.map(|g| (g.to_string(), 1.0)));
    c
}

/// Open the Export dialog as the top bar does, and wait until it knows
/// which of its files exist.
fn open_dialog(h: &mut Harness<'static, State>) {
    {
        let State {
            w, session, jobs, ..
        } = h.state_mut();
        w.open_export(session, jobs);
    }
    h.run_steps(2);
    assert!(h.state().w.export_open(), "the dialog opens");
    settle(h);
}

/// Step until the open dialog has looked at its folder for the files it
/// shows now: first a frame for what was queued (a click, a key) to take
/// effect, then until the lookup it asked for has answered.
fn settle(h: &mut Harness<'static, State>) {
    h.step();
    step_until(h, |st| st.w.export_dialog().settled());
    h.run_steps(2);
}

/// The row checkbox called `name`.
fn row<'h>(h: &'h Harness<'static, State>, name: &'h str) -> egui_kittest::Node<'h> {
    h.get(By::new().role(Role::CheckBox).label(name))
}

/// `name`'s row: whether it can be ticked, whether it is, and the reason
/// it cannot.
fn row_state(h: &Harness<'static, State>, name: &str) -> (bool, bool, Option<String>) {
    let node = row(h, name).accesskit_node();
    (
        !node.is_disabled(),
        node.toggled() == Some(Toggled::True),
        node.description(),
    )
}

/// A ready row, ticked.
fn ticked() -> (bool, bool, Option<String>) {
    (true, true, None)
}

/// A row that cannot be ticked, for `why`.
fn greyed(why: &str) -> (bool, bool, Option<String>) {
    (false, false, Some(why.to_string()))
}

/// The dialog's footer button `label` (the step strip in the top bar has
/// an "Export" too).
fn footer_button<'h>(h: &'h Harness<'static, State>, label: &'h str) -> egui_kittest::Node<'h> {
    query_footer(h, label).unwrap_or_else(|| panic!("no {label} button"))
}

/// [`footer_button`], if there is one.
fn query_footer<'h>(
    h: &'h Harness<'static, State>,
    label: &'h str,
) -> Option<egui_kittest::Node<'h>> {
    let bar = h.state().w.top_bar_rect.expect("the top bar is drawn");
    h.query_all(By::new().role(Role::Button).label(label))
        .find(|n| n.rect().min.y > bar.max.y)
}

/// Whether the footer's `label` button is disabled.
fn footer_disabled(h: &Harness<'static, State>, label: &str) -> bool {
    footer_button(h, label).accesskit_node().is_disabled()
}

/// The text field `label` (the dialog's Folder or File name pattern).
fn field<'h>(h: &'h Harness<'static, State>, label: &'h str) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::TextInput, label)
}

/// Replace what the text field `label` holds with `text`, as typed (the
/// field keeps the keyboard).
fn retype(h: &mut Harness<'static, State>, label: &str, text: &str) {
    field(h, label).focus();
    h.step();
    h.key_combination_modifiers(Modifiers::COMMAND, &[Key::A]);
    h.step();
    if text.is_empty() {
        h.key_press(Key::Backspace);
    } else {
        field(h, label).type_text(text);
    }
    h.run_steps(2);
}

/// The preview's lines, top to bottom (the file names, marked).
fn preview(h: &Harness<'static, State>, names: &[&str]) -> Vec<String> {
    let line = |label: &str| {
        names
            .iter()
            .any(|name| label == *name || label.starts_with(&format!("{name} ")))
    };
    // A label's text is its accessibility value.
    let mut found: Vec<(f32, String)> = h
        .query_all(By::new().role(Role::Label))
        .filter_map(|n| {
            let text = n.accesskit_node().value()?;
            line(&text).then(|| (n.rect().center().y, text))
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, l)| l).collect()
}

/// The files in `dir`, by name.
fn files_in(dir: &std::path::Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn rows_show_readiness_and_ready_ones_are_ticked() {
    let (root, mut s) = ready_session("export-rows");
    // A third group, SII (by hand), never merged; OIII out of date.
    for p in &mut s.mosaic.config.panels[6..] {
        p.manual_group = Some("SII".into());
    }
    s.groups = s.mosaic.groups().ok();
    s.results.insert("OIII".into(), ResultState::Stale);
    save_compositions(
        &mut s.mosaic.config.ui,
        &[
            comp(1, "Ha alone", [Some("Ha"), None, None]),
            comp(2, "HOO", [Some("Ha"), Some("OIII"), Some("OIII")]),
            comp(3, "SHO", [Some("SII"), Some("Ha"), Some("OIII")]),
            comp(4, "Nothing", [None, None, None]),
        ],
    );
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    h.get_by_label("3 groups \u{b7} 4 composites");
    assert_eq!(row_state(&h, "Ha"), ticked());
    assert_eq!(row_state(&h, "OIII"), greyed("out of date"));
    assert_eq!(row_state(&h, "SII"), greyed("not merged"));
    assert_eq!(row_state(&h, "Ha alone"), ticked());
    // A composite is as ready as its least ready group.
    assert_eq!(row_state(&h, "HOO"), greyed("out of date"));
    assert_eq!(row_state(&h, "SHO"), greyed("not merged"));
    assert_eq!(row_state(&h, "Nothing"), greyed("maps no groups"));
    // The reasons show as text (never colour alone).
    assert_eq!(h.query_all_by_label("not merged").count(), 2);
    assert_eq!(h.query_all_by_label("out of date").count(), 2);
    h.get_by_label("maps no groups");
    // Groups first, then composites.
    let y = |name: &str| row(&h, name).rect().center().y;
    assert!(y("Ha") < y("OIII") && y("OIII") < y("SII") && y("SII") < y("Ha alone"));
    // A row greyed cannot be ticked.
    row(&h, "SII").click();
    h.run_steps(2);
    assert_eq!(row_state(&h, "SII"), greyed("not merged"));
    // The readiness behind the rows.
    let st = h.state();
    let s = &st.session;
    let group = |g: &str| readiness_of(s, &ExportTarget::Group(g.into()));
    assert_eq!(group("Ha"), Readiness::Ready);
    assert_eq!(group("OIII"), Readiness::OutOfDate);
    assert_eq!(group("SII"), Readiness::NotMerged);
    let composite = |channels: [Option<&str>; 3]| {
        readiness_of(
            s,
            &ExportTarget::Composite {
                id: 9,
                name: "C".into(),
                channels: channels.map(|g| g.map(str::to_string)),
            },
        )
    };
    assert_eq!(composite([Some("Ha"), None, None]), Readiness::Ready);
    assert_eq!(composite([None; 3]), Readiness::MapsNoGroups);
    assert_eq!(
        composite([Some("SII"), Some("OIII"), None]),
        Readiness::NotMerged,
        "the first group not ready gives the reason"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn settings_changed_does_not_block() {
    let (root, mut s, jobs) = merged("export-settings-changed");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut h = shown(s, jobs);
    h.run_steps(3);
    let st = h.state();
    assert!(draft_changed(&st.session, "Ha") && !out_of_date(&st.session, "Ha"));
    assert_eq!(
        shown_steps(st)[MERGE_STEP],
        strip("Settings changed", StepState::Next)
    );
    // The Export step opens the dialog (the main button offers Merge).
    assert_eq!(st.w.export_block_reason(&st.session, &st.jobs), None);
    h.get_by_label("Export").click();
    h.run_steps(2);
    assert!(h.state().w.export_open());
    settle(&mut h);
    assert_eq!(row_state(&h, "Ha"), ticked());
    assert_eq!(row_state(&h, "OIII"), ticked());
    assert!(!footer_disabled(&h, "Export"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_preview_lists_exact_names() {
    let (root, mut s) = ready_session("export-preview");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[comp(1, "HOO", [Some("Ha"), Some("OIII"), Some("OIII")])],
    );
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    // The default pattern, {session}_{name}, beside the session folder.
    assert_eq!(
        field(&h, "File name pattern").value().as_deref(),
        Some("{session}_{name}")
    );
    let root_text = crate::util::plain_path(&root);
    assert_eq!(
        field(&h, "Folder").value().as_deref(),
        Some(root_text.as_str())
    );
    let names = ["m_Ha.fits", "m_OIII.fits", "m_HOO.fits"];
    assert_eq!(preview(&h, &names), names, "one per ticked row, in order");
    // The format names the extension.
    h.get_by_label("TIFF").click();
    settle(&mut h);
    let tifs = ["m_Ha.tif", "m_OIII.tif", "m_HOO.tif"];
    assert_eq!(preview(&h, &tifs), tifs);
    h.get_by_label_contains("values are not clamped");
    // Only ticked rows.
    row(&h, "OIII").click();
    settle(&mut h);
    assert_eq!(preview(&h, &tifs), ["m_Ha.tif", "m_HOO.tif"]);
    // The pattern's tokens.
    retype(&mut h, "File name pattern", "{name} of {session}");
    settle(&mut h);
    let named = ["Ha of m.tif", "HOO of m.tif"];
    assert_eq!(preview(&h, &named), named);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn colliding_names_block_export() {
    let (root, s) = ready_session("export-collide");
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    assert!(!footer_disabled(&h, "Export"));
    // Without {name}, both groups would go to m.fits.
    retype(&mut h, "File name pattern", "{session}");
    settle(&mut h);
    let message = "Two exports would have the same file name: add {name} to the pattern";
    h.get_by_label(message);
    assert_eq!(h.query_all_by_label("m.fits same name").count(), 2);
    assert!(footer_disabled(&h, "Export"));
    footer_button(&h, "Export").click();
    h.run_steps(2);
    assert!(h.state().w.export_open(), "nothing exported");
    assert!(!h.state().jobs.busy());
    // One row ticked: no collision.
    row(&h, "OIII").click();
    settle(&mut h);
    assert!(h.query_by_label(message).is_none());
    h.get_by_label("m.fits");
    assert!(!footer_disabled(&h, "Export"));
    // Any pattern without {name} does it.
    row(&h, "OIII").click();
    retype(&mut h, "File name pattern", "x");
    settle(&mut h);
    h.get_by_label(message);
    retype(&mut h, "File name pattern", "{session}-{name}");
    settle(&mut h);
    assert!(h.query_by_label(message).is_none());
    assert!(!footer_disabled(&h, "Export"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_pasted_quoted_path_is_taken() {
    let (root, s, jobs) = merged("export-quoted");
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let mut h = shown(s, jobs);
    open_dialog(&mut h);
    // As Explorer's "Copy as path" gives it, with a stray space or two.
    let out_text = crate::util::plain_path(&out);
    retype(&mut h, "Folder", &format!(" \"{out_text}\"  "));
    settle(&mut h);
    assert!(h.query_by_label("Give the folder's full path").is_none());
    assert!(h.query_by_label("The folder doesn't exist").is_none());
    assert!(!footer_disabled(&h, "Export"));
    footer_button(&h, "Export").click();
    h.run_steps(2);
    step_until(&mut h, |st| !st.jobs.busy());
    let want: BTreeSet<String> = ["m_Ha.fits", "m_OIII.fits"].map(String::from).into();
    assert_eq!(files_in(&out), want);
    // Remembered without the quotes.
    assert_eq!(
        h.state().session.mosaic.config.ui[EXPORT_KEY]["folder"],
        json!(out_text)
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_folder_that_cannot_take_the_files_blocks_export() {
    let (root, s) = ready_session("export-folder");
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    assert!(!footer_disabled(&h, "Export"));
    // Each problem is said beside the folder, and Export waits.
    let gone = crate::util::plain_path(&root.join("gone"));
    for (typed, why) in [
        (gone.as_str(), "The folder doesn't exist"),
        ("out", "Give the folder's full path"),
        ("", "Choose a folder."),
    ] {
        retype(&mut h, "Folder", typed);
        settle(&mut h);
        h.get_by_label(why);
        assert!(footer_disabled(&h, "Export"), "{why}");
    }
    // A folder is only looked at: nothing is created in it, not even for
    // a moment (its modified time stays as it was).
    let modified = |dir: &std::path::Path| std::fs::metadata(dir).unwrap().modified().unwrap();
    let before = modified(&out);
    std::thread::sleep(Duration::from_millis(20));
    let out_text = crate::util::plain_path(&out);
    retype(&mut h, "Folder", &out_text);
    h.key_press(Key::Tab);
    settle(&mut h);
    assert!(!footer_disabled(&h, "Export"));
    assert_eq!(modified(&out), before);
    assert_eq!(files_in(&out), BTreeSet::new());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn existing_files_need_overwrite() {
    let (root, s) = ready_session("export-overwrite");
    let ha = root.join("m_Ha.fits");
    std::fs::write(&ha, b"keep").unwrap();
    let mut h = shown(s, runner());
    // While the folder is being looked at, the names show unmarked and
    // Export waits.
    let release = h.state_mut().w.hold_export_checks();
    {
        let State {
            w, session, jobs, ..
        } = h.state_mut();
        w.open_export(session, jobs);
    }
    h.run_steps(3);
    assert_eq!(
        preview(&h, &["m_Ha.fits", "m_OIII.fits"]),
        ["m_Ha.fits", "m_OIII.fits"]
    );
    h.get_by_label("Checking the folder\u{2026}");
    assert!(footer_disabled(&h, "Export"));
    drop(release);
    settle(&mut h);
    // Found: marked, counted, and Export asks to overwrite.
    assert_eq!(
        preview(&h, &["m_Ha.fits", "m_OIII.fits"]),
        ["m_Ha.fits exists", "m_OIII.fits"]
    );
    h.get_by_label("1 file already exists and will be replaced");
    assert!(query_footer(&h, "Export").is_none());
    assert!(!footer_disabled(&h, "Overwrite and export"));
    // Ha unticked: nothing would be replaced.
    row(&h, "Ha").click();
    settle(&mut h);
    assert!(query_footer(&h, "Overwrite and export").is_none());
    assert!(!footer_disabled(&h, "Export"));
    assert!(
        h.query_by_label_contains("will be replaced").is_none(),
        "no overwrite line"
    );
    // Both existing (looked at again as the choice changes): counted.
    std::fs::write(root.join("m_OIII.fits"), b"keep").unwrap();
    row(&h, "Ha").click();
    settle(&mut h);
    h.get_by_label("2 files already exist and will be replaced");
    // Only the explicit Overwrite exports.
    let gate = h.state_mut().jobs.hold_exports();
    footer_button(&h, "Overwrite and export").click();
    h.run_steps(2);
    assert!(!h.state().w.export_open());
    assert_eq!(labels(&h.state().jobs), ["Export Ha", "Export OIII"]);
    // Cancelled before they wrote anything: the files are as they were.
    super::super::top_bar::cancel_step(EXPORT_STEP, &mut h.state_mut().jobs);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    assert_eq!(std::fs::read(&ha).unwrap(), b"keep");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn choices_are_remembered() {
    let (root, s) = ready_session("export-remember");
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let mut h = shown(s, runner());
    h.state_mut()
        .w
        .set_folder_picker(Arc::new(FixedFolder(out.clone())));
    open_dialog(&mut h);
    // Browse… picks the folder; TIFF; a pattern of the name; OIII left out.
    h.get_by_label("Browse\u{2026}").click();
    let out_text = crate::util::plain_path(&out);
    step_until(&mut h, |st| {
        st.w.export_dialog().folder() == out_text.as_str()
    });
    h.get_by_label("TIFF").click();
    retype(&mut h, "File name pattern", "{name}");
    row(&h, "OIII").click();
    settle(&mut h);
    assert_eq!(preview(&h, &["Ha.tif"]), ["Ha.tif"]);
    let gate = h.state_mut().jobs.hold_exports();
    footer_button(&h, "Export").click();
    h.run_steps(2);
    assert!(!h.state().w.export_open());
    assert_eq!(labels(&h.state().jobs), ["Export Ha"]);
    // Kept in the session's settings, and saved with them (the window's
    // save, rate-limited).
    let want = json!({
        "folder": out_text,
        "format": "tiff",
        "pattern": "{name}",
        "excluded": [{"group": "OIII"}],
    });
    assert_eq!(h.state().session.mosaic.config.ui[EXPORT_KEY], want);
    let dir = h.state().session.mosaic.dir.clone();
    step_until(&mut h, |_| {
        mmm_core::mosaic::Mosaic::open(&dir).is_ok_and(|m| m.config.ui[EXPORT_KEY] == want)
    });
    super::super::top_bar::cancel_step(EXPORT_STEP, &mut h.state_mut().jobs);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    // Opened again: as chosen; OIII, ready but unticked, stays unticked.
    open_dialog(&mut h);
    let expect = |h: &Harness<'static, State>| {
        let d = h.state().w.export_dialog();
        assert_eq!(d.folder(), out_text.as_str());
        assert_eq!(d.format(), ExportFormat::Tiff);
        assert_eq!(d.pattern(), "{name}");
        assert_eq!(row_state(h, "Ha"), ticked());
        assert_eq!(row_state(h, "OIII"), (true, false, None));
    };
    expect(&h);
    // Cancel keeps nothing of what changed meanwhile.
    h.get_by_label("FITS").click();
    settle(&mut h);
    footer_button(&h, "Cancel").click();
    h.run_steps(2);
    assert!(!h.state().w.export_open());
    // A new window on the session (as when it opens again) reads them.
    let files = h.state().files.clone();
    h.state_mut().w = SessionWindow::new(files);
    step_until(&mut h, |st| st.w.view().is_some());
    open_dialog(&mut h);
    expect(&h);
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_row_ready_later_starts_ticked() {
    let (root, mut s) = ready_session("export-ready-later");
    // SII, a third group (by hand), is not merged yet.
    for p in &mut s.mosaic.config.panels[6..] {
        p.manual_group = Some("SII".into());
    }
    s.groups = s.mosaic.groups().ok();
    let mut h = shown(s, runner());
    // Exported with OIII unticked.
    open_dialog(&mut h);
    assert_eq!(row_state(&h, "SII"), greyed("not merged"));
    row(&h, "OIII").click();
    settle(&mut h);
    let gate = h.state_mut().jobs.hold_exports();
    footer_button(&h, "Export").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Export Ha"]);
    super::super::top_bar::cancel_step(EXPORT_STEP, &mut h.state_mut().jobs);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    // Then SII is merged and a composite made: both start ticked; only the
    // row unticked stays so.
    {
        let session = &mut h.state_mut().session;
        session.results.insert("SII".into(), ResultState::Complete);
        save_compositions(
            &mut session.mosaic.config.ui,
            &[comp(1, "SHO", [Some("SII"), Some("Ha"), Some("OIII")])],
        );
    }
    h.run_steps(2);
    open_dialog(&mut h);
    assert_eq!(row_state(&h, "Ha"), ticked());
    assert_eq!(row_state(&h, "OIII"), (true, false, None));
    assert_eq!(row_state(&h, "SII"), ticked());
    assert_eq!(row_state(&h, "SHO"), ticked());
    // Ticked again by hand, it is not left out any more.
    row(&h, "OIII").click();
    settle(&mut h);
    let gate = h.state_mut().jobs.hold_exports();
    footer_button(&h, "Export").click();
    h.run_steps(2);
    assert_eq!(
        h.state().session.mosaic.config.ui[EXPORT_KEY]["excluded"],
        json!([])
    );
    super::super::top_bar::cancel_step(EXPORT_STEP, &mut h.state_mut().jobs);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composite_exports_one_file() {
    use mmm_core::formats::InputPanel;

    use crate::testutil::CANVAS;

    let (root, mut s, jobs) = merged("export-composite");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[comp(1, "HOO", [Some("Ha"), Some("OIII"), Some("OIII")])],
    );
    let before = files_in(&root);
    let mut h = shown(s, jobs);
    open_dialog(&mut h);
    row(&h, "Ha").click();
    row(&h, "OIII").click();
    settle(&mut h);
    assert_eq!(preview(&h, &["m_HOO.fits"]), ["m_HOO.fits"]);
    footer_button(&h, "Export").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Export HOO"]);
    step_until(&mut h, |st| !st.jobs.busy());
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label, .. } if label == "Export HOO")),
        "{:?}",
        h.state().events
    );
    // One colour file, the size of the canvas.
    let after = files_in(&root);
    let new: Vec<&String> = after.difference(&before).collect();
    assert_eq!(new, ["m_HOO.fits"]);
    let p = InputPanel::open(&root.join("m_HOO.fits")).unwrap();
    assert_eq!(
        (p.width(), p.height(), p.channels()),
        (CANVAS.0, CANVAS.1, 3)
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composite_with_a_missing_group_is_not_exportable() {
    let (root, mut s) = ready_session("export-missing");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[
            comp(1, "Gone", [Some("SII"), None, None]),
            comp(2, "Half", [Some("Ha"), Some("SII"), None]),
            comp(3, "Fine", [Some("Ha"), Some("OIII"), None]),
        ],
    );
    // Even when it was not left out last time (the groups were).
    s.mosaic.config.ui[EXPORT_KEY] = json!({
        "excluded": [{"group": "Ha"}, {"group": "OIII"}],
    });
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    assert_eq!(row_state(&h, "Gone"), greyed("missing group SII"));
    assert_eq!(row_state(&h, "Half"), greyed("missing group SII"));
    assert_eq!(row_state(&h, "Fine"), ticked());
    // The groups were left out last time.
    assert_eq!(row_state(&h, "Ha"), (true, false, None));
    let st = h.state();
    let gone = ExportTarget::Composite {
        id: 1,
        name: "Gone".into(),
        channels: [Some("SII".into()), None, None],
    };
    assert_eq!(
        readiness_of(&st.session, &gone),
        Readiness::MissingGroup("SII".into())
    );
    assert_eq!(
        st.w.export_dialog().ticked(),
        [Pick::Composite(3)],
        "only the ready one"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_merging_group_is_not_tickable() {
    let (root, mut s, mut jobs) = analyzed("export-merging");
    for g in ["Ha", "OIII"] {
        s.results.insert(g.into(), ResultState::Complete);
    }
    s.merged = true;
    save_compositions(
        &mut s.mosaic.config.ui,
        &[comp(1, "HOO", [Some("Ha"), Some("OIII"), Some("OIII")])],
    );
    let gate = jobs.hold();
    jobs.start_blend(&mut s, "Ha", None);
    let mut h = shown(s, jobs);
    open_dialog(&mut h);
    // Ha merges (and so does HOO, through it): neither can be ticked.
    assert_eq!(row_state(&h, "Ha"), greyed("merging\u{2026}"));
    assert_eq!(row_state(&h, "HOO"), greyed("merging\u{2026}"));
    assert_eq!(row_state(&h, "OIII"), ticked());
    assert_eq!(preview(&h, &["m_OIII.fits"]), ["m_OIII.fits"]);
    // Once the merge is over, Ha can be ticked again.
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(2);
    assert_eq!(row_state(&h, "Ha"), (true, false, None));
    row(&h, "Ha").click();
    h.run_steps(2);
    assert_eq!(row_state(&h, "Ha"), ticked());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn esc_closes_only_the_dialog() {
    let (root, s) = ready_session("export-esc");
    let panel = s.mosaic.config.panels[0].path.clone();
    let mut h = shown(s, runner());
    h.state_mut().w.set_mode(CanvasMode::Plan);
    h.run_steps(2);
    {
        let w = &mut h.state_mut().w;
        w.plan.select(Some(panel.clone()));
        w.selected = Some(panel.clone());
    }
    h.run_steps(2);
    open_dialog(&mut h);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().w.export_open(), "Esc closes the dialog");
    assert_eq!(
        h.state().w.plan().selected(),
        Some(panel.as_path()),
        "and only the dialog"
    );
    // Closed, Esc clears the selection again.
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().w.plan().selected(), None);
    // Esc in a text field of the dialog closes it too.
    open_dialog(&mut h);
    field(&h, "File name pattern").focus();
    h.run_steps(2);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().w.export_open());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_dialog_works_from_the_keyboard() {
    let (root, s) = ready_session("export-keys");
    let mut jobs = runner();
    let gate = jobs.hold_exports();
    let mut h = shown(s, jobs);
    // Ctrl+E opens it, named in Export…'s tooltip.
    let main = h.get_by_label("Export\u{2026}").rect().center();
    let keys = h.ctx.format_shortcut(&super::super::shortcuts::EXPORT);
    let want = format!("Export the merged groups and composites to FITS or TIFF ({keys})");
    assert!(tooltip_shows(&mut h, main, &want), "{want}");
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    h.run_steps(3);
    assert!(h.state().w.export_open());
    settle(&mut h);
    // Focus starts on the first row; Space unticks and ticks it.
    assert_eq!(focused_label(&h).as_deref(), Some("Ha"));
    h.key_press(Key::Space);
    h.run_steps(2);
    assert_eq!(row_state(&h, "Ha"), (true, false, None));
    h.key_press(Key::Space);
    h.run_steps(2);
    assert_eq!(row_state(&h, "Ha"), ticked());
    // Tab to OIII and leave it out.
    tab_to(&mut h, "OIII");
    h.key_press(Key::Space);
    settle(&mut h);
    assert_eq!(row_state(&h, "OIII"), (true, false, None));
    // Tab to Export and press Enter.
    tab_to(&mut h, "Export");
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(!h.state().w.export_open());
    assert_eq!(labels(&h.state().jobs), ["Export Ha"]);
    super::super::top_bar::cancel_step(EXPORT_STEP, &mut h.state_mut().jobs);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn closing_from_the_keyboard_gives_the_focus_back() {
    let (root, s) = ready_session("export-focus-back");
    let mut h = shown(s, runner());
    // Ctrl+E from a control, then Esc: the focus is back on that control.
    h.get_by_label("Outlines").focus();
    h.run_steps(2);
    assert_eq!(focused_label(&h).as_deref(), Some("Outlines"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    h.run_steps(3);
    assert!(h.state().w.export_open());
    settle(&mut h);
    assert_eq!(focused_label(&h).as_deref(), Some("Ha"), "in the dialog");
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!h.state().w.export_open());
    assert_eq!(focused_label(&h).as_deref(), Some("Outlines"), "Esc");
    // Export… pressed with Enter, then Cancel with Enter: back on Export…,
    // and the Enter that cancelled does not open the dialog again.
    h.get_by_label("Export\u{2026}").focus();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(h.state().w.export_open());
    settle(&mut h);
    tab_to(&mut h, "Cancel");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(!h.state().w.export_open(), "Cancel closed it");
    assert_eq!(focused_label(&h).as_deref(), Some("Export\u{2026}"));
    h.run_steps(3);
    assert!(!h.state().w.export_open(), "and it stays closed");
    assert_eq!(focused_label(&h).as_deref(), Some("Export\u{2026}"));
    // Closed with a click (Cancel): the focus is left where the click put it.
    h.get_by_label("Export\u{2026}").focus();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(3);
    settle(&mut h);
    footer_button(&h, "Cancel").click();
    h.run_steps(3);
    assert!(!h.state().w.export_open());
    assert_ne!(focused_label(&h).as_deref(), Some("Export\u{2026}"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_dialog_is_a_dialog_named_export() {
    let (root, s) = ready_session("export-a11y");
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    let dialog = h.get_by_role(Role::Dialog);
    assert_eq!(dialog.accesskit_node().label().as_deref(), Some("Export"));
    assert!(dialog.accesskit_node().data().is_modal());
    assert!(
        dialog.query_by_label("Cancel").is_some(),
        "the footer is in it"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_dialog_opens_once_something_is_merged() {
    let root = temp_dir("export-nothing-merged");
    let s = two_group_session(&root);
    let mut h = shown(s, runner());
    h.run_steps(2);
    let st = h.state();
    assert_eq!(
        st.w.export_block_reason(&st.session, &st.jobs),
        Some("Nothing is merged yet.")
    );
    // Neither Ctrl+E nor the Export step opens it.
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    h.run_steps(2);
    h.get_by_label("Export").click();
    h.run_steps(2);
    assert!(!h.state().w.export_open());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_marks_stay_while_the_folder_is_looked_at_again() {
    let (root, s) = ready_session("export-marks-stay");
    std::fs::write(root.join("m_Ha.fits"), b"keep").unwrap();
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    let overwrite = "Overwrite and export";
    let at = footer_button(&h, overwrite).rect();
    // The names change (TIFF): until the folder has answered for them, the
    // last answer's marks and overwrite line stay, and Export waits where
    // it was.
    let release = h.state_mut().w.hold_export_checks();
    h.get_by_label("TIFF").click();
    h.run_steps(3);
    let tifs = ["m_Ha.tif", "m_OIII.tif"];
    assert_eq!(preview(&h, &tifs), ["m_Ha.tif exists", "m_OIII.tif"]);
    h.get_by_label("1 file already exists and will be replaced");
    h.get_by_label("Checking the folder\u{2026}");
    assert!(footer_disabled(&h, overwrite));
    assert_eq!(footer_button(&h, overwrite).rect(), at, "the footer stays");
    // Answered: no such TIFF.
    drop(release);
    settle(&mut h);
    assert_eq!(preview(&h, &tifs), tifs);
    assert!(h.query_by_label_contains("will be replaced").is_none());
    assert!(!footer_disabled(&h, "Export"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn colliding_names_say_how_to_part_them() {
    let (root, mut s) = ready_session("export-collide-case");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[comp(1, "ha", [Some("Ha"), None, None])],
    );
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    let rename = "Two exports would have the same file name: rename one of them";
    let add_name = "Two exports would have the same file name: add {name} to the pattern";
    // The group Ha and the composite ha differ only in case: one file on
    // Windows and macOS. The pattern has {name} already.
    retype(&mut h, "File name pattern", "{name}");
    settle(&mut h);
    let names = ["Ha.fits", "OIII.fits", "ha.fits"];
    assert_eq!(
        preview(&h, &names),
        ["Ha.fits same name", "OIII.fits", "ha.fits same name"]
    );
    h.get_by_label(rename);
    assert!(h.query_by_label(add_name).is_none());
    assert!(footer_disabled(&h, "Export"));
    // Without {name}, adding it is the way.
    retype(&mut h, "File name pattern", "{session}");
    settle(&mut h);
    h.get_by_label(add_name);
    assert!(h.query_by_label(rename).is_none());
    // The composite left out: none.
    retype(&mut h, "File name pattern", "{name}");
    row(&h, "ha").click();
    settle(&mut h);
    assert!(h.query_by_label(rename).is_none());
    assert!(!footer_disabled(&h, "Export"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// Export Ha alone to `m.fits` (pattern `{session}`) from the dialog; its
/// export is held (`gate`).
fn export_ha_to_m_fits(h: &mut Harness<'static, State>) {
    open_dialog(h);
    row(h, "OIII").click();
    retype(h, "File name pattern", "{session}");
    settle(h);
    assert_eq!(preview(h, &["m.fits"]), ["m.fits"]);
    footer_button(h, "Export").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Export Ha"]);
}

/// Open the dialog again and pick OIII alone, for the same `m.fits`.
fn oiii_to_m_fits(h: &mut Harness<'static, State>) {
    open_dialog(h);
    row(h, "Ha").click();
    row(h, "OIII").click();
    settle(h);
}

#[test]
fn an_export_on_its_way_needs_overwrite() {
    let (root, s, mut jobs) = merged("export-on-its-way");
    let gate = jobs.hold_exports();
    let mut h = shown(s, jobs);
    export_ha_to_m_fits(&mut h);
    assert!(!root.join("m.fits").exists(), "held");
    // m.fits is not there yet, but Ha's export will write it: it counts
    // as there.
    oiii_to_m_fits(&mut h);
    assert_eq!(preview(&h, &["m.fits"]), ["m.fits exists"]);
    h.get_by_label("1 file already exists and will be replaced");
    footer_button(&h, "Overwrite and export").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Export Ha", "Export OIII"]);
    // OIII's export replaces what Ha's wrote: confirmed.
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let finished = |label: &str| {
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label: l, .. } if l == label))
    };
    assert!(
        finished("Export Ha") && finished("Export OIII"),
        "{:?}",
        h.state().events
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_folder_is_looked_at_again_when_an_export_ends() {
    let (root, s, mut jobs) = merged("export-relook");
    let gate = jobs.hold_exports();
    let mut h = shown(s, jobs);
    export_ha_to_m_fits(&mut h);
    oiii_to_m_fits(&mut h);
    assert_eq!(preview(&h, &["m.fits"]), ["m.fits exists"]);
    // Ha's export ends with the dialog open: m.fits is on disk now, and
    // the dialog looks again.
    gate.open();
    step_until(&mut h, |st| st.jobs.export_destinations().is_empty());
    assert!(root.join("m.fits").is_file());
    settle(&mut h);
    assert_eq!(preview(&h, &["m.fits"]), ["m.fits exists"]);
    // Confirmed, OIII's export replaces it.
    footer_button(&h, "Overwrite and export").click();
    h.run_steps(2);
    step_until(&mut h, |st| !st.jobs.busy());
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label, .. } if label == "Export OIII")),
        "{:?}",
        h.state().events
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// Whether the last frame painted a 2 pt accent ring around `rect`.
fn has_focus_ring(h: &Harness<'static, State>, rect: egui::Rect) -> bool {
    h.output().shapes.iter().any(|c| match &c.shape {
        egui::Shape::Rect(r) => {
            r.stroke.width >= 2.0
                && r.stroke.color == mega_ui::theme::palette::ACCENT_400
                && r.rect.contains_rect(rect)
        }
        _ => false,
    })
}

#[test]
fn a_focused_row_shows_the_focus_ring() {
    let (root, s) = ready_session("export-ring");
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    let (ha, oiii) = (row(&h, "Ha").rect(), row(&h, "OIII").rect());
    assert_eq!(focused_label(&h).as_deref(), Some("Ha"));
    assert!(has_focus_ring(&h, ha) && !has_focus_ring(&h, oiii));
    tab_to(&mut h, "OIII");
    assert!(has_focus_ring(&h, oiii) && !has_focus_ring(&h, ha));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder dialog that cannot be shown (its thread dies).
struct BrokenFolder;

impl FolderPicker for BrokenFolder {
    fn pick_folder(&self, _start: Option<&std::path::Path>) -> Option<PathBuf> {
        panic!("no folder dialog here (test)");
    }
}

#[test]
fn a_folder_picker_that_fails_says_so() {
    let (root, s) = ready_session("export-broken-picker");
    let mut h = shown(s, runner());
    h.state_mut().w.set_folder_picker(Arc::new(BrokenFolder));
    open_dialog(&mut h);
    h.get_by_label("Browse\u{2026}").click();
    step_until(&mut h, |st| !st.toasts.is_empty());
    let toast = &h.state().toasts[0].toast;
    assert_eq!(toast.title, "The folder picker could not be shown.");
    assert_eq!(toast.tone, mega_ui::theme::Tone::Problem);
    // The dialog stays, Browse… can be tried again.
    assert!(h.state().w.export_open());
    h.run_steps(2);
    assert!(
        !h.get_by_label("Browse\u{2026}")
            .accesskit_node()
            .is_disabled()
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}
