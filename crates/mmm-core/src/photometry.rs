//! Photometric solve: per-edge robust linear fits on L8 means, then a global
//! per-channel least-squares adjustment yielding per-panel gain/offset
//! corrections.
//!
//! Per overlap edge and channel, the shared full-coverage L8 cells give pairs
//! `(x, y)` (x from panel `a`, y from panel `b`); a straight line `y ≈ g·x + o`
//! is fit with 3 rounds of least squares and 2.5σ residual clipping. The
//! post-clip sufficient statistics `[n, Σx, Σy, Σxx, Σyy, Σxy]` feed a global
//! normal-equation system over unknowns `[g_0, o_0, …, g_{N−1}, o_{N−1}]`
//! minimizing `Σ_e Σ_p ((g_i·x + o_i) − (g_j·y + o_j))²`, with each edge's
//! stats normalized by its `n` so every edge weighs equally. Gauge: the
//! largest-coverage panel of each connected component is fixed at `g=1, o=0`.

use std::path::Path;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::linalg::solve_dense;
use crate::overlap::{OverlapEdge, OverlapGraph};
use crate::summary::L8Summary;
use crate::{Error, Result};

/// Residual clipping threshold, in sigmas.
const CLIP_SIGMA: f64 = 2.5;
/// Fit → clip rounds per edge (the last round only refits).
const FIT_ROUNDS: usize = 3;
/// Absolute floor on the clip threshold so the float-level residuals of an
/// exact fit are never clipped away (real L8-mean noise is far larger).
const CLIP_FLOOR: f64 = 1e-7;

/// Robust linear fit for one overlap edge and channel: `I_b ≈ gain·I_a + offset`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeFit {
    /// Lower panel id of the pair (the fit's `x` side).
    pub a: usize,
    /// Higher panel id of the pair (the fit's `y` side).
    pub b: usize,
    /// Channel index the fit applies to.
    pub channel: u32,
    /// Fitted gain of `I_b ≈ gain·I_a + offset`.
    pub gain: f64,
    /// Fitted offset of `I_b ≈ gain·I_a + offset`.
    pub offset: f64,
    /// L8 cell pairs surviving the residual clip.
    pub n: u64,
    /// Post-clip residual RMS.
    pub rms: f64,
    /// Post-clip sufficient statistics `[n, Σx, Σy, Σxx, Σyy, Σxy]`
    /// (x = panel `a`, y = panel `b`).
    pub stats: [f64; 6],
}

/// Full photometric solution: per-edge fits plus per-panel global corrections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Photometry {
    /// Per-edge, per-channel robust fits (diagnostics; the global solve's
    /// inputs are their sufficient statistics).
    pub edge_fits: Vec<EdgeFit>,
    /// Per-panel gain corrections, indexed `[channel][panel]`.
    pub gains: Vec<Vec<f64>>,
    /// Per-panel offset corrections, indexed `[channel][panel]`.
    pub offsets: Vec<Vec<f64>>,
}

impl Photometry {
    /// Persist as JSON (conventionally `analysis/photometry.json`).
    pub fn save(&self, p: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::format(p, format!("serialize photometry: {e}")))?;
        std::fs::write(p, json).map_err(|e| Error::io(p, e))
    }

    /// Load a solution previously written by [`Photometry::save`].
    pub fn load(p: &Path) -> Result<Photometry> {
        let json = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
        serde_json::from_str(&json).map_err(|e| Error::format(p, format!("bad photometry: {e}")))
    }
}

/// Fit all edges, then solve the global per-channel adjustment.
pub fn solve(summaries: &[L8Summary], graph: &OverlapGraph) -> Result<Photometry> {
    if summaries.is_empty() {
        return Ok(Photometry::default());
    }
    let channels = summaries[0].channels;

    let edge_fits: Vec<EdgeFit> = graph
        .edges
        .par_iter()
        .flat_map_iter(|e| {
            (0..channels).map(move |c| fit_edge(e, c, &summaries[e.a], &summaries[e.b]))
        })
        .collect();

    // Gauge: the largest-coverage panel (count of fully covered L8 cells) of
    // each connected component is fixed at g=1, o=0; ties go to the lowest id.
    let n_panels = summaries.len();
    let refs: Vec<usize> = graph
        .components(n_panels)
        .iter()
        .map(|comp| {
            let mut best = comp[0];
            let mut best_cov = full_cov_cells(&summaries[best]);
            for &p in &comp[1..] {
                let cov = full_cov_cells(&summaries[p]);
                if cov > best_cov {
                    best = p;
                    best_cov = cov;
                }
            }
            best
        })
        .collect();

    let mut gains = Vec::with_capacity(channels as usize);
    let mut offsets = Vec::with_capacity(channels as usize);
    for c in 0..channels {
        // A constant-signal overlap (zero x-variance) makes gain and offset
        // individually unidentifiable and the plain system singular; retry
        // with a tiny ridge toward identity that resolves only the
        // unconstrained directions.
        let (g, o) = match global_solve(c, &edge_fits, &refs, n_panels, 0.0) {
            Ok(z) => z,
            Err(_) => global_solve(c, &edge_fits, &refs, n_panels, 1e-9)?,
        };
        gains.push(g);
        offsets.push(o);
    }

    Ok(Photometry {
        edge_fits,
        gains,
        offsets,
    })
}

/// Count of fully covered L8 cells — the panel-size measure for gauge choice.
fn full_cov_cells(s: &L8Summary) -> u64 {
    s.coverage.iter().filter(|&&c| c == 1.0).count() as u64
}

/// Robust straight-line fit `y ≈ gain·x + offset` over the shared
/// full-coverage cells of one edge, for one channel.
fn fit_edge(e: &OverlapEdge, channel: u32, sa: &L8Summary, sb: &L8Summary) -> EdgeFit {
    let [x0, y0, x1, y1] = e.bbox8;
    let mut xs = Vec::with_capacity(e.n_cells as usize);
    let mut ys = Vec::with_capacity(e.n_cells as usize);
    for y in y0..y1 {
        for x in x0..x1 {
            if sa.cov(x, y) == 1.0 && sb.cov(x, y) == 1.0 {
                xs.push(sa.cell(channel, x, y) as f64);
                ys.push(sb.cell(channel, x, y) as f64);
            }
        }
    }

    let mut keep = vec![true; xs.len()];
    let (mut gain, mut offset) = (1.0f64, 0.0f64);
    let mut stats = [0.0f64; 6];
    let mut rms = 0.0f64;

    for round in 0..FIT_ROUNDS {
        let mut s = [0.0f64; 6];
        for i in 0..xs.len() {
            if !keep[i] {
                continue;
            }
            let (x, y) = (xs[i], ys[i]);
            s[0] += 1.0;
            s[1] += x;
            s[2] += y;
            s[3] += x * x;
            s[4] += y * y;
            s[5] += x * y;
        }
        if s[0] < 2.0 {
            break; // over-clipped to nothing useful; keep the previous fit
        }
        let denom = s[0] * s[3] - s[1] * s[1];
        if denom > s[0] * s[3] * 1e-12 {
            gain = (s[0] * s[5] - s[1] * s[2]) / denom;
            offset = (s[2] - gain * s[1]) / s[0];
        } else {
            // Constant x: gain unidentifiable; match the mean levels.
            gain = 1.0;
            offset = (s[2] - s[1]) / s[0];
        }
        let mut sq = 0.0f64;
        for i in 0..xs.len() {
            if keep[i] {
                let r = ys[i] - (gain * xs[i] + offset);
                sq += r * r;
            }
        }
        rms = (sq / s[0]).sqrt();
        stats = s;
        if round + 1 < FIT_ROUNDS {
            let thr = (CLIP_SIGMA * rms).max(CLIP_FLOOR);
            for i in 0..xs.len() {
                keep[i] = (ys[i] - (gain * xs[i] + offset)).abs() <= thr;
            }
        }
    }

    EdgeFit {
        a: e.a,
        b: e.b,
        channel,
        gain,
        offset,
        n: stats[0] as u64,
        rms,
        stats,
    }
}

/// Assemble and solve one channel's 2N×2N normal-equation system over
/// `z = [g_0, o_0, …]`. Each edge's sufficient statistics are normalized by
/// its `n` (all six divided by n) so every edge weighs equally regardless of
/// overlap area. Reference-panel rows are overwritten with the g=1, o=0 gauge.
/// `ridge > 0` adds `ridge·((g−1)² + o²)` per panel, scaled by the row's
/// accumulated diagonal, to resolve degenerate (constant-overlap) systems.
fn global_solve(
    channel: u32,
    fits: &[EdgeFit],
    refs: &[usize],
    n_panels: usize,
    ridge: f64,
) -> Result<(Vec<f64>, Vec<f64>)> {
    let n = 2 * n_panels;
    let mut a = vec![0.0f64; n * n];
    let mut b = vec![0.0f64; n];

    for f in fits
        .iter()
        .filter(|f| f.channel == channel && f.stats[0] > 0.0)
    {
        let nn = f.stats[0];
        // Normalized sufficient statistics: n' = 1, the rest are means.
        let en = 1.0;
        let sx = f.stats[1] / nn;
        let sy = f.stats[2] / nn;
        let sxx = f.stats[3] / nn;
        let syy = f.stats[4] / nn;
        let sxy = f.stats[5] / nn;
        let (gi, oi, gj, oj) = (2 * f.a, 2 * f.a + 1, 2 * f.b, 2 * f.b + 1);
        // ∂E/∂g_i: +Sxx·g_i +Sx·o_i −Sxy·g_j −Sx·o_j = 0
        a[gi * n + gi] += sxx;
        a[gi * n + oi] += sx;
        a[gi * n + gj] -= sxy;
        a[gi * n + oj] -= sx;
        // ∂E/∂o_i: +Sx·g_i +n·o_i −Sy·g_j −n·o_j = 0
        a[oi * n + gi] += sx;
        a[oi * n + oi] += en;
        a[oi * n + gj] -= sy;
        a[oi * n + oj] -= en;
        // ∂E/∂g_j: −Sxy·g_i −Sy·o_i +Syy·g_j +Sy·o_j = 0
        a[gj * n + gi] -= sxy;
        a[gj * n + oi] -= sy;
        a[gj * n + gj] += syy;
        a[gj * n + oj] += sy;
        // ∂E/∂o_j: −Sx·g_i −n·o_i +Sy·g_j +n·o_j = 0
        a[oj * n + gi] -= sx;
        a[oj * n + oi] -= en;
        a[oj * n + gj] += sy;
        a[oj * n + oj] += en;
    }

    if ridge > 0.0 {
        for p in 0..n_panels {
            let (gp, op) = (2 * p, 2 * p + 1);
            let lg = ridge * (1.0 + a[gp * n + gp]);
            let lo = ridge * (1.0 + a[op * n + op]);
            a[gp * n + gp] += lg;
            b[gp] += lg; // pulls g toward 1
            a[op * n + op] += lo; // pulls o toward 0
        }
    }

    for &r in refs {
        let (gr, or) = (2 * r, 2 * r + 1);
        for c in 0..n {
            a[gr * n + c] = 0.0;
            a[or * n + c] = 0.0;
        }
        a[gr * n + gr] = 1.0;
        b[gr] = 1.0;
        a[or * n + or] = 1.0;
        b[or] = 0.0;
    }

    let z = solve_dense(&mut a, &mut b, n)?;
    let gains = (0..n_panels).map(|p| z[2 * p]).collect();
    let offsets = (0..n_panels).map(|p| z[2 * p + 1]).collect();
    Ok((gains, offsets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlap::OverlapGraph;

    /// 1-channel summary with `cov = 1.0` and `mean = f(x, y)` inside
    /// `region = [x0, y0, x1, y1)`, zero elsewhere.
    fn summary_from(w8: u32, h8: u32, region: [u32; 4], f: impl Fn(u32, u32) -> f32) -> L8Summary {
        let mut s = L8Summary::zeroed(w8, h8, 1);
        for y in region[1]..region[3] {
            for x in region[0]..region[2] {
                let i = (y * w8 + x) as usize;
                s.coverage[i] = 1.0;
                s.mean[i] = f(x, y);
            }
        }
        s
    }

    #[test]
    fn edge_fit_recovers_gain_offset_despite_outliers() {
        // Both panels cover the whole 20×20 grid; y = 2x + 0.01 with 5% of the
        // 400 cells knocked far off the line.
        let (w8, h8) = (20u32, 20u32);
        let xval = |x: u32, y: u32| 0.01 + 1e-4 * (y * 20 + x) as f32;
        let a = summary_from(w8, h8, [0, 0, 20, 20], xval);
        let b = summary_from(w8, h8, [0, 0, 20, 20], |x, y| {
            let v = 2.0 * xval(x, y) + 0.01;
            if (y * 20 + x) % 20 == 0 { v + 0.2 } else { v } // 20 outliers
        });

        let graph = OverlapGraph::build(&[a.clone(), b.clone()]);
        assert_eq!(graph.edges.len(), 1);
        let phot = solve(&[a, b], &graph).unwrap();
        assert_eq!(phot.edge_fits.len(), 1);
        let f = &phot.edge_fits[0];
        assert_eq!((f.a, f.b, f.channel), (0, 1, 0));
        assert!((f.gain - 2.0).abs() < 1e-5, "gain = {}", f.gain);
        assert!((f.offset - 0.01).abs() < 1e-6, "offset = {}", f.offset);
        assert_eq!(f.n, 380, "all 20 outliers clipped, all inliers kept");
        assert_eq!(f.stats[0], f.n as f64);
        assert!(f.rms < 1e-6, "rms = {}", f.rms);
    }

    #[test]
    fn global_solve_inverts_applied_chain_transforms() {
        // Three panels in a chain over a common smooth truth; per-panel (g,o)
        // applied. The solve must recover the inverse transforms exactly
        // (panel 0 has the largest coverage → reference, g=1, o=0).
        let (w8, h8) = (60u32, 20u32);
        let truth = |x: u32, y: u32| 0.02 + 5e-4 * x as f32 + 2e-4 * y as f32;
        let applied: [(f32, f32); 3] = [(1.1, 0.004), (0.8, -0.002), (1.3, 0.01)];
        let regions = [[0, 0, 26, 20], [15, 0, 40, 20], [30, 0, 55, 20]];
        let summaries: Vec<L8Summary> = regions
            .iter()
            .zip(&applied)
            .map(|(&r, &(g, o))| summary_from(w8, h8, r, |x, y| truth(x, y) * g + o))
            .collect();

        let graph = OverlapGraph::build(&summaries);
        assert_eq!(graph.edges.len(), 2, "chain 0–1–2, ends disjoint");
        let phot = solve(&summaries, &graph).unwrap();
        assert_eq!(phot.gains.len(), 1);
        assert_eq!(phot.gains[0].len(), 3);

        // Reference gauge is exact.
        assert_eq!(phot.gains[0][0], 1.0);
        assert_eq!(phot.offsets[0][0], 0.0);

        // Correcting panel p into the reference frame requires
        // g_p = g_ref/g_p^applied, o_p = o_ref − g_p·o_p^applied.
        let (gr, or) = applied[0];
        for (p, &(gp, op)) in applied.iter().enumerate().skip(1) {
            let eg = gr as f64 / gp as f64;
            let eo = or as f64 - eg * op as f64;
            assert!(
                (phot.gains[0][p] - eg).abs() < 1e-6,
                "panel {p} gain {} vs expected {eg}",
                phot.gains[0][p]
            );
            assert!(
                (phot.offsets[0][p] - eo).abs() < 1e-6,
                "panel {p} offset {} vs expected {eo}",
                phot.offsets[0][p]
            );
        }
    }

    #[test]
    fn isolated_panels_get_identity_corrections() {
        // Two disjoint panels: no edges, both are their component's reference.
        let a = summary_from(40, 20, [0, 0, 15, 20], |_, _| 0.5);
        let b = summary_from(40, 20, [25, 0, 40, 20], |_, _| 0.25);
        let graph = OverlapGraph::build(&[a.clone(), b.clone()]);
        assert!(graph.edges.is_empty());
        let phot = solve(&[a, b], &graph).unwrap();
        assert!(phot.edge_fits.is_empty());
        assert_eq!(phot.gains[0], vec![1.0, 1.0]);
        assert_eq!(phot.offsets[0], vec![0.0, 0.0]);
    }

    #[test]
    fn constant_overlap_still_solves_and_matches_levels() {
        // Constant signal in the overlap: gain and offset are individually
        // unidentifiable (zero x-variance) but the level constraint
        // g·y + o = x must still hold and the system must not error out.
        let a = summary_from(40, 20, [0, 0, 25, 20], |_, _| 0.5);
        let b = summary_from(40, 20, [15, 0, 40, 20], |_, _| 0.25);
        let graph = OverlapGraph::build(&[a.clone(), b.clone()]);
        assert_eq!(graph.edges.len(), 1);
        let phot = solve(&[a, b], &graph).unwrap();
        // Panel 0 (largest coverage) is the reference.
        assert_eq!(phot.gains[0][0], 1.0);
        assert_eq!(phot.offsets[0][0], 0.0);
        // Corrected panel 1 level must match panel 0 in the overlap.
        let corrected = phot.gains[0][1] * 0.25 + phot.offsets[0][1];
        assert!((corrected - 0.5).abs() < 1e-6, "corrected = {corrected}");
        for &v in &[phot.gains[0][1], phot.offsets[0][1]] {
            assert!(v.is_finite());
        }
    }

    #[test]
    fn photometry_round_trips_through_json() {
        let phot = Photometry {
            edge_fits: vec![EdgeFit {
                a: 0,
                b: 1,
                channel: 2,
                gain: 1.25,
                offset: -0.005,
                n: 123,
                rms: 3.5e-4,
                stats: [123.0, 1.0, 2.0, 3.0, 4.0, 5.0],
            }],
            gains: vec![vec![1.0, 0.8]],
            offsets: vec![vec![0.0, 0.01]],
        };
        let dir = std::env::temp_dir().join(format!("mmm-phot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("photometry.json");
        phot.save(&path).unwrap();
        let r = Photometry::load(&path).unwrap();
        assert_eq!(r.edge_fits.len(), 1);
        assert_eq!(r.edge_fits[0].n, 123);
        assert_eq!(r.edge_fits[0].stats, phot.edge_fits[0].stats);
        assert_eq!(r.gains, phot.gains);
        assert_eq!(r.offsets, phot.offsets);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
