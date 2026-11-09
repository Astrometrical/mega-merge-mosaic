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
    // Names differing only in case collide too (Windows and macOS folders).
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
fn a_folder_that_cannot_take_the_files_blocks_export() {
    let (root, s) = ready_session("export-folder");
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    // Beside the session folder, where a file could be created.
    let tried = |h: &Harness<'static, State>| {
        h.state()
            .w
            .export_dialog()
            .write_tried()
            .map(std::path::Path::to_path_buf)
    };
    let root_text = crate::util::plain_path(&root);
    assert_eq!(tried(&h), Some(PathBuf::from(&root_text)));
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
    // Typed, nothing is tried in a folder (not in every one on the way).
    let out_text = crate::util::plain_path(&out);
    retype(&mut h, "Folder", &out_text);
    settle(&mut h);
    assert_eq!(tried(&h), Some(PathBuf::from(&root_text)));
    // Left, it is tried, and Export can go.
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(tried(&h), Some(PathBuf::from(&out_text)));
    assert!(!footer_disabled(&h, "Export"));
    assert_eq!(files_in(&out), BTreeSet::new(), "the try left nothing");
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
        "selection": [{"group": "Ha"}],
    });
    assert_eq!(h.state().session.mosaic.config.ui[EXPORT_KEY], want);
    let dir = h.state().session.mosaic.dir.clone();
    step_until(&mut h, |_| {
        mmm_core::mosaic::Mosaic::open(&dir).is_ok_and(|m| m.config.ui[EXPORT_KEY] == want)
    });
    super::super::top_bar::cancel_step(EXPORT_STEP, &mut h.state_mut().jobs);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    // Opened again: as chosen; OIII, ready but not chosen, stays unticked.
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
    // Even when it was chosen last time.
    s.mosaic.config.ui[EXPORT_KEY] = json!({
        "selection": [{"composite": 1}, {"composite": 2}, {"composite": 3}],
    });
    let mut h = shown(s, runner());
    open_dialog(&mut h);
    assert_eq!(row_state(&h, "Gone"), greyed("missing group SII"));
    assert_eq!(row_state(&h, "Half"), greyed("missing group SII"));
    assert_eq!(row_state(&h, "Fine"), ticked());
    // The groups were not chosen last time.
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
