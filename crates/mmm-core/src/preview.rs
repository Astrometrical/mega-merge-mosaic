//! Low-resolution panel previews placed in the shared reference frame, for
//! a Plan view that shows every panel before (and independently of)
//! analysis.
//!
//! A [`Preview`] is a small planar, linear image (zero = no data) of one
//! panel together with its extent in shared-frame canvas pixels
//! ([`Preview::rect`]). Canvas pixel `i` spans `[i, i + 1)`; for a solved
//! frame this is the frame's FITS coordinate, which is exactly where the
//! analysis reprojection puts content (the half-pixel law in [`crate::align`]).
//!
//! - **Aligned frame** (full-canvas registered panels): the covered bbox is
//!   found in a first streaming pass over channel 0, then the covered pixels
//!   (all channels nonzero) are block-averaged into at most `max_side`
//!   pixels on the longer side. Rows are streamed; the canvas is never
//!   materialised.
//! - **Solved frame** (raw panels with their own WCS): the panel's footprint
//!   is forward-mapped into the frame, and every preview pixel is filled by
//!   the inverse mapping (frame → sky → panel) with a bilinear sample of a
//!   block-averaged copy of the panel. A low-resolution warp for display,
//!   not the analysis reprojection.
//!
//! [`run_previews`] builds (or loads from `<session>/previews/`) the
//! previews of every panel on a bounded pool, delivering each as it
//! finishes; it follows the plan / run split of [`crate::mosaic`]
//! ([`plan_previews`] on the owner thread, [`run_previews`] on a worker).
//!
//! Cache files (`.mmmp`): a little-endian `u32` header length, a JSON
//! header, then the planar data as raw little-endian `f32`. Writes are
//! atomic (temporary file, then rename).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::align::{MosaicFrame, boundary_samples};
use crate::analyze::InputSelect;
use crate::formats::InputPanel;
use crate::fsutil::write_atomic;
use crate::job::JobCtl;
use crate::mosaic::{FRAME_FILE, Fingerprint, Mosaic, fingerprint};
use crate::panel_reader::PanelReader;
use crate::reference::{self, ReferenceFrame};
use crate::{Error, Result};

/// Longest side, in pixels, of the previews [`run_previews`] builds.
pub const PREVIEW_MAX_SIDE: u32 = 512;
/// Directory, inside the session directory, holding cached previews.
pub const PREVIEWS_DIR: &str = "previews";

/// A low-resolution preview of one panel, placed in the shared frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// The panel file.
    pub path: PathBuf,
    /// Extent of the preview in shared-frame canvas pixels,
    /// `[x0, y0, x1, y1]`; preview pixel `(i, j)` covers
    /// `x0 + i·(x1 − x0)/width ..` and likewise in y.
    pub rect: [f64; 4],
    /// Preview width in pixels.
    pub width: u32,
    /// Preview height in pixels.
    pub height: u32,
    /// Channel count (the panel's).
    pub channels: u32,
    /// Planar linear samples, `channels × height × width`; zero is no data.
    pub data: Vec<f32>,
}

/// Build the preview of the panel at `path` in `frame`, at most `max_side`
/// pixels on the longer side (module docs for the two methods). Checks
/// cancellation once per preview row.
///
/// Errors when the panel cannot be read, does not fit an aligned frame's
/// canvas, has no covered pixels, or (solved frame) carries no usable
/// astrometric solution.
pub fn build_preview(
    path: &Path,
    frame: &ReferenceFrame,
    max_side: u32,
    ctl: &JobCtl,
) -> Result<Preview> {
    let max_side = u64::from(max_side.max(1));
    let p = match frame {
        ReferenceFrame::Aligned { width, height, .. } => {
            aligned_preview(path, (*width, *height), max_side, ctl)?
        }
        ReferenceFrame::Solved { frame } => solved_preview(path, frame, max_side, ctl)?,
    };
    #[cfg(test)]
    test_hooks::built(path);
    Ok(p)
}

/// Whether a pixel holds data: every channel finite and nonzero (zero in
/// all channels is the no-data sentinel; a partial zero is treated as
/// uncovered, as the analyze summaries do).
#[inline]
fn covered(v: f32) -> bool {
    v != 0.0 && v.is_finite()
}

/// All channels of canvas row `y`, or the reader's latched error.
fn channel_rows<'r>(
    reader: &'r PanelReader,
    path: &Path,
    y: u64,
    nch: u64,
) -> Result<Vec<&'r [f32]>> {
    (0..nch)
        .map(|c| match reader.row(c, y) {
            Some((x0, row)) => {
                debug_assert_eq!(x0, 0, "a full-canvas panel covers its own width");
                Ok(row)
            }
            None => Err(reader
                .backing_error()
                .unwrap_or_else(|| Error::format(path, format!("row {y} could not be read")))),
        })
        .collect()
}

/// Block-mean accumulator for one row of blocks: per-channel sums and the
/// covered-pixel count of each block.
struct BlockRow {
    sums: Vec<f64>,
    counts: Vec<u32>,
    cols: usize,
}

impl BlockRow {
    fn new(cols: usize, nch: usize) -> Self {
        BlockRow {
            sums: vec![0.0; nch * cols],
            counts: vec![0; cols],
            cols,
        }
    }

    /// Accumulate the covered pixels of `rows` (one slice per channel, the
    /// same span) into `block`-px columns.
    fn add(&mut self, rows: &[&[f32]], block: usize) {
        let n = rows[0].len();
        for i in 0..n {
            if rows.iter().all(|r| covered(r[i])) {
                let b = i / block;
                self.counts[b] += 1;
                for (c, r) in rows.iter().enumerate() {
                    self.sums[c * self.cols + b] += f64::from(r[i]);
                }
            }
        }
    }

    /// Write the means into row `j` of planar `out` (`rows` rows high) and
    /// reset for the next row of blocks.
    fn flush(&mut self, out: &mut [f32], j: usize, rows: usize) {
        let plane = self.cols * rows;
        for b in 0..self.cols {
            let n = self.counts[b];
            if n > 0 {
                for c in 0..self.sums.len() / self.cols {
                    out[c * plane + j * self.cols + b] =
                        (self.sums[c * self.cols + b] / f64::from(n)) as f32;
                }
            }
        }
        self.sums.fill(0.0);
        self.counts.fill(0);
    }
}

/// Aligned full-canvas panel: covered bbox (channel 0, streamed), then a
/// block mean of the covered pixels inside it.
fn aligned_preview(
    path: &Path,
    canvas: (u64, u64),
    max_side: u64,
    ctl: &JobCtl,
) -> Result<Preview> {
    let reader = PanelReader::open_file(path)?;
    let (w, h, nch) = reader.canvas();
    if (w, h) != canvas {
        return Err(Error::format(
            path,
            format!(
                "panel is {w}x{h} but the aligned frame's canvas is {}x{}",
                canvas.0, canvas.1
            ),
        ));
    }
    reader.advise_sequential();

    // Pass 1: the bbox of covered channel-0 samples (a superset of the
    // covered pixels), sampling rows at about half the block size a
    // full-canvas bbox would need.
    let stride = (w.max(h).div_ceil(max_side) / 2).max(1);
    let Some([x0, y0, x1, y1]) = covered_bbox(&reader, path, stride, ctl)? else {
        return Err(Error::format(path, "panel has no covered pixels"));
    };

    // Pass 2: block means over the bbox, one row of blocks at a time.
    let block = (x1 - x0).max(y1 - y0).div_ceil(max_side).max(1);
    let pw = (x1 - x0).div_ceil(block) as usize;
    let ph = (y1 - y0).div_ceil(block) as usize;
    let mut data = vec![0f32; nch as usize * pw * ph];
    let mut acc = BlockRow::new(pw, nch as usize);
    for j in 0..ph {
        ctl.check()?;
        let ys = y0 + j as u64 * block;
        for y in ys..(ys + block).min(y1) {
            let rows = channel_rows(&reader, path, y, nch)?;
            let span: Vec<&[f32]> = rows.iter().map(|r| &r[x0 as usize..x1 as usize]).collect();
            acc.add(&span, block as usize);
        }
        acc.flush(&mut data, j, ph);
    }
    if let Some(e) = reader.backing_error() {
        return Err(e);
    }
    Ok(Preview {
        path: path.to_path_buf(),
        rect: [
            x0 as f64,
            y0 as f64,
            (x0 + pw as u64 * block) as f64,
            (y0 + ph as u64 * block) as f64,
        ],
        width: pw as u32,
        height: ph as u32,
        channels: nch as u32,
        data,
    })
}

/// Canvas rows of pass 1 between cancellation checks.
const BBOX_CHECK_ROWS: usize = 64;

/// `[first, last + 1)` of the covered channel-0 samples of row `y`.
fn row_extent(reader: &PanelReader, path: &Path, y: u64) -> Result<Option<(u64, u64)>> {
    let row = channel_rows(reader, path, y, 1)?[0];
    Ok(row.iter().position(|v| covered(*v)).map(|first| {
        let last = row.iter().rposition(|v| covered(*v)).unwrap_or(first);
        (first as u64, last as u64 + 1)
    }))
}

/// The bbox `[x0, y0, x1, y1]` (exclusive) of the covered channel-0
/// samples of a full-canvas panel, `None` when there are none.
///
/// Reads every `stride`-th row, then refines exactly where the extremes can
/// hide: the rows between the first hit and the miss above it, between the
/// last hit and the miss below it, and within `stride` rows of the sampled
/// rows reaching furthest left and right. Exact for a convex footprint (a
/// registered panel is a reprojected rectangle): its edges are convex
/// functions of the row, so each extreme lies within one stride of the
/// sampled extreme. A footprint thinner than the stride that no sampled
/// row meets falls back to a full scan.
fn covered_bbox(
    reader: &PanelReader,
    path: &Path,
    stride: u64,
    ctl: &JobCtl,
) -> Result<Option<[u64; 4]>> {
    let h = reader.canvas().1;
    let k = stride.max(1);
    let mut hits: Vec<(u64, u64, u64)> = Vec::new();
    for (n, y) in (0..h).step_by(k as usize).enumerate() {
        if n % BBOX_CHECK_ROWS == 0 {
            ctl.check()?;
        }
        if let Some((a, b)) = row_extent(reader, path, y)? {
            hits.push((y, a, b));
        }
    }
    let (Some(first), Some(last)) = (hits.first().copied(), hits.last().copied()) else {
        return if k == 1 {
            Ok(None)
        } else {
            covered_bbox(reader, path, 1, ctl)
        };
    };
    let left = hits.iter().min_by_key(|t| t.1).expect("non-empty").0;
    let right = hits.iter().max_by_key(|t| t.2).expect("non-empty").0;
    let mut bb = [u64::MAX, u64::MAX, 0, 0];
    let mut add = |y: u64, (a, b): (u64, u64)| {
        bb = [bb[0].min(a), bb[1].min(y), bb[2].max(b), bb[3].max(y + 1)];
    };
    for &(y, a, b) in &hits {
        add(y, (a, b));
    }
    // Unsampled rows that may hold an extreme.
    let near = |y: u64| y.saturating_sub(k - 1)..(y + k).min(h);
    let mut rows: Vec<u64> = (first.0.saturating_sub(k - 1)..first.0)
        .chain(last.0 + 1..(last.0 + k).min(h))
        .chain(near(left))
        .chain(near(right))
        .filter(|y| y % k != 0)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    for (n, &y) in rows.iter().enumerate() {
        if n % BBOX_CHECK_ROWS == 0 {
            ctl.check()?;
        }
        if let Some(ext) = row_extent(reader, path, y)? {
            add(y, ext);
        }
    }
    Ok(Some(bb))
}

/// A block-averaged copy of a whole raw panel (planar, `cols × rows`,
/// covered-pixel means, zero = no data).
struct Reduced {
    data: Vec<f32>,
    cols: usize,
    rows: usize,
    nch: usize,
    block: f64,
}

impl Reduced {
    fn read(path: &Path, block: u64, ctl: &JobCtl) -> Result<Reduced> {
        let reader = PanelReader::open_file(path)?;
        reader.advise_sequential();
        let (w, h, nch) = reader.canvas();
        let cols = w.div_ceil(block) as usize;
        let rows = h.div_ceil(block) as usize;
        let mut data = vec![0f32; nch as usize * cols * rows];
        let mut acc = BlockRow::new(cols, nch as usize);
        for j in 0..rows {
            ctl.check()?;
            let ys = j as u64 * block;
            for y in ys..(ys + block).min(h) {
                acc.add(&channel_rows(&reader, path, y, nch)?, block as usize);
            }
            acc.flush(&mut data, j, rows);
        }
        if let Some(e) = reader.backing_error() {
            return Err(e);
        }
        Ok(Reduced {
            data,
            cols,
            rows,
            nch: nch as usize,
            block: block as f64,
        })
    }

    /// Bilinear sample at PixInsight image coordinate `(sx, sy)` of the raw
    /// panel into `out` (one value per channel), renormalised over the
    /// covered neighbours. False (and `out` untouched) where none is.
    fn sample(&self, sx: f64, sy: f64, out: &mut [f32]) -> bool {
        // Block m's centre is at image coordinate (m + 0.5)·block.
        let u = sx / self.block - 0.5;
        let v = sy / self.block - 0.5;
        let (i0, j0) = (u.floor(), v.floor());
        let (fu, fv) = (u - i0, v - j0);
        let plane = self.cols * self.rows;
        // The covered neighbours (block index, weight); a block's channel-0
        // mean is zero exactly when it holds no covered pixel.
        let mut taps = [(0usize, 0f64); 4];
        let (mut n, mut wsum) = (0usize, 0f64);
        for (dj, wy) in [(0i64, 1.0 - fv), (1, fv)] {
            for (di, wx) in [(0i64, 1.0 - fu), (1, fu)] {
                let (i, j) = (i0 as i64 + di, j0 as i64 + dj);
                let wgt = wx * wy;
                if wgt <= 0.0 || i < 0 || j < 0 || i >= self.cols as i64 || j >= self.rows as i64 {
                    continue;
                }
                let k = j as usize * self.cols + i as usize;
                if self.data[k] != 0.0 {
                    taps[n] = (k, wgt);
                    n += 1;
                    wsum += wgt;
                }
            }
        }
        if wsum <= 0.0 {
            return false;
        }
        for (c, o) in out.iter_mut().enumerate() {
            let acc: f64 = taps[..n]
                .iter()
                .map(|&(k, w)| w * f64::from(self.data[c * plane + k]))
                .sum();
            *o = (acc / wsum) as f32;
        }
        true
    }
}

/// The footprint of a solved panel (`model`) in `frame`'s canvas pixels,
/// `[x0, y0, x1, y1]`: its boundary forward-mapped (pixel → sky → frame).
/// The frame's FITS coordinate is the canvas pixel coordinate (pixel `i`
/// spans `[i, i + 1)`) under the placement law.
fn solved_footprint(
    path: &Path,
    model: &crate::astrometry::WcsModel,
    frame: &MosaicFrame,
) -> Result<[f64; 4]> {
    let flin = frame.linear_wcs();
    let (mut fx0, mut fy0) = (f64::INFINITY, f64::INFINITY);
    let (mut fx1, mut fy1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (px, py) in boundary_samples(model.width as f64, model.height as f64) {
        let (ra, dec) = model.pixel_to_sky(px, py);
        let (x, y) = flin.sky_to_pixel(ra, dec);
        fx0 = fx0.min(x);
        fx1 = fx1.max(x);
        fy0 = fy0.min(y);
        fy1 = fy1.max(y);
    }
    if ![fx0, fx1, fy0, fy1].iter().all(|v| v.is_finite()) || fx1 <= fx0 || fy1 <= fy0 {
        return Err(Error::format(
            path,
            "panel footprint cannot be placed in the reference frame",
        ));
    }
    Ok([fx0, fy0, fx1, fy1])
}

/// Where the panel at `path` lies in `frame`, from its header only (no
/// pixel data): for a solved frame, its WCS footprint in canvas pixels
/// `[x0, y0, x1, y1]` (the extent [`build_preview`] gives it, to within one
/// preview pixel); for an aligned frame `None`, since a full-canvas panel's
/// covered area needs a pixel scan.
pub fn footprint(path: &Path, frame: &ReferenceFrame) -> Result<Option<[f64; 4]>> {
    let ReferenceFrame::Solved { frame } = frame else {
        return Ok(None);
    };
    let panel = InputPanel::open(path)?;
    let model = panel.wcs_model().map_err(|r| Error::format(path, r))?;
    drop(panel);
    model.ensure_grids();
    solved_footprint(path, &model, frame).map(Some)
}

/// Solved raw panel: forward-map its footprint into the frame, then fill
/// each preview pixel by the inverse mapping and a bilinear sample of a
/// block-averaged copy of the panel.
fn solved_preview(
    path: &Path,
    frame: &MosaicFrame,
    max_side: u64,
    ctl: &JobCtl,
) -> Result<Preview> {
    let panel = InputPanel::open(path)?;
    let model = panel.wcs_model().map_err(|r| Error::format(path, r))?;
    if (model.width, model.height) != (panel.width(), panel.height()) {
        return Err(Error::format(
            path,
            format!(
                "solved geometry {}x{} does not match panel {}x{}",
                model.width,
                model.height,
                panel.width(),
                panel.height()
            ),
        ));
    }
    drop(panel);
    model.ensure_grids();
    let flin = frame.linear_wcs();
    let [fx0, fy0, fx1, fy1] = solved_footprint(path, &model, frame)?;
    let step = ((fx1 - fx0).max(fy1 - fy0) / max_side as f64).max(1.0);
    let pw = ((fx1 - fx0) / step).ceil().max(1.0) as usize;
    let ph = ((fy1 - fy0) / step).ceil().max(1.0) as usize;

    // Average source blocks no larger than a preview pixel.
    let lcd = model.linear.cd;
    let panel_scale = (lcd[0][0] * lcd[1][1] - lcd[0][1] * lcd[1][0]).abs().sqrt();
    let block = if panel_scale > 0.0 {
        ((step * frame.scale_deg / panel_scale).floor() as u64).max(1)
    } else {
        1
    };
    let reduced = Reduced::read(path, block, ctl)?;
    let nch = reduced.nch;
    let (sw, sh) = (model.width as f64, model.height as f64);
    let mut data = vec![0f32; nch * pw * ph];
    let mut px = vec![0f32; nch];
    for j in 0..ph {
        ctl.check()?;
        let cy = fy0 + (j as f64 + 0.5) * step;
        for i in 0..pw {
            let cx = fx0 + (i as f64 + 0.5) * step;
            let (ra, dec) = flin.pixel_to_sky(cx, cy);
            let Some((sx, sy)) = model.sky_to_pixel(ra, dec) else {
                continue;
            };
            if !(0.0..=sw).contains(&sx) || !(0.0..=sh).contains(&sy) {
                continue;
            }
            if reduced.sample(sx, sy, &mut px) {
                for (c, v) in px.iter().enumerate() {
                    data[c * pw * ph + j * pw + i] = *v;
                }
            }
        }
    }
    Ok(Preview {
        path: path.to_path_buf(),
        rect: [fx0, fy0, fx0 + pw as f64 * step, fy0 + ph as f64 * step],
        width: pw as u32,
        height: ph as u32,
        channels: nch as u32,
        data,
    })
}

/// The cache file of a panel's preview:
/// `<session_dir>/previews/<16-hex hash of path, length and mtime>.mmmp`.
pub fn cache_path(session_dir: &Path, path: &Path, fp: &Fingerprint) -> PathBuf {
    // FNV-1a 64 (stable across builds, unlike `DefaultHasher`).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Field separator, so concatenations cannot collide.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    feed(path.as_os_str().as_encoded_bytes());
    feed(&fp.len.to_le_bytes());
    feed(&fp.modified_unix_ms.to_le_bytes());
    session_dir
        .join(PREVIEWS_DIR)
        .join(format!("{hash:016x}.mmmp"))
}

/// Format version of `.mmmp` files.
const PREVIEW_FORMAT: u32 = 1;

/// The JSON header of a `.mmmp` file.
#[derive(Serialize, Deserialize)]
struct Header {
    format: u32,
    path: PathBuf,
    rect: [f64; 4],
    width: u32,
    height: u32,
    channels: u32,
    /// The frame the preview was placed in, when written by
    /// [`run_previews`]: a cached preview is only reused in the same frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<ReferenceFrame>,
}

/// Write `p` to `file` atomically (module docs for the format), creating
/// the parent directory.
pub fn save_preview(p: &Preview, file: &Path) -> Result<()> {
    write_preview(p, None, file)
}

fn write_preview(p: &Preview, frame: Option<&ReferenceFrame>, file: &Path) -> Result<()> {
    let n = p.width as usize * p.height as usize * p.channels as usize;
    if p.data.len() != n {
        return Err(Error::format(
            file,
            format!(
                "preview data has {} samples, expected {n} for {}x{}x{}",
                p.data.len(),
                p.width,
                p.height,
                p.channels
            ),
        ));
    }
    let header = serde_json::to_vec(&Header {
        format: PREVIEW_FORMAT,
        path: p.path.clone(),
        rect: p.rect,
        width: p.width,
        height: p.height,
        channels: p.channels,
        frame: frame.cloned(),
    })
    .map_err(|e| Error::format(file, format!("cannot serialise preview header: {e}")))?;
    let mut bytes = Vec::with_capacity(4 + header.len() + 4 * n);
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header);
    for v in &p.data {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    write_atomic(file, &bytes)
}

/// Read a preview written by [`save_preview`] (or cached by
/// [`run_previews`]). Malformed files are refused naming `file`.
pub fn load_preview(file: &Path) -> Result<Preview> {
    read_preview(file).map(|(p, _)| p)
}

fn read_preview(file: &Path) -> Result<(Preview, Option<ReferenceFrame>)> {
    let bytes = std::fs::read(file).map_err(|e| Error::io(file, e))?;
    let bad = |why: String| Error::format(file, format!("not a preview file: {why}"));
    let hlen = bytes
        .get(..4)
        .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
        .ok_or_else(|| bad("too short".into()))?;
    let hbytes = bytes
        .get(4..4 + hlen)
        .ok_or_else(|| bad("truncated header".into()))?;
    let h: Header = serde_json::from_slice(hbytes).map_err(|e| bad(e.to_string()))?;
    if h.format != PREVIEW_FORMAT {
        return Err(bad(format!("format {} is not supported", h.format)));
    }
    let n = h.width as usize * h.height as usize * h.channels as usize;
    let body = &bytes[4 + hlen..];
    if body.len() != 4 * n {
        return Err(bad(format!(
            "{} data bytes, expected {} for {}x{}x{}",
            body.len(),
            4 * n,
            h.width,
            h.height,
            h.channels
        )));
    }
    let data = body
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Ok((
        Preview {
            path: h.path,
            rect: h.rect,
            width: h.width,
            height: h.height,
            channels: h.channels,
            data,
        },
        h.frame,
    ))
}

/// Everything [`run_previews`] needs, detached from the [`Mosaic`] (made by
/// [`plan_previews`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPlan {
    /// The mosaic directory; previews are cached under [`PREVIEWS_DIR`].
    pub session_dir: PathBuf,
    /// The shared reference frame file ([`FRAME_FILE`]).
    pub frame_path: PathBuf,
    /// Every panel, in configuration order, with the fingerprint recorded
    /// at its last analysis. Informational: the worker fingerprints each
    /// file afresh for the cache key, so a stale record is never trusted.
    pub panels: Vec<(PathBuf, Option<Fingerprint>)>,
    /// Input interpretation for deriving the frame when it is missing (the
    /// base [`crate::settings::AnalyzeSettings::input`]).
    pub input: InputSelect,
}

/// Plan the previews of every panel of `m` (owner thread; no file access).
pub fn plan_previews(m: &Mosaic) -> PreviewPlan {
    PreviewPlan {
        session_dir: m.dir.clone(),
        frame_path: m.dir.join(FRAME_FILE),
        panels: m
            .config
            .panels
            .iter()
            .map(|p| (p.path.clone(), p.fingerprint.clone()))
            .collect(),
        input: m.config.analyze.input,
    }
}

/// Most worker threads [`run_previews`] uses (each holds one panel).
const MAX_THREADS: usize = 4;

/// Build or load every planned preview (worker thread; no [`Mosaic`]).
///
/// 1. Derives the reference frame header-only with [`reference::derive`]
///    (the function analysis uses) over the panels that open — never
///    trusting a frame file left by an earlier input set. If that fails and
///    the input is not forced aligned, it derives over the panels with a
///    usable astrometric solution instead, and the others become per-panel
///    errors. Only a frame derived from every panel is saved to
///    [`PreviewPlan::frame_path`], and only when it differs from the file
///    there (atomically). Panels that cannot be opened are per-panel errors.
/// 2. For each panel, loads its cached preview ([`cache_path`], reused only
///    when it was placed in the same frame) or builds it with
///    [`PREVIEW_MAX_SIDE`] and caches it, on a dedicated pool of at most
///    four threads, each holding one panel at a time. When no decode cache
///    directory is set ([`crate::decoded::cache_dir`]), it is pointed at
///    `<session_dir>/decoded`, so compressed XISF panels are decoded to
///    disk once and shared with analysis instead of held in memory.
/// 3. Calls `on_ready` (from a pool thread) as each preview finishes, and
///    reports `("previews", done, total)` after every panel, failed or not.
///
/// Returns the per-panel failures `(path, message)` in plan order; each is
/// also logged. Whole-run failures — cancellation ([`Error::Cancelled`]),
/// a frame that cannot be derived, no pool — are the `Err`.
pub fn run_previews(
    plan: &PreviewPlan,
    ctl: &JobCtl,
    on_ready: &(dyn Fn(Preview) + Sync),
) -> Result<Vec<(PathBuf, String)>> {
    ctl.check()?;
    let total = plan.panels.len() as u64;
    let mut errors: Vec<(usize, PathBuf, String)> = Vec::new();
    let frame = preview_frame(plan, ctl, &mut errors)?;
    if crate::decoded::cache_dir().is_none() {
        crate::decoded::set_cache_dir(plan.session_dir.join(DECODED_DIR));
    }
    let failed: Vec<usize> = errors.iter().map(|e| e.0).collect();
    let done = AtomicU64::new(failed.len() as u64);
    ctl.report("previews", done.load(Ordering::Relaxed), total)?;
    let errors = Mutex::new(errors);
    let todo: Vec<(usize, &PathBuf)> = plan
        .panels
        .iter()
        .enumerate()
        .filter(|(k, _)| !failed.contains(k))
        .map(|(k, (p, _))| (k, p))
        .collect();
    if !todo.is_empty() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(todo.len().min(MAX_THREADS))
            .thread_name(|i| format!("mmm-preview-{i}"))
            .build()
            .map_err(|e| Error::compute(format!("cannot start the preview pool: {e}")))?;
        pool.install(|| {
            todo.par_iter().with_max_len(1).try_for_each(|(k, path)| {
                ctl.check()?;
                match cached_or_build(plan, &frame, path, ctl) {
                    Ok(p) => on_ready(p),
                    Err(e) if e.is_cancelled() => return Err(e),
                    Err(e) => {
                        tracing::warn!("preview of {} failed: {e}", path.display());
                        errors.lock().unwrap_or_else(|e| e.into_inner()).push((
                            *k,
                            (*path).clone(),
                            e.to_string(),
                        ));
                    }
                }
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                ctl.report("previews", d, total)
            })
        })?;
    }
    let mut errors = errors.into_inner().unwrap_or_else(|e| e.into_inner());
    errors.sort_by_key(|e| e.0);
    Ok(errors.into_iter().map(|(_, p, m)| (p, m)).collect())
}

/// Decode cache directory, inside the session directory, that
/// [`run_previews`] uses when none is set (the CLI's convention).
const DECODED_DIR: &str = "decoded";

/// The frame for a preview run (see [`run_previews`] step 1), recording
/// panels left out of it in `errors`.
fn preview_frame(
    plan: &PreviewPlan,
    ctl: &JobCtl,
    errors: &mut Vec<(usize, PathBuf, String)>,
) -> Result<ReferenceFrame> {
    let mut opened: Vec<(usize, &PathBuf, InputPanel)> = Vec::new();
    for (k, (path, _)) in plan.panels.iter().enumerate() {
        ctl.check()?;
        match InputPanel::open(path) {
            Ok(panel) => opened.push((k, path, panel)),
            Err(e) => errors.push((k, path.clone(), e.to_string())),
        }
    }
    if opened.is_empty() {
        return Err(Error::compute(
            "no panel could be opened to derive the reference frame",
        ));
    }
    let paths: Vec<PathBuf> = opened.iter().map(|o| o.1.clone()).collect();
    let (frame, complete) = match reference::derive(&paths, plan.input) {
        Ok(frame) => (frame, errors.is_empty()),
        Err(e) if plan.input != InputSelect::Aligned => {
            // One unsolved panel must not hide every other preview: place
            // the solved ones and report the rest.
            let mut solved: Vec<PathBuf> = Vec::new();
            for (k, path, panel) in &opened {
                match panel.wcs_model() {
                    Ok(_) => solved.push((*path).clone()),
                    Err(reason) => errors.push((
                        *k,
                        (*path).clone(),
                        format!("cannot place the panel: {reason}"),
                    )),
                }
            }
            if solved.is_empty() || solved.len() == paths.len() {
                return Err(e);
            }
            (reference::derive(&solved, plan.input)?, false)
        }
        Err(e) => return Err(e),
    };
    drop(opened);
    if complete && !ReferenceFrame::load(&plan.frame_path).is_ok_and(|on_disk| on_disk == frame) {
        frame.save(&plan.frame_path)?;
    }
    Ok(frame)
}

/// The cached preview of `path` in `frame`, or a freshly built (and
/// cached) one. A failed cache write is logged, not an error.
fn cached_or_build(
    plan: &PreviewPlan,
    frame: &ReferenceFrame,
    path: &Path,
    ctl: &JobCtl,
) -> Result<Preview> {
    let fp = fingerprint(path)?;
    let file = cache_path(&plan.session_dir, path, &fp);
    if let Ok((p, Some(f))) = read_preview(&file)
        && f == *frame
        && p.path == path
    {
        return Ok(p);
    }
    let p = build_preview(path, frame, PREVIEW_MAX_SIDE, ctl)?;
    if let Err(e) = write_preview(&p, Some(frame), &file) {
        tracing::warn!("cannot cache the preview of {}: {e}", path.display());
    }
    Ok(p)
}

/// Test-only count of [`build_preview`] calls per panel path.
#[cfg(test)]
mod test_hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static BUILDS: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    pub(super) fn built(path: &Path) {
        let mut g = BUILDS.lock().unwrap();
        *g.get_or_insert_with(HashMap::new)
            .entry(path.to_path_buf())
            .or_default() += 1;
    }

    pub(super) fn count(path: &Path) -> usize {
        let g = BUILDS.lock().unwrap();
        g.as_ref().and_then(|m| m.get(path)).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::CancelToken;
    use crate::mosaic::MosaicConfig;
    use crate::synth::{write_fits, write_xisf};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-preview-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const W: u64 = 300;
    const H: u64 = 200;

    /// Two-channel aligned canvas: content in x 40..140, y 30..90 with
    /// ch0 = 1 + x/10 + y/100 and ch1 = 2, a fully empty pixel at (41, 31)
    /// and a pixel at (77, 55) with only ch1 zero (so not covered).
    fn aligned_planes() -> Vec<f32> {
        let mut p = vec![0f32; (2 * W * H) as usize];
        for y in 30..90u64 {
            for x in 40..140u64 {
                let i = (y * W + x) as usize;
                p[i] = 1.0 + x as f32 / 10.0 + y as f32 / 100.0;
                p[(W * H) as usize + i] = 2.0;
            }
        }
        let hole = (31 * W + 41) as usize;
        p[hole] = 0.0;
        p[(W * H) as usize + hole] = 0.0;
        p[(W * H) as usize + (55 * W + 77) as usize] = 0.0;
        p
    }

    /// Naive block mean of covered pixels over `s`-px blocks from (40, 30).
    fn naive(planes: &[f32], s: u64, pw: u64, ph: u64) -> Vec<f32> {
        let mut out = vec![0f32; (2 * pw * ph) as usize];
        for by in 0..ph {
            for bx in 0..pw {
                let (mut s0, mut s1, mut n) = (0f64, 0f64, 0u32);
                for y in 30 + by * s..(30 + (by + 1) * s).min(H) {
                    for x in 40 + bx * s..(40 + (bx + 1) * s).min(W) {
                        let i = (y * W + x) as usize;
                        let (a, b) = (planes[i], planes[(W * H) as usize + i]);
                        if a != 0.0 && b != 0.0 {
                            s0 += a as f64;
                            s1 += b as f64;
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    out[(by * pw + bx) as usize] = (s0 / n as f64) as f32;
                    out[(pw * ph + by * pw + bx) as usize] = (s1 / n as f64) as f32;
                }
            }
        }
        out
    }

    fn aligned_frame() -> ReferenceFrame {
        ReferenceFrame::Aligned {
            width: W,
            height: H,
            wcs: None,
        }
    }

    #[test]
    fn aligned_preview_block_mean_and_rect() {
        let dir = tmp("aligned");
        let planes = aligned_planes();
        let x = dir.join("a.xisf");
        write_xisf(&x, W, H, 2, &planes).unwrap();
        let f = dir.join("a.fits");
        write_fits(&f, W, H, 2, &planes, -32, &[]).unwrap();
        let ctl = JobCtl::none();
        // (max_side, block, preview width, preview height)
        for (max_side, s, pw, ph) in [
            (10u32, 10u64, 10u64, 6u64),
            (7, 15, 7, 4),
            (512, 1, 100, 60),
        ] {
            let want = naive(&planes, s, pw, ph);
            for path in [&x, &f] {
                let p = build_preview(path, &aligned_frame(), max_side, &ctl).unwrap();
                assert_eq!(&p.path, path);
                assert_eq!((p.width, p.height, p.channels), (pw as u32, ph as u32, 2));
                assert_eq!(
                    p.rect,
                    [40.0, 30.0, (40 + pw * s) as f64, (30 + ph * s) as f64]
                );
                assert_eq!(p.data.len(), want.len());
                for (k, (a, b)) in p.data.iter().zip(&want).enumerate() {
                    assert!(
                        (a - b).abs() < 1e-5,
                        "{path:?} side {max_side} [{k}]: {a} vs {b}"
                    );
                }
            }
        }
        // A panel of another geometry does not fit the aligned frame.
        let other = dir.join("o.xisf");
        write_xisf(&other, 10, 10, 1, &[0.5; 100]).unwrap();
        let e = build_preview(&other, &aligned_frame(), 64, &ctl).unwrap_err();
        assert!(e.to_string().contains("10x10"), "{e}");
        // An empty panel has nothing to preview.
        let empty = dir.join("e.xisf");
        write_xisf(&empty, W, H, 1, &vec![0.0; (W * H) as usize]).unwrap();
        let e = build_preview(&empty, &aligned_frame(), 64, &ctl).unwrap_err();
        assert!(e.to_string().contains("no covered pixels"), "{e}");
        // Cancellation is observed.
        let tok = CancelToken::new();
        tok.cancel();
        let e = build_preview(&x, &aligned_frame(), 64, &JobCtl::new(tok, None)).unwrap_err();
        assert!(e.is_cancelled());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_file_roundtrip_and_rejects_garbage() {
        let dir = tmp("file");
        let p = Preview {
            path: dir.join("x.xisf"),
            rect: [1.5, 2.0, 30.25, 40.0],
            width: 3,
            height: 2,
            channels: 2,
            data: (0..12).map(|i| i as f32 * 0.5).collect(),
        };
        let file = dir.join("nested").join("p.mmmp");
        save_preview(&p, &file).unwrap();
        assert_eq!(load_preview(&file).unwrap(), p);
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        // Truncated and garbage files are refused naming the file.
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() - 4]).unwrap();
        let e = load_preview(&file).unwrap_err().to_string();
        assert!(e.contains("p.mmmp"), "{e}");
        std::fs::write(&file, b"nope").unwrap();
        assert!(load_preview(&file).is_err());
        // A preview whose data does not match its geometry is not written.
        let bad = Preview {
            data: vec![0.0; 5],
            ..p
        };
        assert!(save_preview(&bad, &dir.join("bad.mmmp")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_path_keys_on_path_and_fingerprint() {
        let s = Path::new("/s");
        let fp = Fingerprint {
            len: 10,
            modified_unix_ms: 5,
        };
        let a = cache_path(s, Path::new("/d/a.xisf"), &fp);
        assert_eq!(a.parent().unwrap(), s.join(PREVIEWS_DIR));
        let name = a.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 16 + ".mmmp".len(), "{name}");
        assert!(name.ends_with(".mmmp"));
        assert!(name[..16].chars().all(|c| c.is_ascii_hexdigit()), "{name}");
        assert_eq!(a, cache_path(s, Path::new("/d/a.xisf"), &fp));
        assert_ne!(a, cache_path(s, Path::new("/d/b.xisf"), &fp));
        let later = Fingerprint {
            modified_unix_ms: 6,
            ..fp.clone()
        };
        assert_ne!(a, cache_path(s, Path::new("/d/a.xisf"), &later));
        let longer = Fingerprint { len: 11, ..fp };
        assert_ne!(a, cache_path(s, Path::new("/d/a.xisf"), &longer));
    }

    #[test]
    fn preview_cache_roundtrip_and_hit() {
        let dir = tmp("cache");
        let planes = aligned_planes();
        let paths: Vec<PathBuf> = (0..2).map(|k| dir.join(format!("p{k}.xisf"))).collect();
        for p in &paths {
            write_xisf(p, W, H, 2, &planes).unwrap();
        }
        let m = Mosaic::create(&dir.join("m"), MosaicConfig::new(paths.clone())).unwrap();
        let plan = plan_previews(&m);
        let collect = || {
            let got = std::sync::Mutex::new(Vec::new());
            let errs =
                run_previews(&plan, &JobCtl::none(), &|p| got.lock().unwrap().push(p)).unwrap();
            assert!(errs.is_empty(), "{errs:?}");
            let mut got = got.into_inner().unwrap();
            got.sort_by(|a, b| a.path.cmp(&b.path));
            got
        };
        let first = collect();
        assert_eq!(first.len(), 2);
        for (p, path) in first.iter().zip(&paths) {
            assert_eq!(&p.path, path);
            assert_eq!(builds(path), 1);
            let file = cache_path(&m.dir, path, &fingerprint(path).unwrap());
            assert_eq!(&load_preview(&file).unwrap(), p);
        }
        // Second run: served from the cache, nothing rebuilt.
        assert_eq!(collect(), first);
        for path in &paths {
            assert_eq!(builds(path), 1, "{path:?} rebuilt");
        }
        // A changed panel misses the cache and is rebuilt.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut changed = planes.clone();
        changed.iter_mut().for_each(|v| *v *= 2.0);
        write_xisf(&paths[0], W, H, 2, &changed).unwrap();
        let third = collect();
        assert_eq!(builds(&paths[0]), 2);
        assert_eq!(builds(&paths[1]), 1);
        assert!((third[0].data[0] - 2.0 * first[0].data[0]).abs() < 1e-5);
        // A stale or foreign frame file is not trusted: the frame is
        // re-derived (unchanged here), the file restored, nothing rebuilt.
        let mut foreign = aligned_frame();
        if let ReferenceFrame::Aligned { wcs, .. } = &mut foreign {
            *wcs = Some(sample_wcs());
        }
        foreign.save(&plan.frame_path).unwrap();
        collect();
        assert_eq!(
            ReferenceFrame::load(&plan.frame_path).unwrap(),
            aligned_frame()
        );
        assert_eq!(builds(&paths[1]), 1);
        // A different derived frame (inputs edited: a first panel carrying a
        // canvas WCS) misses the cache.
        let solved = dir.join("w.xisf");
        crate::synth::write_xisf_solved(
            &solved,
            W,
            H,
            2,
            &planes,
            &crate::synth::SynthWcs {
                crval: [10.0, 20.0],
                refimg: [150.0, 100.0],
                cd: [[-1e-3, 0.0], [0.0, 1e-3]],
            },
        )
        .unwrap();
        let mut edited = plan.clone();
        edited.panels.insert(0, (solved, None));
        let got = std::sync::Mutex::new(0usize);
        let errs = run_previews(&edited, &JobCtl::none(), &|_| *got.lock().unwrap() += 1).unwrap();
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(*got.lock().unwrap(), 3);
        assert!(matches!(
            ReferenceFrame::load(&plan.frame_path).unwrap(),
            ReferenceFrame::Aligned { wcs: Some(_), .. }
        ));
        assert_eq!(builds(&paths[1]), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn sample_wcs() -> crate::astrometry::LinearWcs {
        crate::astrometry::LinearWcs {
            crval: [10.0, 20.0],
            crpix: [150.5, 100.5],
            cd: [[-1e-3, 0.0], [0.0, 1e-3]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        }
    }

    #[test]
    fn strided_bbox_matches_full_scan() {
        let dir = tmp("bbox");
        let ctl = JobCtl::none();
        // A rectangle rotated by ~20°, a nearly flat sliver and a strip
        // thinner than the stride, each on its own canvas.
        type Shape = Box<dyn Fn(f64, f64) -> bool>;
        let shapes: Vec<(&str, Shape)> = vec![
            (
                "rotated",
                Box::new(|x, y| {
                    let (dx, dy) = (x - 150.0, y - 100.0);
                    let (s, c) = 20f64.to_radians().sin_cos();
                    (c * dx + s * dy).abs() < 70.0 && (-s * dx + c * dy).abs() < 40.0
                }),
            ),
            (
                "sliver",
                Box::new(|x, y| {
                    let (dx, dy) = (x - 120.0, y - 77.0);
                    let (s, c) = 3f64.to_radians().sin_cos();
                    (c * dx + s * dy).abs() < 90.0 && (-s * dx + c * dy).abs() < 6.0
                }),
            ),
            (
                "thin",
                Box::new(|x, y| (101.0..103.0).contains(&y) && (20.0..280.0).contains(&x)),
            ),
        ];
        for (name, inside) in shapes {
            let mut p = vec![0f32; (W * H) as usize];
            for y in 0..H {
                for x in 0..W {
                    if inside(x as f64 + 0.5, y as f64 + 0.5) {
                        p[(y * W + x) as usize] = 0.3;
                    }
                }
            }
            let path = dir.join(format!("{name}.xisf"));
            write_xisf(&path, W, H, 1, &p).unwrap();
            let r = PanelReader::open_file(&path).unwrap();
            let full = covered_bbox(&r, &path, 1, &ctl).unwrap().unwrap();
            for k in 2..=30 {
                assert_eq!(
                    covered_bbox(&r, &path, k, &ctl).unwrap(),
                    Some(full),
                    "{name} stride {k}"
                );
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// How many times `path`'s preview was built (test counter).
    fn builds(path: &Path) -> usize {
        test_hooks::count(path)
    }
}
