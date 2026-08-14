//! Seam & ownership diagnostics.
//!
//! Two views of the same question — *where are the seams, and how big a step
//! could a viewer see across each one?*
//!
//! - [`seam_deltas`]: per overlap edge, the mean channel-max
//!   `|corrected_a − corrected_b|` over the edge's *boundary cells* — L8
//!   cells whose 4-neighbourhood contains the other panel of the pair. The
//!   corrections are the full blend-time ones (gain, offset, residual
//!   surface), so `seam Δ` measures the photometric step actually left along
//!   the line where detail ownership switches — a better predictor of a
//!   visible seam than an edge's fit rms, which averages over the whole
//!   overlap band.
//! - [`write_seam_map`]: a PNG of the autostretched L8 luminance of the
//!   blended preview with each panel's owned region subtly tinted (12-hue
//!   palette), owner boundaries darkened, and each region labelled with its
//!   panel id in 3×5 bitmap digits at the region centroid (no font
//!   dependencies).

use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use rayon::prelude::*;

use crate::blend::{
    BlendParams, RowSink, blend, corrected_cell_means, load_summaries, output_bbox,
    panel_correction_terms,
};
use crate::output::png::{mtf, stretch_params};
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::seam::{OwnerMap, compute_owner_map};
use crate::session::Session;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::Surfaces;
use crate::{Error, Result};

/// Subtle region tint: fraction by which the owner's hue displaces gray.
const TINT: f32 = 0.4;

/// Brightness factor applied to owner-boundary pixels (drawn dark).
const BOUNDARY_DIM: f32 = 0.3;

/// 12-hue palette (30° steps around the HSV wheel, full saturation). Panel
/// `p` uses hue `(5·p) mod 12`, so consecutive panel ids land on
/// well-separated hues.
const PALETTE: [[f32; 3]; 12] = [
    [1.0, 0.0, 0.0],
    [1.0, 0.5, 0.0],
    [1.0, 1.0, 0.0],
    [0.5, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 1.0, 0.5],
    [0.0, 1.0, 1.0],
    [0.0, 0.5, 1.0],
    [0.0, 0.0, 1.0],
    [0.5, 0.0, 1.0],
    [1.0, 0.0, 1.0],
    [1.0, 0.0, 0.5],
];

/// 3×5 bitmap digits: 5 rows of 3 bits each, MSB = left column.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b010, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

/// One edge's seam residual.
#[derive(Debug, Clone, Copy)]
pub struct SeamDelta {
    /// Boundary cells measured (those fully covered by both panels).
    pub n: usize,
    /// Mean over those cells of the channel-max |corrected_a − corrected_b|.
    pub delta: f64,
}

/// Cells whose 4-neighbourhood contains a different owner: one
/// `(cell index, own panel, neighbouring panel)` entry per distinct
/// neighbouring owner of each such cell. Unowned cells (`u16::MAX`) neither
/// appear nor count as a different owner — panel rims are not seams.
pub fn boundary_cells(owner: &OwnerMap) -> Vec<(usize, u16, u16)> {
    let (w, h) = (owner.w8 as usize, owner.h8 as usize);
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let o = owner.owner[i];
            if o == u16::MAX {
                continue;
            }
            let mut seen = [u16::MAX; 4];
            let mut ns = 0usize;
            let mut visit = |j: usize, out: &mut Vec<(usize, u16, u16)>| {
                let n = owner.owner[j];
                if n != u16::MAX && n != o && !seen[..ns].contains(&n) {
                    seen[ns] = n;
                    ns += 1;
                    out.push((i, o, n));
                }
            };
            if x > 0 {
                visit(i - 1, &mut out);
            }
            if x + 1 < w {
                visit(i + 1, &mut out);
            }
            if y > 0 {
                visit(i - w, &mut out);
            }
            if y + 1 < h {
                visit(i + w, &mut out);
            }
        }
    }
    out
}

/// Per-edge seam residuals: for each unordered panel pair `(a, b)` that meets
/// along an ownership boundary, the mean channel-max
/// `|corrected_a − corrected_b|` over the pair's boundary cells (both sides
/// of the seam). Cells not fully covered by *both* panels are skipped — a
/// partially covered cell's mean is not a valid cross-panel comparison.
pub fn seam_deltas(
    summaries: &[L8Summary],
    owner: &OwnerMap,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    canvas: (u64, u64, u64),
) -> HashMap<(usize, usize), SeamDelta> {
    let nch = canvas.2 as usize;
    let corr: Vec<Vec<f32>> = summaries
        .par_iter()
        .enumerate()
        .map(|(p, s)| {
            let (g, o, t) = panel_correction_terms(phot, surfaces, p, nch);
            corrected_cell_means(s, &g, &o, &t, canvas)
        })
        .collect();

    let w = owner.w8 as usize;
    let cells = w * owner.h8 as usize;
    let mut acc: HashMap<(usize, usize), (usize, f64)> = HashMap::new();
    for (i, a, b) in boundary_cells(owner) {
        let (a, b) = (a as usize, b as usize);
        let (x8, y8) = ((i % w) as u32, (i / w) as u32);
        if summaries[a].cov(x8, y8) < 1.0 || summaries[b].cov(x8, y8) < 1.0 {
            continue;
        }
        let d = (0..nch)
            .map(|c| (corr[a][c * cells + i] - corr[b][c * cells + i]).abs())
            .fold(0.0f32, f32::max);
        let e = acc.entry((a.min(b), a.max(b))).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += d as f64;
    }
    acc.into_iter()
        .map(|(k, (n, sum))| {
            (
                k,
                SeamDelta {
                    n,
                    delta: sum / n as f64,
                },
            )
        })
        .collect()
}

/// Load the session's L8 summaries and compute the shared owner map exactly
/// as the blender does (connected star masks included) — the inputs both
/// diagnostics need.
pub fn load_owner_map(
    session: &Session,
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    feather_px: f32,
) -> Result<(Vec<L8Summary>, OwnerMap)> {
    let summaries = load_summaries(session)?;
    let owner = compute_owner_map(
        &summaries,
        graph,
        phot,
        surfaces,
        session.canvas,
        feather_px,
    );
    Ok((summaries, owner))
}

/// In-memory planar sink for the L8 preview blend.
#[derive(Default)]
struct BufSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl RowSink for BufSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        (self.w, self.h, self.ch) = (w as usize, h as usize, ch as usize);
        self.data = vec![0.0f32; self.w * self.h * self.ch];
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let band_rows = rows.len() / (self.ch * self.w);
        for c in 0..self.ch {
            for r in 0..band_rows {
                let src = &rows[(c * band_rows + r) * self.w..][..self.w];
                let dst = (c * self.h + y0 as usize + r) * self.w;
                self.data[dst..dst + self.w].copy_from_slice(src);
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Write the seam/ownership map PNG: autostretched L8 luminance of the
/// blended preview, owner regions tinted with a 12-hue palette
/// (subtle), owner boundaries drawn dark, and panel ids drawn at region
/// centroids as 3×5 bitmap digits. `owner` must come from the same session
/// (see [`load_owner_map`]); `feather_px` should match the blend's.
pub fn write_seam_map(
    session: &Session,
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    owner: &OwnerMap,
    feather_px: f32,
    path: &Path,
) -> Result<()> {
    // Blended L8 preview (at 1/8 the base band is the whole signal, so this
    // is the feather path regardless of mode).
    let params = BlendParams {
        feather_px,
        downsample: 8,
        ..Default::default()
    };
    let mut sink = BufSink::default();
    blend(session, phot, surfaces, graph, &params, &mut sink)?;
    let (w, h, ch) = (sink.w, sink.h, sink.ch);
    let plane = w * h;

    // The preview's crop origin on the L8 grid (mirrors the preview blend).
    let bbox = output_bbox(session, &params)?;
    let (gx0, gy0) = (
        (bbox[0] / BLOCK as u64) as usize,
        (bbox[1] / BLOCK as u64) as usize,
    );
    let w8 = owner.w8 as usize;

    // Autostretched luminance (channel mean; covered ⟺ all channels nonzero).
    let mut lum = vec![0.0f32; plane];
    let mut covered: Vec<f32> = Vec::new();
    for (i, l) in lum.iter_mut().enumerate() {
        if (0..ch).all(|c| sink.data[c * plane + i] != 0.0) {
            *l = (0..ch).map(|c| sink.data[c * plane + i]).sum::<f32>() / ch as f32;
            covered.push(*l);
        }
    }
    let (clip, mid) = stretch_params(&mut covered);
    let stretch = |v: f32| mtf(mid, ((v - clip) / (1.0 - clip)).clamp(0.0, 1.0));

    let mut boundary = vec![false; w8 * owner.h8 as usize];
    for (i, _, _) in boundary_cells(owner) {
        boundary[i] = true;
    }

    // Compose: gray luminance → subtle owner tint → dark boundaries.
    let mut buf = vec![0u8; plane * 3];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let g = stretch(lum[i]);
            let cell = (gy0 + y) * w8 + gx0 + x;
            let o = owner.owner[cell];
            let mut rgb = [g, g, g];
            if o != u16::MAX {
                let hue = PALETTE[(o as usize * 5) % PALETTE.len()];
                for (v, &hc) in rgb.iter_mut().zip(&hue) {
                    *v *= 1.0 - TINT * (1.0 - hc);
                }
            }
            if boundary[cell] {
                for v in &mut rgb {
                    *v *= BOUNDARY_DIM;
                }
            }
            for (c, &v) in rgb.iter().enumerate() {
                buf[i * 3 + c] = (v * 255.0 + 0.5) as u8;
            }
        }
    }

    // Panel id labels at the centroid of each owned region within the crop.
    let scale = (w.min(h) / 256).clamp(1, 4);
    let mut sums = vec![(0u64, 0u64, 0u64); session.panels.len()];
    for y in 0..h {
        for x in 0..w {
            let o = owner.owner[(gy0 + y) * w8 + gx0 + x] as usize;
            if let Some(s) = sums.get_mut(o) {
                s.0 += x as u64;
                s.1 += y as u64;
                s.2 += 1;
            }
        }
    }
    for (p, &(sx, sy, n)) in sums.iter().enumerate() {
        if let (Some(cx), Some(cy)) = (sx.checked_div(n), sy.checked_div(n)) {
            draw_label(&mut buf, w, h, cx as i64, cy as i64, p, scale);
        }
    }

    let file = File::create(path).map_err(|e| Error::io(path, e))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&buf))
        .map_err(|e| Error::format(path, format!("seam-map PNG: {e}")))?;
    Ok(())
}

/// End-of-run mosaic statistics, written as `summary.json` into the session
/// directory by [`write_session_reports`] so a host can present a closing
/// summary without recomputing anything.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct MosaicSummary {
    /// Number of input panels.
    pub panels: usize,
    /// Full-resolution output dimensions `[width, height, channels]` — the
    /// blend's data bounding box, not the full canvas.
    pub output: [u64; 3],
    /// `output` width × height in megapixels.
    pub megapixels: f64,
    /// Fraction of the output bounding box covered by at least one panel,
    /// measured on the L8 owner grid.
    pub covered_frac: f64,
    /// Number of overlap edges in the session's overlap graph.
    pub overlap_edges: usize,
    /// Median over edges of `overlap cells / smaller panel's covered cells`.
    pub median_overlap_frac: f64,
    /// Median over edges of the corrected seam step (see [`seam_deltas`]);
    /// `0` when there are no measurable edges.
    pub seam_delta_median: f64,
    /// Maximum over edges of the corrected seam step.
    pub seam_delta_max: f64,
}

/// Median of an unsorted slice; `0` when empty.
fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The IPC worker's post-blend hook: computes the owner map once, writes the
/// seam/ownership map PNG (`seam_map.png`, see [`write_seam_map`]) when
/// `seam_map` is set, always writes `summary.json`, and returns the summary
/// stats. `feather_px` should match the blend's.
pub fn write_session_reports(
    session: &Session,
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    feather_px: f32,
    seam_map: bool,
) -> Result<MosaicSummary> {
    let (summaries, owner) = load_owner_map(session, graph, phot, surfaces, feather_px)?;
    if seam_map {
        write_seam_map(
            session,
            graph,
            phot,
            surfaces,
            &owner,
            feather_px,
            &session.dir.join("seam_map.png"),
        )?;
    }

    // Output geometry: the same data bounding box the blend streamed.
    let params = BlendParams {
        feather_px,
        ..Default::default()
    };
    let bbox = output_bbox(session, &params)?;
    let (w, h) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
    let ch = session.canvas.2;

    // Covered fraction of the output bbox, on the L8 owner grid (a cell is
    // covered iff some panel owns it).
    let w8 = owner.w8 as usize;
    let (gx0, gy0) = (
        (bbox[0] / BLOCK as u64) as usize,
        (bbox[1] / BLOCK as u64) as usize,
    );
    let (gx1, gy1) = (
        (bbox[2].div_ceil(BLOCK as u64) as usize).min(w8),
        (bbox[3].div_ceil(BLOCK as u64) as usize).min(owner.h8 as usize),
    );
    let mut covered = 0u64;
    for y in gy0..gy1 {
        for x in gx0..gx1 {
            if owner.owner[y * w8 + x] != u16::MAX {
                covered += 1;
            }
        }
    }
    let crop_cells = ((gx1 - gx0) * (gy1 - gy0)).max(1) as f64;

    // Overlap fraction per edge: overlap cells relative to the smaller
    // panel's fully-covered cell count (the same coverage == 1.0 criterion
    // OverlapGraph::build counts with).
    let covered_cells: Vec<u64> = summaries
        .iter()
        .map(|s| s.coverage.iter().filter(|&&c| c == 1.0).count() as u64)
        .collect();
    let overlap_fracs: Vec<f64> = graph
        .edges
        .iter()
        .map(|e| e.n_cells as f64 / covered_cells[e.a].min(covered_cells[e.b]).max(1) as f64)
        .collect();

    let deltas = seam_deltas(&summaries, &owner, phot, surfaces, session.canvas);
    let delta_values: Vec<f64> = deltas.values().map(|d| d.delta).collect();
    let seam_delta_max = delta_values.iter().copied().fold(0.0f64, f64::max);

    let summary = MosaicSummary {
        panels: session.panels.len(),
        output: [w, h, ch],
        megapixels: (w * h) as f64 / 1e6,
        covered_frac: covered as f64 / crop_cells,
        overlap_edges: graph.edges.len(),
        median_overlap_frac: median(overlap_fracs),
        seam_delta_median: median(delta_values),
        seam_delta_max,
    };

    let path = session.dir.join("summary.json");
    let js = serde_json::to_string_pretty(&summary)
        .map_err(|e| Error::format(&path, format!("summary JSON: {e}")))?;
    std::fs::write(&path, js).map_err(|e| Error::io(&path, e))?;
    Ok(summary)
}

/// Draw `id` in 3×5 bitmap digits (scaled), centered on `(cx, cy)`, white on
/// a 1-px dark outline. Out-of-bounds pixels are clipped.
fn draw_label(buf: &mut [u8], w: usize, h: usize, cx: i64, cy: i64, id: usize, scale: usize) {
    let text: Vec<usize> = id
        .to_string()
        .bytes()
        .map(|b| (b - b'0') as usize)
        .collect();
    let s = scale as i64;
    let x0 = cx - (text.len() as i64 * 4 - 1) * s / 2; // 3 cols + 1 gap each
    let y0 = cy - 5 * s / 2;
    let mut fill = |px: i64, py: i64, v: u8| {
        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
            let i = (py as usize * w + px as usize) * 3;
            buf[i..i + 3].fill(v);
        }
    };
    // Outline pass (dark, dilated by 1 px), then glyph pass (white).
    for (grow, v) in [(1, 0u8), (0, 255u8)] {
        for (d, &digit) in text.iter().enumerate() {
            let gx = x0 + d as i64 * 4 * s;
            for (row, bits) in DIGITS[digit].iter().enumerate() {
                for col in 0..3i64 {
                    if bits & (0b100 >> col) == 0 {
                        continue;
                    }
                    let (bx, by) = (gx + col * s, y0 + row as i64 * s);
                    for yy in by - grow..by + s + grow {
                        for xx in bx - grow..bx + s + grow {
                            fill(xx, yy, v);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze;
    use crate::synth::write_xisf;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-diag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Mandatory test: boundary cells = cells whose 4-neighbourhood contains
    /// a different owner. Unowned (`u16::MAX`) cells neither appear nor make
    /// their neighbours boundary cells.
    #[test]
    fn boundary_cells_are_4_neighbour_owner_changes() {
        // 4×3 grid, left half owner 0, right half owner 1, one unowned hole:
        //   0 0 1 1
        //   0 . 1 1
        //   0 0 1 1
        const M: u16 = u16::MAX;
        let owner = OwnerMap {
            w8: 4,
            h8: 3,
            owner: vec![0, 0, 1, 1, 0, M, 1, 1, 0, 0, 1, 1],
        };
        let mut got = boundary_cells(&owner);
        got.sort_unstable();
        // Only the horizontally adjacent (0|1) pairs in rows 0 and 2 qualify;
        // the hole's neighbours get nothing from it, and diagonal contact
        // (e.g. cell 5's diagonals) does not count.
        assert_eq!(got, vec![(1, 0, 1), (2, 1, 0), (9, 0, 1), (10, 1, 0)]);
    }

    /// A cell bordering two different owners reports one entry per owner.
    #[test]
    fn boundary_cell_bordering_two_owners_reports_both() {
        //   1 0 2   → the middle cell borders 1 and 2 (and reports each once,
        //   1 0 2     despite two 2-neighbours in column layout below).
        let owner = OwnerMap {
            w8: 3,
            h8: 2,
            owner: vec![1, 0, 2, 1, 0, 2],
        };
        let mut got = boundary_cells(&owner);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![
                (0, 1, 0),
                (1, 0, 1),
                (1, 0, 2),
                (2, 2, 0),
                (3, 1, 0),
                (4, 0, 1),
                (4, 0, 2),
                (5, 2, 0),
            ]
        );
    }

    /// Two single-channel panels on an 8×4-cell grid (canvas 64×32): A covers
    /// x8 < 5 with mean `a_mean`, B covers x8 ≥ 3 with mean `b_mean`. Owner:
    /// x8 < 4 → A, else B; the boundary cells are columns 3 and 4.
    fn two_panel_case(a_mean: f32, b_mean: f32) -> (Vec<L8Summary>, OwnerMap) {
        let (w8, h8) = (8u32, 4u32);
        let mk = |x_lo: u32, x_hi: u32, mean: f32| -> L8Summary {
            let mut s = L8Summary::zeroed(w8, h8, 1);
            for y in 0..h8 {
                for x in x_lo..x_hi {
                    let i = (y * w8 + x) as usize;
                    s.coverage[i] = 1.0;
                    s.mean[i] = mean;
                }
            }
            s
        };
        let owner = OwnerMap {
            w8,
            h8,
            owner: (0..w8 * h8)
                .map(|i| if i % w8 < 4 { 0 } else { 1 })
                .collect(),
        };
        (vec![mk(0, 5, a_mean), mk(3, 8, b_mean)], owner)
    }

    /// Mandatory test: seam Δ on a hand-built 2-panel case — identity
    /// corrections give exactly the mean step, and gains/offsets are applied
    /// before differencing.
    #[test]
    fn seam_delta_measures_corrected_step_on_boundary_cells() {
        let (summaries, owner) = two_panel_case(0.10, 0.13);
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0, 1.0]],
            offsets: vec![vec![0.0, 0.0]],
        };
        let deltas = seam_deltas(&summaries, &owner, &phot, None, (64, 32, 1));
        assert_eq!(deltas.len(), 1);
        let d = deltas[&(0, 1)];
        // Boundary cells: columns 3 (owner 0) and 4 (owner 1), 4 rows each,
        // all fully covered by both panels.
        assert_eq!(d.n, 8);
        assert!((d.delta - 0.03).abs() < 1e-6, "identity delta: {}", d.delta);

        // Corrections applied: B' = 2·0.13 + 0.01 = 0.27 → Δ = 0.17.
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0, 2.0]],
            offsets: vec![vec![0.0, 0.01]],
        };
        let deltas = seam_deltas(&summaries, &owner, &phot, None, (64, 32, 1));
        let d = deltas[&(0, 1)];
        assert_eq!(d.n, 8);
        assert!(
            (d.delta - 0.17).abs() < 1e-6,
            "corrected delta: {}",
            d.delta
        );
    }

    /// Boundary cells not fully covered by both panels are skipped.
    #[test]
    fn seam_delta_skips_partially_covered_cells() {
        let (mut summaries, owner) = two_panel_case(0.10, 0.13);
        summaries[1].coverage[3] = 0.5; // B only partially covers cell (3, 0)
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0, 1.0]],
            offsets: vec![vec![0.0, 0.0]],
        };
        let deltas = seam_deltas(&summaries, &owner, &phot, None, (64, 32, 1));
        let d = deltas[&(0, 1)];
        assert_eq!(d.n, 7, "one boundary cell must be skipped");
        assert!((d.delta - 0.03).abs() < 1e-6);
    }

    /// Mandatory test: the seam map PNG writes and round-trips its
    /// dimensions (the L8 preview crop of the union bbox).
    #[test]
    fn seam_map_png_round_trips_dimensions() {
        let dir = tmpdir("map");
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
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0, 1.0]],
            offsets: vec![vec![0.0, 0.0]],
        };
        let (_, owner) = load_owner_map(&session, &graph, &phot, None, 16.0).unwrap();

        let path = dir.join("seam.png");
        write_seam_map(&session, &graph, &phot, None, &owner, 16.0, &path).unwrap();

        // Union bbox [8,8,120,64) → L8 crop [1,15)x[1,8) → 14×7 RGB.
        let decoder = png::Decoder::new(File::open(&path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (14, 7));
        assert_eq!(info.color_type, png::ColorType::Rgb);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The session-level post-blend hook (the worker's) computes the owner
    /// map once, drops `seam_map.png` (when asked) plus `summary.json` into
    /// the session directory, and returns the summary stats.
    #[test]
    fn session_reports_land_in_session_dir() {
        let dir = tmpdir("session-map");
        let (w, h) = (128u64, 64u64);
        let mut frame = vec![0f32; (w * h) as usize];
        for y in 8..56 {
            for x in 8..80 {
                frame[(y * w + x) as usize] = 0.2;
            }
        }
        let a = dir.join("a.xisf");
        write_xisf(&a, w, h, 1, &frame).unwrap();
        frame.fill(0.0);
        for y in 16..64 {
            for x in 48..120 {
                frame[(y * w + x) as usize] = 0.4;
            }
        }
        let b = dir.join("b.xisf");
        write_xisf(&b, w, h, 1, &frame).unwrap();

        let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0, 1.0]],
            offsets: vec![vec![0.0, 0.0]],
        };

        let summary = write_session_reports(&session, &graph, &phot, None, 16.0, true).unwrap();

        let png_path = session.dir.join("seam_map.png");
        let decoder = png::Decoder::new(File::open(&png_path).unwrap());
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().color_type, png::ColorType::Rgb);

        // summary.json exists and round-trips to the returned stats.
        let js = std::fs::read_to_string(session.dir.join("summary.json")).unwrap();
        let loaded: MosaicSummary = serde_json::from_str(&js).unwrap();
        assert_eq!(loaded.panels, summary.panels);
        assert_eq!(loaded.output, summary.output);

        // Panel A covers [8,80)x[8,56), B covers [48,120)x[16,64):
        // union bbox [8,120)x[8,64) → 112 x 56, 1 channel, one overlap edge.
        assert_eq!(summary.panels, 2);
        assert_eq!(summary.output, [112, 56, 1]);
        assert!((summary.megapixels - 112.0 * 56.0 / 1e6).abs() < 1e-12);
        assert_eq!(summary.overlap_edges, 1);
        assert!(
            summary.median_overlap_frac > 0.0 && summary.median_overlap_frac < 1.0,
            "overlap frac: {}",
            summary.median_overlap_frac
        );
        assert!(
            summary.covered_frac > 0.7 && summary.covered_frac <= 1.0,
            "covered frac: {}",
            summary.covered_frac
        );
        // Uncorrected 0.2-vs-0.4 panels leave a real seam step.
        assert!(summary.seam_delta_median > 0.0);
        assert!(summary.seam_delta_max >= summary.seam_delta_median);

        // seam_map = false skips the PNG but still writes the summary.
        let dir2 = dir.join("no-map.mmm-session");
        std::fs::create_dir_all(&dir2).unwrap();
        let session2 = analyze(&[dir.join("a.xisf"), dir.join("b.xisf")], &dir2).unwrap();
        let graph2 = OverlapGraph::load(&session2.overlap_graph_path()).unwrap();
        write_session_reports(&session2, &graph2, &phot, None, 16.0, false).unwrap();
        assert!(!session2.dir.join("seam_map.png").exists());
        assert!(session2.dir.join("summary.json").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
