//! Blending into the tiled result store: level-0 tiles must match a direct
//! whole-canvas blend, ROI blends touch only the aligned rect, parents are
//! covered means of their children, and a cancelled blend keeps its
//! finished tiles valid while unfinished ones stay stale. Parents are built
//! during the blend, match a from-scratch rebuild, and the rebuild cancels.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use mmm_core::Result;
use mmm_core::analyze::analyze;
use mmm_core::blend::{BlendMode, BlendParams, Extent, RowSink, blend};
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::settings::BlendSettings;
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthSpec, generate};
use mmm_core::tiles::{
    TILE, TileGeometry, TileReader, TileState, TileStore, blend_tiles, build_parents,
};

/// Comparison tolerance (controller ruling: matches the existing
/// `roi_matches_full_blend_subregion` tolerance).
const TOL: f32 = 1e-6;

/// In-memory sink collecting the whole (small) blended output, planar.
struct MemSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl MemSink {
    fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            ch: 0,
            data: Vec::new(),
        }
    }

    fn at(&self, c: usize, x: usize, y: usize) -> f32 {
        self.data[(c * self.h + y) * self.w + x]
    }
}

impl RowSink for MemSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.w = w as usize;
        self.h = h as usize;
        self.ch = ch as usize;
        self.data = vec![f32::NAN; self.w * self.h * self.ch];
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let band_rows = rows.len() / (self.ch * self.w);
        for c in 0..self.ch {
            for r in 0..band_rows {
                let src = &rows[(c * band_rows + r) * self.w..][..self.w];
                let off = (c * self.h + y0 as usize + r) * self.w;
                self.data[off..off + self.w].copy_from_slice(src);
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

struct Fixture {
    dir: PathBuf,
    session: Session,
    phot: Photometry,
    graph: OverlapGraph,
    surf: Surfaces,
}

impl Fixture {
    fn new(tag: &str, canvas: (u64, u64)) -> Self {
        let dir =
            std::env::temp_dir().join(format!("mmm-tiles-blend-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = SynthSpec {
            canvas,
            channels: 3,
            grid: (2, 2),
            overlap_frac: 0.25,
            n_stars: 40,
            noise_sigma: 0.002,
            panel_gain_range: (0.7, 1.4),
            panel_offset_range: (-0.01, 0.02),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![],
            panel_spike_angle: vec![],
            panel_defects: vec![],
            mid_blobs: 0,
            shift_blobs: false,
            core: None,
            seed: 7,
        };
        let res = generate(&spec, &dir.join("panels")).unwrap();
        let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
        let phot = Photometry::load(&session.photometry_path()).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        let surf = Surfaces::load(&session.surfaces_path()).unwrap();
        Fixture {
            dir,
            session,
            phot,
            graph,
            surf,
        }
    }

    fn geometry(&self) -> TileGeometry {
        TileGeometry {
            width: self.session.canvas.0,
            height: self.session.canvas.1,
            channels: self.session.canvas.2 as u32,
            tile: TILE,
        }
    }

    fn store(&self, name: &str) -> TileStore {
        TileStore::create(&self.dir.join(name), self.geometry()).unwrap()
    }

    /// Direct whole-canvas blend with the same settings `blend_tiles` uses.
    fn full_blend(&self, settings: &BlendSettings) -> MemSink {
        let params = BlendParams {
            downsample: 1,
            extent: Some(Extent::Canvas),
            ..settings.to_params()
        };
        let mut sink = MemSink::new();
        blend(
            &self.session,
            &self.phot,
            Some(&self.surf),
            &self.graph,
            &params,
            &mut sink,
        )
        .unwrap();
        sink
    }

    fn blend_tiles(
        &self,
        settings: &BlendSettings,
        rect: Option<[u64; 4]>,
        store: &mut TileStore,
        ctl: &JobCtl,
    ) -> Result<[u64; 4]> {
        blend_tiles(
            &self.session,
            &self.phot,
            Some(&self.surf),
            &self.graph,
            settings,
            rect,
            store,
            ctl,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn tile_buf(g: TileGeometry) -> Vec<f32> {
    vec![0.0; g.channels as usize * g.tile as usize * g.tile as usize]
}

/// Assert level-0 tile `(tx, ty)` is valid and equals the full blend (zero
/// beyond the image edge).
fn assert_tile_matches(store: &TileStore, full: &MemSink, tx: u32, ty: u32) {
    let g = store.geometry();
    let t = g.tile as usize;
    let mut buf = tile_buf(g);
    assert!(
        store.read_tile(0, tx, ty, &mut buf),
        "tile ({tx},{ty}) invalid"
    );
    for c in 0..g.channels as usize {
        for y in 0..t {
            for x in 0..t {
                let (cx, cy) = (tx as usize * t + x, ty as usize * t + y);
                let a = buf[(c * t + y) * t + x];
                let b = if cx < full.w && cy < full.h {
                    full.at(c, cx, cy)
                } else {
                    0.0
                };
                assert!(
                    (a - b).abs() < TOL,
                    "tile ({tx},{ty}) ch {c} pixel ({cx},{cy}): {a} vs {b}"
                );
            }
        }
    }
}

fn assert_all_tiles_match(fx: &Fixture, canvas: (u64, u64), tag: &str) {
    let settings = BlendSettings::default();
    let full = fx.full_blend(&settings);
    let mut store = fx.store(tag);
    let r = fx
        .blend_tiles(&settings, None, &mut store, &JobCtl::none())
        .unwrap();
    assert_eq!(r, [0, 0, canvas.0, canvas.1]);
    let g = store.geometry();
    let (nx, ny) = g.tiles(0);
    for ty in 0..ny {
        for tx in 0..nx {
            assert_tile_matches(&store, &full, tx, ty);
        }
    }
    for l in 0..g.levels() {
        let (nx, ny) = g.tiles(l);
        assert_eq!(store.valid_count(l), (nx * ny) as usize, "level {l}");
    }
}

#[test]
fn tiles_match_full_blend() {
    let canvas = (768, 512);
    let fx = Fixture::new("full", canvas);
    assert_all_tiles_match(&fx, canvas, "full.mmmtile");
}

#[test]
fn odd_canvas_tiles_match_full_blend() {
    let canvas = (600, 420);
    let fx = Fixture::new("odd", canvas);
    assert_all_tiles_match(&fx, canvas, "odd.mmmtile");
}

#[test]
fn roi_tiles_match_full_blend() {
    let fx = Fixture::new("roi", (768, 640));
    let settings = BlendSettings::default();
    let full = fx.full_blend(&settings);
    let mut store = fx.store("roi.mmmtile");
    let r = fx
        .blend_tiles(
            &settings,
            Some([300, 200, 520, 300]),
            &mut store,
            &JobCtl::none(),
        )
        .unwrap();
    assert_eq!(r, [256, 0, 768, 512]);
    let g = store.geometry();
    let (nx, ny) = g.tiles(0);
    for ty in 0..ny {
        for tx in 0..nx {
            let inside = (1..3).contains(&tx) && (0..2).contains(&ty);
            if inside {
                assert_tile_matches(&store, &full, tx, ty);
            } else {
                assert!(!store.is_valid(0, tx, ty), "tile ({tx},{ty}) outside rect");
            }
        }
    }
    assert_eq!(store.valid_count(0), 4);
}

#[test]
fn parents_are_covered_means() {
    let fx = Fixture::new("parents", (600, 420));
    let mut store = fx.store("parents.mmmtile");
    fx.blend_tiles(&BlendSettings::default(), None, &mut store, &JobCtl::none())
        .unwrap();
    let g = store.geometry();
    let t = g.tile as usize;
    let nch = g.channels as usize;

    // Level 0 as a dense image, for hand computation.
    let (w0, h0) = g.level_dims(0);
    let (w0, h0) = (w0 as usize, h0 as usize);
    let mut l0 = vec![0.0f32; nch * w0 * h0];
    let mut buf = tile_buf(g);
    let (nx, ny) = g.tiles(0);
    for ty in 0..ny {
        for tx in 0..nx {
            assert!(store.read_tile(0, tx, ty, &mut buf));
            for c in 0..nch {
                for y in 0..t {
                    for x in 0..t {
                        let (cx, cy) = (tx as usize * t + x, ty as usize * t + y);
                        if cx < w0 && cy < h0 {
                            l0[(c * h0 + cy) * w0 + cx] = buf[(c * t + y) * t + x];
                        }
                    }
                }
            }
        }
    }

    let (w1, h1) = g.level_dims(1);
    let (nx1, ny1) = g.tiles(1);
    let mut covered_parents = 0;
    for ty in 0..ny1 {
        for tx in 0..nx1 {
            assert!(store.read_tile(1, tx, ty, &mut buf), "level-1 tile invalid");
            for y in 0..t {
                for x in 0..t {
                    let (px, py) = (tx as usize * t + x, ty as usize * t + y);
                    if px >= w1 as usize || py >= h1 as usize {
                        for c in 0..nch {
                            assert_eq!(buf[(c * t + y) * t + x], 0.0);
                        }
                        continue;
                    }
                    let mut sums = vec![0.0f32; nch];
                    let mut n = 0;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let (cx, cy) = (2 * px + dx, 2 * py + dy);
                            if cx >= w0 || cy >= h0 {
                                continue;
                            }
                            let v: Vec<f32> =
                                (0..nch).map(|c| l0[(c * h0 + cy) * w0 + cx]).collect();
                            if v.iter().any(|&s| s != 0.0) {
                                n += 1;
                                for c in 0..nch {
                                    sums[c] += v[c];
                                }
                            }
                        }
                    }
                    if n > 0 {
                        covered_parents += 1;
                    }
                    for c in 0..nch {
                        let want = if n > 0 { sums[c] / n as f32 } else { 0.0 };
                        let got = buf[(c * t + y) * t + x];
                        assert!(
                            (got - want).abs() <= 1e-6 * want.abs().max(1.0),
                            "level-1 ({px},{py}) ch {c}: {got} vs {want}"
                        );
                    }
                }
            }
        }
    }
    assert!(covered_parents > 0);
}

#[test]
fn cancelled_blend_keeps_finished_tiles() {
    // Five level-0 tile rows; the blend delivers 256-row bands, so
    // cancelling at the report for row 512 leaves tile rows 0-1 finished.
    let fx = Fixture::new("cancel", (600, 1100));
    let settings = BlendSettings::default();
    let full = fx.full_blend(&settings);
    let mut store = fx.store("cancel.mmmtile");
    // Pre-populate with other settings, so finished tiles must have been
    // rewritten and unfinished ones must not pass as current.
    let other = BlendSettings {
        mode: BlendMode::Feather,
        ..settings.clone()
    };
    fx.blend_tiles(&other, None, &mut store, &JobCtl::none())
        .unwrap();
    let tok = CancelToken::new();
    let cancel = tok.clone();
    let obs = move |stage: &str, done: u64, _: u64| {
        if stage == "blend" && done >= 512 {
            cancel.cancel();
        }
    };
    let ctl = JobCtl::new(tok, Some(&obs));
    let err = fx
        .blend_tiles(&settings, None, &mut store, &ctl)
        .unwrap_err();
    assert!(err.is_cancelled(), "{err}");

    let g = store.geometry();
    let (nx, ny) = g.tiles(0);
    assert_eq!(ny, 5);
    for ty in 0..ny {
        for tx in 0..nx {
            if ty < 2 {
                // Finished: valid, with the full blend's data.
                assert_tile_matches(&store, &full, tx, ty);
            } else {
                assert!(!store.is_valid(0, tx, ty), "unfinished ({tx},{ty}) valid");
            }
        }
    }
    // A parent is valid exactly when every in-image child is valid.
    for l in 1..g.levels() {
        let (cnx, cny) = g.tiles(l - 1);
        let (pnx, pny) = g.tiles(l);
        for ty in 0..pny {
            for tx in 0..pnx {
                let kids_valid = (0..4).all(|q| {
                    let (cx, cy) = (2 * tx + (q & 1), 2 * ty + (q >> 1));
                    cx >= cnx || cy >= cny || store.is_valid(l - 1, cx, cy)
                });
                assert_eq!(
                    store.is_valid(l, tx, ty),
                    kids_valid,
                    "level {l} tile ({tx},{ty})"
                );
            }
        }
    }
    // Level 1's top row has only finished children, so it was rebuilt.
    assert!(store.is_valid(1, 0, 0) && store.is_valid(1, 1, 0));
    assert_eq!(store.valid_count(1), 2);

    // Reblending completes the rest.
    fx.blend_tiles(&settings, None, &mut store, &JobCtl::none())
        .unwrap();
    for l in 0..g.levels() {
        let (nx, ny) = g.tiles(l);
        assert_eq!(store.valid_count(l), (nx * ny) as usize, "level {l}");
    }
}

#[test]
fn wrong_geometry_errors() {
    let fx = Fixture::new("geom", (600, 420));
    let g = TileGeometry {
        width: 601,
        ..fx.geometry()
    };
    let mut store = TileStore::create(&fx.dir.join("bad.mmmtile"), g).unwrap();
    assert!(
        fx.blend_tiles(&BlendSettings::default(), None, &mut store, &JobCtl::none())
            .is_err()
    );
    let g = TileGeometry {
        channels: 1,
        ..fx.geometry()
    };
    let mut store = TileStore::create(&fx.dir.join("bad1.mmmtile"), g).unwrap();
    assert!(
        fx.blend_tiles(&BlendSettings::default(), None, &mut store, &JobCtl::none())
            .is_err()
    );
}

#[test]
fn parents_valid_during_blend() {
    // Five level-0 tile rows; the first report past the halfway point comes
    // after rows 0-2 are written, so level-1 row 0 has all its children.
    let fx = Fixture::new("during", (600, 1100));
    let path = fx.dir.join("during.mmmtile");
    let mut store = TileStore::create(&path, fx.geometry()).unwrap();
    let reader = TileReader::open(&path).unwrap();
    let checked = AtomicBool::new(false);
    let seen = AtomicBool::new(false);
    let obs = |stage: &str, done: u64, total: u64| {
        if stage != "blend" || 2 * done < total || checked.swap(true, Ordering::Relaxed) {
            return;
        }
        let g = reader.geometry();
        let (nx, ny) = g.tiles(1);
        let mut buf = tile_buf(g);
        let any = (0..ny).any(|ty| (0..nx).any(|tx| reader.read_tile(1, tx, ty, &mut buf)));
        seen.store(any, Ordering::Relaxed);
    };
    let ctl = JobCtl::new(CancelToken::new(), Some(&obs));
    fx.blend_tiles(&BlendSettings::default(), None, &mut store, &ctl)
        .unwrap();
    assert!(checked.load(Ordering::Relaxed), "no report past halfway");
    assert!(
        seen.load(Ordering::Relaxed),
        "no valid level-1 tile past halfway"
    );
}

#[test]
fn parents_still_match_full_build() {
    let fx = Fixture::new("rebuild", (600, 1100));
    let mut store = fx.store("inc.mmmtile");
    fx.blend_tiles(&BlendSettings::default(), None, &mut store, &JobCtl::none())
        .unwrap();
    // Copy level 0 into a fresh store and build its parents from scratch.
    let g = store.geometry();
    let mut fresh = fx.store("fresh.mmmtile");
    let mut buf = tile_buf(g);
    let (nx, ny) = g.tiles(0);
    for ty in 0..ny {
        for tx in 0..nx {
            assert!(store.read_tile(0, tx, ty, &mut buf));
            fresh.write_tile(0, tx, ty, &buf).unwrap();
        }
    }
    build_parents(&mut fresh, [0, 0, g.width, g.height], &JobCtl::none()).unwrap();
    let mut want = tile_buf(g);
    for l in 1..g.levels() {
        let (nx, ny) = g.tiles(l);
        for ty in 0..ny {
            for tx in 0..nx {
                assert!(
                    store.read_tile(l, tx, ty, &mut buf),
                    "level {l} ({tx},{ty})"
                );
                assert!(fresh.read_tile(l, tx, ty, &mut want));
                for (i, (a, b)) in buf.iter().zip(&want).enumerate() {
                    assert!(
                        (a - b).abs() <= TOL,
                        "level {l} tile ({tx},{ty}) sample {i}: {a} vs {b}"
                    );
                }
            }
        }
    }
}

#[test]
fn build_parents_cancels() {
    let dir = std::env::temp_dir().join(format!("mmm-tiles-bpcancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let g = TileGeometry {
        width: 600,
        height: 420,
        channels: 3,
        tile: TILE,
    };
    let mut store = TileStore::create(&dir.join("c.mmmtile"), g).unwrap();
    let tok = CancelToken::new();
    tok.cancel();
    let err = build_parents(&mut store, [0, 0, 600, 420], &JobCtl::new(tok, None)).unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every tile at every level, read viewer-side: `(state, data)` per tile,
/// level by level, row-major (`None` data for a missing or torn tile).
fn snapshot(reader: &TileReader) -> Vec<Vec<(TileState, Option<Vec<f32>>)>> {
    let g = reader.geometry();
    (0..g.levels())
        .map(|l| {
            let (nx, ny) = g.tiles(l);
            let mut tiles = Vec::new();
            for ty in 0..ny {
                for tx in 0..nx {
                    let mut buf = tile_buf(g);
                    let st = reader.state(l, tx, ty);
                    let got = reader.read_tile_any(l, tx, ty, &mut buf);
                    if let Some(s) = got {
                        assert_eq!(s, st, "level {l} ({tx},{ty}) state changed");
                    }
                    tiles.push((st, got.map(|_| buf)));
                }
            }
            tiles
        })
        .collect()
}

/// The covered mean of four children (quadrant order (0,0) (1,0) (0,1)
/// (1,1); `None` = outside the grid or never written).
fn covered_mean(g: TileGeometry, kids: [Option<&Vec<f32>>; 4]) -> Vec<f32> {
    let t = g.tile as usize;
    let nch = g.channels as usize;
    let mut out = tile_buf(g);
    for y in 0..t {
        for x in 0..t {
            let mut sums = vec![0.0f32; nch];
            let mut n = 0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    let Some(kid) = kids[(sy / t) * 2 + sx / t] else {
                        continue;
                    };
                    let (lx, ly) = (sx % t, sy % t);
                    let v: Vec<f32> = (0..nch).map(|c| kid[(c * t + ly) * t + lx]).collect();
                    if v.iter().any(|&s| s != 0.0) {
                        n += 1;
                        for c in 0..nch {
                            sums[c] += v[c];
                        }
                    }
                }
            }
            for c in 0..nch {
                out[(c * t + y) * t + x] = if n > 0 { sums[c] / n as f32 } else { 0.0 };
            }
        }
    }
    out
}

#[test]
fn roi_reblend_after_settings_change_updates_parents() {
    // 5×4 level-0 tiles, 3×2 at level 1, 2×1 at level 2, 1×1 at level 3.
    let fx = Fixture::new("roi-reblend", (1100, 900));
    let path = fx.dir.join("r.mmmtile");
    let mut store = TileStore::create(&path, fx.geometry()).unwrap();
    let reader = TileReader::open(&path).unwrap();
    let g = store.geometry();
    assert_eq!(g.levels(), 4);
    let old = BlendSettings::default();
    fx.blend_tiles(&old, None, &mut store, &JobCtl::none())
        .unwrap();
    let before = snapshot(&reader);

    // A settings change stales everything; then blend an ROI whose aligned
    // rect (level-0 tiles x 1..3, y 1..3) straddles every level-1 parent
    // boundary it touches.
    store.invalidate_all();
    let new = BlendSettings {
        mode: BlendMode::Feather,
        ..old.clone()
    };
    let r = fx
        .blend_tiles(
            &new,
            Some([300, 300, 700, 600]),
            &mut store,
            &JobCtl::none(),
        )
        .unwrap();
    assert_eq!(r, [256, 256, 768, 768]);
    let after = snapshot(&reader);

    // Level 0: the ROI's tiles valid with new data, the rest stale.
    let (nx0, _) = g.tiles(0);
    let mut changed0 = 0;
    for (i, (st, data)) in after[0].iter().enumerate() {
        let (tx, ty) = (i as u32 % nx0, i as u32 / nx0);
        let inside = (1..3).contains(&tx) && (1..3).contains(&ty);
        let want = if inside {
            TileState::Valid
        } else {
            TileState::Stale
        };
        assert_eq!(*st, want, "level 0 ({tx},{ty})");
        if data != &before[0][i].1 {
            assert!(inside, "level 0 ({tx},{ty}) outside the ROI changed");
            changed0 += 1;
        }
    }
    assert!(changed0 > 0, "the new settings changed nothing");

    // Every parent: the covered mean of its children's current data; valid
    // exactly when every in-image child is.
    for l in 1..g.levels() {
        let (cnx, cny) = g.tiles(l - 1);
        let (pnx, pny) = g.tiles(l);
        let mut changed = 0;
        for ty in 0..pny {
            for tx in 0..pnx {
                let kid = |q: u32| {
                    let (cx, cy) = (2 * tx + (q & 1), 2 * ty + (q >> 1));
                    (cx < cnx && cy < cny).then(|| &after[l as usize - 1][(cy * cnx + cx) as usize])
                };
                let all_valid = (0..4).all(|q| kid(q).is_none_or(|k| k.0 == TileState::Valid));
                let kids = [0, 1, 2, 3].map(|q| kid(q).and_then(|k| k.1.as_ref()));
                let want = covered_mean(g, kids);
                let i = (ty * pnx + tx) as usize;
                let (st, data) = &after[l as usize][i];
                let want_st = if all_valid {
                    TileState::Valid
                } else {
                    TileState::Stale
                };
                assert_eq!(*st, want_st, "level {l} ({tx},{ty}) state");
                let data = data.as_ref().expect("parent has data");
                for (j, (a, b)) in data.iter().zip(&want).enumerate() {
                    assert!(
                        (a - b).abs() <= TOL * b.abs().max(1.0),
                        "level {l} ({tx},{ty}) sample {j}: {a} vs {b}"
                    );
                }
                if Some(data) != before[l as usize][i].1.as_ref() {
                    changed += 1;
                }
            }
        }
        assert!(changed > 0, "no level-{l} parent picked up the ROI");
    }
}

#[test]
fn full_reblend_after_roi_restores_valid_pyramid() {
    let fx = Fixture::new("roi-full", (1100, 900));
    let path = fx.dir.join("f.mmmtile");
    let mut store = TileStore::create(&path, fx.geometry()).unwrap();
    let reader = TileReader::open(&path).unwrap();
    let g = store.geometry();
    let old = BlendSettings::default();
    fx.blend_tiles(&old, None, &mut store, &JobCtl::none())
        .unwrap();
    let before = snapshot(&reader);
    store.invalidate_all();
    let new = BlendSettings {
        mode: BlendMode::Feather,
        ..old.clone()
    };
    fx.blend_tiles(
        &new,
        Some([300, 300, 700, 600]),
        &mut store,
        &JobCtl::none(),
    )
    .unwrap();
    // Back to the old settings over the whole canvas: everything valid and
    // identical to the first full blend.
    store.invalidate_all();
    fx.blend_tiles(&old, None, &mut store, &JobCtl::none())
        .unwrap();
    let after = snapshot(&reader);
    for l in 0..g.levels() as usize {
        for (i, ((st, data), (_, want))) in after[l].iter().zip(&before[l]).enumerate() {
            assert_eq!(*st, TileState::Valid, "level {l} tile {i}");
            assert_eq!(data, want, "level {l} tile {i} differs");
        }
    }
}
