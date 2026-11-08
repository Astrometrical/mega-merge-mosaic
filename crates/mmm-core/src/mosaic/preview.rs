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

/// Most output pixels one preview patch may hold (2048²). A rect whose
/// aligned size is larger is blended at 1/8 instead of full resolution (see
/// [`preview_downsample`]).
pub const PREVIEW_MAX_PIXELS: u64 = 4_194_304;

/// The reduced downsample the blend can run a region at: the 1/8 level it
/// builds from its summaries ([`crate::blend::blend`] supports only
/// downsample 1 and 8, with or without an ROI).
const L8_DOWNSAMPLE: u32 = 8;

/// Everything a background preview needs, detached from the [`Mosaic`]
/// (made by [`plan_preview`], run by [`run_preview`]). Making it reads no
/// file; [`run_preview`] aligns the rect and picks the downsample.
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
    /// [`plan_preview`]. A smaller value forces the 1/8 level sooner.
    pub max_pixels: u64,
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
    /// The downsample used: 1 (full resolution) or 8 (level-0 pixels per
    /// patch pixel), see [`preview_downsample`].
    pub downsample: u32,
    /// Planar samples: `channels` planes of `height` rows of `width`.
    pub data: Vec<f32>,
}

/// The downsample for a `w`×`h` level-0 rect under `max_pixels`: 1 when
/// `w*h` fits, else 8 (the only reduced factor the blend supports, so never
/// 2..7) when the 1/8 grid fits; `None` when even that is over the cap.
/// Pass [`PREVIEW_MAX_PIXELS`] for the standard cap.
pub fn preview_downsample(w: u64, h: u64, max_pixels: u64) -> Option<u32> {
    [1, L8_DOWNSAMPLE].into_iter().find(|&d| {
        let d = d as u64;
        w.div_ceil(d) * h.div_ceil(d) <= max_pixels
    })
}

/// Plan a preview of `rect` (level-0 canvas coordinates) of `group` (owner
/// thread). Reads no file: only the in-memory analysis checks run.
///
/// Refuses (with [`Error::Compute`]) a group that has never been analysed
/// or whose analysis is out of date. [`run_preview`] aligns the rect and
/// chooses the downsample, and refuses a rect that is empty or too large.
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
    let downsample = preview_downsample(w, h, plan.max_pixels)
        .ok_or_else(|| Error::compute("preview region is too large to preview"))?;
    let graph = OverlapGraph::load(&session.overlap_graph_path())?;
    let phot = Photometry::load(&session.photometry_path())?;
    let surfaces = if session.surfaces_path().exists() {
        Some(Surfaces::load(&session.surfaces_path())?)
    } else {
        None
    };
    let params = BlendParams {
        downsample,
        extent: Some(Extent::Canvas),
        roi: Some(rect),
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
        rect,
        width: sink.w as u32,
        height: sink.h as u32,
        channels: sink.ch as u32,
        downsample,
        data: sink.data,
    })
}
