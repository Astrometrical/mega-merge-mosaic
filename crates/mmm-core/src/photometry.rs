//! Photometric solve: per-edge robust linear fits on L8 means, then a global
//! per-channel least-squares adjustment yielding per-panel gain/offset
//! corrections.
//!
//! Per overlap edge and channel, the shared full-coverage L8 cells give pairs
//! `(x, y)` (x from panel `a`, y from panel `b`); a straight line `y ≈ g·x + o`
//! is fit with 3 rounds of *symmetric* (Deming, equal noise variances) least
//! squares and 2.5σ vertical-residual clipping. The symmetric fit is immune
//! to the errors-in-variables attenuation that biases an ordinary y-on-x
//! slope toward 0 when the overlap's cell-scale signal variance is comparable
//! to the noise — the regime of faint-background overlaps, where attenuated
//! slopes chained across a mosaic used to collapse the global gains.
//! An identifiability guard closes the remaining hole: when the overlap's
//! estimated signal variance falls below the noise variance the gain is
//! unmeasurable (a symmetric fit on pure noise returns a meaningless ±1), so
//! the edge falls back to `gain = 1` + mean-level match and is flagged.
//!
//! Global adjustment per channel, from the per-edge *fitted lines* — never
//! the raw second moments, which would re-introduce the attenuation bias —
//! in two decoupled stages: gains first, as a robust (L1 via IRLS) solve of
//! the log-gain potential system over the overlap graph (see
//! [`global_solve`] for why L1: loop consistency must outvote slopes
//! contaminated by structure only one panel sees), then offsets linearly
//! from the corrected mean-level constraints with gains fixed. A weak ridge
//! pulls gains toward 1 so panels reached only through unidentifiable edges
//! stay at unity instead of drifting. Gauge: the largest-coverage panel of
//! each connected component is fixed at `g=1, o=0`. [`GainMode::Unity`]
//! skips gain fitting entirely: `g = 1` for every panel and offsets solved
//! from the mean-level constraints alone.

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
/// Identifiability: minimum correlation of the post-clip (x, y) pairs for
/// the gain to be trusted. 0.5 is the correlation of a genuine unit-gain
/// relation whose cell-scale signal variance just matches the noise variance;
/// pure noise and one-sided structure both measure ≈ 0.
const IDENT_MIN_R: f64 = 0.5;
/// Weak pull of every panel gain toward 1 in the global solve — decisive only
/// for gains no identifiable edge constrains.
const GAIN_RIDGE: f64 = 1e-6;
/// Maximum IRLS rounds of the robust (L1) global gain solve — each is one
/// small dense solve; the loop exits early once the weights stop moving.
const IRLS_ROUNDS: usize = 60;
/// Residual floor of the L1 IRLS weights, in log-gain units: edges within
/// this loop error (≈ 0.01% gain) keep full weight, so a consistent graph
/// reproduces the least-squares solution exactly.
const L1_FLOOR: f64 = 1e-4;

/// How the photometric solve treats per-panel gains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GainMode {
    /// Fit per-panel gains from identifiable overlap edges (the default).
    #[default]
    Fit,
    /// Force `gain = 1` for every panel and solve offsets only — for mosaics
    /// known to be photometrically homogeneous (same rig, exposure, filter).
    Unity,
}

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
    /// Whether the overlap carried enough signal variance to measure the
    /// gain. `false` means the fit fell back to `gain = 1` + level match and
    /// the global solve uses only this edge's level constraint. Sessions
    /// written before this field existed load as `true`.
    #[serde(default = "default_true")]
    pub gain_identifiable: bool,
}

/// Serde default for [`EdgeFit::gain_identifiable`] on pre-existing sessions.
fn default_true() -> bool {
    true
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

    /// Load a solution previously written by [`Photometry::save`]. A missing
    /// file gets a hint: the analyze stage writes it.
    pub fn load(p: &Path) -> Result<Photometry> {
        let json = std::fs::read_to_string(p).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::format(p, "photometry results missing — re-run `mmm analyze`")
            } else {
                Error::io(p, e)
            }
        })?;
        serde_json::from_str(&json).map_err(|e| Error::format(p, format!("bad photometry: {e}")))
    }
}

/// Fit all edges, then solve the global per-channel adjustment under `mode`.
pub fn solve(summaries: &[L8Summary], graph: &OverlapGraph, mode: GainMode) -> Result<Photometry> {
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
        let (g, o) = match mode {
            GainMode::Unity => {
                let ones = vec![1.0; n_panels];
                let o = offset_solve(c, &edge_fits, &refs, n_panels, &ones)?;
                (ones, o)
            }
            GainMode::Fit => global_solve(c, &edge_fits, &refs, n_panels)?,
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
    let mut us = Vec::with_capacity(e.n_cells as usize);
    let mut vs = Vec::with_capacity(e.n_cells as usize);
    let (uw, vh) = (
        (x1.saturating_sub(x0)).max(1) as f64,
        (y1.saturating_sub(y0)).max(1) as f64,
    );
    for y in y0..y1 {
        for x in x0..x1 {
            if sa.cov(x, y) == 1.0 && sb.cov(x, y) == 1.0 {
                xs.push(sa.cell(channel, x, y) as f64);
                ys.push(sb.cell(channel, x, y) as f64);
                us.push((x - x0) as f64 / uw);
                vs.push((y - y0) as f64 / vh);
            }
        }
    }

    // Clip rounds use the ordinary y-on-x line: its slope is biased toward 0
    // by noise in x, but it is stable against the one-sided outliers (stars,
    // hot pixels) whose rejection is the only job of these rounds.
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

    // Final reported line. The slope is measured on *detrended* values: each
    // side's own best-fit plane over the overlap is subtracted first, so the
    // gain comes from non-planar shared structure (stars, nebulosity
    // texture), which scales with gain — while smooth gradients, shared or
    // per-panel, drop out instead of aliasing into the slope. (A purely
    // planar overlap is honestly gain-unidentifiable: plane·g + o is
    // indistinguishable from a gradient difference; the surfaces stage owns
    // those.) The gain is trusted only when the detrended sides genuinely
    // co-vary, correlation r ≥ IDENT_MIN_R — pure noise, and structure
    // visible to only one panel, both measure r ≈ 0. Identifiable edges get
    // the symmetric (Deming, equal noise variances) slope — the positive-
    // discriminant root, sign following vxy — which is free of the y-on-x
    // attenuation bias; the offset always matches the raw mean levels.
    let mut identifiable = false;
    if stats[0] >= 2.0 {
        let n = stats[0];
        let (mx, my) = (stats[1] / n, stats[2] / n);
        let (vxx, vyy, vxy) = detrended_moments(&xs, &ys, &us, &vs, &keep);
        let corr2 = vxy * vxy / (vxx * vyy).max(f64::MIN_POSITIVE);
        if vxx > (stats[3] / n) * 1e-12 && corr2 >= IDENT_MIN_R * IDENT_MIN_R {
            let d = vyy - vxx;
            gain = (d + (d * d + 4.0 * vxy * vxy).sqrt()) / (2.0 * vxy);
            offset = my - gain * mx;
            identifiable = true;
        } else {
            gain = 1.0;
            offset = my - mx;
        }
        let mut sq = 0.0f64;
        for i in 0..xs.len() {
            if keep[i] {
                let r = ys[i] - (gain * xs[i] + offset);
                sq += r * r;
            }
        }
        rms = (sq / n).sqrt();
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
        gain_identifiable: identifiable,
    }
}

/// Central second moments of the kept `(x, y)` pairs after subtracting each
/// side's own best-fit plane `c0 + c1·u + c2·v` over the normalized overlap
/// coordinates. With fewer kept cells than comfortably constrain a plane the
/// detrend degrades to plain mean subtraction.
fn detrended_moments(
    xs: &[f64],
    ys: &[f64],
    us: &[f64],
    vs: &[f64],
    keep: &[bool],
) -> (f64, f64, f64) {
    let idx: Vec<usize> = (0..xs.len()).filter(|&i| keep[i]).collect();
    let n = idx.len() as f64;
    if n < 2.0 {
        return (0.0, 0.0, 0.0);
    }
    let plane = |ws: &[f64]| -> Option<[f64; 3]> {
        if idx.len() < 8 {
            return None;
        }
        let mut a = [0.0f64; 9];
        let mut b = [0.0f64; 3];
        for &i in &idx {
            let row = [1.0, us[i], vs[i]];
            for (r, &cr) in row.iter().enumerate() {
                for (c, &cc) in row.iter().enumerate() {
                    a[r * 3 + c] += cr * cc;
                }
                b[r] += cr * ws[i];
            }
        }
        solve_dense(&mut a, &mut b, 3)
            .ok()
            .map(|z| [z[0], z[1], z[2]])
    };
    let (px, py) = (plane(xs), plane(ys));
    let eval = |p: &Option<[f64; 3]>, mean: f64, i: usize| -> f64 {
        match p {
            Some(c) => c[0] + c[1] * us[i] + c[2] * vs[i],
            None => mean,
        }
    };
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    for &i in &idx {
        sx += xs[i];
        sy += ys[i];
    }
    let (mx, my) = (sx / n, sy / n);
    let (mut vxx, mut vyy, mut vxy) = (0.0f64, 0.0f64, 0.0f64);
    for &i in &idx {
        let rx = xs[i] - eval(&px, mx, i);
        let ry = ys[i] - eval(&py, my, i);
        vxx += rx * rx;
        vyy += ry * ry;
        vxy += rx * ry;
    }
    (vxx / n, vyy / n, vxy / n)
}

/// Accumulate one weighted least-squares row `coeffs·z = rhs` into the
/// normal equations `ata·z = atb` (only the listed sparse coefficients).
fn add_row(ata: &mut [f64], atb: &mut [f64], n: usize, coeffs: &[(usize, f64)], rhs: f64) {
    for &(i, ci) in coeffs {
        for &(j, cj) in coeffs {
            ata[i * n + j] += ci * cj;
        }
        atb[i] += ci * rhs;
    }
}

/// One channel's global adjustment, in two decoupled stages.
///
/// **Gains** are node potentials in log space: each identifiable edge
/// contributes `λ_a − λ_b = ln(gain)` (`λ = ln g`), weighted by the edge's
/// relative cell count (a slope measured on 5× the cells earns 5× the say —
/// tiny corner overlaps must not drag on well-measured side overlaps). The
/// system is solved to an L1 (least-absolute-deviations) objective via IRLS:
/// on a potential problem L1 concentrates any loop inconsistency onto the
/// fewest edges, so a slope contaminated by structure only one panel sees —
/// residual vignetting, a reflection — is outvoted by the consistent
/// majority instead of dragging its whole neighbourhood (which is what a
/// least-squares solve does: it spreads the error around the loop until the
/// culprit's residual looks no worse than its neighbours'). A weak
/// [`GAIN_RIDGE`] pulls every λ toward 0 (g toward 1), decisive only for
/// panels no identifiable edge reaches; reference panels are gauged λ = 0.
///
/// **Offsets** then follow linearly from [`offset_solve`] with the gains
/// held fixed.
fn global_solve(
    channel: u32,
    fits: &[EdgeFit],
    refs: &[usize],
    n_panels: usize,
) -> Result<(Vec<f64>, Vec<f64>)> {
    let contributing = || {
        fits.iter()
            .filter(|f| f.channel == channel && f.stats[0] > 0.0)
    };
    let mean_n = {
        let (mut sum, mut cnt) = (0.0f64, 0u64);
        for f in contributing() {
            sum += f.stats[0];
            cnt += 1;
        }
        if cnt == 0 { 1.0 } else { sum / cnt as f64 }
    };

    // (panel a, panel b, ln gain, base weight) per identifiable edge with a
    // usable (positive, finite) slope.
    let gain_rows: Vec<(usize, usize, f64, f64)> = contributing()
        .filter(|f| f.gain_identifiable && f.gain.is_finite() && f.gain > 0.0)
        .map(|f| (f.a, f.b, f.gain.ln(), f.stats[0] / mean_n))
        .collect();

    let solve_potentials = |robust: &[f64]| -> Result<Vec<f64>> {
        let mut a = vec![0.0f64; n_panels * n_panels];
        let mut b = vec![0.0f64; n_panels];
        for (&(i, j, ln_gain, w_base), &rw) in gain_rows.iter().zip(robust) {
            let w = (w_base * rw).sqrt(); // row scale = sqrt of LS weight
            add_row(&mut a, &mut b, n_panels, &[(i, w), (j, -w)], w * ln_gain);
        }
        for p in 0..n_panels {
            a[p * n_panels + p] += GAIN_RIDGE; // toward λ = 0 (g = 1)
        }
        for &r in refs {
            for c in 0..n_panels {
                a[r * n_panels + c] = 0.0;
            }
            a[r * n_panels + r] = 1.0;
            b[r] = 0.0;
        }
        solve_dense(&mut a, &mut b, n_panels)
    };

    let mut robust = vec![1.0f64; gain_rows.len()];
    let mut lambda = solve_potentials(&robust)?;
    for _ in 1..IRLS_ROUNDS {
        if gain_rows.is_empty() {
            break;
        }
        // IRLS toward L1: weight 1/|loop residual|, floored so consistent
        // edges (|r| ≤ L1_FLOOR ≈ 0.01% gain error) saturate, then
        // normalized so the most consistent edge keeps weight 1 (the
        // absolute scale must not drift against the ridge).
        let mut next: Vec<f64> = gain_rows
            .iter()
            .map(|&(i, j, ln_gain, _)| {
                let r = (lambda[i] - lambda[j] - ln_gain).abs();
                L1_FLOOR / r.max(L1_FLOOR)
            })
            .collect();
        let wmax = next
            .iter()
            .fold(0.0f64, |m, &w| m.max(w))
            .max(f64::MIN_POSITIVE);
        let mut changed = false;
        for (w, rw) in next.iter_mut().zip(&robust) {
            *w /= wmax;
            changed |= (*w - rw).abs() > 1e-3 * rw.max(*w);
        }
        robust = next;
        if !changed {
            break;
        }
        lambda = solve_potentials(&robust)?;
    }

    let mut gains: Vec<f64> = lambda.iter().map(|&l| l.exp()).collect();
    // The gauge is exact by definition; enforce it against LU round-off.
    for &r in refs {
        gains[r] = 1.0;
    }
    let offsets = offset_solve(channel, fits, refs, n_panels, &gains)?;
    Ok((gains, offsets))
}

/// Offset solve with the gains held fixed: minimize the corrected mean-level
/// mismatch `(g_a·mean(x) + o_a) − (g_b·mean(y) + o_b)` over all edges (a
/// graph Laplacian in the offsets), reference panels gauged to `o = 0`.
/// [`GainMode::Unity`] passes all-ones gains.
fn offset_solve(
    channel: u32,
    fits: &[EdgeFit],
    refs: &[usize],
    n_panels: usize,
    gains: &[f64],
) -> Result<Vec<f64>> {
    let mut a = vec![0.0f64; n_panels * n_panels];
    let mut b = vec![0.0f64; n_panels];
    for f in fits
        .iter()
        .filter(|f| f.channel == channel && f.stats[0] > 0.0)
    {
        // o_a − o_b = g_b·mean(y) − g_a·mean(x)
        let d = (gains[f.b] * f.stats[2] - gains[f.a] * f.stats[1]) / f.stats[0];
        add_row(&mut a, &mut b, n_panels, &[(f.a, 1.0), (f.b, -1.0)], d);
    }
    for &r in refs {
        for c in 0..n_panels {
            a[r * n_panels + c] = 0.0;
        }
        a[r * n_panels + r] = 1.0;
        b[r] = 0.0;
    }
    let mut o = solve_dense(&mut a, &mut b, n_panels)?;
    for &r in refs {
        o[r] = 0.0; // exact gauge (see global_solve)
    }
    Ok(o)
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
        // 400 cells knocked far off the line. The signal carries a bilinear
        // term so it stays non-planar (a purely planar overlap is honestly
        // gain-unidentifiable under the detrended fit).
        let (w8, h8) = (20u32, 20u32);
        let xval =
            |x: u32, y: u32| 0.01 + 1e-4 * (y * 20 + x) as f32 + 2e-3 * (x * y) as f32 / 400.0;
        let a = summary_from(w8, h8, [0, 0, 20, 20], xval);
        let b = summary_from(w8, h8, [0, 0, 20, 20], |x, y| {
            let v = 2.0 * xval(x, y) + 0.01;
            if (y * 20 + x) % 20 == 0 { v + 0.2 } else { v } // 20 outliers
        });

        let graph = OverlapGraph::build(&[a.clone(), b.clone()]);
        assert_eq!(graph.edges.len(), 1);
        let phot = solve(&[a, b], &graph, GainMode::Fit).unwrap();
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
        // (panel 0 has the largest coverage → reference, g=1, o=0). The truth
        // carries a bilinear term so overlaps stay non-planar and the gains
        // identifiable under the detrended fit.
        let (w8, h8) = (60u32, 20u32);
        let truth = |x: u32, y: u32| {
            0.02 + 5e-4 * x as f32 + 2e-4 * y as f32 + 1e-4 * (x * y) as f32 / 20.0
        };
        let applied: [(f32, f32); 3] = [(1.1, 0.004), (0.8, -0.002), (1.3, 0.01)];
        let regions = [[0, 0, 26, 20], [15, 0, 40, 20], [30, 0, 55, 20]];
        let summaries: Vec<L8Summary> = regions
            .iter()
            .zip(&applied)
            .map(|(&r, &(g, o))| summary_from(w8, h8, r, |x, y| truth(x, y) * g + o))
            .collect();

        let graph = OverlapGraph::build(&summaries);
        assert_eq!(graph.edges.len(), 2, "chain 0–1–2, ends disjoint");
        let phot = solve(&summaries, &graph, GainMode::Fit).unwrap();
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
        let phot = solve(&[a, b], &graph, GainMode::Fit).unwrap();
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
        let phot = solve(&[a, b], &graph, GainMode::Fit).unwrap();
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

    /// Deterministic pseudo-noise: uniform in [−amp, amp] from a tiny LCG.
    fn lcg(seed: u64) -> impl FnMut(f64) -> f64 {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        move |amp: f64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let u = (state >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
            (2.0 * u - 1.0) * amp
        }
    }

    #[test]
    fn edge_fit_resists_noise_attenuation() {
        // Signal variance only ~2× the per-panel noise variance and a true
        // gain of 2: OLS y-on-x attenuates the slope to ~1.4, a symmetric
        // errors-in-variables fit must stay near 2.
        let (w8, h8) = (40u32, 40u32);
        let mut noise = lcg(7);
        let mut truth = vec![0.0f32; (w8 * h8) as usize];
        let mut xv = vec![0.0f32; (w8 * h8) as usize];
        let mut yv = vec![0.0f32; (w8 * h8) as usize];
        for i in 0..truth.len() {
            let t = 0.01 + noise(3e-5); // sky with cell-scale structure
            truth[i] = t as f32;
            xv[i] = (t + noise(2e-5)) as f32;
            yv[i] = (2.0 * t + 0.01 + noise(2e-5)) as f32;
        }
        let a = summary_from(w8, h8, [0, 0, w8, h8], |x, y| xv[(y * w8 + x) as usize]);
        let b = summary_from(w8, h8, [0, 0, w8, h8], |x, y| yv[(y * w8 + x) as usize]);

        let graph = OverlapGraph::build(&[a.clone(), b.clone()]);
        let phot = solve(&[a, b], &graph, GainMode::Fit).unwrap();
        let f = &phot.edge_fits[0];
        assert!(f.gain_identifiable, "signal is 2× noise: identifiable");
        assert!(
            (1.8..2.2).contains(&f.gain),
            "gain = {} (OLS would give ~1.4)",
            f.gain
        );
    }

    #[test]
    fn noise_dominated_edge_falls_back_to_level_match() {
        // No shared structure at all — the two panels see independent noise
        // on flat backgrounds 0.005 apart. Gain is unmeasurable: the fit must
        // say so and match the mean levels at gain 1.
        let (w8, h8) = (40u32, 40u32);
        let mut noise = lcg(11);
        let vals: Vec<(f32, f32)> = (0..w8 * h8)
            .map(|_| ((0.001 + noise(2e-5)) as f32, (0.006 + noise(2e-5)) as f32))
            .collect();
        let a = summary_from(w8, h8, [0, 0, w8, h8], |x, y| vals[(y * w8 + x) as usize].0);
        let b = summary_from(w8, h8, [0, 0, w8, h8], |x, y| vals[(y * w8 + x) as usize].1);

        let graph = OverlapGraph::build(&[a.clone(), b.clone()]);
        let phot = solve(&[a, b], &graph, GainMode::Fit).unwrap();
        let f = &phot.edge_fits[0];
        assert!(!f.gain_identifiable, "pure noise must not identify a gain");
        assert_eq!(f.gain, 1.0);
        assert!((f.offset - 0.005).abs() < 5e-6, "offset = {}", f.offset);
    }

    #[test]
    fn global_solve_does_not_collapse_gains_over_noise_chain() {
        // Five panels in a row whose overlaps carry nothing but noise on
        // differing flat backgrounds — the historical failure collapsed every
        // gain toward 0 by chaining attenuated slopes. All gains must stay at
        // 1 and the corrected levels must agree across each overlap.
        let (w8, h8) = (100u32, 20u32);
        let bases = [0.001f64, 0.0062, 0.0009, 0.0058, 0.0011];
        let mut noise = lcg(23);
        let mut cells = vec![vec![0.0f32; (w8 * h8) as usize]; bases.len()];
        for (p, base) in bases.iter().enumerate() {
            for cell in cells[p].iter_mut() {
                *cell = (base + noise(2e-5)) as f32;
            }
        }
        // Panel p covers columns [20p, 20p+25) — 5-cell overlaps.
        let summaries: Vec<L8Summary> = (0..bases.len())
            .map(|p| {
                let x0 = 20 * p as u32;
                let x1 = (20 * p as u32 + 25).min(w8);
                summary_from(w8, h8, [x0, 0, x1, h8], |x, y| {
                    cells[p][(y * w8 + x) as usize]
                })
            })
            .collect();

        let graph = OverlapGraph::build(&summaries);
        assert_eq!(graph.edges.len(), 4, "chain of adjacent overlaps");
        let phot = solve(&summaries, &graph, GainMode::Fit).unwrap();
        for (p, &g) in phot.gains[0].iter().enumerate() {
            assert!((g - 1.0).abs() < 0.05, "panel {p} gain {g} drifted from 1");
        }
        // Corrected mean levels agree across every overlap.
        for f in &phot.edge_fits {
            let (mx, my) = (f.stats[1] / f.stats[0], f.stats[2] / f.stats[0]);
            let la = phot.gains[0][f.a] * mx + phot.offsets[0][f.a];
            let lb = phot.gains[0][f.b] * my + phot.offsets[0][f.b];
            assert!(
                (la - lb).abs() < 1e-5,
                "edge {}-{} corrected levels {la} vs {lb}",
                f.a,
                f.b
            );
        }
    }

    #[test]
    fn loop_consistency_overrides_contaminated_edge() {
        // Four identity panels (true gain 1 everywhere) in a ring, strong
        // shared texture in every overlap — but panel 3 carries extra
        // *unshared* structure inside its overlap with panel 2 (think
        // residual vignetting or a reflection), which inflates that edge's
        // fitted slope well away from 1. The three clean edges close the
        // loop at gain 1, so the robust global solve must reject the
        // contaminated edge's slope instead of splitting the difference.
        let (w8, h8) = (60u32, 60u32);
        let mut noise = lcg(97);
        let texture: Vec<f64> = (0..(w8 * h8) as usize)
            .map(|_| 0.01 + noise(3e-4))
            .collect();
        let contamination: Vec<f64> = (0..(w8 * h8) as usize).map(|_| noise(3e-4)).collect();
        // 2×2 grid with 4-cell overlap bands: the cycle 0–1–3–2–0 reaches
        // panel 3 both through the clean 1–3 edge and the contaminated 2–3
        // edge (extra structure confined to x < 32 below the 4-corner
        // region, so only panel 3's overlap with panel 2 sees it).
        let t = |x: u32, y: u32, extra: bool| {
            let i = (y * w8 + x) as usize;
            (texture[i] + if extra { 0.8 * contamination[i] } else { 0.0 }) as f32
        };
        let summaries = vec![
            summary_from(w8, h8, [0, 0, 32, 32], |x, y| t(x, y, false)),
            summary_from(w8, h8, [28, 0, 60, 32], |x, y| t(x, y, false)),
            summary_from(w8, h8, [0, 28, 32, 60], |x, y| t(x, y, false)),
            summary_from(w8, h8, [28, 28, 60, 60], |x, y| t(x, y, x < 32 && y >= 32)),
        ];
        let graph = OverlapGraph::build(&summaries);
        assert!(graph.edges.len() >= 4, "grid with shared texture");
        let phot = solve(&summaries, &graph, GainMode::Fit).unwrap();
        let bad = phot
            .edge_fits
            .iter()
            .find(|f| f.a == 2 && f.b == 3)
            .expect("contaminated edge fitted");
        assert!(
            (bad.gain - 1.0).abs() > 0.1,
            "the contaminated edge must actually mis-fit (gain {}), or this \
             test exercises nothing",
            bad.gain
        );
        for (p, &g) in phot.gains[0].iter().enumerate() {
            assert!(
                (g - 1.0).abs() < 0.03,
                "panel {p} gain {g}: loop consistency must override the \
                 contaminated edge"
            );
        }
    }

    #[test]
    fn signal_edge_recovers_gain_through_noise_neighbours() {
        // Panel 1 is a 0.8×-scaled copy of panel 0 over strong shared
        // structure (identifiable edge); panel 2 shares only noise with
        // panel 1. The solve must recover g1 = 1/0.8 from the signal edge
        // while g2 stays pinned near 1 instead of inheriting garbage.
        let (w8, h8) = (60u32, 20u32);
        let mut noise = lcg(41);
        let truth: Vec<f64> = (0..(w8 * h8) as usize)
            .map(|_| 0.01 + noise(3e-4))
            .collect();
        let n2: Vec<f64> = (0..(w8 * h8) as usize).map(|_| noise(2e-5)).collect();
        let n3: Vec<f64> = (0..(w8 * h8) as usize).map(|_| noise(2e-5)).collect();
        let summaries = vec![
            summary_from(w8, h8, [0, 0, 26, h8], |x, y| {
                truth[(y * w8 + x) as usize] as f32
            }),
            summary_from(w8, h8, [15, 0, 40, h8], |x, y| {
                (0.8 * truth[(y * w8 + x) as usize]) as f32
            }),
            // Overlaps panel 1 in [30, 40): flat + own noise, no shared signal.
            summary_from(w8, h8, [30, 0, 55, h8], |x, y| {
                (0.008 + n2[(y * w8 + x) as usize] + n3[(y * w8 + x) as usize]) as f32
            }),
        ];
        let graph = OverlapGraph::build(&summaries);
        assert_eq!(graph.edges.len(), 2);
        let phot = solve(&summaries, &graph, GainMode::Fit).unwrap();
        assert_eq!(phot.gains[0][0], 1.0, "panel 0 is the gauge");
        assert!(
            (phot.gains[0][1] - 1.25).abs() < 0.05,
            "signal edge gain: {}",
            phot.gains[0][1]
        );
        assert!(
            (phot.gains[0][2] - 1.0).abs() < 0.05,
            "noise-linked panel gain: {}",
            phot.gains[0][2]
        );
    }

    #[test]
    fn unity_mode_fixes_gains_and_matches_levels() {
        // Same chain-transform setup the fit-mode test uses, but per-panel
        // *gain* perturbations must be ignored: unity mode pins g=1 and still
        // matches corrected mean levels across overlaps via offsets alone.
        let (w8, h8) = (60u32, 20u32);
        let truth = |x: u32, y: u32| 0.02 + 5e-4 * x as f32 + 2e-4 * y as f32;
        let applied: [(f32, f32); 3] = [(1.0, 0.004), (1.0, -0.002), (1.0, 0.01)];
        let regions = [[0, 0, 26, 20], [15, 0, 40, 20], [30, 0, 55, 20]];
        let summaries: Vec<L8Summary> = regions
            .iter()
            .zip(&applied)
            .map(|(&r, &(g, o))| summary_from(w8, h8, r, |x, y| truth(x, y) * g + o))
            .collect();

        let graph = OverlapGraph::build(&summaries);
        let phot = solve(&summaries, &graph, GainMode::Unity).unwrap();
        assert_eq!(phot.gains[0], vec![1.0, 1.0, 1.0]);
        assert_eq!(phot.offsets[0][0], 0.0, "gauge panel");
        assert!((phot.offsets[0][1] - 0.006).abs() < 1e-6);
        assert!((phot.offsets[0][2] - (-0.006)).abs() < 1e-6);
    }

    #[test]
    fn edge_fit_without_identifiable_flag_loads_as_identifiable() {
        // Sessions written before the flag existed must load as identifiable.
        let json = r#"{"a":0,"b":1,"channel":0,"gain":1.1,"offset":0.0,
                       "n":10,"rms":1e-5,"stats":[10.0,1.0,2.0,3.0,4.0,5.0]}"#;
        let f: EdgeFit = serde_json::from_str(json).unwrap();
        assert!(f.gain_identifiable);
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
                gain_identifiable: true,
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
