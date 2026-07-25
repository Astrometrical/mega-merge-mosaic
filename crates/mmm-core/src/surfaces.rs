//! Residual surface correction: per-panel low-order 2-D polynomial fields
//! `s_i(x, y)` (order ≤ 2 over normalized canvas coordinates) fitted per
//! channel to the overlap residuals that remain *after* the global photometric
//! gain/offset solve, solved globally so corrections agree around loops.
//! Blend applies `v' = g·v + o + s_i(x, y)`.
//!
//! Signal-protection guard rail (from the phase-1 user validation, see
//! `docs/DESIGN.md` §POC results): the fit must never absorb *signal*
//! differences — only background cells steer it. Per panel and channel a cell
//! is background iff its corrected L8 mean ≤ median + 3×MAD over the panel's
//! covered cells; residual cells failing that in either panel are excluded,
//! survivors are sigma-clipped at 2.5σ, a small ridge (λ = 1e-3, normalized by
//! cell counts) pulls every surface toward zero, and the reference panel's
//! constant term is gauged to 0. Per-panel max|s| and the background MAD are
//! recorded so `mmm report` can flag runaway corrections (max|s| > 5×MAD).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::linalg::solve_dense;
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::summary::{BLOCK, L8Summary};
use crate::{Error, Result};

/// Background-cell exclusion: cells brighter than median + K·MAD are signal.
const BG_MAD_K: f64 = 3.0;
/// Residual sigma-clip threshold, in sigmas.
const CLIP_SIGMA: f64 = 2.5;
/// Sigma-clip rounds over each edge's residuals.
const CLIP_ROUNDS: usize = 2;
/// Absolute floor on the clip threshold so exact-fit float noise survives.
const CLIP_FLOOR: f64 = 1e-7;
/// Ridge weight pulling every surface toward zero (normalized by cell counts).
const RIDGE_LAMBDA: f64 = 1e-3;
/// Absolute diagonal stabilizer resolving directions no data constrains.
const STABILIZER: f64 = 1e-9;
/// `mmm report` warns when max|s| exceeds this multiple of the background MAD.
pub const WARN_MAD_FACTOR: f64 = 5.0;

/// Per-panel, per-channel residual correction surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Surfaces {
    /// 0 = constant, 1 = plane, 2 = quadratic.
    pub order: u32,
    /// `[channel][panel][n_terms]`; terms `1, x, y, x², xy, y²` over
    /// normalized canvas coords `x = X/canvas_w`, `y = Y/canvas_h`.
    pub coeffs: Vec<Vec<Vec<f64>>>,
    /// Diagnostics: max |s| over each panel's covered footprint,
    /// `[channel][panel]` — lets `mmm report` flag runaway corrections.
    #[serde(default)]
    pub max_abs_s: Vec<Vec<f64>>,
    /// Diagnostics: background MAD of the corrected L8 means, `[channel][panel]`.
    #[serde(default)]
    pub bg_mad: Vec<Vec<f64>>,
}

/// Number of polynomial terms for a surface order.
pub fn n_terms(order: u32) -> usize {
    match order {
        0 => 1,
        1 => 3,
        _ => 6,
    }
}

/// Basis vector `[1, x, y, x², xy, y²]` truncated to `t` terms.
#[inline]
fn basis(t: usize, x: f64, y: f64, phi: &mut [f64; 6]) {
    phi[0] = 1.0;
    if t >= 3 {
        phi[1] = x;
        phi[2] = y;
    }
    if t == 6 {
        phi[3] = x * x;
        phi[4] = x * y;
        phi[5] = y * y;
    }
}

impl Surfaces {
    /// Evaluate `s` for `(ch, panel)` at normalized canvas coords `(x, y)`.
    #[inline]
    pub fn eval(&self, ch: usize, panel: usize, x: f64, y: f64) -> f64 {
        let c = &self.coeffs[ch][panel];
        let mut v = c[0];
        if c.len() >= 3 {
            v += c[1] * x + c[2] * y;
        }
        if c.len() == 6 {
            v += (c[3] * x + c[4] * y) * x + c[5] * y * y;
        }
        v
    }

    /// Persist as JSON (conventionally `analysis/surfaces.json`).
    pub fn save(&self, p: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::format(p, format!("serialize surfaces: {e}")))?;
        std::fs::write(p, json).map_err(|e| Error::io(p, e))
    }

    /// Load surfaces previously written by [`Surfaces::save`].
    pub fn load(p: &Path) -> Result<Surfaces> {
        let json = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
        serde_json::from_str(&json).map_err(|e| Error::format(p, format!("bad surfaces: {e}")))
    }
}

/// Fit all panels' surfaces per channel from the post-photometry overlap
/// residuals on the L8 grid.
///
/// Per edge and channel, every L8 cell fully covered by both panels yields a
/// residual `r = (g_i·x + o_i) − (g_j·y + o_j)` at the cell center. Background
/// exclusion drops cells that are signal-dominated in *either* panel, then the
/// survivors are sigma-clipped at 2.5σ about the edge's mean residual. The
/// remaining cells feed a global least squares over all panels' coefficients,
/// minimizing `Σ_e ⟨(r + s_i − s_j)²⟩_e + λ Σ_p ⟨s_p²⟩_p` (each edge and each
/// panel ridge normalized by its cell count so areas don't dominate), with the
/// reference panel's constant term gauged to 0 per connected component.
pub fn fit_surfaces(
    summaries: &[L8Summary],
    graph: &OverlapGraph,
    phot: &Photometry,
    canvas: (u64, u64, u64),
    order: u32,
) -> Result<Surfaces> {
    if summaries.is_empty() {
        return Ok(Surfaces::default());
    }
    let channels = summaries[0].channels as usize;
    let n_panels = summaries.len();
    let t = n_terms(order);
    let (cw, ch) = (canvas.0 as f64, canvas.1 as f64);

    // Gauge panels: must match photometry's choice — the largest-coverage
    // panel (count of fully covered L8 cells) of each connected component,
    // ties to the lowest id.
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

    // Normalized canvas coordinates of an L8 cell center.
    let cell_xy = |x8: u32, y8: u32| -> (f64, f64) {
        (
            (x8 as f64 + 0.5) * BLOCK as f64 / cw,
            (y8 as f64 + 0.5) * BLOCK as f64 / ch,
        )
    };

    let mut coeffs = Vec::with_capacity(channels);
    let mut max_abs_s = Vec::with_capacity(channels);
    let mut bg_mads = Vec::with_capacity(channels);

    for c in 0..channels {
        let gain = |p: usize| {
            phot.gains
                .get(c)
                .and_then(|v| v.get(p))
                .copied()
                .unwrap_or(1.0)
        };
        let offs = |p: usize| {
            phot.offsets
                .get(c)
                .and_then(|v| v.get(p))
                .copied()
                .unwrap_or(0.0)
        };

        // Guard rail: per-panel background threshold median + 3×MAD over the
        // corrected means of the panel's fully covered cells.
        let mut thresholds = vec![f64::INFINITY; n_panels];
        let mut mads = vec![0.0f64; n_panels];
        for (p, s) in summaries.iter().enumerate() {
            let mut vals: Vec<f64> = Vec::new();
            for y8 in 0..s.h8 {
                for x8 in 0..s.w8 {
                    if s.cov(x8, y8) == 1.0 {
                        vals.push(gain(p) * s.cell(c as u32, x8, y8) as f64 + offs(p));
                    }
                }
            }
            if let Some((med, mad)) = median_mad(&mut vals) {
                thresholds[p] = med + BG_MAD_K * mad;
                mads[p] = mad;
            }
        }

        // Assemble the (n_panels·t)² normal equations.
        let n = n_panels * t;
        let mut a = vec![0.0f64; n * n];
        let mut b = vec![0.0f64; n];
        let mut phi = [0.0f64; 6];

        for e in &graph.edges {
            let (sa, sb) = (&summaries[e.a], &summaries[e.b]);
            let (gi, oi) = (gain(e.a), offs(e.a));
            let (gj, oj) = (gain(e.b), offs(e.b));

            // Background cells of this edge with their residuals.
            let mut cells: Vec<(f64, f64, f64)> = Vec::new(); // (x, y, r)
            for y8 in e.bbox8[1]..e.bbox8[3] {
                for x8 in e.bbox8[0]..e.bbox8[2] {
                    if sa.cov(x8, y8) < 1.0 || sb.cov(x8, y8) < 1.0 {
                        continue;
                    }
                    let ci = gi * sa.cell(c as u32, x8, y8) as f64 + oi;
                    let cj = gj * sb.cell(c as u32, x8, y8) as f64 + oj;
                    // Signal in either panel excludes the cell.
                    if ci > thresholds[e.a] || cj > thresholds[e.b] {
                        continue;
                    }
                    let (x, y) = cell_xy(x8, y8);
                    cells.push((x, y, ci - cj));
                }
            }

            // Sigma-clip survivors at 2.5σ about the edge's mean residual.
            let mut keep = vec![true; cells.len()];
            for _ in 0..CLIP_ROUNDS {
                let m = keep.iter().filter(|&&k| k).count();
                if m < 2 {
                    break;
                }
                let mean = cells
                    .iter()
                    .zip(&keep)
                    .filter(|&(_, &k)| k)
                    .map(|((_, _, r), _)| r)
                    .sum::<f64>()
                    / m as f64;
                let var = cells
                    .iter()
                    .zip(&keep)
                    .filter(|&(_, &k)| k)
                    .map(|((_, _, r), _)| (r - mean) * (r - mean))
                    .sum::<f64>()
                    / m as f64;
                let thr = (CLIP_SIGMA * var.sqrt()).max(CLIP_FLOOR);
                for (k, (_, _, r)) in keep.iter_mut().zip(&cells) {
                    *k = (r - mean).abs() <= thr;
                }
            }

            let m = keep.iter().filter(|&&k| k).count();
            if m == 0 {
                continue;
            }
            let w = 1.0 / m as f64;
            let (bi, bj) = (e.a * t, e.b * t);
            for ((x, y, r), _) in cells.iter().zip(&keep).filter(|&(_, &k)| k) {
                basis(t, *x, *y, &mut phi);
                for u in 0..t {
                    let pu = w * phi[u];
                    for v in 0..t {
                        let puv = pu * phi[v];
                        a[(bi + u) * n + (bi + v)] += puv;
                        a[(bj + u) * n + (bj + v)] += puv;
                        a[(bi + u) * n + (bj + v)] -= puv;
                        a[(bj + u) * n + (bi + v)] -= puv;
                    }
                    b[bi + u] -= pu * r;
                    b[bj + u] += pu * r;
                }
            }
        }

        // Ridge toward zero over each panel's own footprint (normalized by
        // its cell count): keeps unconstrained directions — including the
        // whole surface of edge-less panels — pinned at zero.
        for (p, s) in summaries.iter().enumerate() {
            let mut m = 0u64;
            let mut pp = [0.0f64; 36];
            for y8 in 0..s.h8 {
                for x8 in 0..s.w8 {
                    if s.cov(x8, y8) < 1.0 {
                        continue;
                    }
                    let (x, y) = cell_xy(x8, y8);
                    basis(t, x, y, &mut phi);
                    for u in 0..t {
                        for v in 0..t {
                            pp[u * t + v] += phi[u] * phi[v];
                        }
                    }
                    m += 1;
                }
            }
            if m == 0 {
                continue;
            }
            let w = RIDGE_LAMBDA / m as f64;
            let bp = p * t;
            for u in 0..t {
                for v in 0..t {
                    a[(bp + u) * n + (bp + v)] += w * pp[u * t + v];
                }
            }
        }

        // Tiny absolute stabilizer: directions the data and footprint ridge
        // cannot see (e.g. a panel whose footprint has fewer cells than
        // terms) resolve to exactly zero (their rhs is zero) instead of
        // making the system singular. Negligible (1e-9 relative) elsewhere.
        for k in 0..n {
            a[k * n + k] += STABILIZER * (1.0 + a[k * n + k]);
        }

        // Gauge: reference panels' constant terms are exactly 0.
        for &r in &refs {
            let row = r * t;
            for col in 0..n {
                a[row * n + col] = 0.0;
            }
            a[row * n + row] = 1.0;
            b[row] = 0.0;
        }

        let z = solve_dense(&mut a, &mut b, n)?;
        let ch_coeffs: Vec<Vec<f64>> = (0..n_panels)
            .map(|p| z[p * t..(p + 1) * t].to_vec())
            .collect();

        // Diagnostics: max |s| over each panel's covered footprint.
        let mut ch_max = vec![0.0f64; n_panels];
        for (p, s) in summaries.iter().enumerate() {
            let cf = &ch_coeffs[p];
            for y8 in 0..s.h8 {
                for x8 in 0..s.w8 {
                    if s.cov(x8, y8) < 1.0 {
                        continue;
                    }
                    let (x, y) = cell_xy(x8, y8);
                    basis(t, x, y, &mut phi);
                    let v: f64 = (0..t).map(|u| cf[u] * phi[u]).sum();
                    ch_max[p] = ch_max[p].max(v.abs());
                }
            }
        }

        coeffs.push(ch_coeffs);
        max_abs_s.push(ch_max);
        bg_mads.push(mads);
    }

    Ok(Surfaces {
        order,
        coeffs,
        max_abs_s,
        bg_mad: bg_mads,
    })
}

/// Count of fully covered L8 cells — must mirror photometry's gauge measure.
fn full_cov_cells(s: &L8Summary) -> u64 {
    s.coverage.iter().filter(|&&c| c == 1.0).count() as u64
}

/// Median and MAD (median absolute deviation) of `vals`; `None` when empty.
/// Sorts `vals` in place.
fn median_mad(vals: &mut [f64]) -> Option<(f64, f64)> {
    if vals.is_empty() {
        return None;
    }
    let med = median_of_sorted(sorted(vals));
    let mut devs: Vec<f64> = vals.iter().map(|&v| (v - med).abs()).collect();
    let mad = median_of_sorted(sorted(&mut devs));
    Some((med, mad))
}

fn sorted(v: &mut [f64]) -> &[f64] {
    v.sort_by(f64::total_cmp);
    v
}

fn median_of_sorted(v: &[f64]) -> f64 {
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze;
    use crate::synth::{SynthResult, SynthSpec, generate};
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-surf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 2×2 synthetic mosaic with per-panel gradient planes, unit gains, zero
    /// offsets, no noise. `n_stars` varies per test.
    fn grad_spec(n_stars: usize) -> SynthSpec {
        SynthSpec {
            canvas: (256, 192),
            channels: 1,
            grid: (2, 2),
            overlap_frac: 0.4,
            n_stars,
            noise_sigma: 0.0,
            panel_gain_range: (1.0, 1.0),
            panel_offset_range: (0.0, 0.0),
            panel_gradient_range: (-0.01, 0.01),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![],
            panel_spike_angle: vec![],
            panel_defects: vec![],
            mid_blobs: 0,
            shift_blobs: false,
            seed: 7,
        }
    }

    /// Analyze the panels and load back the summaries + graph.
    fn analyzed(dir: &Path, res: &SynthResult) -> (Vec<L8Summary>, OverlapGraph, (u64, u64, u64)) {
        let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
        let summaries: Vec<L8Summary> = (0..res.panel_paths.len())
            .map(|id| L8Summary::read(&session.summary_path(id)).unwrap())
            .collect();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        (summaries, graph, session.canvas)
    }

    fn identity_phot(n_panels: usize, ch: usize) -> Photometry {
        Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0; n_panels]; ch],
            offsets: vec![vec![0.0; n_panels]; ch],
        }
    }

    /// Max |injected gradient| over any panel window — the "amplitude" the
    /// 5%-residual bound is measured against.
    fn injected_amplitude(res: &SynthResult, canvas: (u64, u64)) -> f64 {
        let (w, h) = (canvas.0 as f64, canvas.1 as f64);
        let mut amp = 0.0f64;
        for (p, &(a, b, c)) in res.applied_grad.iter().enumerate() {
            let [x0, y0, x1, y1] = res.windows[p];
            for &x in &[x0, x1] {
                for &y in &[y0, y1] {
                    let v = a as f64 + b as f64 * x as f64 / w + c as f64 * y as f64 / h;
                    amp = amp.max(v.abs());
                }
            }
        }
        amp
    }

    /// Post-correction residual RMS over all edges' shared background cells.
    fn overlap_residual_rms(
        summaries: &[L8Summary],
        graph: &OverlapGraph,
        phot: &Photometry,
        surf: &Surfaces,
        canvas: (u64, u64, u64),
    ) -> f64 {
        let (cw, chh) = (canvas.0 as f64, canvas.1 as f64);
        let mut sq = 0.0f64;
        let mut n = 0u64;
        for e in &graph.edges {
            let (sa, sb) = (&summaries[e.a], &summaries[e.b]);
            for y8 in e.bbox8[1]..e.bbox8[3] {
                for x8 in e.bbox8[0]..e.bbox8[2] {
                    if sa.cov(x8, y8) < 1.0 || sb.cov(x8, y8) < 1.0 {
                        continue;
                    }
                    let x = (x8 as f64 + 0.5) * BLOCK as f64 / cw;
                    let y = (y8 as f64 + 0.5) * BLOCK as f64 / chh;
                    let ca = phot.gains[0][e.a] * sa.cell(0, x8, y8) as f64
                        + phot.offsets[0][e.a]
                        + surf.eval(0, e.a, x, y);
                    let cb = phot.gains[0][e.b] * sb.cell(0, x8, y8) as f64
                        + phot.offsets[0][e.b]
                        + surf.eval(0, e.b, x, y);
                    sq += (ca - cb) * (ca - cb);
                    n += 1;
                }
            }
        }
        assert!(n > 100, "too few overlap cells: {n}");
        (sq / n as f64).sqrt()
    }

    /// Mandatory test 1: injected per-panel gradients, no stars/noise → the
    /// fitted surfaces cancel the injected differences: post-correction
    /// overlap residual RMS < 5% of the injected gradient amplitude.
    #[test]
    fn surfaces_cancel_injected_gradients() {
        let dir = tmpdir("cancel");
        let res = generate(&grad_spec(0), &dir.join("panels")).unwrap();
        let (summaries, graph, canvas) = analyzed(&dir, &res);
        assert_eq!(graph.edges.len(), 6, "2x2 grid: 4 side edges + 2 diagonals");

        // Gains are truly 1 and offsets 0 by construction, so identity
        // photometry is exact and the whole residual is the gradient field.
        let phot = identity_phot(4, 1);
        let surf = fit_surfaces(&summaries, &graph, &phot, canvas, 2).unwrap();
        assert_eq!(surf.order, 2);
        assert_eq!(surf.coeffs.len(), 1);
        assert_eq!(surf.coeffs[0].len(), 4);
        assert_eq!(surf.coeffs[0][0].len(), 6);

        let amp = injected_amplitude(&res, (canvas.0, canvas.1));
        assert!(
            amp > 1e-3,
            "spec should inject a visible gradient, amp = {amp}"
        );

        let rms_before = overlap_residual_rms(&summaries, &graph, &phot, &zero_like(&surf), canvas);
        let rms_after = overlap_residual_rms(&summaries, &graph, &phot, &surf, canvas);
        eprintln!(
            "amp {amp:.4e}  overlap residual RMS before {rms_before:.4e} after {rms_after:.4e}"
        );
        assert!(
            rms_after < 0.05 * amp,
            "post-correction residual RMS {rms_after:.4e} >= 5% of amplitude {amp:.4e}"
        );
        assert!(
            rms_after < 0.2 * rms_before,
            "correction barely improved the residual"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn zero_like(s: &Surfaces) -> Surfaces {
        Surfaces {
            order: s.order,
            coeffs: s
                .coeffs
                .iter()
                .map(|ch| ch.iter().map(|p| vec![0.0; p.len()]).collect())
                .collect(),
            max_abs_s: vec![],
            bg_mad: vec![],
        }
    }

    /// Mandatory test 2: same mosaic with bright stars added → the fitted
    /// surfaces are unchanged within 20% (signal exclusion works; star flux
    /// must not bend the fit). A deliberate 5% gain error on panel 0 makes
    /// star cells carry huge residuals — exactly what the guard rail excludes.
    #[test]
    fn star_signal_does_not_bend_surfaces() {
        let dir_a = tmpdir("nostars");
        let dir_b = tmpdir("stars");
        let res_a = generate(&grad_spec(0), &dir_a.join("panels")).unwrap();
        let res_b = generate(&grad_spec(30), &dir_b.join("panels")).unwrap();
        // Same seed, perturbation RNG independent of stars: identical gradients.
        assert_eq!(res_a.applied_grad, res_b.applied_grad);

        let (sum_a, graph_a, canvas) = analyzed(&dir_a, &res_a);
        let (sum_b, graph_b, _) = analyzed(&dir_b, &res_b);

        // Deliberately wrong gain on panel 0: residuals on its edges contain
        // 0.05·flux, enormous at star cells, negligible in background.
        let mut phot = identity_phot(4, 1);
        phot.gains[0][0] = 1.05;

        let surf_a = fit_surfaces(&sum_a, &graph_a, &phot, canvas, 2).unwrap();
        let surf_b = fit_surfaces(&sum_b, &graph_b, &phot, canvas, 2).unwrap();

        // Compare the fitted fields over each panel's window on a grid.
        let (w, h) = (canvas.0 as f64, canvas.1 as f64);
        let mut max_field = 0.0f64;
        for p in 0..4 {
            let [x0, y0, x1, y1] = res_a.windows[p];
            for y in (y0..y1).step_by(8) {
                for x in (x0..x1).step_by(8) {
                    let v = surf_a.eval(0, p, x as f64 / w, y as f64 / h);
                    max_field = max_field.max(v.abs());
                }
            }
        }
        assert!(
            max_field > 1e-3,
            "no-star surfaces should be non-trivial: {max_field:.3e}"
        );

        let mut max_diff = 0.0f64;
        for p in 0..4 {
            let [x0, y0, x1, y1] = res_a.windows[p];
            for y in (y0..y1).step_by(8) {
                for x in (x0..x1).step_by(8) {
                    let (xn, yn) = (x as f64 / w, y as f64 / h);
                    let d = (surf_a.eval(0, p, xn, yn) - surf_b.eval(0, p, xn, yn)).abs();
                    max_diff = max_diff.max(d);
                }
            }
        }
        eprintln!("max |s| without stars {max_field:.4e}, max star-induced change {max_diff:.4e}");
        assert!(
            max_diff <= 0.2 * max_field,
            "stars changed the surfaces by {max_diff:.4e} > 20% of {max_field:.4e}"
        );

        std::fs::remove_dir_all(&dir_a).unwrap();
        std::fs::remove_dir_all(&dir_b).unwrap();
    }

    #[test]
    fn eval_matches_polynomial_terms() {
        let s = Surfaces {
            order: 2,
            coeffs: vec![vec![vec![0.5, 1.0, -2.0, 3.0, 4.0, -5.0]]],
            max_abs_s: vec![],
            bg_mad: vec![],
        };
        let (x, y) = (0.3, 0.7);
        let expect = 0.5 + 1.0 * x - 2.0 * y + 3.0 * x * x + 4.0 * x * y - 5.0 * y * y;
        assert!((s.eval(0, 0, x, y) - expect).abs() < 1e-12);

        let s1 = Surfaces {
            order: 1,
            coeffs: vec![vec![vec![0.1, 0.2, 0.3]]],
            max_abs_s: vec![],
            bg_mad: vec![],
        };
        assert!((s1.eval(0, 0, x, y) - (0.1 + 0.2 * x + 0.3 * y)).abs() < 1e-12);

        let s0 = Surfaces {
            order: 0,
            coeffs: vec![vec![vec![0.25]]],
            max_abs_s: vec![],
            bg_mad: vec![],
        };
        assert_eq!(s0.eval(0, 0, x, y), 0.25);
    }

    #[test]
    fn surfaces_round_trip_through_json() {
        let s = Surfaces {
            order: 2,
            coeffs: vec![vec![vec![1.0; 6], vec![2.0; 6]]],
            max_abs_s: vec![vec![0.001, 0.002]],
            bg_mad: vec![vec![0.0005, 0.0006]],
        };
        let dir = tmpdir("json");
        let path = dir.join("surfaces.json");
        s.save(&path).unwrap();
        let r = Surfaces::load(&path).unwrap();
        assert_eq!(r.order, 2);
        assert_eq!(r.coeffs, s.coeffs);
        assert_eq!(r.max_abs_s, s.max_abs_s);
        assert_eq!(r.bg_mad, s.bg_mad);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Gauge + no-op sanity: with zero residuals (identical panels), the fit
    /// must return ~zero surfaces and the reference constant term exactly 0.
    #[test]
    fn zero_residuals_give_zero_surfaces() {
        let dir = tmpdir("zero");
        let mut spec = grad_spec(0);
        spec.panel_gradient_range = (0.0, 0.0);
        let res = generate(&spec, &dir.join("panels")).unwrap();
        let (summaries, graph, canvas) = analyzed(&dir, &res);
        let phot = identity_phot(4, 1);
        let surf = fit_surfaces(&summaries, &graph, &phot, canvas, 2).unwrap();
        for p in 0..4 {
            for (ti, &c) in surf.coeffs[0][p].iter().enumerate() {
                assert!(c.abs() < 1e-6, "panel {p} term {ti} = {c:.3e}, expected ~0");
            }
            assert!(surf.max_abs_s[0][p] < 1e-6);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
