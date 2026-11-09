//! Transient in-memory merge previews of a canvas region.
//!
//! A preview blends one rect of an analysed group into memory: it opens no
//! result store, writes no result, provenance or `mosaic.json`, and records
//! nothing. (Reading panels at full resolution may fill the decoded-panel
//! cache, as any panel read does: [`crate::decoded`].)
//!
//! How the rect is blended depends on its tile-aligned size
//! ([`preview_scale`]):
//!
//! - **Up to [`PREVIEW_MAX_PIXELS`]**: at full resolution, the same blend a
//!   whole-canvas merge runs over that region, so its pixels equal the
//!   stored tiles there.
//! - **Up to [`PREVIEW_FULL_RES_MAX_PIXELS`]**: at full resolution in the
//!   merge mode chosen, box-averaged as the bands stream in by the smallest
//!   integer factor that brings the patch under [`PREVIEW_MAX_PIXELS`].
//!   Only pixels with data are averaged (zero in every channel is no data),
//!   so a block with none stays zero.
//! - **Larger**: coarsely, from the 1/8 summaries ([`crate::blend::blend`]
//!   at downsample 8), which feather the summary means in every merge mode:
//!   fast, but the patch does not show the merge mode
//!   ([`PreviewPatch::coarse`]).

use std::path::PathBuf;

use super::Mosaic;
use crate::blend::{BlendParams, Extent, RowSink, blend};
use crate::error::{Error, Result};
use crate::job::{CtlSink, JobCtl};
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::session::Session;
use crate::settings::BlendSettings;
use crate::surfaces::Surfaces;
use crate::tiles::{TILE, TileGeometry};

/// Most output pixels one preview patch may hold (2048²). A rect whose
/// tile-aligned size is larger is blended at full resolution and
/// box-averaged by the smallest integer factor that fits under it, up to
/// [`PREVIEW_FULL_RES_MAX_PIXELS`]; beyond that, coarsely at 1/8 (see
/// [`preview_scale`]).
pub const PREVIEW_MAX_PIXELS: u64 = 4_194_304;

/// Most level-0 pixels of a tile-aligned rect (32 MP) a preview blends at
/// full resolution, so that it shows the merge mode. A larger rect is
/// blended from the 1/8 summaries for speed, and the patch is
/// [`PreviewPatch::coarse`] (see [`preview_scale`]).
pub const PREVIEW_FULL_RES_MAX_PIXELS: u64 = 33_554_432;

/// The downsample of the coarse path: the 1/8 level the blend builds from
/// its summaries ([`crate::blend::blend`] supports only downsample 1 and 8,
/// with or without an ROI).
const L8_DOWNSAMPLE: u32 = 8;

/// Everything a background preview needs, detached from the [`Mosaic`]
/// (made by [`plan_preview`], run by [`run_preview`]). Making it reads no
/// file; [`run_preview`] aligns the rect and picks how to blend it
/// ([`preview_scale`] under the two caps here).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPlan {
    /// The group name.
    pub group: String,
    /// The group's session directory.
    pub session_dir: PathBuf,
    /// The blend settings the preview uses.
    pub settings: BlendSettings,
    /// The REQUESTED level-0 canvas rect (`[x0, y0, x1, y1]`); the run
    /// aligns it outward to tiles ([`TileGeometry::align_rect`]).
    pub rect: [u64; 4],
    /// Output-pixel cap for the patch: [`PREVIEW_MAX_PIXELS`] from
    /// [`plan_preview`]. A smaller value box-averages sooner.
    pub max_pixels: u64,
    /// Most aligned level-0 pixels blended at full resolution:
    /// [`PREVIEW_FULL_RES_MAX_PIXELS`] from [`plan_preview`]. A smaller
    /// value takes the coarse 1/8 path sooner.
    pub full_res_max_pixels: u64,
}

/// A previewed region, planar and held in memory only.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPatch {
    /// The tile-aligned level-0 rect actually blended.
    pub rect: [u64; 4],
    /// Width in patch pixels.
    pub width: u32,
    /// Height in patch pixels.
    pub height: u32,
    /// Channel count.
    pub channels: u32,
    /// Level-0 pixels per patch pixel along each axis: 1 (full resolution),
    /// the box-average factor of a full-resolution blend, or 8 on the coarse
    /// path (see [`preview_scale`]). The last column and row may cover fewer.
    pub downsample: u32,
    /// Blended from the 1/8 summaries, which feather in every merge mode:
    /// the patch does not show the merge mode (a rect over
    /// [`PREVIEW_FULL_RES_MAX_PIXELS`]).
    pub coarse: bool,
    /// Planar samples: `channels` planes of `height` rows of `width`.
    pub data: Vec<f32>,
}

/// How [`run_preview`] makes the patch of a tile-aligned rect
/// ([`preview_scale`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewScale {
    /// Level-0 pixels per patch pixel along each axis
    /// ([`PreviewPatch::downsample`]).
    pub downsample: u32,
    /// The coarse 1/8 path ([`PreviewPatch::coarse`]); otherwise a
    /// full-resolution blend, box-averaged by `downsample` when it is over 1.
    pub coarse: bool,
}

/// How a `w`×`h` tile-aligned level-0 rect is previewed (pass
/// [`PREVIEW_MAX_PIXELS`] and [`PREVIEW_FULL_RES_MAX_PIXELS`] for the
/// standard caps):
///
/// - `w*h` up to `full_res_max_pixels`: blended at full resolution, then
///   box-averaged by the smallest integer factor `d` (1 when it fits)
///   bringing `ceil(w/d) * ceil(h/d)` under `max_pixels`; not coarse.
/// - Larger: the coarse 1/8 path (`d` = 8) when its grid fits under
///   `max_pixels`; `None` (refused) when even that does not.
pub fn preview_scale(
    w: u64,
    h: u64,
    max_pixels: u64,
    full_res_max_pixels: u64,
) -> Option<PreviewScale> {
    if w == 0 || h == 0 || max_pixels == 0 {
        return None;
    }
    let fits = |d: u64| w.div_ceil(d).saturating_mul(h.div_ceil(d)) <= max_pixels;
    let pixels = w.saturating_mul(h);
    if pixels <= full_res_max_pixels {
        // Any factor that fits has d² ≥ w*h / max_pixels: start there.
        let mut d = (pixels / max_pixels).isqrt().max(1);
        while !fits(d) {
            d += 1;
        }
        return Some(PreviewScale {
            downsample: u32::try_from(d).ok()?,
            coarse: false,
        });
    }
    fits(u64::from(L8_DOWNSAMPLE)).then_some(PreviewScale {
        downsample: L8_DOWNSAMPLE,
        coarse: true,
    })
}

/// Plan a preview of `rect` (level-0 canvas coordinates) of `group` (owner
/// thread). Reads no file: only the in-memory analysis checks run.
///
/// Refuses (with [`Error::Compute`]) a group that has never been analysed
/// or whose analysis is out of date. [`run_preview`] aligns the rect and
/// chooses how to blend it, and refuses a rect that is empty or too large.
pub fn plan_preview(
    m: &Mosaic,
    group: &str,
    rect: [u64; 4],
    settings: BlendSettings,
) -> Result<PreviewPlan> {
    if !m.config.group_dirs.contains_key(group) {
        return Err(Error::compute(format!(
            "group '{group}' has not been analyzed yet — analyze first"
        )));
    }
    if m.config.needs_analyze.contains(group) {
        return Err(Error::compute(format!(
            "analyze settings for group '{group}' changed since it was analyzed — analyze first"
        )));
    }
    Ok(PreviewPlan {
        group: group.to_string(),
        session_dir: m.group_session_dir(group),
        settings,
        rect,
        max_pixels: PREVIEW_MAX_PIXELS,
        full_res_max_pixels: PREVIEW_FULL_RES_MAX_PIXELS,
    })
}

/// Collects every band of the blend's output into one planar buffer,
/// box-averaging `factor` × `factor` blocks as the bands stream in (factor
/// 1 copies). Only pixels with data (some channel non-zero) are averaged,
/// every channel of them; a block with none stays zero, so the no-data rule
/// holds in the patch.
struct PatchSink {
    /// Output pixels per patch pixel along each axis.
    factor: usize,
    /// The blend's output width.
    src_w: usize,
    /// Patch width, height and channels.
    w: usize,
    h: usize,
    ch: usize,
    /// Planar sums (factor > 1, until `finish` divides) or samples.
    data: Vec<f32>,
    /// Pixels with data summed into each patch pixel (factor > 1).
    counts: Vec<u32>,
}

impl PatchSink {
    /// A sink averaging `factor` × `factor` blocks (at least 1).
    fn new(factor: u32) -> Self {
        PatchSink {
            factor: factor.max(1) as usize,
            src_w: 0,
            w: 0,
            h: 0,
            ch: 0,
            data: Vec::new(),
            counts: Vec::new(),
        }
    }
}

impl RowSink for PatchSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        let f = self.factor;
        self.src_w = w as usize;
        (self.w, self.h, self.ch) = (
            (w as usize).div_ceil(f),
            (h as usize).div_ceil(f),
            ch as usize,
        );
        self.data = vec![0.0; self.w * self.h * self.ch];
        if f > 1 {
            self.counts = vec![0; self.w * self.h];
        }
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let (sw, ch, f) = (self.src_w, self.ch, self.factor);
        let band_rows = rows.len() / (ch * sw).max(1);
        if f == 1 {
            for c in 0..ch {
                for r in 0..band_rows {
                    let src = &rows[(c * band_rows + r) * sw..][..sw];
                    let off = (c * self.h + y0 as usize + r) * sw;
                    self.data[off..off + sw].copy_from_slice(src);
                }
            }
            return Ok(());
        }
        let plane = self.w * self.h;
        for r in 0..band_rows {
            let row = (y0 as usize + r) / f * self.w;
            for x in 0..sw {
                let at = |c: usize| rows[(c * band_rows + r) * sw + x];
                if (0..ch).all(|c| at(c) == 0.0) {
                    continue;
                }
                let o = row + x / f;
                self.counts[o] += 1;
                for c in 0..ch {
                    self.data[c * plane + o] += at(c);
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let plane = self.w * self.h;
        for (o, &n) in self.counts.iter().enumerate() {
            if n > 1 {
                for c in 0..self.ch {
                    self.data[c * plane + o] /= n as f32;
                }
            }
        }
        Ok(())
    }
}

/// Run a planned preview (any thread; no [`Mosaic`] needed). Loads the
/// group's analysis from `plan.session_dir` and blends the rect into
/// memory as [`preview_scale`] says (at full resolution, box-averaged when
/// over the patch cap, or coarsely at 1/8 beyond the full-resolution cap),
/// cancellably (progress stage `"preview"`). Opens no store and writes no
/// result (panel reads may fill the decoded-panel cache).
pub fn run_preview(plan: &PreviewPlan, ctl: &JobCtl) -> Result<PreviewPatch> {
    ctl.check()?;
    let session = Session::open(&plan.session_dir)?;
    let (width, height, channels) = session.canvas;
    let geometry = TileGeometry {
        width,
        height,
        channels: channels as u32,
        tile: TILE,
    };
    let rect = geometry.align_rect(plan.rect);
    let (w, h) = (
        rect[2].saturating_sub(rect[0]),
        rect[3].saturating_sub(rect[1]),
    );
    if w == 0 || h == 0 {
        return Err(Error::compute("preview region is outside the canvas"));
    }
    let scale = preview_scale(w, h, plan.max_pixels, plan.full_res_max_pixels)
        .ok_or_else(|| Error::compute("preview region is too large to preview"))?;
    let graph = OverlapGraph::load(&session.overlap_graph_path())?;
    let phot = Photometry::load(&session.photometry_path())?;
    let surfaces = if session.surfaces_path().exists() {
        Some(Surfaces::load(&session.surfaces_path())?)
    } else {
        None
    };
    // The coarse path blends at 1/8 itself; otherwise at full resolution,
    // the sink averaging the blocks.
    let (blend_downsample, factor) = if scale.coarse {
        (L8_DOWNSAMPLE, 1)
    } else {
        (1, scale.downsample)
    };
    let params = BlendParams {
        downsample: blend_downsample,
        extent: Some(Extent::Canvas),
        roi: Some(rect),
        ..plan.settings.to_params()
    };
    let mut sink = PatchSink::new(factor);
    blend(
        &session,
        &phot,
        surfaces.as_ref(),
        &graph,
        &params,
        &mut CtlSink::new(&mut sink, ctl, "preview"),
    )?;
    Ok(PreviewPatch {
        rect,
        width: sink.w as u32,
        height: sink.h as u32,
        channels: sink.ch as u32,
        downsample: scale.downsample,
        coarse: scale.coarse,
        data: sink.data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Planar band rows of `ch` planes, each `rows` × `w`, from per-pixel
    /// samples `px[r][x] = [c0, c1, …]`.
    fn planar(px: &[Vec<[f32; 2]>]) -> Vec<f32> {
        let mut out = Vec::new();
        for c in 0..2 {
            for row in px {
                out.extend(row.iter().map(|p| p[c]));
            }
        }
        out
    }

    #[test]
    fn decimation_ignores_no_data() {
        // A 5 × 3 two-channel output averaged 2 × 2: a 3 × 2 patch, the
        // last column and row partial blocks.
        let mut sink = PatchSink::new(2);
        sink.begin(5, 3, 2).unwrap();
        let z = [0.0, 0.0];
        // Row by row, as bands of one and two rows.
        sink.band(0, &planar(&[vec![[1.0, 2.0], z, z, z, [5.0, 1.0]]]))
            .unwrap();
        sink.band(
            1,
            &planar(&[
                // A pixel with data has a channel at zero: it counts.
                vec![[3.0, 0.0], z, z, z, [7.0, 3.0]],
                vec![z, z, [4.0, 6.0], z, z],
            ]),
        )
        .unwrap();
        sink.finish().unwrap();
        assert_eq!((sink.w, sink.h, sink.ch), (3, 2, 2));
        let at = |c: usize, x: usize, y: usize| sink.data[(c * 2 + y) * 3 + x];
        // Block (0, 0): two pixels with data, two without.
        assert_eq!((at(0, 0, 0), at(1, 0, 0)), (2.0, 1.0));
        // Block (1, 0): no data at all: stays zero.
        assert_eq!((at(0, 1, 0), at(1, 1, 0)), (0.0, 0.0));
        // Block (2, 0): the partial last column.
        assert_eq!((at(0, 2, 0), at(1, 2, 0)), (6.0, 2.0));
        // The partial last row: one pixel with data in (1, 1).
        assert_eq!((at(0, 1, 1), at(1, 1, 1)), (4.0, 6.0));
        assert_eq!((at(0, 0, 1), at(1, 0, 1)), (0.0, 0.0));
        assert_eq!((at(0, 2, 1), at(1, 2, 1)), (0.0, 0.0));

        // Factor 1 copies.
        let mut sink = PatchSink::new(1);
        sink.begin(2, 1, 2).unwrap();
        sink.band(0, &planar(&[vec![[1.0, 2.0], z]])).unwrap();
        sink.finish().unwrap();
        assert_eq!(sink.data, [1.0, 0.0, 2.0, 0.0]);
    }
}
