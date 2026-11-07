//! What the start screen's hero shows for a recent session, and the small
//! thumbnail of a view for its shelf card.
//!
//! - **Merged** (re-merge needed included): one view per group with a
//!   result, read live from its store, plus each saved composition whose
//!   groups all have one ([`CompositeTileSource`]). Each group shows
//!   through its saved Blend display stretch, else the automatic one.
//! - **Planned, not merged:** one view per group drawn from its cached
//!   previews ([`render_overview`]) into an in-memory picture
//!   ([`ImageTileSource`]), through the saved Plan display stretch, else
//!   an automatic one; compositions mix those pictures as
//!   [`CompositeTileSource`] mixes tiles ([`Mix`]).
//! - **Otherwise** no views, and a line saying why.
//!
//! Everything here is blocking file I/O, for a background thread. It never
//! opens the session: it takes no lock (only the busy probe), runs no
//! analysis, and reads result stores through
//! [`MosaicTileSource::open_plain`] (plain reads, never a mapping), so a
//! store another process recreates cannot fault it. A session found busy
//! is never read live.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::ColorImage;
use mega_ui::hero::HeroView;
use mega_ui::viewer::{Fetched, ImageTileSource, TileKey};
use mmm_core::mosaic::{Mosaic, SINGLE_GROUP, session_busy};
use mmm_core::preview::{
    Overview, Preview, load_cached_previews, preview_frame_path, render_overview,
};
use mmm_core::reference::ReferenceFrame;
use mmm_core::session_summary::{ResultState, SessionSummary};
use mmm_core::stretch::{StretchParams, auto_stretch, channel_stats};

use super::facts::{Availability, CardFacts, CardState};
use crate::blend_tab::saved_stretch;
use crate::compositions::{Composition, composition_stretch_key, load_compositions};
use crate::stretch_panel::{PLAN_STRETCH_KEY, saved_stretch_at};
use crate::tile_source::{CompositeTileSource, Mix, MosaicTileSource, channel_tints, planar_image};

/// Longest side, in pixels, of a planned session's overview pictures (never
/// more than the canvas's own).
const OVERVIEW_MAX_SIDE: u32 = 2048;
/// Tile size of the overview pictures' in-memory pyramids.
const OVERVIEW_TILE: u32 = 256;
/// Most overview pixels the automatic display stretch reads.
const STATS_MAX_PIXELS: usize = 1 << 18;
/// The hero's line for a session with nothing to show yet.
pub const EMPTY_MESSAGE: &str = "No panels yet — resume to add them";
/// The hero's line for a session open in another window or process.
const BUSY_MESSAGE: &str = "Open in another window";
/// The hero's line for a session that cannot be read.
const UNREADABLE_MESSAGE: &str = "Can't read this session";
/// The label of the one planned view of a session without a summary.
const ALL_PANELS: &str = "All panels";

/// What the hero shows for one session.
pub struct HeroData {
    /// The images, in order: groups, then compositions.
    pub views: Vec<HeroView>,
    /// The view to show first: the one last shown in the session, else 0.
    pub start: usize,
    /// Why there are no views (e.g. "Missing"), if that is the case.
    pub message: Option<String>,
}

impl HeroData {
    /// No views, and `message` saying why.
    fn without_views(message: &str) -> Self {
        HeroData {
            views: Vec::new(),
            start: 0,
            message: Some(message.to_string()),
        }
    }
}

/// A view and the key the summary's `last_view` names it by: the group
/// name, or [`composition_stretch_key`] of a composition's name.
struct Keyed {
    key: String,
    view: HeroView,
}

/// What the hero shows for the session `facts` describes (blocking: call
/// off the UI thread). Loads the session's configuration itself (result
/// paths, saved stretches, compositions) without taking its lock, and
/// checks again that no other window has opened it since `facts`.
pub fn build_hero(facts: &CardFacts) -> HeroData {
    match facts.availability {
        Availability::Missing => return HeroData::without_views("Missing"),
        Availability::Busy => return HeroData::without_views(BUSY_MESSAGE),
        Availability::Unreadable(_) => return HeroData::without_views(UNREADABLE_MESSAGE),
        Availability::Available => {}
    }
    // Opened elsewhere since the facts were read: still never read live.
    if session_busy(&facts.path) {
        return HeroData::without_views(BUSY_MESSAGE);
    }
    let planned = match facts.state {
        CardState::NoPanels | CardState::Unknown => return HeroData::without_views(EMPTY_MESSAGE),
        CardState::Merged | CardState::MergedExported | CardState::ReMergeNeeded => false,
        CardState::PlanReady => true,
    };
    let Ok(m) = Mosaic::open(&facts.path) else {
        return HeroData::without_views(UNREADABLE_MESSAGE);
    };
    let summary = facts.summary.as_ref();
    let keyed = if planned {
        planned_views(&m, summary)
    } else {
        merged_views(&m, summary)
    };
    let last = summary.and_then(|s| s.last_view.as_deref());
    let start = last
        .and_then(|l| keyed.iter().position(|k| k.key == l))
        .unwrap_or(0);
    HeroData {
        views: keyed.into_iter().map(|k| k.view).collect(),
        start,
        message: None,
    }
}

/// The image of `view`'s top-level tile `(0, 0)` (the whole image, at most
/// one tile big), when it has one, valid or stale. Blocking: call off the
/// UI thread.
pub fn thumbnail(view: &HeroView) -> Option<ColorImage> {
    let g = view.source.geometry();
    let top = TileKey {
        level: g.levels.saturating_sub(1),
        tx: 0,
        ty: 0,
    };
    match view.source.fetch(top) {
        Fetched::Valid(img) | Fetched::Stale(img) => Some(img),
        Fetched::Missing => None,
    }
}

/// "{group} · {n} panels".
fn group_label(group: &str, panels: usize) -> String {
    format!("{group} · {panels} panels")
}

/// The live views of a merged session: each summary group with a result,
/// then each composition whose groups all have one.
fn merged_views(m: &Mosaic, summary: Option<&SessionSummary>) -> Vec<Keyed> {
    let ui = &m.config.ui;
    let mut out = Vec::new();
    let mut sources: BTreeMap<&str, Arc<MosaicTileSource>> = BTreeMap::new();
    let groups = summary.map_or(&[][..], |s| &s.groups);
    for g in groups.iter().filter(|g| g.result != ResultState::None) {
        let path = m.result_path(&g.name);
        let source = match MosaicTileSource::open_plain(&path, None) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!("hero: cannot read {}: {e}", path.display());
                continue;
            }
        };
        let channels = source.tile_geometry().channels as usize;
        let saved = saved_stretch(ui, &g.name).filter(|s| s.len() == channels);
        if let Some(s) = saved.or_else(|| source.auto(false)) {
            source.set_stretch(s);
        }
        out.push(Keyed {
            key: g.name.clone(),
            view: HeroView {
                label: group_label(&g.name, g.panels.len()),
                source: source.clone(),
            },
        });
        sources.insert(&g.name, source);
    }
    for c in load_compositions(ui) {
        // A channel's group without a result leaves the composition out.
        let mapped = c
            .channels
            .iter()
            .map(|ch| match ch {
                None => Some(None),
                Some((g, w)) => sources.get(g.as_str()).map(|s| Some((s.clone(), *w))),
            })
            .collect::<Option<Vec<_>>>();
        let Some(Ok(composite)) = mapped.map(|ch| CompositeTileSource::new(&ch)) else {
            continue;
        };
        out.push(Keyed {
            key: composition_stretch_key(&c.name),
            view: HeroView {
                label: c.name.clone(),
                source: Arc::new(composite),
            },
        });
    }
    out
}

/// The views of a planned session, drawn from its cached previews: each
/// summary group (one "All panels" view of every preview without a
/// summary), then each composition whose groups all have a picture.
fn planned_views(m: &Mosaic, summary: Option<&SessionSummary>) -> Vec<Keyed> {
    let Some(canvas) = ReferenceFrame::load(&preview_frame_path(&m.dir))
        .map(|f| f.canvas())
        .ok()
        .or_else(|| summary.and_then(|s| s.canvas))
    else {
        return Vec::new();
    };
    let longest = u32::try_from(canvas.0.max(canvas.1)).unwrap_or(u32::MAX);
    let max_side = OVERVIEW_MAX_SIDE.min(longest);
    let previews = load_cached_previews(&m.dir);
    // (key, label, previews) of each group view.
    let groups: Vec<(String, String, Vec<&Preview>)> = match summary {
        Some(s) => s
            .groups
            .iter()
            .map(|g| {
                let members = previews
                    .iter()
                    .filter(|p| g.panels.contains(&p.path))
                    .collect();
                (
                    g.name.clone(),
                    group_label(&g.name, g.panels.len()),
                    members,
                )
            })
            .collect(),
        None => vec![(
            SINGLE_GROUP.to_string(),
            ALL_PANELS.to_string(),
            previews.iter().collect(),
        )],
    };
    let ui = &m.config.ui;
    // (key, label, picture) of each view.
    let mut pictures: Vec<(String, String, ColorImage)> = Vec::new();
    for (key, label, members) in groups {
        let Some(o) = render_overview(&members, canvas, max_side) else {
            continue;
        };
        let img = overview_image(&o, saved_stretch_at(ui, PLAN_STRETCH_KEY, &key));
        pictures.push((key, label, img));
    }
    let mixed: Vec<(String, String, ColorImage)> = load_compositions(ui)
        .iter()
        .filter_map(|c| {
            let img = mix_pictures(c, &pictures)?;
            Some((composition_stretch_key(&c.name), c.name.clone(), img))
        })
        .collect();
    pictures
        .into_iter()
        .chain(mixed)
        .map(|(key, label, img)| Keyed {
            key,
            view: HeroView {
                label,
                source: Arc::new(ImageTileSource::new(img, OVERVIEW_TILE)),
            },
        })
        .collect()
}

/// Composition `c` mixed from the group `pictures` (key, label, image),
/// each tinted by its channels' colours times the weights; `None` when it
/// maps no group or a group without a picture.
fn mix_pictures(c: &Composition, pictures: &[(String, String, ColorImage)]) -> Option<ColorImage> {
    let picture = |g: &str| pictures.iter().find(|p| p.0 == g).map(|p| &p.2);
    let layers = channel_tints(&c.channels, |a, b| a == b);
    let (first, _) = layers.first()?;
    let mut mix = Mix::new(picture(first)?.size);
    for (g, tint) in &layers {
        mix.add(picture(g)?, *tint);
    }
    Some(mix.image())
}

/// Overview `o` as RGBA8 by [`planar_image`]'s rules (no data is
/// transparent), through `saved` when it has one entry per shown channel
/// (3 for colour, else 1), else the automatic stretch of `o`.
fn overview_image(o: &Overview, saved: Option<Vec<StretchParams>>) -> ColorImage {
    let (w, h, channels) = (o.width as usize, o.height as usize, o.channels as usize);
    let shown = if channels >= 3 { 3 } else { 1 };
    let stretch = saved
        .filter(|s| s.len() == shown)
        .unwrap_or_else(|| overview_auto(o));
    planar_image(&o.data, w * h, w, (w, h), channels, &stretch)
}

/// The automatic (unlinked) display stretch of `o`, from at most
/// [`STATS_MAX_PIXELS`] evenly strided pixels (no-data pixels ignored).
fn overview_auto(o: &Overview) -> Vec<StretchParams> {
    let channels = o.channels as usize;
    let plane = o.width as usize * o.height as usize;
    let step = plane.div_ceil(STATS_MAX_PIXELS).max(1);
    let picked: Vec<usize> = (0..plane).step_by(step).collect();
    let mut planar = Vec::with_capacity(picked.len() * channels);
    for c in 0..channels {
        planar.extend(picked.iter().map(|&i| o.data[c * plane + i]));
    }
    auto_stretch(&channel_stats(&planar, picked.len(), channels), false)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use egui::Color32;
    use mega_ui::viewer::{Fetched, TileKey};
    use mmm_core::job::JobCtl;
    use mmm_core::mosaic::grouping::Groups;
    use mmm_core::mosaic::{
        CONFIG_FILE, Mosaic, MosaicConfig, analyze_all, blend_group, lock_session,
    };
    use mmm_core::preview::{plan_previews, run_previews};
    use mmm_core::session_summary::{SUMMARY_FILE, result_state, write_summary};
    use mmm_core::tiles::{TileGeometry, TileStore};

    use super::*;
    use crate::blend_tab::save_stretch;
    use crate::compositions::{Composition, save_compositions};
    use crate::start::RecentEntry;
    use crate::start::facts::{Availability, CardFacts, CardState, load_facts};
    use crate::start::tests::temp_dir;
    use crate::stretch_panel::save_stretch_at;
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
            save_stretch(&mut cfg.ui, "Ha", Some(&saturating()));
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
    fn composition_view_is_added() {
        let root = temp_dir("hero-comp");
        let dir = merged_session(&root, Some("comp:HOO"));
        // A composition of a group never merged is left out.
        let other = Composition {
            name: "Lum".into(),
            channels: [Some(("L".into(), 1.0)), None, None],
        };
        save_comps(&dir, &[hoo(), other]);
        let hero = build_hero(&facts_of(&dir));
        assert_eq!(labels(&hero), ["Ha · 4 panels", "OIII · 4 panels", "HOO"]);
        assert_eq!(hero.start, 2, "last_view comp:HOO");
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
}
