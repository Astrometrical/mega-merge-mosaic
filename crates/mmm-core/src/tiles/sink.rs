//! Producing tiles: a [`RowSink`] that writes a blend into a [`TileStore`],
//! the parent-level (pyramid) rebuild, and [`blend_tiles`], which ties a
//! (possibly ROI) blend, the parents, and job control together.

use crate::blend::{BlendParams, Extent, RowSink, blend};
use crate::error::{Error, Result};
use crate::job::{CancelToken, CtlSink, JobCtl};
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::session::Session;
use crate::settings::BlendSettings;
use crate::surfaces::Surfaces;

use super::{TileState, TileStore};

/// A [`RowSink`] writing blended rows into the level-0 tiles of a rect.
///
/// Rows are accumulated one tile row (`tile` image rows) at a time; each
/// complete tile row is written to the store (and its tiles marked valid)
/// as soon as its last row arrives. Bands must arrive in order, as the
/// blend delivers them. After each tile row is written, the parents over
/// it are rebuilt ([`build_parents`]), so zoomed-out views follow the
/// blend: a parent is rewritten (stale) with each new child and becomes
/// valid as soon as all its in-image children are.
pub struct TileSink<'a> {
    store: &'a mut TileStore,
    rect: [u64; 4],
    ch: usize,
    /// One tile row of output, planar: `ch × tile × w`.
    buf: Vec<f32>,
    /// Next output row expected (relative to the rect).
    next_y: u64,
    /// Rows of the current tile row already in `buf`.
    pending: usize,
    /// Scratch for one tile.
    tile_buf: Vec<f32>,
    /// Rows of the rect (from its top) whose tiles are all written.
    written: u64,
    /// Rows of the rect (from its top) whose parents have been rebuilt.
    parents_built: u64,
    /// Cancellation for the incremental parent rebuilds.
    cancel: CancelToken,
}

impl<'a> TileSink<'a> {
    /// A sink for the tile-aligned `rect` (level-0 coords, as returned by
    /// [`super::TileGeometry::align_rect`]); invalidates the rect's tiles at
    /// every level immediately. Errors if `rect` is empty or not aligned.
    /// The parent rebuild after each tile row checks `ctl` for cancellation.
    pub fn new(store: &'a mut TileStore, rect: [u64; 4], ctl: &JobCtl) -> Result<Self> {
        let g = store.geometry();
        if rect[0] >= rect[2] || rect[1] >= rect[3] || g.align_rect(rect) != rect {
            return Err(Error::compute(format!(
                "tile sink rect {rect:?} is empty or not tile-aligned"
            )));
        }
        store.invalidate_rect(rect);
        Ok(Self {
            store,
            rect,
            ch: g.channels as usize,
            buf: Vec::new(),
            next_y: 0,
            pending: 0,
            tile_buf: Vec::new(),
            written: 0,
            parents_built: 0,
            cancel: ctl.cancel_token().clone(),
        })
    }

    /// Rows of the rect, from its top, whose level-0 tiles have all been
    /// written (and marked valid) by this sink: a multiple of the tile size,
    /// or the rect's height once [`RowSink::finish`] succeeded.
    pub fn rows_written(&self) -> u64 {
        self.written
    }

    fn width(&self) -> usize {
        (self.rect[2] - self.rect[0]) as usize
    }

    fn height(&self) -> u64 {
        self.rect[3] - self.rect[1]
    }

    /// Write the buffered tile row (rows `next_y - pending .. next_y`) to
    /// the store, clear the buffer, then rebuild the parents over the row.
    fn flush_tile_row(&mut self) -> Result<()> {
        let g = self.store.geometry();
        let t = g.tile as usize;
        let w = self.width();
        let first_row = self.rect[1] + self.next_y - self.pending as u64;
        let ty = (first_row / t as u64) as u32;
        let tx0 = (self.rect[0] / t as u64) as u32;
        self.tile_buf.resize(self.ch * t * t, 0.0);
        for i in 0..w.div_ceil(t) {
            let x0 = i * t;
            let n = t.min(w - x0);
            self.tile_buf.fill(0.0);
            for c in 0..self.ch {
                for r in 0..self.pending {
                    let src = &self.buf[(c * t + r) * w + x0..][..n];
                    self.tile_buf[(c * t + r) * t..][..n].copy_from_slice(src);
                }
            }
            self.store
                .write_tile(0, tx0 + i as u32, ty, &self.tile_buf)?;
        }
        self.buf.fill(0.0);
        self.written = self.next_y;
        self.pending = 0;
        let row = [
            self.rect[0],
            first_row,
            self.rect[2],
            self.rect[1] + self.written,
        ];
        build_parents(self.store, row, &JobCtl::new(self.cancel.clone(), None))?;
        self.parents_built = self.written;
        Ok(())
    }
}

impl RowSink for TileSink<'_> {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        if w != self.rect[2] - self.rect[0] || h != self.height() || ch != self.ch as u64 {
            return Err(Error::compute(format!(
                "tile sink expects {}×{}×{}, blend produces {w}×{h}×{ch}",
                self.width(),
                self.height(),
                self.ch
            )));
        }
        let t = self.store.geometry().tile as usize;
        self.buf = vec![0.0; self.ch * t * self.width()];
        self.next_y = 0;
        self.pending = 0;
        self.written = 0;
        self.parents_built = 0;
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let w = self.width();
        let per_row = self.ch * w;
        if y0 != self.next_y || per_row == 0 || !rows.len().is_multiple_of(per_row) {
            return Err(Error::compute(format!(
                "tile sink: band at row {y0} ({} samples) out of order or misshapen",
                rows.len()
            )));
        }
        let band_rows = rows.len() / per_row;
        if y0 + band_rows as u64 > self.height() {
            return Err(Error::compute("tile sink: band runs past the rect"));
        }
        let t = self.store.geometry().tile as usize;
        for r in 0..band_rows {
            for c in 0..self.ch {
                let src = &rows[(c * band_rows + r) * w..][..w];
                self.buf[(c * t + self.pending) * w..][..w].copy_from_slice(src);
            }
            self.pending += 1;
            self.next_y += 1;
            if self.pending == t {
                self.flush_tile_row()?;
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.next_y != self.height() {
            return Err(Error::compute(format!(
                "tile sink: blend delivered {} of {} rows",
                self.next_y,
                self.height()
            )));
        }
        if self.pending > 0 {
            self.flush_tile_row()?;
        }
        Ok(())
    }
}

/// Rebuild, for every level ≥ 1, the tiles covering `rect` (level-0 coords)
/// from their four children, lowest level first.
///
/// Checks `ctl` for cancellation once per parent tile row; a cancelled
/// rebuild returns [`Error::Cancelled`], leaving parents it did not reach
/// as they were.
///
/// Each parent pixel is the per-channel mean of its (up to four) child
/// pixels that are covered — any channel non-zero — or zero if none are.
/// Children are taken as they are, valid or stale (an ROI blend after a
/// settings change leaves a parent on the ROI's edge with children of
/// both kinds); a missing child (never written) is uncovered and costs no
/// copy. The parent is written valid if every child tile inside the image
/// is valid, otherwise stale ([`TileStore::write_tile_stale`]: complete,
/// shown dimmed). A parent with no child to read is not written: it is
/// invalidated (staying missing if it never was written).
pub fn build_parents(store: &mut TileStore, rect: [u64; 4], ctl: &JobCtl) -> Result<()> {
    let g = store.geometry();
    let t = g.tile as usize;
    let nch = g.channels as usize;
    let tile_len = nch * t * t;
    // Children in quadrant order (dx, dy) = (0,0), (1,0), (0,1), (1,1);
    // `None` = outside the child grid, missing, or unreadable.
    let mut kids: [Option<Vec<f32>>; 4] = Default::default();
    let mut spare: Vec<Vec<f32>> = Vec::new();
    let mut parent = vec![0.0f32; tile_len];
    let mut sums = vec![0.0f32; nch];
    for level in 1..g.levels() {
        let (cnx, cny) = g.tiles(level - 1);
        let mut row = None;
        for (tx, ty) in g.tiles_in_rect(level, rect) {
            if row != Some(ty) {
                ctl.check()?;
                row = Some(ty);
            }
            let mut all_valid = true;
            let mut any = false;
            for (q, kid) in kids.iter_mut().enumerate() {
                if let Some(buf) = kid.take() {
                    spare.push(buf);
                }
                let (cx, cy) = (2 * tx + (q as u32 & 1), 2 * ty + (q as u32 >> 1));
                if cx >= cnx || cy >= cny {
                    continue;
                }
                // Missing children are skipped without a copy.
                if store.state(level - 1, cx, cy) == TileState::Missing {
                    all_valid = false;
                    continue;
                }
                let mut buf = spare.pop().unwrap_or_else(|| vec![0.0; tile_len]);
                // Single writer, so no copy is torn; a child left mid-write
                // by a crash has no complete copy and counts as missing.
                match store.read_tile_any(level - 1, cx, cy, &mut buf) {
                    Some(st) => {
                        all_valid &= st == TileState::Valid;
                        any = true;
                        *kid = Some(buf);
                    }
                    None => {
                        all_valid = false;
                        spare.push(buf);
                    }
                }
            }
            if !any {
                let idx = store.raw.layout.index(level, tx, ty);
                store.raw.invalidate(idx);
                continue;
            }
            for y in 0..t {
                for x in 0..t {
                    sums.fill(0.0);
                    let mut n = 0u32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let (sx, sy) = (2 * x + dx, 2 * y + dy);
                            let q = (sy / t) * 2 + sx / t;
                            let Some(kid) = &kids[q] else { continue };
                            let (lx, ly) = (sx % t, sy % t);
                            let px = |c: usize| kid[(c * t + ly) * t + lx];
                            if (0..nch).any(|c| px(c) != 0.0) {
                                n += 1;
                                for (c, s) in sums.iter_mut().enumerate() {
                                    *s += px(c);
                                }
                            }
                        }
                    }
                    for (c, &s) in sums.iter().enumerate() {
                        parent[(c * t + y) * t + x] = if n > 0 { s / n as f32 } else { 0.0 };
                    }
                }
            }
            if all_valid {
                store.write_tile(level, tx, ty, &parent)?;
            } else {
                store.write_tile_stale(level, tx, ty, &parent)?;
            }
        }
    }
    Ok(())
}

/// Blend `rect` (level-0 canvas coords; `None` = the whole canvas) into
/// `store` and rebuild its parent levels; returns the tile-aligned rect
/// actually blended ([`super::TileGeometry::align_rect`]).
///
/// The blend runs at full resolution over [`Extent::Canvas`] with the ROI
/// set to the aligned rect, so every tile matches the same region of a
/// whole-canvas blend. Parents are rebuilt during the blend after each
/// level-0 tile row ([`TileSink`]), then over any rows the blend left
/// without them. Progress is reported as stage `"blend"` (rows of the
/// rect) and then `("pyramid", 1, 1)` once the parents are built.
///
/// The rect's tiles are invalidated (at every level) before blending, and
/// each level-0 tile row is marked valid as soon as it is written. Parents
/// on the rect's edge (or over a not-yet-written row) are rebuilt from
/// their children's current data, valid or stale, and left stale; a parent
/// is valid exactly when all its in-image children are. On any error —
/// including cancellation through `ctl` — finished work is kept: level-0
/// tiles the run did not fully write are invalidated (they stay stale),
/// the parents over finished rows are rebuilt by these rules, and parents
/// over unfinished rows only keep their earlier (stale) data; then the
/// error is returned.
/// Errors if the store's geometry does not match `session.canvas`.
#[allow(clippy::too_many_arguments)]
pub fn blend_tiles(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    graph: &OverlapGraph,
    settings: &BlendSettings,
    rect: Option<[u64; 4]>,
    store: &mut TileStore,
    ctl: &JobCtl,
) -> Result<[u64; 4]> {
    let g = store.geometry();
    let (cw, chh, cch) = session.canvas;
    if (g.width, g.height, g.channels as u64) != (cw, chh, cch) {
        return Err(Error::compute(format!(
            "tile store is {}×{}×{}, session canvas is {cw}×{chh}×{cch}",
            g.width, g.height, g.channels
        )));
    }
    let aligned = g.align_rect(rect.unwrap_or([0, 0, cw, chh]));
    let params = BlendParams {
        downsample: 1,
        extent: Some(Extent::Canvas),
        roi: Some(aligned),
        ..settings.to_params()
    };
    let mut written = 0;
    let mut with_parents = 0;
    let blended = (|| -> Result<()> {
        let mut tiles = TileSink::new(store, aligned, ctl)?;
        let res = blend(
            session,
            phot,
            surfaces,
            graph,
            &params,
            &mut CtlSink::new(&mut tiles, ctl, "blend"),
        );
        written = tiles.rows_written();
        with_parents = tiles.parents_built;
        res
    })();
    if let Err(e) = blended {
        keep_finished(store, aligned, written, with_parents);
        return Err(e);
    }
    // Any error from here on leaves the store consistent: every level-0
    // tile is written, and a parent not yet rebuilt is still stale.
    // The sink rebuilt the parents over every row it finished, so this
    // final pass normally has nothing left to do.
    let rest = [
        aligned[0],
        aligned[1] + with_parents,
        aligned[2],
        aligned[3],
    ];
    if rest[1] < rest[3] {
        build_parents(store, rest, ctl)?;
    }
    ctl.report("pyramid", 1, 1)?;
    Ok(aligned)
}

/// After a failed blend of `rect`: invalidate the level-0 tiles below the
/// first `written` rows (not fully written by this run), then rebuild the
/// parents over rows `parents_built..written` only — the rows the sink
/// wrote but did not get to rebuild parents over (its incremental rebuild
/// was cancelled). Parents over earlier rows are already current, and
/// parents wholly over unfinished rows were invalidated by
/// [`TileSink::new`] and keep their earlier data, consistent with their
/// (equally untouched) children; so every parent ends rebuilt from its
/// children's current data, valid exactly when all its in-image children
/// are, without re-reading finished tiles.
fn keep_finished(store: &mut TileStore, rect: [u64; 4], written: u64, parents_built: u64) {
    let g = store.geometry();
    let done = (rect[1] + written).min(rect[3]);
    let unfinished = [rect[0], done, rect[2], rect[3]];
    for (tx, ty) in g.tiles_in_rect(0, unfinished) {
        let idx = store.raw.layout.index(0, tx, ty);
        store.raw.invalidate(idx);
    }
    let from = (rect[1] + parents_built).min(done);
    if from < done {
        // Not cancellable (the job's ctl is likely cancelled already), so
        // `build_parents` only fails on a tile-length mismatch, impossible
        // here; should it fail, parents it did not reach keep the
        // invalidation `TileSink::new` gave them.
        let _ = build_parents(store, [rect[0], from, rect[2], done], &JobCtl::none());
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::analyze::analyze;
    use crate::synth::{SynthSpec, generate};
    use crate::tiles::{TILE, TileGeometry};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-sink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn geometry(width: u64, height: u64) -> TileGeometry {
        TileGeometry {
            width,
            height,
            channels: 3,
            tile: TILE,
        }
    }

    #[test]
    fn parent_skips_missing_children_and_stays_stale() {
        let d = tmp("missing-kid");
        let g = geometry(600, 420);
        let mut store = TileStore::create(&d.join("s.mmmtile"), g).unwrap();
        let t = g.tile as usize;
        // Level-1 tile (0,0) has children (0,0) (1,0) (0,1) (1,1): two
        // valid, one stale (written, then invalidated), one missing.
        store.write_tile(0, 0, 0, &vec![0.5; g.tile_len()]).unwrap();
        store.write_tile(0, 1, 0, &vec![0.5; g.tile_len()]).unwrap();
        store
            .write_tile(0, 0, 1, &vec![0.25; g.tile_len()])
            .unwrap();
        store.invalidate_rect([0, 256, 1, 257]);
        assert_eq!(store.state(0, 0, 1), TileState::Stale);
        let copies = store.copy_counter();
        build_parents(&mut store, [0, 0, 512, 512], &JobCtl::none()).unwrap();
        // Three present kids for level-1 (0,0), then that tile alone for
        // level-2 (0,0) (its sibling level-1 (1,0) is missing).
        assert_eq!(
            copies.load(Ordering::Relaxed),
            4,
            "only present kids copied"
        );
        assert_eq!(store.state(1, 1, 0), TileState::Missing);
        assert_eq!(store.state(2, 0, 0), TileState::Stale);
        // Rebuilt from the best data, but not valid (a child is not).
        let mut p = vec![0.0; g.tile_len()];
        assert_eq!(store.read_tile_any(1, 0, 0, &mut p), Some(TileState::Stale));
        // Top half from the valid kids, bottom-left from the stale one,
        // bottom-right (the missing kid) uncovered.
        assert_eq!(p[0], 0.5);
        assert_eq!(p[(t - 1) / 2 * t + t - 1], 0.5);
        assert_eq!(p[(t / 2) * t], 0.25);
        assert_eq!(p[(t / 2) * t + t / 2], 0.0);
        drop(store);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn parent_with_only_missing_children_copies_nothing() {
        let d = tmp("nocopy");
        let g = geometry(600, 420);
        let mut store = TileStore::create(&d.join("s.mmmtile"), g).unwrap();
        let copies = store.copy_counter();
        build_parents(&mut store, [0, 0, 600, 420], &JobCtl::none()).unwrap();
        assert_eq!(copies.load(Ordering::Relaxed), 0, "children copied");
        for l in 1..g.levels() {
            let (nx, ny) = g.tiles(l);
            for ty in 0..ny {
                for tx in 0..nx {
                    assert_eq!(store.state(l, tx, ty), TileState::Missing);
                }
            }
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_near_end_rereads_no_finished_tiles() {
        let d = tmp("late-cancel");
        let spec = SynthSpec {
            canvas: (600, 1100),
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
        let res = generate(&spec, &d.join("panels")).unwrap();
        let session = analyze(&res.panel_paths, &d.join("s.mmm-session")).unwrap();
        let phot = Photometry::load(&session.photometry_path()).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        let surf = Surfaces::load(&session.surfaces_path()).unwrap();
        let g = geometry(600, 1100);
        let mut store = TileStore::create(&d.join("r.mmmtile"), g).unwrap();

        // Cancel once four of the five tile rows (and their parents) are
        // written; count tile copies made after that point.
        let copies = store.copy_counter();
        let at_cancel = AtomicUsize::new(usize::MAX);
        let tok = CancelToken::new();
        let cancel = tok.clone();
        let obs = |stage: &str, done: u64, _: u64| {
            if stage == "blend" && done >= 1024 && !cancel.is_cancelled() {
                at_cancel.store(copies.load(Ordering::Relaxed), Ordering::Relaxed);
                cancel.cancel();
            }
        };
        let ctl = JobCtl::new(tok, Some(&obs));
        let err = blend_tiles(
            &session,
            &phot,
            Some(&surf),
            &graph,
            &BlendSettings::default(),
            None,
            &mut store,
            &ctl,
        )
        .unwrap_err();
        assert!(err.is_cancelled(), "{err}");
        assert_eq!(
            copies.load(Ordering::Relaxed),
            at_cancel.load(Ordering::Relaxed),
            "finished tiles re-read after the cancel"
        );

        // The cancel contract: finished rows valid, the rest not, and a
        // parent valid exactly when its in-image children are.
        let (nx, ny) = g.tiles(0);
        for ty in 0..ny {
            for tx in 0..nx {
                assert_eq!(store.is_valid(0, tx, ty), ty < 4, "({tx},{ty})");
            }
        }
        for l in 1..g.levels() {
            let (cnx, cny) = g.tiles(l - 1);
            let (pnx, pny) = g.tiles(l);
            for ty in 0..pny {
                for tx in 0..pnx {
                    let kids = (0..4).all(|q| {
                        let (cx, cy) = (2 * tx + (q & 1), 2 * ty + (q >> 1));
                        cx >= cnx || cy >= cny || store.is_valid(l - 1, cx, cy)
                    });
                    assert_eq!(store.is_valid(l, tx, ty), kids, "level {l} ({tx},{ty})");
                }
            }
        }
        assert!(store.valid_count(1) > 0);
        drop(store);
        let _ = std::fs::remove_dir_all(&d);
    }
}
