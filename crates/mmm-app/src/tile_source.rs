//! The Blend tab's tile source: a group's tiled result store
//! (`result.mmmt`) read through a [`TileReader`], display-stretched to RGBA
//! on the viewer's loader threads.
//!
//! The display stretch runs on the CPU, per tile, on the loader threads
//! (spec §8 trade-off): it keeps the viewer backend-agnostic for the later
//! lift into workflow-platform (glow) or wgpu. A slider drag re-converts the
//! visible tiles (about 130 at 1080p) in parallel on the loaders; a GPU path
//! is a follow-up if drags fall short of "next frame".
//!
//! **Store recreation.** A [`TileReader`] must never be alive while a blend
//! recreates (truncates) its file: the mapping would fault (`SIGBUS`).
//! `run_blend` recreates a store only when it is missing, unreadable, or of
//! a different geometry than the group's analysed canvas. The Blend tab
//! therefore keeps a source open only after [`open_result`] has checked
//! that the store's geometry matches the group's `session.json` canvas (so
//! a blend reopens it rather than recreating it), and re-checks after every
//! analysis; see `blend_tab` for the full guard.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, ColorImage};
use mega_ui::viewer::{Fetched, PyramidGeometry, TileKey, TileSource};
use mmm_core::session::Session;
use mmm_core::stretch::{ChannelStats, StretchParams, apply, auto_stretch, channel_stats};
use mmm_core::tiles::{TileGeometry, TileReader, TileState};

/// Copies of a tile torn by a concurrent write tried before giving up (the
/// next generation bump fetches it again).
const READ_ATTEMPTS: usize = 3;
/// Most tiles of one level read for display-stretch statistics: levels with
/// more are never scanned (the coarse levels hold the whole image).
pub const MAX_STATS_TILES: usize = 64;
/// The identity stretch: linear, black 0, white 1.
pub const LINEAR: StretchParams = StretchParams {
    black: 0.0,
    white: 1.0,
    midtones: 0.5,
};

/// A group's result store as a [`TileSource`], with a display stretch.
pub struct MosaicTileSource {
    reader: TileReader,
    tiles: TileGeometry,
    geometry: PyramidGeometry,
    /// Per-channel stretch; locked only to swap or clone the `Arc` (never
    /// during a fetch's I/O), so the UI thread never waits on a fetch.
    stretch: Mutex<Arc<Vec<StretchParams>>>,
    generation: AtomicU64,
}

impl MosaicTileSource {
    /// Wrap `reader`. `stretch` (one entry per channel) defaults to
    /// [`LINEAR`]; a wrong-length stretch is ignored too.
    pub fn new(reader: TileReader, stretch: Option<Vec<StretchParams>>) -> Self {
        let tiles = reader.geometry();
        let channels = tiles.channels as usize;
        let stretch = stretch
            .filter(|s| s.len() == channels)
            .unwrap_or_else(|| vec![LINEAR; channels]);
        MosaicTileSource {
            geometry: PyramidGeometry::new(tiles.width, tiles.height, tiles.tile),
            tiles,
            reader,
            stretch: Mutex::new(Arc::new(stretch)),
            generation: AtomicU64::new(0),
        }
    }

    /// Open the store at `path` (blocking: call off the UI thread).
    pub fn open(path: &Path, stretch: Option<Vec<StretchParams>>) -> mmm_core::Result<Self> {
        Ok(Self::new(TileReader::open(path)?, stretch))
    }

    /// The store's geometry.
    pub fn tile_geometry(&self) -> TileGeometry {
        self.tiles
    }

    /// The current display stretch, one entry per channel.
    pub fn stretch(&self) -> Vec<StretchParams> {
        self.stretch_arc().as_ref().clone()
    }

    fn stretch_arc(&self) -> Arc<Vec<StretchParams>> {
        self.stretch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replace the display stretch (ignored unless one entry per channel)
    /// and bump the generation so every tile is converted again.
    pub fn set_stretch(&self, stretch: Vec<StretchParams>) {
        if stretch.len() != self.tiles.channels as usize {
            return;
        }
        *self.stretch.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(stretch);
        self.bump();
    }

    /// Mark every tile as possibly changed (e.g. a blend progressed).
    pub fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Per-channel statistics of the coarsest level with any valid or stale
    /// tile (scanning down from the top while a level has at most
    /// [`MAX_STATS_TILES`] tiles); `None` if no such level has data.
    /// Blocking: call off the UI thread.
    pub fn coarse_stats(&self) -> Option<Vec<ChannelStats>> {
        let g = self.tiles;
        let (t, channels) = (g.tile as usize, g.channels as usize);
        let mut buf = vec![0.0f32; channels * t * t];
        for level in (0..g.levels()).rev() {
            let (nx, ny) = g.tiles(level);
            if nx as usize * ny as usize > MAX_STATS_TILES {
                return None;
            }
            let mut planes: Vec<Vec<f32>> = vec![Vec::new(); channels];
            for ty in 0..ny {
                for tx in 0..nx {
                    if self.read(level, tx, ty, &mut buf).is_none() {
                        continue;
                    }
                    let (w, h) = self.visible(level, tx, ty);
                    for (c, plane) in planes.iter_mut().enumerate() {
                        for y in 0..h {
                            let row = c * t * t + y * t;
                            plane.extend_from_slice(&buf[row..row + w]);
                        }
                    }
                }
            }
            let pixels = planes[0].len();
            if pixels > 0 {
                return Some(channel_stats(&planes.concat(), pixels, channels));
            }
        }
        None
    }

    /// The automatic (unlinked) display stretch from [`Self::coarse_stats`].
    pub fn auto(&self, linked: bool) -> Option<Vec<StretchParams>> {
        self.coarse_stats().map(|s| auto_stretch(&s, linked))
    }

    /// Visible size of tile `(tx, ty)` at `level` (edge tiles are cropped).
    fn visible(&self, level: u32, tx: u32, ty: u32) -> (usize, usize) {
        let t = u64::from(self.tiles.tile);
        let (lw, lh) = self.tiles.level_dims(level);
        let w = (lw - u64::from(tx) * t).min(t);
        let h = (lh - u64::from(ty) * t).min(t);
        (w as usize, h as usize)
    }

    /// Copy a valid or stale tile, retrying a copy torn by a write.
    fn read(&self, level: u32, tx: u32, ty: u32, buf: &mut [f32]) -> Option<TileState> {
        for _ in 0..READ_ATTEMPTS {
            if let Some(state) = self.reader.read_tile_any(level, tx, ty, buf) {
                return Some(state);
            }
            if self.reader.state(level, tx, ty) == TileState::Missing {
                return None;
            }
        }
        None
    }
}

impl TileSource for MosaicTileSource {
    fn geometry(&self) -> PyramidGeometry {
        self.geometry
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn fetch(&self, key: TileKey) -> Fetched {
        let g = self.tiles;
        if key.level >= g.levels() {
            return Fetched::Missing;
        }
        let (nx, ny) = g.tiles(key.level);
        if key.tx >= nx || key.ty >= ny {
            return Fetched::Missing;
        }
        let t = g.tile as usize;
        let channels = g.channels as usize;
        let mut buf = vec![0.0f32; channels * t * t];
        let Some(state) = self.read(key.level, key.tx, key.ty, &mut buf) else {
            return Fetched::Missing;
        };
        let (w, h) = self.visible(key.level, key.tx, key.ty);
        let img = tile_image(&buf, t, w, h, channels, &self.stretch_arc());
        match state {
            TileState::Valid => Fetched::Valid(img),
            TileState::Stale | TileState::Missing => Fetched::Stale(img),
        }
    }
}

/// Convert the visible `w × h` corner of a planar tile (`channels` planes of
/// `tile × tile` samples) to RGBA8 through the per-channel `stretch`. One
/// channel (or two) is grey from channel 0; three or more are RGB from the
/// first three. A pixel with every channel zero (no data) is transparent.
pub fn tile_image(
    planar: &[f32],
    tile: usize,
    w: usize,
    h: usize,
    channels: usize,
    stretch: &[StretchParams],
) -> ColorImage {
    let plane = tile * tile;
    let byte = |c: usize, v: f32| (apply(&stretch[c], v) * 255.0).round() as u8;
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let i = y * tile + x;
            if (0..channels).all(|c| planar[c * plane + i] == 0.0) {
                pixels.push(Color32::TRANSPARENT);
            } else if channels >= 3 {
                pixels.push(Color32::from_rgb(
                    byte(0, planar[i]),
                    byte(1, planar[plane + i]),
                    byte(2, planar[2 * plane + i]),
                ));
            } else {
                pixels.push(Color32::from_gray(byte(0, planar[i])));
            }
        }
    }
    ColorImage::new([w, h], pixels)
}

/// What [`open_result`] found for a group.
pub enum Opened {
    /// The store, open, with its display stretch set.
    Ready {
        /// The open store.
        source: MosaicTileSource,
        /// Whether the stretch is the saved one (else automatic).
        saved: bool,
    },
    /// Nothing to show yet; the reason, for the user.
    Absent(String),
}

/// Open `group`'s result store for viewing (blocking: call off the UI
/// thread). The store is kept open only if its geometry matches the group's
/// analysed canvas (`session.json` in `session_dir`); otherwise the next
/// blend would recreate the file under the mapping, so the reader is
/// dropped here and the result reported out of date.
///
/// The display stretch is `saved` if given, else the automatic (unlinked)
/// stretch from the coarsest level ([`MosaicTileSource::auto`]).
pub fn open_result(
    result_path: &Path,
    session_dir: &Path,
    saved: Option<Vec<StretchParams>>,
) -> Opened {
    if !result_path.exists() {
        return Opened::Absent("Not blended yet.".into());
    }
    let expected = match Session::open(session_dir) {
        Ok(s) => s.canvas,
        Err(e) => return Opened::Absent(format!("Cannot read the group's analysis: {e}")),
    };
    let source = match MosaicTileSource::open(result_path, None) {
        Ok(s) => s,
        Err(e) => return Opened::Absent(format!("Cannot open the result ({e}); blend again.")),
    };
    let g = source.tile_geometry();
    if (g.width, g.height, u64::from(g.channels)) != expected {
        return Opened::Absent(
            "The result predates the latest analysis; blend to refresh it.".into(),
        );
    }
    let saved = saved.filter(|s| s.len() == g.channels as usize);
    let used_saved = saved.is_some();
    if let Some(s) = saved.or_else(|| source.auto(false)) {
        source.set_stretch(s);
    }
    Opened::Ready {
        source,
        saved: used_saved,
    }
}

#[cfg(test)]
mod tests {
    use mmm_core::tiles::TileStore;

    use super::*;
    use crate::start::tests::temp_dir;

    const T: usize = 256;

    /// 600 × 300: level 0 is 3 × 2 tiles; levels 300 × 150 and 150 × 75.
    fn geom(channels: u32) -> TileGeometry {
        TileGeometry {
            width: 600,
            height: 300,
            channels,
            tile: T as u32,
        }
    }

    fn key(level: u32, tx: u32, ty: u32) -> TileKey {
        TileKey { level, tx, ty }
    }

    #[test]
    fn tile_source_maps_states() {
        let path = temp_dir("tsrc-states").join("result.mmmt");
        let mut store = TileStore::create(&path, geom(1)).unwrap();
        let data = vec![0.5f32; T * T];
        store.write_tile(0, 0, 0, &data).unwrap();
        store.write_tile(0, 1, 0, &data).unwrap();
        store.write_tile(0, 2, 1, &data).unwrap();
        // Stale: written, then invalidated (only level-0 tile (1, 0) of the
        // written ones lies in this rect).
        store.invalidate_rect([256, 0, 512, 256]);
        let src = MosaicTileSource::open(&path, None).unwrap();
        assert_eq!(src.geometry(), PyramidGeometry::new(600, 300, 256));
        assert_eq!(src.geometry().levels, 3);
        assert!(matches!(src.fetch(key(0, 0, 0)), Fetched::Valid(img) if img.size == [256, 256]));
        assert!(matches!(src.fetch(key(0, 1, 0)), Fetched::Stale(_)));
        assert!(matches!(src.fetch(key(0, 2, 0)), Fetched::Missing));
        // An edge tile is cropped to the image: 600 − 512 by 300 − 256.
        assert!(matches!(src.fetch(key(0, 2, 1)), Fetched::Valid(img) if img.size == [88, 44]));
        // Out of range keys are missing, never a panic.
        assert!(matches!(src.fetch(key(9, 0, 0)), Fetched::Missing));
        assert!(matches!(src.fetch(key(0, 7, 0)), Fetched::Missing));
        drop(store);
    }

    #[test]
    fn rgba_conversion_applies_stretch_and_transparency() {
        let p = |black, white, midtones| StretchParams {
            black,
            white,
            midtones,
        };
        // Mono, tile 2, image 2 × 1: no data, then 0.5 through black 0.25 /
        // white 0.75 (normalised 0.5, linear midtones) -> grey 128.
        let mono = tile_image(&[0.0, 0.5, 9.0, 9.0], 2, 2, 1, 1, &[p(0.25, 0.75, 0.5)]);
        assert_eq!(mono.size, [2, 1]);
        assert_eq!(mono.pixels[0], egui::Color32::TRANSPARENT);
        assert_eq!(mono.pixels[1], egui::Color32::from_rgb(128, 128, 128));

        // RGB, tile 2, image 2 × 2, a stretch per channel.
        let stretch = [p(0.0, 1.0, 0.5), p(0.5, 1.0, 0.5), p(0.0, 1.0, 0.25)];
        #[rustfmt::skip]
        let planar = [
            1.0, 0.0, 0.0, 0.2, // R
            0.75, 0.0, 0.3, 0.2, // G
            0.25, 0.0, 0.0, 0.2, // B
        ];
        let rgb = tile_image(&planar, 2, 2, 2, 3, &stretch);
        assert_eq!(rgb.size, [2, 2]);
        // R 1.0 -> 255; G (0.75 − 0.5) / 0.5 = 0.5 -> 128; B mtf(0.25, 0.25) = 0.5 -> 128.
        assert_eq!(rgb.pixels[0], egui::Color32::from_rgb(255, 128, 128));
        // All channels zero: no data, transparent.
        assert_eq!(rgb.pixels[1], egui::Color32::TRANSPARENT);
        // Zero in some channels only is data: opaque, black there.
        assert_eq!(rgb.pixels[2], egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(rgb.pixels[2].a(), 255);

        // A stretch change shows in the next fetch and bumps the generation.
        let path = temp_dir("tsrc-rgba").join("result.mmmt");
        let mut store = TileStore::create(&path, geom(1)).unwrap();
        store.write_tile(0, 0, 0, &vec![0.5f32; T * T]).unwrap();
        let src = MosaicTileSource::open(&path, Some(vec![p(0.0, 1.0, 0.5)])).unwrap();
        let grey = |f: Fetched| match f {
            Fetched::Valid(img) => img.pixels[0].r(),
            other => panic!("{other:?}"),
        };
        assert_eq!(grey(src.fetch(key(0, 0, 0))), 128);
        let g0 = src.generation();
        src.set_stretch(vec![p(0.0, 0.5, 0.5)]);
        assert!(src.generation() > g0);
        assert_eq!(grey(src.fetch(key(0, 0, 0))), 255);
        src.bump();
        assert!(src.generation() > g0 + 1);
        drop(store);
    }

    #[test]
    fn auto_stretch_from_coarse_level() {
        let path = temp_dir("tsrc-auto").join("result.mmmt");
        let g = geom(1);
        let mut store = TileStore::create(&path, g).unwrap();
        // Level 0 is bright and flat; the top level (2: 150 × 75 in one
        // tile) holds a ramp. Statistics come from the top level only.
        store.write_tile(0, 0, 0, &vec![0.9f32; T * T]).unwrap();
        let (w, h) = (150, 75);
        let mut top = vec![0.0f32; T * T];
        let mut ramp = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = 0.01 + 0.00002 * (y * w + x) as f32;
                top[y * T + x] = v;
                ramp.push(v);
            }
        }
        store.write_tile(2, 0, 0, &top).unwrap();
        let src = MosaicTileSource::open(&path, None).unwrap();
        let stats = src.coarse_stats().expect("top level readable");
        ramp.sort_by(f32::total_cmp);
        let median = ramp[ramp.len() / 2];
        assert!((stats[0].median - median).abs() < 1e-4, "{stats:?}");

        src.set_stretch(auto_stretch(&stats, false));
        let Fetched::Valid(img) = src.fetch(key(2, 0, 0)) else {
            panic!("top tile");
        };
        let mut greys: Vec<u8> = img.pixels.iter().map(|c| c.r()).collect();
        greys.sort_unstable();
        let mid = greys[greys.len() / 2];
        assert!((i32::from(mid) - 64).abs() <= 2, "median grey {mid}");

        // Nothing readable at any level: no statistics.
        let empty = temp_dir("tsrc-auto-empty").join("result.mmmt");
        drop(TileStore::create(&empty, g).unwrap());
        assert!(
            MosaicTileSource::open(&empty, None)
                .unwrap()
                .coarse_stats()
                .is_none()
        );
        drop(store);
    }
    #[test]
    fn result_is_kept_open_only_when_it_matches_the_analysis() {
        use mmm_core::job::JobCtl;
        use mmm_core::mosaic::{analyze_all, blend_group};

        let root = temp_dir("tsrc-open");
        let mut m = crate::synth_mosaic::two_group_mosaic(&root);
        let (result, dir) = (m.result_path("Ha"), m.group_session_dir("Ha"));
        assert!(matches!(
            open_result(&result, &dir, None),
            Opened::Absent(_)
        ));
        analyze_all(&mut m, &JobCtl::none()).unwrap();
        let Opened::Absent(why) = open_result(&result, &dir, None) else {
            panic!("no result yet");
        };
        assert_eq!(why, "Not blended yet.");
        blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();

        // Matching geometry: open, with an automatic stretch.
        let Opened::Ready { source, saved } = open_result(&result, &dir, None) else {
            panic!("blended result opens");
        };
        assert!(!saved);
        assert_ne!(source.stretch()[0], LINEAR);
        drop(source);
        let mine = vec![LINEAR; 3];
        let Opened::Ready { source, saved } = open_result(&result, &dir, Some(mine.clone())) else {
            panic!("blended result opens");
        };
        assert!(saved);
        assert_eq!(source.stretch(), mine);
        drop(source);

        // A store of another geometry would be recreated by the next blend:
        // it is reported out of date and no reader is kept.
        let mut g = TileStore::open(&result).unwrap().geometry();
        g.width += 1;
        drop(TileStore::create(&result, g).unwrap());
        let Opened::Absent(why) = open_result(&result, &dir, None) else {
            panic!("out of date");
        };
        assert!(why.contains("predates the latest analysis"), "{why}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
