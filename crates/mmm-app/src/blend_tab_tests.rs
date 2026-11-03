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
#[test]
fn blend_disabled_until_result_checked_and_reader_released() {
    use mmm_core::tiles::{TileGeometry, TileStore};

    let mut tab = BlendTab::new();
    tab.group = Some("Ha".into());
    assert!(tab.can_blend(Readiness::Analyzed));
    assert!(!tab.can_blend(Readiness::NeedsAnalyze));
    assert!(!tab.can_blend(Readiness::NotAnalyzed));

    // While an analysis runs (a blend would queue behind it and start
    // before the result is checked again).
    tab.analyze_job = Some(7);
    assert!(!tab.can_blend(Readiness::Analyzed));
    // Its end asks for a check of the result: still disabled.
    tab.on_event(&JobEvent::Cancelled { id: 7 });
    assert!(tab.analyze_job.is_none() && tab.reopen);
    assert!(!tab.can_blend(Readiness::Analyzed));
    tab.reopen = false;

    // A given-up source still mapped (by a loader thread, say).
    let path = temp_dir("blend-retired").join("result.mmmt");
    let g = TileGeometry {
        width: 300,
        height: 200,
        channels: 1,
        tile: 256,
    };
    drop(TileStore::create(&path, g).unwrap());
    let held = Arc::new(MosaicTileSource::open(&path, None).unwrap());
    tab.source = Some(held.clone());
    tab.retire();
    assert!(tab.source.is_none() && tab.viewer.is_none());
    tab.retired.retain(|w| w.strong_count() > 0);
    assert!(!tab.can_blend(Readiness::Analyzed));
    drop(held);
    tab.retired.retain(|w| w.strong_count() > 0);
    assert!(tab.can_blend(Readiness::Analyzed));

    // A blend that ends with no result open asks for an open.
    tab.blend_job = Some(9);
    tab.on_event(&JobEvent::Finished {
        id: 9,
        label: "Blend Ha".into(),
    });
    assert!(tab.blend_job.is_none() && tab.reopen && tab.want_health);
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
        assert!(tab.opening.is_none(), "{g}: opened during its blend");
        assert!(tab.reopen, "{g}: open forgotten");
    }

    // Once the blends end, the deferred open happens.
    gate.open();
    crate::jobs::tests::drain(&mut jobs, &mut s);
    tab.update(&ctx, &mut s, &jobs);
    assert!(tab.opening.is_some() && !tab.reopen);
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
    tab.group = Some("Ha".into());
    tab.source = Some(Arc::new(MosaicTileSource::open(&path, None).unwrap()));
    tab.linked = true;
    tab.auto_now(&ctx, &mut s);
    assert!(tab.manual && tab.save_auto_result);
    let deadline = Instant::now() + Duration::from_secs(30);
    while tab.auto.is_some() {
        assert!(Instant::now() < deadline, "auto stretch never ended");
        std::thread::sleep(Duration::from_millis(2));
        tab.poll_auto(&mut s);
    }
    // Nothing was saved, so the stretch is automatic again...
    assert_eq!(saved_stretch(&s.mosaic.config.ui, "Ha"), None);
    assert!(!tab.manual, "left manual with no saved stretch");
    // ...and the next finished blend stretches automatically.
    tab.blend_job = Some(3);
    tab.on_event(&JobEvent::Finished {
        id: 3,
        label: "Blend Ha".into(),
    });
    assert!(tab.want_auto);
    let _ = std::fs::remove_dir_all(&root);
}
