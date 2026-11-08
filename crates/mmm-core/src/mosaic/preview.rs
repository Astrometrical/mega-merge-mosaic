//! Transient in-memory merge previews of a canvas region.
//!
//! A preview blends one rect of an analysed group into memory: it opens no
//! result store, writes no file and records nothing. At downsample 1 it is
//! the same blend a whole-canvas merge runs over that region, so its pixels
//! equal the stored tiles there.

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

/// Most output pixels one preview patch may hold (2048²).
pub const PREVIEW_MAX_PIXELS: u64 = 4_194_304;

/// The downsample the blend can run a region at: full resolution, or the
/// 1/8 level the blend builds from its summaries ([`crate::blend::blend`]
/// supports no other factor, with or without an ROI).
const L8_DOWNSAMPLE: u32 = 8;

/// Everything a background preview needs, detached from the [`Mosaic`]
/// (made by [`plan_preview`], run by [`run_preview`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPlan {
    /// The group name.
    pub group: String,
    /// The group's session directory.
    pub session_dir: PathBuf,
    /// The blend settings the preview uses.
    pub settings: BlendSettings,
    /// Tile-aligned level-0 canvas rect to blend (`[x0, y0, x1, y1]`).
    pub rect: [u64; 4],
    /// Downsample factor (1 or 8) chosen by [`preview_downsample`].
    pub downsample: u32,
}

/// A previewed region, planar and held in memory only.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPatch {
    /// The tile-aligned level-0 rect it covers.
    pub rect: [u64; 4],
    /// Width in patch pixels.
    pub width: u32,
    /// Height in patch pixels.
    pub height: u32,
    /// Channel count.
    pub channels: u32,
    /// Level-0 pixels per patch pixel (1 or 8).
    pub downsample: u32,
    /// Planar samples: `channels` planes of `height` rows of `width`.
    pub data: Vec<f32>,
}

/// The downsample for a `w`×`h` level-0 rect: 1 when it fits under
/// [`PREVIEW_MAX_PIXELS`], else 8 (the only reduced level the blend
/// supports) when it fits then; `None` when even that is over the cap.
pub fn preview_downsample(w: u64, h: u64) -> Option<u32> {
    [1, L8_DOWNSAMPLE].into_iter().find(|&d| {
        let d = d as u64;
        w.div_ceil(d) * h.div_ceil(d) <= PREVIEW_MAX_PIXELS
    })
}

/// Plan a preview of `rect` (level-0 canvas coordinates) of `group` (owner
/// thread). The rect is aligned outward to tiles
/// ([`TileGeometry::align_rect`]) and the downsample chosen by
/// [`preview_downsample`].
///
/// Refuses (with [`Error::Compute`]) a group that has never been analysed,
/// whose analysis is out of date, or whose aligned rect is empty or too
/// large to preview even at 1/8.
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
    let session_dir = m.group_session_dir(group);
    let (width, height, channels) = Session::open(&session_dir)?.canvas;
    let geometry = TileGeometry {
        width,
        height,
        channels: channels as u32,
        tile: TILE,
    };
    let rect = geometry.align_rect(rect);
    let (w, h) = (
        rect[2].saturating_sub(rect[0]),
        rect[3].saturating_sub(rect[1]),
    );
    if w == 0 || h == 0 {
        return Err(Error::compute("preview region is outside the canvas"));
    }
    let downsample = preview_downsample(w, h)
        .ok_or_else(|| Error::compute("preview region is too large to preview"))?;
    Ok(PreviewPlan {
        group: group.to_string(),
        session_dir,
        settings,
        rect,
        downsample,
    })
}

/// Collects every band into one planar buffer.
#[derive(Default)]
struct PatchSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl RowSink for PatchSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        (self.w, self.h, self.ch) = (w as usize, h as usize, ch as usize);
        self.data = vec![0.0; self.w * self.h * self.ch];
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let band_rows = rows.len() / (self.ch * self.w).max(1);
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

/// Run a planned preview (any thread; no [`Mosaic`] needed). Loads the
/// group's analysis from `plan.session_dir` and blends the rect into
/// memory, cancellably (progress stage `"preview"`). Opens no store and
/// writes nothing.
pub fn run_preview(plan: &PreviewPlan, ctl: &JobCtl) -> Result<PreviewPatch> {
    ctl.check()?;
    let session = Session::open(&plan.session_dir)?;
    let graph = OverlapGraph::load(&session.overlap_graph_path())?;
    let phot = Photometry::load(&session.photometry_path())?;
    let surfaces = if session.surfaces_path().exists() {
        Some(Surfaces::load(&session.surfaces_path())?)
    } else {
        None
    };
    let params = BlendParams {
        downsample: plan.downsample,
        extent: Some(Extent::Canvas),
        roi: Some(plan.rect),
        ..plan.settings.to_params()
    };
    let mut sink = PatchSink::default();
    blend(
        &session,
        &phot,
        surfaces.as_ref(),
        &graph,
        &params,
        &mut CtlSink::new(&mut sink, ctl, "preview"),
    )?;
    Ok(PreviewPatch {
        rect: plan.rect,
        width: sink.w as u32,
        height: sink.h as u32,
        channels: sink.ch as u32,
        downsample: plan.downsample,
        data: sink.data,
    })
}
