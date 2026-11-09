//! What the start screen's hero shows for a recent session, and the small
//! thumbnail of a view for its shelf card.
//!
//! - **Merged** (partly merged and re-merge needed included: any group
//!   with a result): one view per group with a result, read live from its
//!   store, plus each saved composition whose groups all have one
//!   ([`CompositeTileSource`]). Each group shows through its saved Blend
//!   display stretch, else the automatic one.
//! - **Planned, not merged:** one view per group drawn from its cached
//!   previews ([`render_overview`]) into an in-memory picture
//!   ([`ImageTileSource`]), through the saved Plan display stretch, else
//!   an automatic one; compositions mix those pictures as
//!   [`CompositeTileSource`] mixes tiles ([`Mix`]).
//! - **Otherwise**, or when nothing could be read, no views and a line
//!   saying why: a hero is never blank without a reason.
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
use crate::compositions::{
    Composition, composition_stretch_key, load_compositions, migrate_composition_ids,
};
use crate::display_stretch::DisplayStretch;
use crate::stretch_panel::{PLAN_STRETCH_KEY, saved_stretch, saved_stretch_at};
use crate::tile_source::{CompositeTileSource, Mix, MosaicTileSource, channel_tints, planar_image};

/// Longest side, in pixels, of a planned session's overview pictures (never
/// more than the canvas's own).
const OVERVIEW_MAX_SIDE: u32 = 2048;
/// Tile size of the overview pictures' in-memory pyramids.
const OVERVIEW_TILE: u32 = 256;
/// Most overview pixels the automatic display stretch reads.
const STATS_MAX_PIXELS: usize = 1 << 18;
/// The hero's line for a session without panels.
pub const EMPTY_MESSAGE: &str = "No panels yet — resume to add them";
/// The hero's line for a session with panels but nothing to show: nothing
/// known yet, or no result or preview could be read.
pub const NOTHING_MESSAGE: &str = "Nothing to show yet — resume to build the preview";
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
    /// Why there are no views (e.g. "Missing"): `Some` exactly when `views`
    /// is empty.
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
/// name, or [`composition_stretch_key`] of a composition's id.
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
    match facts.state {
        CardState::NoPanels => return HeroData::without_views(EMPTY_MESSAGE),
        CardState::Unknown => return HeroData::without_views(NOTHING_MESSAGE),
        CardState::PlanReady
        | CardState::PartlyMerged
        | CardState::Merged
        | CardState::MergedExported
        | CardState::SettingsChanged
        | CardState::ReMergeNeeded => {}
    }
    let Ok(m) = Mosaic::open(&facts.path) else {
        return HeroData::without_views(UNREADABLE_MESSAGE);
    };
    let summary = facts.summary.as_ref();
    // Any group with a result shows its full-resolution tiles; the plan's
    // pictures only while no group has one.
    let merged = summary.is_some_and(|s| s.groups.iter().any(|g| g.result != ResultState::None));
    let keyed = if merged {
        merged_views(&m, summary)
    } else {
        planned_views(&m, summary)
    };
    // Never a blank hero without a reason.
    if keyed.is_empty() {
        return HeroData::without_views(NOTHING_MESSAGE);
    }
    let last = summary.and_then(|s| s.last_view.as_deref());
    let start = last
        .and_then(|l| {
            // An exact key wins over all; an earlier mmm named a
            // composition's view by its name, so that match comes only
            // when no key matched (a composite named "2" is not id 2).
            let legacy =
                |k: &Keyed| k.key.starts_with("comp:") && l == format!("comp:{}", k.view.label);
            keyed
                .iter()
                .position(|k| k.key == l)
                .or_else(|| keyed.iter().position(legacy))
        })
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
    // Read as the window would, once it has moved an older session's keys.
    let mut migrated = m.config.ui.clone();
    migrate_composition_ids(&mut migrated);
    let ui = &migrated;
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
        if let Some(s) = saved.or_else(|| source.auto(DisplayStretch::default())) {
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
            key: composition_stretch_key(c.id),
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
    // Read as the window would, once it has moved an older session's keys.
    let mut migrated = m.config.ui.clone();
    migrate_composition_ids(&mut migrated);
    let ui = &migrated;
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
            Some((composition_stretch_key(c.id), c.name.clone(), img))
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
/// each tinted by its channels' colours ([`channel_tints`]); `None` when it
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

/// The tests live in a sibling file to keep this one short; a `#[path]`
/// child module keeps their access to private items.
#[cfg(test)]
#[path = "hero_views_tests.rs"]
mod tests;
