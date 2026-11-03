use std::time::{Duration, Instant};

use mmm_core::mosaic::MosaicConfig;

use super::*;
use crate::start::tests::temp_dir;

#[test]
fn stretch_persisted_per_group_in_ui() {
    let a = StretchParams {
        black: 0.01,
        white: 1.0,
        midtones: 0.2,
    };
    let b = StretchParams {
        black: 0.02,
        white: 0.9,
        midtones: 0.3,
    };
    let mut cfg = MosaicConfig::new(Vec::new());
    cfg.ui = serde_json::json!({ "other": 1 });
    save_stretch(&mut cfg.ui, "Ha", Some(&[a]));
    save_stretch(&mut cfg.ui, "OIII", Some(&[a, b, b]));
    let dir = temp_dir("blend-stretch");
    cfg.save(&dir).unwrap();
    let back = MosaicConfig::load(&dir).unwrap();
    assert_eq!(saved_stretch(&back.ui, "Ha"), Some(vec![a]));
    assert_eq!(saved_stretch(&back.ui, "OIII"), Some(vec![a, b, b]));
    assert_eq!(saved_stretch(&back.ui, "SII"), None);
    assert!(back.ui["blend_stretch"]["Ha"].is_array());
    assert_eq!(back.ui["other"], 1);

    // Back to automatic: the group's entry goes, the rest stays.
    let mut ui = back.ui;
    save_stretch(&mut ui, "Ha", None);
    assert_eq!(saved_stretch(&ui, "Ha"), None);
    assert_eq!(saved_stretch(&ui, "OIII"), Some(vec![a, b, b]));
    assert_eq!(ui["other"], 1);
    // A non-object `ui` (e.g. null from an older file) is replaced.
    let mut null = serde_json::Value::Null;
    save_stretch(&mut null, "Ha", Some(&[b]));
    assert_eq!(saved_stretch(&null, "Ha"), Some(vec![b]));

    // Saves are debounced: at most one per SAVE_INTERVAL.
    let t0 = Instant::now();
    let mut d = Debounce::default();
    assert!(!d.due(t0));
    d.touch();
    assert!(d.due(t0), "the first change saves at once");
    d.saved(t0);
    d.touch();
    assert!(!d.due(t0 + Duration::from_millis(300)));
    assert!(d.due(t0 + SAVE_INTERVAL));
    d.saved(t0 + SAVE_INTERVAL);
    assert!(!d.due(t0 + 3 * SAVE_INTERVAL), "nothing new to save");
    let _ = std::fs::remove_dir_all(&dir);
}
/// The member state of `group`.
fn member<'a>(tab: &'a mut BlendTab, group: &str) -> &'a mut Member {
    tab.results.get_mut(group).expect("a member of the view")
}

#[test]
fn blend_disabled_until_result_checked_and_reader_released() {
    use mmm_core::tiles::{TileGeometry, TileStore};

    let mut tab = BlendTab::new();
    tab.select("Ha".into());
    // Not checked yet: the result is still to be opened.
    assert!(!tab.can_blend(Readiness::Analyzed));
    member(&mut tab, "Ha").reopen = false;
    assert!(tab.can_blend(Readiness::Analyzed));
    assert!(!tab.can_blend(Readiness::NeedsAnalyze));
    assert!(!tab.can_blend(Readiness::NotAnalyzed));

    // While an analysis runs (a blend would queue behind it and start
    // before the result is checked again).
    tab.analyze_job = Some(7);
    assert!(!tab.can_blend(Readiness::Analyzed));
    // Its end asks for a check of the result: still disabled.
    tab.on_event(&JobEvent::Cancelled { id: 7 });
    assert!(tab.analyze_job.is_none() && member(&mut tab, "Ha").reopen);
    assert!(!tab.can_blend(Readiness::Analyzed));
    member(&mut tab, "Ha").reopen = false;

    // A given-up source still mapped (by a loader thread, say).
    let root = temp_dir("blend-retired");
    let g = TileGeometry {
        width: 300,
        height: 200,
        channels: 1,
        tile: 256,
    };
    let store = |name: &str| {
        let path = root.join(name);
        drop(TileStore::create(&path, g).unwrap());
        Arc::new(MosaicTileSource::open(&path, None).unwrap())
    };
    let held = store("ha.mmmt");
    member(&mut tab, "Ha").source = Some(held.clone());
    tab.retire_all();
    assert!(tab.source().is_none() && tab.viewer.is_none());
    tab.retired.retain(|w| w.strong_count() > 0);
    assert!(!tab.can_blend(Readiness::Analyzed));
    drop(held);
    tab.retired.retain(|w| w.strong_count() > 0);
    assert!(tab.can_blend(Readiness::Analyzed));

    // A composite holds every member's reader: none of the view's groups
    // may blend until it has been dropped, after all members are given up.
    let ha = store("ha2.mmmt");
    let oiii = store("oiii.mmmt");
    tab.apply_view(
        ViewKind::Composition(0),
        vec!["Ha".to_string(), "OIII".to_string()],
    );
    for (name, src) in [("Ha", &ha), ("OIII", &oiii)] {
        let m = member(&mut tab, name);
        m.reopen = false;
        m.source = Some(src.clone());
    }
    let composite = Arc::new(
        CompositeTileSource::new(&[Some((ha.clone(), 1.0)), Some((oiii.clone(), 1.0)), None])
            .unwrap(),
    );
    drop((ha, oiii));
    // Only OIII is given up (say its result went out of date), but the
    // composite still maps it.
    let m = tab.results.get_mut("OIII").unwrap();
    m.retire(&mut tab.retired);
    tab.retired.retain(|w| w.strong_count() > 0);
    assert!(!tab.can_blend(Readiness::Analyzed));
    drop(composite);
    tab.retired.retain(|w| w.strong_count() > 0);
    assert!(tab.retired.is_empty());

    // A blend that ends with no result open asks for an open.
    tab.select("Ha".into());
    member(&mut tab, "Ha").reopen = false;
    tab.blend_jobs.insert(9, "Ha".into());
    tab.on_event(&JobEvent::Finished {
        id: 9,
        label: "Blend Ha".into(),
    });
    assert!(tab.blend_jobs.is_empty() && member(&mut tab, "Ha").reopen && tab.want_health);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_open_while_shown_groups_blend_is_unstarted() {
    let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-unstarted-open");
    let ctx = egui::Context::default();
    let gate = jobs.hold();
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);

    // Switching to a group whose blend is running without progress, or
    // queued, opens nothing: the blend may still (re)create the store.
    let mut tab = BlendTab::new();
    for g in ["Ha", "OIII"] {
        tab.select(g.into());
        tab.update(&ctx, &mut s, &jobs);
        assert!(
            member(&mut tab, g).opening.is_none(),
            "{g}: opened during its blend"
        );
        assert!(member(&mut tab, g).reopen, "{g}: open forgotten");
    }

    // Once the blends end, the deferred open happens.
    gate.open();
    crate::jobs::tests::drain(&mut jobs, &mut s);
    tab.update(&ctx, &mut s, &jobs);
    let m = member(&mut tab, "OIII");
    assert!(m.opening.is_some() && !m.reopen);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pending_stretch_save_is_flushed() {
    let root = temp_dir("blend-flush-save");
    let mut s = crate::testutil::two_group_session(&root);
    s.mosaic.save().unwrap();
    let p = StretchParams {
        black: 0.01,
        white: 0.9,
        midtones: 0.2,
    };
    let mut tab = BlendTab::new();
    // A change saved at once, then a second one held back by the rate
    // limit (not due yet)...
    tab.save.saved(Instant::now());
    save_stretch(&mut s.mosaic.config.ui, "Ha", Some(&[p]));
    tab.save.touch();
    assert!(!tab.save.due(Instant::now()));
    // ...is written by the flush (window closing), and only once.
    tab.flush_save(&s);
    let back = MosaicConfig::load(&s.mosaic.dir).unwrap();
    assert_eq!(saved_stretch(&back.ui, "Ha"), Some(vec![p]));
    assert!(!tab.save.due(Instant::now() + SAVE_INTERVAL));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn linked_auto_without_statistics_falls_back_to_automatic() {
    use mmm_core::tiles::{TileGeometry, TileStore};

    let root = temp_dir("blend-auto-none");
    let mut s = crate::testutil::two_group_session(&root);
    let ctx = egui::Context::default();
    // An empty (never blended) result: no statistics to stretch from.
    let path = root.join("result.mmmt");
    let g = TileGeometry {
        width: 300,
        height: 200,
        channels: 3,
        tile: 256,
    };
    drop(TileStore::create(&path, g).unwrap());
    let mut tab = BlendTab::new();
    tab.select("Ha".into());
    let m = member(&mut tab, "Ha");
    m.reopen = false;
    m.source = Some(Arc::new(MosaicTileSource::open(&path, None).unwrap()));
    m.linked = true;
    tab.auto_now(&ctx, &mut s);
    let m = member(&mut tab, "Ha");
    assert!(m.manual && m.save_auto_result);
    let deadline = Instant::now() + Duration::from_secs(30);
    while member(&mut tab, "Ha").auto.is_some() {
        assert!(Instant::now() < deadline, "auto stretch never ended");
        std::thread::sleep(Duration::from_millis(2));
        tab.poll_auto(&mut s);
    }
    // Nothing was saved, so the stretch is automatic again...
    assert_eq!(saved_stretch(&s.mosaic.config.ui, "Ha"), None);
    assert!(
        !member(&mut tab, "Ha").manual,
        "left manual with no saved stretch"
    );
    // ...and the next finished blend stretches automatically.
    tab.blend_jobs.insert(3, "Ha".into());
    tab.on_event(&JobEvent::Finished {
        id: 3,
        label: "Blend Ha".into(),
    });
    assert!(member(&mut tab, "Ha").want_auto);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn roi_blend_starts_one_blend_per_view_group() {
    use crate::compositions::{Composition, ViewKind, save_compositions};
    use crate::testutil::CANVAS;

    let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-roi-groups");
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 0.5)),
    ];
    save_compositions(&mut s.mosaic.config.ui, &[hoo]);
    let mut tab = BlendTab::new();
    tab.set_view(&s, ViewKind::Composition(0));
    assert_eq!(tab.members, ["Ha", "OIII"]);
    assert_eq!(
        tab.group(),
        Some("Ha"),
        "the first member's stretch is shown"
    );

    // A drawn rect becomes "ROI 1" in whole canvas pixels, selected and
    // saved in the session's ui.
    tab.on_drawn(&mut s, [10.4, 20.0, 299.5, 200.0], CANVAS);
    let rois = tab.rois.rois();
    assert_eq!(rois.len(), 1);
    assert_eq!(rois[0].name, "ROI 1");
    assert_eq!(rois[0].canvas, [10, 20, 300, 200]);
    assert_eq!(tab.rois.selected, Some(0));
    assert!(tab.save.pending() && tab.rois.want_sync && tab.rois.want_fresh);
    assert_eq!(
        crate::rois::load_rois(&s.mosaic.config.ui),
        tab.rois.rois(),
        "kept in ui"
    );
    tab.on_drawn(&mut s, [0.0, 0.0, 50.0, 50.0], CANVAS);
    assert_eq!(tab.rois.rois()[1].name, "ROI 2");
    assert_eq!(tab.rois.selected, Some(1));
    tab.rois.selected = Some(0);

    // Not while the ROIs are being reprojected (the rect may be from an
    // older frame).
    tab.blend_roi(&mut s, &mut jobs);
    assert!(!jobs.busy() && tab.blend_jobs.is_empty());
    tab.rois.want_sync = false;

    // Blend ROI: one blend of the ROI for each group of the view.
    let gate = jobs.hold();
    tab.blend_roi(&mut s, &mut jobs);
    let labels: Vec<String> = jobs.views().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["Blend Ha", "Blend OIII"]);
    let mut groups: Vec<&String> = tab.blend_jobs.values().collect();
    groups.sort();
    assert_eq!(groups, ["Ha", "OIII"]);
    gate.open();
    for e in crate::jobs::tests::drain(&mut jobs, &mut s) {
        assert!(matches!(e, JobEvent::Finished { .. }), "{e:?}");
        tab.on_event(&e);
    }
    assert!(tab.blend_jobs.is_empty());
    let blends: Vec<_> = s
        .mosaic
        .config
        .provenance
        .iter()
        .filter(|p| p.action == "blend")
        .map(|p| (p.group.clone(), p.detail["rect"].clone()))
        .collect();
    assert_eq!(blends.len(), 2);
    // Each blend covered the ROI (recorded tile-aligned: 256-px tiles).
    for (_, rect) in &blends {
        assert_eq!(*rect, serde_json::json!([0, 0, 512, 256]));
    }

    // A group view blends just that group's ROI...
    tab.set_view(&s, ViewKind::Group("OIII".into()));
    assert_eq!(tab.members, ["OIII"]);
    let gate = jobs.hold();
    tab.blend_roi(&mut s, &mut jobs);
    let labels: Vec<String> = jobs.views().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["Blend OIII"]);
    gate.open();
    crate::jobs::tests::drain(&mut jobs, &mut s);
    // ...but "Blend whole mosaic" blends every group (spec §5.4).
    let gate = jobs.hold();
    tab.blend_whole(&mut s, &mut jobs);
    let labels: Vec<String> = jobs.views().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["Blend Ha", "Blend OIII"]);
    gate.open();
    crate::jobs::tests::drain(&mut jobs, &mut s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn composition_shows_blended_members() {
    use crate::compositions::{Composition, ViewKind, load_compositions, save_compositions};

    let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-partial-comp");
    jobs.start_blend(&mut s, "Ha", None);
    crate::jobs::tests::drain(&mut jobs, &mut s);
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 0.5)),
    ];
    save_compositions(&mut s.mosaic.config.ui, &[hoo]);
    let compositions = load_compositions(&s.mosaic.config.ui);
    let ctx = egui::Context::default();
    let mut tab = BlendTab::new();
    tab.set_view(&s, ViewKind::Composition(0));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !tab.results.values().all(Member::idle) {
        assert!(Instant::now() < deadline, "results never opened");
        std::thread::sleep(Duration::from_millis(2));
        tab.update(&ctx, &mut s, &jobs);
    }
    tab.sync_viewer(&compositions);
    // OIII was never blended: Ha shows, and OIII is named.
    assert!(tab.results["Ha"].source.is_some());
    assert!(tab.results["OIII"].source.is_none());
    assert!(tab.viewer.is_some(), "the blended member is shown");
    assert_eq!(tab.member_notes(), ["OIII: Not blended yet."]);
    assert_eq!(tab.shown_text(), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// Answers the save dialog with a fixed path.
struct FixedSave(std::path::PathBuf);

impl crate::export::SavePicker for FixedSave {
    fn pick_save(
        &self,
        _format: crate::export::ExportFormat,
        _name: &str,
    ) -> Option<std::path::PathBuf> {
        Some(self.0.clone())
    }
}

struct ExportState {
    tab: BlendTab,
    session: OpenSession,
    jobs: JobRunner,
    events: Vec<JobEvent>,
}

#[test]
fn export_dialog_warns_then_runs() {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mmm_core::formats::InputPanel;

    use crate::start::tests::step_until;
    use crate::testutil::CANVAS;

    let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-export-dialog");
    // Only the top-left tile of Ha is blended: 5 of its 6 tiles missing.
    jobs.start_blend(&mut s, "Ha", Some([0, 0, 256, 256]));
    crate::jobs::tests::drain(&mut jobs, &mut s);
    let picked = root.join("picked.fits");
    let mut tab = BlendTab::new();
    tab.select("Ha".into());
    tab.export.set_picker(Arc::new(FixedSave(picked.clone())));
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, st: &mut ExportState| {
                // The tab's background work (opening the result), then
                // the dialog, as `BlendTab::ui` does each frame.
                st.tab.update(ui.ctx(), &mut st.session, &st.jobs);
                st.tab.export_ui(ui.ctx(), &mut st.session, &mut st.jobs);
                let events = st.jobs.poll(&mut st.session);
                for e in &events {
                    st.tab.on_event(e);
                }
                st.events.extend(events);
            },
            ExportState {
                tab,
                session: s,
                jobs,
                events: Vec::new(),
            },
        );
    // Wait until the result is checked (Blend becomes possible).
    step_until(&mut h, |st| st.tab.can_blend(Readiness::Analyzed));
    {
        let st = h.state_mut();
        st.tab.open_export(&st.session);
    }
    h.run_steps(2);
    assert!(h.state().tab.export.is_open());

    // Browse… fills the destination from the save dialog.
    h.get_by_label("Browse…").click();
    let want = picked.display().to_string();
    step_until(&mut h, |st| st.tab.export.path() == want);

    // Export checks first: the result is partly missing, so it warns.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
        .click();
    step_until(&mut h, |st| st.tab.export.warning_shown());
    h.get_by_label_contains("Ha: 5 missing tile(s)");
    assert!(
        !h.state().jobs.busy(),
        "nothing runs before the user decides"
    );
    assert!(!picked.exists());

    // "Blend whole mosaic first" blends the chosen groups; no export.
    h.get_by_label("Blend whole mosaic first").click();
    h.run_steps(1);
    assert!(!h.state().tab.export.is_open());
    let labels: Vec<String> = h
        .state()
        .jobs
        .views()
        .into_iter()
        .map(|v| v.label)
        .collect();
    assert_eq!(labels, ["Blend Ha"]);
    step_until(&mut h, |st| {
        !st.jobs.busy() && st.tab.can_blend(Readiness::Analyzed)
    });
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label, .. } if label == "Blend Ha"))
    );

    // Stale it all, then "Export anyway" runs the export as a job.
    {
        let st = h.state_mut();
        mmm_core::mosaic::invalidate_group(&st.session.mosaic, "Ha").unwrap();
        st.tab.open_export(&st.session);
    }
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
        .click();
    step_until(&mut h, |st| st.tab.export.warning_shown());
    h.get_by_label_contains("Ha: 6 stale tile(s)");
    h.get_by_label("Export anyway").click();
    h.run_steps(1);
    assert!(!h.state().tab.export.is_open());
    let labels: Vec<String> = h
        .state()
        .jobs
        .views()
        .into_iter()
        .map(|v| v.label)
        .collect();
    assert_eq!(labels, ["Export Ha"]);
    step_until(&mut h, |st| !st.jobs.busy());
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label, .. } if label == "Export Ha")),
        "{:?}",
        h.state().events
    );
    let p = InputPanel::open(&picked).unwrap();
    assert_eq!(
        (p.width(), p.height(), p.channels()),
        (CANVAS.0, CANVAS.1, 3)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn absent_reason_hidden_while_reopening() {
    // A reason from an earlier open is not reported while the result is
    // being opened again (the shown text is "Opening the result…" then).
    let mut tab = BlendTab::new();
    tab.select("Ha".into());
    let m = member(&mut tab, "Ha");
    m.absent = Some("Cannot read the group's analysis: gone".into());
    m.reopen = true;
    assert_eq!(tab.absent(), None);
    member(&mut tab, "Ha").reopen = false;
    assert_eq!(tab.absent(), Some("Cannot read the group's analysis: gone"));
}

/// The export dialog alone, as the Blend tab drives it: the user's
/// decisions are collected, nothing is started.
struct DialogState {
    dialog: crate::export::ExportDialog,
    session: OpenSession,
    jobs: JobRunner,
    blending: std::collections::BTreeSet<String>,
    actions: Vec<crate::export::ExportAction>,
}

fn dialog_harness(
    session: OpenSession,
    jobs: JobRunner,
    groups: Vec<String>,
) -> egui_kittest::Harness<'static, DialogState> {
    let mut dialog = crate::export::ExportDialog::new(Arc::new(FixedSave(Default::default())));
    dialog.open(&session, groups);
    egui_kittest::Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, st: &mut DialogState| {
                st.jobs.poll(&mut st.session);
                st.blending = st
                    .session
                    .mosaic
                    .config
                    .group_dirs
                    .keys()
                    .filter(|g| st.jobs.blend_pending(g))
                    .cloned()
                    .collect();
                let can_blend = true;
                if let Some(a) = st.dialog.ui(ui.ctx(), &st.session, can_blend, &st.blending) {
                    st.actions.push(a);
                }
            },
            DialogState {
                dialog,
                session,
                jobs,
                blending: Default::default(),
                actions: Vec::new(),
            },
        )
}

fn export_button<'h>(h: &'h egui_kittest::Harness<'static, DialogState>) -> egui_kittest::Node<'h> {
    use egui_kittest::kittest::Queryable;
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
}

fn export_disabled(h: &egui_kittest::Harness<'static, DialogState>) -> bool {
    use egui_kittest::kittest::NodeT;
    export_button(h).accesskit_node().is_disabled()
}

#[test]
fn export_disabled_while_a_chosen_group_blends() {
    use crate::start::tests::step_until;

    let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-export-blending");
    let gate = jobs.hold();
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    let mut h = dialog_harness(s, jobs, vec!["Ha".into()]);
    {
        let st = h.state_mut();
        st.dialog
            .set_path(&root.join("ha.fits").display().to_string());
    }
    h.run_steps(2);
    // Ha's blend runs: no check (it maps the store the blend may recreate).
    assert!(export_disabled(&h));
    export_button(&h).click();
    h.run_steps(2);
    assert!(h.state().dialog.idle(), "no check started");
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(2);
    assert!(!export_disabled(&h));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_snapshots_choices_and_confirms_overwrite() {
    use egui_kittest::kittest::Queryable;

    use crate::export::{ExportAction, ExportFormat};
    use crate::start::tests::step_until;

    let (root, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-export-overwrite");
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    crate::jobs::tests::drain(&mut jobs, &mut s);

    // One group to an existing file: confirm first, listing it.
    let dest = root.join("ha.fits");
    std::fs::write(&dest, b"keep").unwrap();
    let mut h = dialog_harness(s, jobs, vec!["Ha".into()]);
    h.state_mut().dialog.set_path(&dest.display().to_string());
    h.run_steps(2);
    export_button(&h).click();
    step_until(&mut h, |st| st.dialog.overwrite_shown());
    h.get_by_label_contains(&dest.display().to_string());
    // Back: nothing decided, the file untouched.
    h.get_by_label("Back").click();
    h.run();
    assert!(h.state().actions.is_empty());
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep");
    // Again, then change the choices while the confirmation shows: the
    // export uses what was checked.
    export_button(&h).click();
    step_until(&mut h, |st| st.dialog.overwrite_shown());
    {
        let st = h.state_mut();
        st.dialog
            .set_path(&root.join("other.tif").display().to_string());
        st.dialog.set_format(ExportFormat::Tiff);
    }
    h.get_by_label("Overwrite").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        [ExportAction::Export(
            vec![("Ha".to_string(), dest.clone())],
            ExportFormat::Fits
        )]
    );
    drop(h);

    // Several groups: the derived names are checked too.
    let (root2, mut s, mut jobs) = crate::jobs::tests::analyzed("blend-export-overwrite2");
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    crate::jobs::tests::drain(&mut jobs, &mut s);
    let tag = s.mosaic.config.group_dirs["OIII"].clone();
    let existing = root2.join(format!("m-{tag}.fits"));
    std::fs::write(&existing, b"keep").unwrap();
    let mut h = dialog_harness(s, jobs, vec!["Ha".into(), "OIII".into()]);
    h.state_mut()
        .dialog
        .set_path(&root2.join("m.fits").display().to_string());
    h.run_steps(2);
    export_button(&h).click();
    step_until(&mut h, |st| st.dialog.overwrite_shown());
    h.get_by_label_contains(&existing.display().to_string());
    assert!(
        h.query_by_label_contains(&root2.join("m-Ha").display().to_string())
            .is_none(),
        "only existing files are listed"
    );
    h.get_by_label("Overwrite").click();
    h.run_steps(2);
    assert!(matches!(&h.state().actions[..], [ExportAction::Export(d, _)] if d.len() == 2));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[test]
fn previews_do_not_move_persisted_rois() {
    use mmm_core::mosaic::{FRAME_FILE, Mosaic, set_inputs};
    use mmm_core::reference::ReferenceFrame;
    use mmm_core::synth::{SynthWcs, write_xisf_solved};

    use crate::rois::{Roi, load_rois, reproject, save_rois};

    let root = temp_dir("blend-roi-frame");
    let (w, h) = (200u64, 150u64);
    let panel = |name: &str, refimg: [f64; 2]| {
        let path = root.join(name);
        let wcs = SynthWcs {
            crval: [83.8, -5.4],
            refimg,
            cd: [[-1e-3, 0.0], [0.0, 1e-3]],
        };
        write_xisf_solved(&path, w, h, 1, &vec![0.1f32; (w * h) as usize], &wcs).unwrap();
        path
    };
    let (a, b) = (
        panel("a.xisf", [100.0, 75.0]),
        panel("b.xisf", [40.0, 45.0]),
    );
    let mut mosaic = Mosaic::create(
        &root.join("s.mmm-session"),
        MosaicConfig::new(vec![a.clone(), b.clone()]),
    )
    .unwrap();
    // The frame of the last analysis (as analysis saves it), and an ROI
    // anchored on the sky in it.
    let input = mosaic.config.analyze.input;
    let analysed = mmm_core::reference::derive(&[a.clone(), b.clone()], input).unwrap();
    analysed.save(&mosaic.dir.join(FRAME_FILE)).unwrap();
    let roi = reproject(
        &[Roi {
            name: "ROI 1".into(),
            note: String::new(),
            sky: None,
            canvas: [20, 20, 80, 60],
        }],
        Some(&analysed),
    )
    .remove(0);
    assert!(roi.sky.is_some());
    save_rois(&mut mosaic.config.ui, std::slice::from_ref(&roi));
    // A panel added up and to the left: the panels' frame grows and its
    // origin moves (no analysis yet).
    let c = panel("c.xisf", [250.0, 200.0]);
    let panels = MosaicConfig::new(vec![a.clone(), b.clone(), c.clone()]).panels;
    let grouping = mosaic.config.grouping.clone();
    set_inputs(&mut mosaic, panels, grouping).unwrap();
    let grown = mmm_core::reference::derive(&[a, b, c], input).unwrap();
    assert_ne!(grown, analysed);
    assert_ne!(
        reproject(std::slice::from_ref(&roi), Some(&grown))[0].canvas,
        roi.canvas
    );
    let groups = mosaic.groups().ok();
    let mut s = OpenSession {
        mosaic,
        frame: Some(analysed.clone()),
        groups,
        lock: None,
    };
    // The previews run in the panels' frame...
    let mut jobs = JobRunner::new(egui::Context::default());
    jobs.start_previews(&s);
    crate::jobs::tests::drain(&mut jobs, &mut s);
    assert_eq!(
        ReferenceFrame::load(&s.mosaic.dir.join(FRAME_FILE)).unwrap(),
        analysed,
        "previews rewrote the analysed frame"
    );
    // ...and the Blend tab's ROI sync, on opening, keeps the ROI where the
    // analysed frame puts it.
    let ctx = egui::Context::default();
    let mut tab = BlendTab::new();
    tab.rois.load_once(&s.mosaic.config.ui);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        tab.update(&ctx, &mut s, &jobs);
        if tab.rois.synced() && tab.rois.idle() {
            break;
        }
        assert!(Instant::now() < deadline, "the ROI sync never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(tab.rois(), std::slice::from_ref(&roi));
    assert_eq!(load_rois(&s.mosaic.config.ui), [roi]);
    let _ = std::fs::remove_dir_all(&root);
}
