//! Producing tiles: a [`RowSink`] that writes a blend into a [`TileStore`],
//! the parent-level (pyramid) rebuild, and [`blend_tiles`], which ties a
//! (possibly ROI) blend, the parents, and job control together.

use crate::blend::{BlendParams, Extent, RowSink, blend};
use crate::error::{Error, Result};
use crate::job::{CtlSink, JobCtl};
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::session::Session;
use crate::settings::BlendSettings;
use crate::surfaces::Surfaces;

use super::TileStore;

/// A [`RowSink`] writing blended rows into the level-0 tiles of a rect.
///
/// Rows are accumulated one tile row (`tile` image rows) at a time; each
/// complete tile row is written to the store (and its tiles marked valid)
/// as soon as its last row arrives. Bands must arrive in order, as the
/// blend delivers them. Parent levels are not touched beyond the initial
/// invalidation — see [`build_parents`].
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
}

impl<'a> TileSink<'a> {
    /// A sink for the tile-aligned `rect` (level-0 coords, as returned by
    /// [`super::TileGeometry::align_rect`]); invalidates the rect's tiles at
    /// every level immediately. Errors if `rect` is empty or not aligned.
    pub fn new(store: &'a mut TileStore, rect: [u64; 4]) -> Result<Self> {
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
    /// the store, then clear the buffer.
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
/// Each parent pixel is the per-channel mean of its (up to four) child
/// pixels that are covered — any channel non-zero — or zero if none are. A
/// parent is written (and marked valid) only if every child tile inside the
/// image is valid; otherwise it is left invalid.
pub fn build_parents(store: &mut TileStore, rect: [u64; 4]) -> Result<()> {
    let g = store.geometry();
    let t = g.tile as usize;
    let nch = g.channels as usize;
    let tile_len = nch * t * t;
    // Children in quadrant order (dx, dy) = (0,0), (1,0), (0,1), (1,1);
    // `None` = outside the child grid.
    let mut kids: [Option<Vec<f32>>; 4] = Default::default();
    let mut parent = vec![0.0f32; tile_len];
    let mut sums = vec![0.0f32; nch];
    for level in 1..g.levels() {
        let (cnx, cny) = g.tiles(level - 1);
        for (tx, ty) in g.tiles_in_rect(level, rect) {
            let mut all_valid = true;
            for (q, kid) in kids.iter_mut().enumerate() {
                let (cx, cy) = (2 * tx + (q as u32 & 1), 2 * ty + (q as u32 >> 1));
                if cx >= cnx || cy >= cny {
                    *kid = None;
                    continue;
                }
                let buf = kid.get_or_insert_with(|| vec![0.0; tile_len]);
                if !store.read_tile(level - 1, cx, cy, buf) {
                    all_valid = false;
                    break;
                }
            }
            if !all_valid {
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
            store.write_tile(level, tx, ty, &parent)?;
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
/// whole-canvas blend. Progress is reported as stage `"blend"` (rows of the
/// rect) and then `("pyramid", 1, 1)` once the parents are built.
///
/// The rect's tiles are invalidated (at every level) before blending, and
/// each level-0 tile row is marked valid as soon as it is written. On any
/// error — including cancellation through `ctl` — finished work is kept:
/// level-0 tiles the run did not fully write are invalidated (they stay
/// stale), then the parents over the rect are rebuilt, so a parent is valid
/// exactly when all its in-image children are; then the error is returned.
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
    let blended = (|| -> Result<()> {
        let mut tiles = TileSink::new(store, aligned)?;
        let res = blend(
            session,
            phot,
            surfaces,
            graph,
            &params,
            &mut CtlSink::new(&mut tiles, ctl, "blend"),
        );
        written = tiles.rows_written();
        res
    })();
    if let Err(e) = blended {
        keep_finished(store, aligned, written);
        return Err(e);
    }
    // Any error from here on leaves the store consistent: every level-0
    // tile is written, and a parent not yet rebuilt is still invalid.
    build_parents(store, aligned)?;
    ctl.report("pyramid", 1, 1)?;
    Ok(aligned)
}

/// After a failed blend of `rect`: invalidate the level-0 tiles below the
/// first `written` rows (not fully written by this run), then rebuild the
/// parents over `rect` from whatever is valid.
fn keep_finished(store: &mut TileStore, rect: [u64; 4], written: u64) {
    let g = store.geometry();
    let unfinished = [rect[0], (rect[1] + written).min(rect[3]), rect[2], rect[3]];
    for (tx, ty) in g.tiles_in_rect(0, unfinished) {
        let idx = store.raw.layout.index(0, tx, ty);
        store.raw.invalidate(idx);
    }
    // `build_parents` only fails on a tile-length mismatch, impossible here;
    // should it fail, parents it did not reach keep the invalidation
    // `TileSink::new` gave them.
    let _ = build_parents(store, rect);
}
