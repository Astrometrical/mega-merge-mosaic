//! Tests of the hero views and thumbnails (`hero_views`), on synthetic
//! sessions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use egui::Color32;
use mega_ui::viewer::{Fetched, TileKey};
use mmm_core::job::JobCtl;
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{CONFIG_FILE, Mosaic, MosaicConfig, analyze_all, blend_group, lock_session};
use mmm_core::preview::{PREVIEWS_DIR, plan_previews, run_previews};
use mmm_core::session_summary::{SUMMARY_FILE, result_state, write_summary};
use mmm_core::tiles::{TileGeometry, TileStore};

use super::*;
use crate::compositions::{Composition, save_compositions};
use crate::start::RecentEntry;
use crate::start::facts::{Availability, CardFacts, CardState, load_facts};
use crate::start::tests::temp_dir;
use crate::stretch_panel::{BLEND_STRETCH_KEY, save_stretch_at};
use crate::summary_writer::summarize;
use crate::testutil::{CANVAS, two_group_session};
use crate::tile_source::MosaicTileSource;

fn facts_of(dir: &Path) -> CardFacts {
    load_facts(&RecentEntry {
        path: dir.to_path_buf(),
        last_opened: 1,
        pinned: false,
    })
}

fn labels(hero: &HeroData) -> Vec<&str> {
    hero.views.iter().map(|v| v.label.as_str()).collect()
}

/// Write `m`'s summary with each group's result state as it is on disk.
fn write_summary_of(m: &Mosaic, groups: &Groups, last_view: Option<&str>) {
    let results = groups
        .processing_groups()
        .into_iter()
        .map(|(g, _)| {
            let state = result_state(&m.result_path(&g));
            (g, state)
        })
        .collect();
    let s = summarize(
        &m.config,
        Some(groups),
        None,
        Some(CANVAS),
        last_view.map(str::to_string),
        &results,
        &BTreeMap::new(),
    );
    write_summary(&m.dir, &s).unwrap();
}

/// A two-group session in `root` with both groups analysed and blended,
/// and its summary written; returns the session folder.
fn merged_session(root: &Path, last_view: Option<&str>) -> PathBuf {
    let mut m = two_group_session(root).mosaic;
    let groups = analyze_all(&mut m, &JobCtl::none()).unwrap();
    for g in ["Ha", "OIII"] {
        blend_group(&mut m, g, None, &JobCtl::none()).unwrap();
    }
    write_summary_of(&m, &groups, last_view);
    m.dir
}

/// Save `compositions` in the session at `dir`.
fn save_comps(dir: &Path, compositions: &[Composition]) {
    edit_config(dir, |cfg| save_compositions(&mut cfg.ui, compositions));
}

fn hoo() -> Composition {
    Composition {
        id: 1,
        name: "HOO".into(),
        channels: [
            Some(("Ha".into(), 1.0)),
            Some(("OIII".into(), 1.0)),
            Some(("OIII".into(), 1.0)),
        ],
    }
}

/// The pixels of `view`'s top tile.
fn top_pixels(view: &HeroView) -> Vec<Color32> {
    thumbnail(view).expect("a top tile").pixels
}

/// A display stretch (3 channels) that turns any signal white.
fn saturating() -> Vec<StretchParams> {
    let p = StretchParams {
        black: 0.0,
        white: 1e-6,
        midtones: 0.5,
    };
    vec![p; 3]
}

/// The share of `view`'s opaque top-tile pixels that are white.
fn white_share(view: &HeroView) -> f64 {
    let px = top_pixels(view);
    let opaque = px.iter().filter(|p| p.a() > 0).count();
    let white = px.iter().filter(|p| **p == Color32::WHITE).count();
    white as f64 / opaque.max(1) as f64
}

/// Change the configuration of the session at `dir` with `edit`.
fn edit_config(dir: &Path, edit: impl FnOnce(&mut MosaicConfig)) {
    let mut cfg = MosaicConfig::load(dir).unwrap();
    edit(&mut cfg);
    cfg.save(dir).unwrap();
}

/// Built on a background thread and sent to the UI thread.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<HeroData>();
};

#[test]
fn merged_session_has_a_view_per_group() {
    let root = temp_dir("hero-merged");
    let dir = merged_session(&root, Some("OIII"));
    let facts = facts_of(&dir);
    assert_eq!(facts.state, CardState::Merged);
    let hero = build_hero(&facts);
    assert_eq!(labels(&hero), ["Ha · 4 panels", "OIII · 4 panels"]);
    assert_eq!(hero.start, 1, "last_view OIII");
    assert_eq!(hero.message, None);
    // Live tiles of the full-resolution result, display-stretched.
    let g = hero.views[0].source.geometry();
    assert_eq!((g.width, g.height), CANVAS);
    let tile = hero.views[0].source.fetch(TileKey {
        level: 0,
        tx: 0,
        ty: 0,
    });
    assert!(matches!(tile, Fetched::Valid(_)), "{tile:?}");
    // The automatic stretch, until a Blend stretch is saved for the group.
    assert!(white_share(&hero.views[0]) < 0.5);
    edit_config(&dir, |cfg| {
        save_stretch_at(&mut cfg.ui, BLEND_STRETCH_KEY, "Ha", Some(&saturating()));
    });
    assert!(white_share(&build_hero(&facts).views[0]) > 0.5);
    // No last view: the first.
    let mut s = facts.summary.clone().unwrap();
    s.last_view = None;
    write_summary(&dir, &s).unwrap();
    assert_eq!(build_hero(&facts_of(&dir)).start, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn partly_merged_session_shows_its_merged_groups() {
    let root = temp_dir("hero-partly");
    let mut m = two_group_session(&root).mosaic;
    let groups = analyze_all(&mut m, &JobCtl::none()).unwrap();
    run_previews(&plan_previews(&m), &JobCtl::none(), &|_| {}).unwrap();
    // Ha merged, OIII not yet.
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    write_summary_of(&m, &groups, None);
    let facts = facts_of(&m.dir);
    assert_eq!(facts.state, CardState::PartlyMerged);
    let hero = build_hero(&facts);
    // The merged group's full-resolution result, read live, rather than
    // every group's low-resolution plan picture.
    assert_eq!(labels(&hero), ["Ha · 4 panels"]);
    assert_eq!(hero.message, None);
    let g = hero.views[0].source.geometry();
    assert_eq!((g.width, g.height), CANVAS);
    let tile = hero.views[0].source.fetch(TileKey {
        level: 0,
        tx: 0,
        ty: 0,
    });
    assert!(matches!(tile, Fetched::Valid(_)), "{tile:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn composition_view_is_added() {
    let root = temp_dir("hero-comp");
    let dir = merged_session(&root, Some("comp:HOO"));
    // A composition of a group never merged is left out.
    let other = Composition {
        id: 2,
        name: "Lum".into(),
        channels: [Some(("L".into(), 1.0)), None, None],
    };
    save_comps(&dir, &[hoo(), other]);
    let hero = build_hero(&facts_of(&dir));
    assert_eq!(labels(&hero), ["Ha · 4 panels", "OIII · 4 panels", "HOO"]);
    assert_eq!(hero.start, 2, "last_view comp:HOO (named, as earlier mmm)");
    // Ha in red, OIII in green and blue.
    let px = top_pixels(&hero.views[2]);
    assert!(
        px.iter()
            .any(|p| p.a() > 0 && p.r() > 0 && p.g() > 0 && p.g() == p.b()),
        "mixed colours"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn composition_view_is_found_by_id() {
    let root = temp_dir("hero-comp-id");
    let dir = merged_session(&root, Some("comp:1"));
    save_comps(&dir, &[hoo()]);
    let hero = build_hero(&facts_of(&dir));
    assert_eq!(hero.start, 2, "last_view comp:1");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn planned_session_renders_previews() {
    let root = temp_dir("hero-planned");
    let s = two_group_session(&root);
    let m = &s.mosaic;
    run_previews(&plan_previews(m), &JobCtl::none(), &|_| {}).unwrap();
    write_summary_of(m, s.groups.as_ref().unwrap(), None);
    save_comps(&m.dir, &[hoo()]);
    let facts = facts_of(&m.dir);
    assert_eq!(facts.state, CardState::PlanReady);
    let hero = build_hero(&facts);
    assert_eq!(labels(&hero), ["Ha · 4 panels", "OIII · 4 panels", "HOO"]);
    assert_eq!(hero.message, None);
    // An in-memory picture of the whole canvas, never larger than it.
    let g = hero.views[0].source.geometry();
    assert_eq!((g.width, g.height), CANVAS);
    assert!(top_pixels(&hero.views[0]).iter().any(|p| p.a() > 0));
    assert!(
        top_pixels(&hero.views[2])
            .iter()
            .any(|p| p.r() > 0 && p.g() > 0 && p.g() == p.b()),
        "the composition mixes the groups' overviews"
    );
    // The automatic stretch, until a Plan stretch is saved for the group.
    assert!(white_share(&hero.views[0]) < 0.5);
    edit_config(&m.dir, |cfg| {
        save_stretch_at(&mut cfg.ui, PLAN_STRETCH_KEY, "Ha", Some(&saturating()));
    });
    assert!(white_share(&build_hero(&facts).views[0]) > 0.5);

    // Without a summary: one view of every preview.
    std::fs::remove_file(m.dir.join(SUMMARY_FILE)).unwrap();
    let facts = facts_of(&m.dir);
    assert_eq!(facts.state, CardState::PlanReady);
    assert_eq!(labels(&build_hero(&facts)), ["All panels"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn busy_session_has_no_live_views() {
    let root = temp_dir("hero-busy");
    let dir = merged_session(&root, None);
    // Facts read before another window opened it: found busy now.
    let before = facts_of(&dir);
    assert_eq!(before.availability, Availability::Available);
    let lock = lock_session(&dir).unwrap();
    for facts in [before, facts_of(&dir)] {
        let hero = build_hero(&facts);
        assert!(hero.views.is_empty());
        assert_eq!(hero.message.as_deref(), Some("Open in another window"));
    }
    drop(lock);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unavailable_sessions_say_why() {
    let root = temp_dir("hero-unavailable");
    let missing = build_hero(&facts_of(&root.join("gone.mmm-session")));
    assert!(missing.views.is_empty());
    assert_eq!(missing.message.as_deref(), Some("Missing"));
    let dir = two_group_session(&root).mosaic.dir;
    std::fs::write(dir.join(CONFIG_FILE), "{").unwrap();
    let bad = build_hero(&facts_of(&dir));
    assert!(bad.views.is_empty());
    assert_eq!(bad.message.as_deref(), Some("Can't read this session"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn empty_session_says_so() {
    let root = temp_dir("hero-empty");
    let dir = root.join("e.mmm-session");
    Mosaic::create(&dir, MosaicConfig::new(Vec::new())).unwrap();
    let facts = facts_of(&dir);
    assert_eq!(facts.state, CardState::NoPanels);
    let hero = build_hero(&facts);
    assert!(hero.views.is_empty());
    assert_eq!(
        hero.message.as_deref(),
        Some("No panels yet — resume to add them")
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The line of a session with panels but nothing to show.
const NOTHING: &str = "Nothing to show yet — resume to build the preview";

#[test]
fn unknown_session_has_nothing_to_show() {
    let root = temp_dir("hero-unknown");
    // Panels, but no summary and no previews.
    let dir = two_group_session(&root).mosaic.dir;
    let facts = facts_of(&dir);
    assert_eq!(facts.state, CardState::Unknown);
    let hero = build_hero(&facts);
    assert!(hero.views.is_empty());
    assert_eq!(hero.message.as_deref(), Some(NOTHING));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unreadable_results_have_nothing_to_show() {
    let root = temp_dir("hero-unreadable-results");
    let s = two_group_session(&root);
    let (m, groups) = (&s.mosaic, s.groups.as_ref().unwrap());

    // Merged per the summary, but Ha's store is truncated and OIII's gone.
    let complete = groups
        .processing_groups()
        .into_iter()
        .map(|(g, _)| (g, ResultState::Complete))
        .collect();
    let summary = summarize(
        &m.config,
        Some(groups),
        None,
        Some(CANVAS),
        None,
        &complete,
        &BTreeMap::new(),
    );
    write_summary(&m.dir, &summary).unwrap();
    let ha = m.result_path("Ha");
    std::fs::create_dir_all(ha.parent().unwrap()).unwrap();
    std::fs::write(&ha, "truncated").unwrap();
    let facts = facts_of(&m.dir);
    assert_eq!(facts.state, CardState::Merged);
    let hero = build_hero(&facts);
    assert!(hero.views.is_empty());
    assert_eq!(hero.message.as_deref(), Some(NOTHING));

    // Planned, but the only cached preview is unreadable: no overview.
    let previews = m.dir.join(PREVIEWS_DIR);
    std::fs::create_dir_all(&previews).unwrap();
    std::fs::write(previews.join("junk.mmmp"), "junk").unwrap();
    std::fs::remove_file(&ha).unwrap();
    write_summary_of(m, groups, None);
    let facts = facts_of(&m.dir);
    assert_eq!(facts.state, CardState::PlanReady);
    let hero = build_hero(&facts);
    assert!(hero.views.is_empty());
    assert_eq!(hero.message.as_deref(), Some(NOTHING));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn thumbnail_is_the_top_tile() {
    let root = temp_dir("hero-thumb");
    let dir = merged_session(&root, None);
    let hero = build_hero(&facts_of(&dir));
    let thumb = thumbnail(&hero.views[0]).expect("top tile");
    let [w, h] = thumb.size;
    assert!(w > 0 && h > 0 && w <= 256 && h <= 256, "{w} × {h}");
    // The whole canvas in one tile: its aspect.
    let aspect = CANVAS.0 as f64 / CANVAS.1 as f64;
    assert!((w as f64 / h as f64 - aspect).abs() < 0.05, "{w} × {h}");
    assert!(thumb.pixels.iter().any(|p| p.a() > 0));

    // Nothing at the top level yet: no thumbnail.
    let empty = root.join("empty.mmmt");
    let g = TileGeometry {
        width: 600,
        height: 300,
        channels: 1,
        tile: 256,
    };
    drop(TileStore::create(&empty, g).unwrap());
    let view = HeroView {
        label: "empty".into(),
        source: Arc::new(MosaicTileSource::open_plain(&empty, None).unwrap()),
    };
    assert!(thumbnail(&view).is_none());
    let _ = std::fs::remove_dir_all(&root);
}
