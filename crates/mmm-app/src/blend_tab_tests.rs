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
