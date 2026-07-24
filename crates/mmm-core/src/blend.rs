//! Feather blend: stream the photometrically-corrected union of panels as
//! row bands to a [`RowSink`].
//!
//! Output canvas = union of panel content bboxes (cropped — never the full
//! mosaic canvas). Per pixel and panel, the weight is
//! `max(clamp(d_px/feather, 0, 1), MIN_WEIGHT)` where `d_px` is 8× the
//! bilinear sample of the panel's L8 chamfer distance map at `(x/8, y/8)` —
//! pixels near a panel's rim get tiny weight so interpolation garbage loses to
//! any overlapping partner, while single-coverage rims still survive
//! normalization. A pixel is covered ⟺ all channels are nonzero; uncovered
//! pixels contribute nothing and fully uncovered output pixels are 0.
//!
//! Bands are computed rayon-parallel (over rows within a band) but delivered
//! to the sink strictly in order. `downsample == 8` blends from the L8 summary
//! means over fully-covered cells instead of touching the full-res mmaps —
//! seconds instead of minutes, for previews.

use rayon::prelude::*;

use crate::formats::xisf::XisfPanel;
use crate::overlap::{OverlapGraph, distance_map};
use crate::photometry::Photometry;
use crate::session::Session;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::Surfaces;
use crate::{Error, Result};

/// Weight floor for covered pixels: rim pixels survive normalization when
/// alone but lose ~completely to any overlapping partner.
pub const MIN_WEIGHT: f32 = 1e-4;

/// Parameters of the feather blend.
#[derive(Debug, Clone)]
pub struct BlendParams {
    /// Feather ramp length in canvas pixels.
    pub feather_px: f32,
    /// 1 = full resolution, 8 = blend from the L8 summaries (preview).
    pub downsample: u32,
    /// Output rows per band delivered to the sink.
    pub band_rows: usize,
}

impl Default for BlendParams {
    fn default() -> Self {
        Self { feather_px: 256.0, downsample: 1, band_rows: 256 }
    }
}

/// Streaming consumer of blended rows (planar per band).
pub trait RowSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()>;
    /// One band starting at output row `y0`; `rows` is planar:
    /// `ch` planes × `band_rows` × `w`, row-major.
    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()>;
    fn finish(&mut self) -> Result<()>;
}

/// Union of the panel content bboxes: `[x0, y0, x1, y1]`, exclusive.
/// Errors if the session has no panel with content.
pub fn union_bbox(session: &Session) -> Result<[u64; 4]> {
    let mut it = session.panels.iter().filter(|p| p.bbox[2] > p.bbox[0] && p.bbox[3] > p.bbox[1]);
    let first = it
        .next()
        .ok_or_else(|| Error::format(&session.dir, "session has no panels with content"))?;
    Ok(it.fold(first.bbox, |acc, p| {
        [
            acc[0].min(p.bbox[0]),
            acc[1].min(p.bbox[1]),
            acc[2].max(p.bbox[2]),
            acc[3].max(p.bbox[3]),
        ]
    }))
}

/// One panel's blend-time context: correction, geometry, and feather source.
struct PanelPrep {
    /// Content bbox in canvas pixels, `[x0, y0, x1, y1]` exclusive.
    bbox: [u64; 4],
    /// Per-channel photometric gain/offset (identity when absent).
    gains: Vec<f32>,
    offsets: Vec<f32>,
    /// Per-channel residual surface coefficients, padded to the 6 terms
    /// `1, x, y, x², xy, y²` (normalized canvas coords); all-zero when the
    /// session has no surfaces.
    surf: Vec<[f64; 6]>,
    summary: L8Summary,
    /// Chamfer distance to the nearest not-fully-covered L8 cell, in cells.
    dist: Vec<f32>,
}

impl PanelPrep {
    /// Row constants for the surface at normalized row coordinate `yn`:
    /// `s(xn) = a + xn·(b + xn·c)` per channel (Horner in `xn`).
    #[inline]
    fn surf_row(&self, yn: f64) -> Vec<(f32, f32, f32)> {
        self.surf
            .iter()
            .map(|t| {
                let a = t[0] + (t[2] + t[5] * yn) * yn;
                let b = t[1] + t[4] * yn;
                (a as f32, b as f32, t[3] as f32)
            })
            .collect()
    }
}

/// Load summaries, compute distance maps, and resolve per-panel corrections.
fn prep_panels(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
) -> Result<Vec<PanelPrep>> {
    let ch = session.canvas.2 as usize;
    session
        .panels
        .par_iter()
        .map(|p| {
            let summary = L8Summary::read(&session.summary_path(p.id))?;
            let mut dist = distance_map(&summary.coverage, summary.w8, summary.h8);
            // A panel with no uncovered cell at all yields INFINITY; make it a
            // large finite value so bilinear interpolation cannot produce NaN.
            for d in &mut dist {
                if !d.is_finite() {
                    *d = 1e30;
                }
            }
            let correction = |table: &Vec<Vec<f64>>, default: f64| -> Vec<f32> {
                (0..ch)
                    .map(|c| {
                        table.get(c).and_then(|t| t.get(p.id)).copied().unwrap_or(default) as f32
                    })
                    .collect()
            };
            let surf: Vec<[f64; 6]> = (0..ch)
                .map(|c| {
                    let mut padded = [0.0f64; 6];
                    if let Some(coeffs) =
                        surfaces.and_then(|s| s.coeffs.get(c)).and_then(|t| t.get(p.id))
                    {
                        padded[..coeffs.len()].copy_from_slice(coeffs);
                    }
                    padded
                })
                .collect();
            Ok(PanelPrep {
                bbox: p.bbox,
                gains: correction(&phot.gains, 1.0),
                offsets: correction(&phot.offsets, 0.0),
                surf,
                summary,
                dist,
            })
        })
        .collect()
}

/// Bilinear sample of an L8-grid plane at fractional cell coordinates.
#[inline]
fn bilinear(d: &[f32], w8: u32, h8: u32, gx: f32, gy: f32) -> f32 {
    let (w, h) = (w8 as usize, h8 as usize);
    let gx = gx.clamp(0.0, (w - 1) as f32);
    let gy = gy.clamp(0.0, (h - 1) as f32);
    let x0 = gx as usize;
    let y0 = gy as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = gx - x0 as f32;
    let fy = gy - y0 as f32;
    let top = d[y0 * w + x0] * (1.0 - fx) + d[y0 * w + x1] * fx;
    let bot = d[y1 * w + x0] * (1.0 - fx) + d[y1 * w + x1] * fx;
    top * (1.0 - fy) + bot * fy
}

/// Feather weight from a distance in pixels.
#[inline]
fn weight(d_px: f32, inv_feather: f32) -> f32 {
    (d_px * inv_feather).clamp(0.0, 1.0).max(MIN_WEIGHT)
}

/// Feather-blend the session's panels into `sink`, applying the photometric
/// corrections and (when given) the residual surfaces: `v' = g·v + o + s(x,y)`.
pub fn blend(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    _graph: &OverlapGraph,
    params: &BlendParams,
    sink: &mut dyn RowSink,
) -> Result<()> {
    if session.panels.is_empty() {
        return Err(Error::format(&session.dir, "session has no panels"));
    }
    if params.feather_px.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err(Error::format(
            &session.dir,
            format!("feather_px must be positive, got {}", params.feather_px),
        ));
    }
    match params.downsample {
        1 => blend_full(session, phot, surfaces, params, sink),
        8 => blend_l8(session, phot, surfaces, params, sink),
        d => Err(Error::format(
            &session.dir,
            format!("unsupported downsample {d} (only 1 or 8)"),
        )),
    }
}

/// Full-resolution blend streamed from the panels' mmaps.
fn blend_full(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    params: &BlendParams,
    sink: &mut dyn RowSink,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let preps = prep_panels(session, phot, surfaces)?;
    let panels: Vec<XisfPanel> = session
        .panels
        .par_iter()
        .map(|p| {
            let x = XisfPanel::open(&p.path)?;
            if (x.width(), x.height(), x.channels()) != session.canvas {
                return Err(Error::format(
                    &p.path,
                    format!(
                        "panel geometry {}x{}x{} does not match session canvas {}x{}x{}",
                        x.width(),
                        x.height(),
                        x.channels(),
                        session.canvas.0,
                        session.canvas.1,
                        session.canvas.2
                    ),
                ));
            }
            x.advise_sequential();
            Ok(x)
        })
        .collect::<Result<_>>()?;

    let bbox = union_bbox(session)?;
    let (cx0, cy0) = (bbox[0], bbox[1]);
    let out_w = (bbox[2] - bbox[0]) as usize;
    let out_h = (bbox[3] - bbox[1]) as usize;
    tracing::info!(out_w, out_h, nch, prep_s = t0.elapsed().as_secs_f64(), "blend full-res");

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_block = 1.0 / BLOCK as f32;
    let inv_cw = 1.0f32 / session.canvas.0 as f32;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let mut band = vec![0.0f32; nch * band_rows * out_w];

    for y0 in (0..out_h).step_by(band_rows) {
        let rows_here = band_rows.min(out_h - y0);
        let band_cy0 = cy0 + y0 as u64;
        let band_cy1 = band_cy0 + rows_here as u64;
        let active: Vec<usize> = preps
            .iter()
            .enumerate()
            .filter(|(_, p)| p.bbox[1] < band_cy1 && p.bbox[3] > band_cy0)
            .map(|(i, _)| i)
            .collect();

        // Compute rows in parallel; the collected Vec preserves row order so
        // the sink always receives bands (and rows within them) in order.
        let rows_out: Vec<Vec<f32>> = (0..rows_here)
            .into_par_iter()
            .map(|r| {
                let cy = band_cy0 + r as u64;
                let mut acc = vec![0.0f32; nch * out_w];
                let mut wsum = vec![0.0f32; out_w];
                let gy = cy as f32 * inv_block;
                let mut rows: Vec<&[f32]> = Vec::with_capacity(nch);
                for &pi in &active {
                    let p = &preps[pi];
                    if cy < p.bbox[1] || cy >= p.bbox[3] {
                        continue;
                    }
                    rows.clear();
                    for c in 0..nch as u64 {
                        rows.push(panels[pi].row(c, cy));
                    }
                    // Residual surface, reduced to per-row constants:
                    // s(xn) = a + xn·(b + xn·c) per channel (Horner).
                    let srow = p.surf_row(cy as f64 * inv_ch);
                    let xs = p.bbox[0].max(cx0);
                    let xe = p.bbox[2].min(bbox[2]);
                    for x in xs..xe {
                        let xi = x as usize;
                        if rows.iter().any(|row| row[xi] == 0.0) {
                            continue; // uncovered: any channel zero
                        }
                        let gx = x as f32 * inv_block;
                        let d_px = BLOCK as f32
                            * bilinear(&p.dist, p.summary.w8, p.summary.h8, gx, gy);
                        let wgt = weight(d_px, inv_feather);
                        let o = (x - cx0) as usize;
                        wsum[o] += wgt;
                        let xn = x as f32 * inv_cw;
                        for (c, row) in rows.iter().enumerate() {
                            let (sa, sb, sc) = srow[c];
                            acc[c * out_w + o] += wgt
                                * (row[xi] * p.gains[c]
                                    + p.offsets[c]
                                    + sa
                                    + xn * (sb + xn * sc));
                        }
                    }
                }
                normalize(&mut acc, &wsum, out_w, nch);
                acc
            })
            .collect();

        let bs = &mut band[..nch * rows_here * out_w];
        assemble_band(bs, &rows_out, out_w, nch);
        sink.band(y0 as u64, bs)?;
    }
    sink.finish()?;
    tracing::info!(total_s = t0.elapsed().as_secs_f64(), "blend full-res done");
    Ok(())
}

/// Preview blend from the L8 summary means over fully-covered cells; output
/// geometry is the L8 grid cropped to the union bbox / 8.
fn blend_l8(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    params: &BlendParams,
    sink: &mut dyn RowSink,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let preps = prep_panels(session, phot, surfaces)?;
    let bbox = union_bbox(session)?;
    let (w8, h8) = (preps[0].summary.w8, preps[0].summary.h8);
    let b = BLOCK as u64;
    let gx0 = (bbox[0] / b) as u32;
    let gy0 = (bbox[1] / b) as u32;
    let gx1 = (bbox[2].div_ceil(b) as u32).min(w8);
    let gy1 = (bbox[3].div_ceil(b) as u32).min(h8);
    let out_w = (gx1 - gx0) as usize;
    let out_h = (gy1 - gy0) as usize;
    tracing::info!(out_w, out_h, nch, prep_s = t0.elapsed().as_secs_f64(), "blend from L8");

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_cw = 1.0f64 / session.canvas.0 as f64;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let mut band = vec![0.0f32; nch * band_rows * out_w];

    for y0 in (0..out_h).step_by(band_rows) {
        let rows_here = band_rows.min(out_h - y0);
        let rows_out: Vec<Vec<f32>> = (0..rows_here)
            .into_par_iter()
            .map(|r| {
                let y8 = gy0 + (y0 + r) as u32;
                let mut acc = vec![0.0f32; nch * out_w];
                let mut wsum = vec![0.0f32; out_w];
                // Surfaces are evaluated at cell centers, matching the fit.
                let yn = (y8 as f64 + 0.5) * b as f64 * inv_ch;
                for p in &preps {
                    if y8 as u64 * b >= p.bbox[3] || (y8 as u64 + 1) * b <= p.bbox[1] {
                        continue;
                    }
                    let s = &p.summary;
                    let srow = p.surf_row(yn);
                    for x8 in gx0..gx1 {
                        if s.cov(x8, y8) < 1.0 {
                            continue; // only fully covered cells blend
                        }
                        let d_px =
                            BLOCK as f32 * p.dist[y8 as usize * s.w8 as usize + x8 as usize];
                        let wgt = weight(d_px, inv_feather);
                        let o = (x8 - gx0) as usize;
                        wsum[o] += wgt;
                        let xn = ((x8 as f64 + 0.5) * b as f64 * inv_cw) as f32;
                        for c in 0..nch {
                            let (sa, sb, sc) = srow[c];
                            acc[c * out_w + o] += wgt
                                * (s.cell(c as u32, x8, y8) * p.gains[c]
                                    + p.offsets[c]
                                    + sa
                                    + xn * (sb + xn * sc));
                        }
                    }
                }
                normalize(&mut acc, &wsum, out_w, nch);
                acc
            })
            .collect();

        let bs = &mut band[..nch * rows_here * out_w];
        assemble_band(bs, &rows_out, out_w, nch);
        sink.band(y0 as u64, bs)?;
    }
    sink.finish()?;
    tracing::info!(total_s = t0.elapsed().as_secs_f64(), "blend from L8 done");
    Ok(())
}

/// Divide accumulated `w·v` by `Σw` where covered; uncovered stays 0.
fn normalize(acc: &mut [f32], wsum: &[f32], out_w: usize, nch: usize) {
    for (o, &w) in wsum.iter().enumerate() {
        if w > 0.0 {
            let inv = 1.0 / w;
            for c in 0..nch {
                acc[c * out_w + o] *= inv;
            }
        }
    }
}

/// Repack per-row `[ch × w]` results into the planar band layout
/// `ch planes × band_rows × w`.
fn assemble_band(band: &mut [f32], rows_out: &[Vec<f32>], out_w: usize, nch: usize) {
    let rows_here = rows_out.len();
    for (r, rowv) in rows_out.iter().enumerate() {
        for c in 0..nch {
            band[(c * rows_here + r) * out_w..][..out_w]
                .copy_from_slice(&rowv[c * out_w..][..out_w]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze;
    use crate::synth::write_xisf;
    use std::path::{Path, PathBuf};

    struct MemSink {
        w: usize,
        h: usize,
        ch: usize,
        data: Vec<f32>,
        finished: bool,
    }

    impl MemSink {
        fn new() -> Self {
            Self { w: 0, h: 0, ch: 0, data: Vec::new(), finished: false }
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
            assert_eq!(rows.len() % (self.ch * self.w), 0);
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
            self.finished = true;
            Ok(())
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-blend-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two constant single-channel panels on a 128×64 canvas:
    /// A = 0.2 over x∈[8,80), y∈[8,56); B = 0.4 over x∈[48,120), y∈[16,64).
    /// Union bbox = [8,8,120,64) → output 112×56.
    fn make_panels(dir: &Path) -> (Session, OverlapGraph) {
        let (w, h) = (128u64, 64u64);
        let mut frame = vec![0f32; (w * h) as usize];
        let fill = |frame: &mut [f32], v: f32, x0: u64, y0: u64, x1: u64, y1: u64| {
            for y in y0..y1 {
                for x in x0..x1 {
                    frame[(y * w + x) as usize] = v;
                }
            }
        };
        fill(&mut frame, 0.2, 8, 8, 80, 56);
        let a = dir.join("a.xisf");
        write_xisf(&a, w, h, 1, &frame).unwrap();

        frame.fill(0.0);
        fill(&mut frame, 0.4, 48, 16, 120, 64);
        let b = dir.join("b.xisf");
        write_xisf(&b, w, h, 1, &frame).unwrap();

        let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        (session, graph)
    }

    fn identity_phot(n_panels: usize, ch: usize) -> Photometry {
        Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0; n_panels]; ch],
            offsets: vec![vec![0.0; n_panels]; ch],
        }
    }

    #[test]
    fn union_bbox_covers_all_panels() {
        let dir = tmpdir("bbox");
        let (session, _) = make_panels(&dir);
        assert_eq!(union_bbox(&session).unwrap(), [8, 8, 120, 64]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn feather_blend_two_overlapping_panels() {
        let dir = tmpdir("feather");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let params = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 16 };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        // Output is the cropped union bbox, and every pixel was written.
        assert_eq!((sink.w, sink.h, sink.ch), (112, 56, 1));
        assert!(sink.finished);
        assert!(sink.data.iter().all(|v| v.is_finite()), "no NaN/Inf anywhere");

        // Canvas (x, y) → output (x−8, y−8).
        let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);

        // Zero-covered region untouched (only B covers x≥80, but B starts y=16).
        assert_eq!(at(100, 10), 0.0);

        // Single-coverage interiors: weights normalize out exactly.
        assert!((at(24, 32) - 0.2).abs() < 1e-6, "A interior: {}", at(24, 32));
        assert!((at(104, 40) - 0.4).abs() < 1e-6, "B interior: {}", at(104, 40));

        // Overlap midpoint: both panels at full weight → exact average.
        assert!((at(64, 36) - 0.3).abs() < 1e-6, "overlap midpoint: {}", at(64, 36));

        // Monotone ramp from A's value to B's value across the overlap.
        let mut prev = f32::NEG_INFINITY;
        for x in 48..80 {
            let v = at(x, 36);
            assert!(v >= prev - 1e-5, "ramp not monotone at x={x}: {v} < {prev}");
            prev = v;
        }
        assert!(at(48, 36) < 0.3 && at(79, 36) > 0.3, "ramp spans the average");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn photometric_corrections_are_applied() {
        let dir = tmpdir("phot");
        let (session, graph) = make_panels(&dir);
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![2.0, 1.0]],
            offsets: vec![vec![0.01, 0.0]],
        };
        let params = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 64 };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);
        // A interior: 0.2·2 + 0.01 = 0.41; B untouched; overlap = mean of both.
        assert!((at(24, 32) - 0.41).abs() < 1e-6);
        assert!((at(104, 40) - 0.4).abs() < 1e-6);
        assert!((at(64, 36) - 0.405).abs() < 1e-6);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn surfaces_are_applied_during_accumulation() {
        let dir = tmpdir("surf");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        // Panel A gets s(x,y) = 0.05 + 0.1·x − 0.02·y + 0.2·x² (normalized
        // canvas coords, canvas 128×64); panel B gets zero.
        let surf = crate::surfaces::Surfaces {
            order: 2,
            coeffs: vec![vec![
                vec![0.05, 0.1, -0.02, 0.2, 0.0, 0.0],
                vec![0.0; 6],
            ]],
            max_abs_s: vec![],
            bg_mad: vec![],
        };
        let s_at = |x: f64, y: f64| {
            let (xn, yn) = (x / 128.0, y / 64.0);
            0.05 + 0.1 * xn - 0.02 * yn + 0.2 * xn * xn
        };

        let params = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 16 };
        let mut sink = MemSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
        let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);

        // A interior: 0.2 + s(x,y); B interior unchanged; overlap midpoint
        // (both full weight): mean of corrected values.
        let (ax, ay) = (24u64, 32u64);
        let expect_a = 0.2 + s_at(ax as f64, ay as f64) as f32;
        assert!((at(ax, ay) - expect_a).abs() < 1e-5, "A interior {} vs {expect_a}", at(ax, ay));
        assert!((at(104, 40) - 0.4).abs() < 1e-6, "B interior must stay uncorrected");
        let expect_mid = 0.5 * ((0.2 + s_at(64.0, 36.0) as f32) + 0.4);
        assert!(
            (at(64, 36) - expect_mid).abs() < 1e-5,
            "overlap midpoint {} vs {expect_mid}",
            at(64, 36)
        );

        // Same corrections must hold on the L8 preview path (cell centers).
        let params8 = BlendParams { feather_px: 16.0, downsample: 8, band_rows: 4 };
        let mut sink8 = MemSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params8, &mut sink8).unwrap();
        // Canvas cell (3,4) is A-only; center pixel (28, 36).
        let got = sink8.at(0, 3 - 1, 4 - 1);
        let expect_cell = 0.2 + s_at(28.0, 36.0) as f32;
        assert!((got - expect_cell).abs() < 1e-5, "L8 A-only cell {got} vs {expect_cell}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn downsample_blends_from_l8_summaries() {
        let dir = tmpdir("l8");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let params = BlendParams { feather_px: 16.0, downsample: 8, band_rows: 3 };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        // L8 grid cropped to union bbox / 8: x8∈[1,15), y8∈[1,8) → 14×7.
        assert_eq!((sink.w, sink.h, sink.ch), (14, 7, 1));
        assert!(sink.data.iter().all(|v| v.is_finite()));

        // Canvas cell (x8, y8) → output (x8−1, y8−1).
        let at = |x8: usize, y8: usize| sink.at(0, x8 - 1, y8 - 1);
        assert!((at(3, 4) - 0.2).abs() < 1e-6, "A-only cell: {}", at(3, 4));
        assert!((at(8, 4) - 0.3).abs() < 1e-6, "overlap cell: {}", at(8, 4));
        assert_eq!(at(13, 1), 0.0, "cell covered by neither panel");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unsupported_downsample() {
        let dir = tmpdir("badds");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let params = BlendParams { feather_px: 16.0, downsample: 4, band_rows: 16 };
        let mut sink = MemSink::new();
        assert!(blend(&session, &phot, None, &graph, &params, &mut sink).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
