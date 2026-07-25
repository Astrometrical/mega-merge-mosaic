//! Opt-in global background flatten.
//!
//! The residual surfaces (see [`crate::surfaces`]) deliberately correct only
//! *differences between panels* — a large-scale gradient common to every
//! panel (real sky glow, IFN-scale illumination) is invisible to them by
//! design. This module fits one low-order polynomial per channel to the
//! *merged* L8 background and lets the blender subtract its varying part,
//! `f(x, y) − f(canvas center)`, during output — the central level is
//! preserved, only the tilt/curvature goes.
//!
//! Guard rails (the fit must never absorb signal):
//! - only L8 cells *fully covered* by at least one panel are considered;
//! - cells covered by any covering panel's connected star mask
//!   ([`crate::seam::star_mask`] — star cores, spike arms, structure) are
//!   excluded;
//! - cells whose merged value exceeds the background median + 3×MAD (per
//!   channel, over the mask-clear cells) are excluded;
//! - if fewer than [`MIN_BG_FRAC`] of the considered cells survive, the fit
//!   **refuses** with a clear error rather than flatten a signal-dominated
//!   (pure-nebula) mosaic.

use std::path::Path;

use rayon::prelude::*;

use crate::blend::{corrected_cell_means, panel_correction_terms};
use crate::overlap::distance_map;
use crate::photometry::Photometry;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::{Surfaces, n_terms};
use crate::{Error, Result};

/// Bright-cell exclusion: merged cells above median + K·MAD are signal.
const BG_MAD_K: f64 = 3.0;

/// Refuse to flatten when fewer than this fraction of the considered
/// (fully-covered) cells are background.
pub const MIN_BG_FRAC: f64 = 0.2;

/// One fitted global background polynomial per channel.
#[derive(Debug, Clone)]
pub struct Flatten {
    /// 1 = plane, 2 = quadratic.
    pub order: u32,
    /// `[channel][n_terms]`; terms `1, x, y, x², xy, y²` over normalized
    /// canvas coords `x = X/canvas_w`, `y = Y/canvas_h` (the convention of
    /// [`crate::surfaces`]).
    pub coeffs: Vec<Vec<f64>>,
    /// Diagnostic: fraction of considered cells that were background.
    pub bg_frac: f64,
}

impl Flatten {
    /// Evaluate `f` for `ch` at normalized canvas coords `(xn, yn)`.
    #[inline]
    pub fn eval(&self, ch: usize, xn: f64, yn: f64) -> f64 {
        let c = &self.coeffs[ch];
        let mut v = c[0];
        if c.len() >= 3 {
            v += c[1] * xn + c[2] * yn;
        }
        if c.len() == 6 {
            v += (c[3] * xn + c[4] * yn) * xn + c[5] * yn * yn;
        }
        v
    }

    /// What the blender subtracts: `f(x, y) − f(canvas center)`. The canvas
    /// center is `(0.5, 0.5)` in normalized coords by construction.
    #[inline]
    pub fn delta(&self, ch: usize, xn: f64, yn: f64) -> f64 {
        self.eval(ch, xn, yn) - self.eval(ch, 0.5, 0.5)
    }

    /// Fold the subtraction of `delta` into per-panel surface terms (padded
    /// `1, x, y, x², xy, y²` layout, one entry per channel): subtracting the
    /// same global field from every panel's correction is exactly subtracting
    /// it from the blended output, on every blend path.
    pub(crate) fn apply_to_surf(&self, surf: &mut [[f64; 6]]) {
        for (c, s) in surf.iter_mut().enumerate() {
            let center = self.eval(c, 0.5, 0.5);
            for (k, &v) in self.coeffs[c].iter().enumerate() {
                s[k] -= v;
            }
            s[0] += center;
        }
    }
}

/// Fit the global background polynomial (order 1 or 2) per channel from the
/// merged L8 background.
///
/// Per L8 cell fully covered by ≥1 panel, the merged value is the
/// distance-weighted blend of the panels' corrected cell means (gain, offset,
/// residual surface — the blend-time corrections). Cells star-masked in any
/// covering panel are dropped, then per channel cells above the background
/// median + 3×MAD (over the survivors). The rest feed a per-channel least
/// squares over normalized cell-center coordinates. Errors — refusing to
/// flatten — when fewer than [`MIN_BG_FRAC`] of the considered cells are
/// background. `ctx` is only used for error messages.
pub fn fit_flatten(
    summaries: &[L8Summary],
    masks: &[Vec<bool>],
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    canvas: (u64, u64, u64),
    order: u32,
    ctx: &Path,
) -> Result<Flatten> {
    if !(1..=2).contains(&order) {
        return Err(Error::format(
            ctx,
            format!("flatten order must be 1 or 2, got {order}"),
        ));
    }
    if summaries.is_empty() {
        return Err(Error::format(
            ctx,
            "flatten needs at least one panel summary",
        ));
    }
    let nch = canvas.2 as usize;
    let (w8, h8) = (summaries[0].w8 as usize, summaries[0].h8 as usize);
    let cells = w8 * h8;

    // Blend-time corrected cell means and chamfer distance weights per panel.
    let corr: Vec<Vec<f32>> = summaries
        .par_iter()
        .enumerate()
        .map(|(p, s)| {
            let (g, o, t) = panel_correction_terms(phot, surfaces, p, nch);
            corrected_cell_means(s, &g, &o, &t, canvas)
        })
        .collect();
    let dist: Vec<Vec<f32>> = summaries
        .par_iter()
        .map(|s| {
            let mut d = distance_map(&s.coverage, s.w8, s.h8);
            // A panel with no uncovered cell at all yields INFINITY; make it a
            // large finite weight so the merge stays NaN-free.
            for v in &mut d {
                if !v.is_finite() {
                    *v = 1e30;
                }
            }
            d
        })
        .collect();

    // Merged background: distance-weighted blend over fully covering panels;
    // a cell star-masked in any covering panel is excluded outright.
    let mut merged = vec![0.0f64; nch * cells];
    let mut clear = vec![false; cells];
    let mut considered = 0u64;
    let mut acc = vec![0.0f64; nch];
    for i in 0..cells {
        let mut any = false;
        let mut masked = false;
        let mut wsum = 0.0f64;
        acc.iter_mut().for_each(|a| *a = 0.0);
        for (p, s) in summaries.iter().enumerate() {
            if s.coverage[i] < 1.0 {
                continue;
            }
            any = true;
            if masks[p][i] {
                masked = true;
                break;
            }
            // Deep-interior cells dominate, like the blend's feather weights;
            // fully covered cells have distance ≥ 1, so no floor is needed.
            let w = dist[p][i].max(1.0) as f64;
            wsum += w;
            for (c, a) in acc.iter_mut().enumerate() {
                *a += w * corr[p][c * cells + i] as f64;
            }
        }
        if !any {
            continue;
        }
        considered += 1;
        if !masked && wsum > 0.0 {
            clear[i] = true;
            for c in 0..nch {
                merged[c * cells + i] = acc[c] / wsum;
            }
        }
    }
    if considered == 0 {
        return Err(Error::format(
            ctx,
            "flatten found no fully-covered L8 cells",
        ));
    }

    // Per-channel bright cut: merged > median + 3×MAD (over mask-clear cells)
    // in any channel marks the cell as signal.
    let mut bright = vec![false; cells];
    for c in 0..nch {
        let mut vals: Vec<f64> = (0..cells)
            .filter(|&i| clear[i])
            .map(|i| merged[c * cells + i])
            .collect();
        let Some((med, mad)) = median_mad(&mut vals) else {
            continue;
        };
        let thr = med + BG_MAD_K * mad;
        for (i, b) in bright.iter_mut().enumerate() {
            if clear[i] && merged[c * cells + i] > thr {
                *b = true;
            }
        }
    }

    let bg: Vec<usize> = (0..cells).filter(|&i| clear[i] && !bright[i]).collect();
    let bg_frac = bg.len() as f64 / considered as f64;
    if bg_frac < MIN_BG_FRAC {
        return Err(Error::format(
            ctx,
            format!(
                "refusing to flatten: only {:.0}% of the covered cells are background \
                 (need ≥ {:.0}%) — the mosaic is signal-dominated (nebula-heavy); \
                 run without --flatten",
                100.0 * bg_frac,
                100.0 * MIN_BG_FRAC
            ),
        ));
    }

    // Per-channel least squares over the background cells, at normalized
    // cell-center coordinates (the surfaces.rs convention).
    let t = n_terms(order);
    let (cw, chh) = (canvas.0 as f64, canvas.1 as f64);
    let mut coeffs = Vec::with_capacity(nch);
    for c in 0..nch {
        let mut a = vec![0.0f64; t * t];
        let mut b = vec![0.0f64; t];
        let mut phi = [0.0f64; 6];
        for &i in &bg {
            let (x8, y8) = (i % w8, i / w8);
            let xn = (x8 as f64 + 0.5) * BLOCK as f64 / cw;
            let yn = (y8 as f64 + 0.5) * BLOCK as f64 / chh;
            basis(t, xn, yn, &mut phi);
            let v = merged[c * cells + i];
            for u in 0..t {
                for w in 0..t {
                    a[u * t + w] += phi[u] * phi[w];
                }
                b[u] += phi[u] * v;
            }
        }
        coeffs.push(crate::linalg::solve_dense(&mut a, &mut b, t)?);
    }
    tracing::info!(order, bg_frac, n_bg = bg.len(), "global flatten fitted");
    Ok(Flatten {
        order,
        coeffs,
        bg_frac,
    })
}

/// Basis vector `[1, x, y, x², xy, y²]` truncated to `t` terms — the same
/// convention as `crate::surfaces` (whose helper is private).
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

/// Median and MAD of `vals` (sorted in place); `None` when empty.
fn median_mad(vals: &mut [f64]) -> Option<(f64, f64)> {
    fn median(v: &mut [f64]) -> f64 {
        v.sort_by(f64::total_cmp);
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            0.5 * (v[n / 2 - 1] + v[n / 2])
        }
    }
    if vals.is_empty() {
        return None;
    }
    let med = median(vals);
    let mut devs: Vec<f64> = vals.iter().map(|&v| (v - med).abs()).collect();
    let mad = median(&mut devs);
    Some((med, mad))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn identity_phot(n_panels: usize, ch: usize) -> Photometry {
        Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0; n_panels]; ch],
            offsets: vec![vec![0.0; n_panels]; ch],
        }
    }

    /// Fully covered single-panel summary whose cell means follow `f` at the
    /// normalized cell centers.
    fn field_summary(
        w8: u32,
        h8: u32,
        canvas: (u64, u64),
        f: impl Fn(f64, f64) -> f64,
    ) -> L8Summary {
        let mut s = L8Summary::zeroed(w8, h8, 1);
        for y8 in 0..h8 {
            for x8 in 0..w8 {
                let i = (y8 * w8 + x8) as usize;
                s.coverage[i] = 1.0;
                let xn = (x8 as f64 + 0.5) * BLOCK as f64 / canvas.0 as f64;
                let yn = (y8 as f64 + 0.5) * BLOCK as f64 / canvas.1 as f64;
                s.mean[i] = f(xn, yn) as f32;
            }
        }
        s
    }

    #[test]
    fn fit_recovers_planar_background() {
        let canvas = (128u64, 96u64, 1u64);
        let field = |xn: f64, yn: f64| 0.03 + 0.02 * xn - 0.01 * yn;
        let s = field_summary(16, 12, (canvas.0, canvas.1), field);
        let masks = vec![vec![false; 16 * 12]];
        let phot = identity_phot(1, 1);

        let f = fit_flatten(&[s], &masks, &phot, None, canvas, 1, &PathBuf::new()).unwrap();
        assert_eq!(f.order, 1);
        assert_eq!(f.coeffs.len(), 1);
        assert_eq!(f.coeffs[0].len(), 3);
        for (got, want) in f.coeffs[0].iter().zip([0.03, 0.02, -0.01]) {
            assert!((got - want).abs() < 1e-6, "coeff {got} vs {want}");
        }
        // delta preserves the central level and reproduces the tilt.
        assert!(f.delta(0, 0.5, 0.5).abs() < 1e-12);
        let d = f.delta(0, 0.9, 0.1);
        let want = field(0.9, 0.1) - field(0.5, 0.5);
        assert!((d - want).abs() < 1e-6, "delta {d} vs {want}");
        assert!((f.bg_frac - 1.0).abs() < 1e-12);
    }

    #[test]
    fn fit_order2_recovers_quadratic_background() {
        let canvas = (256u64, 256u64, 1u64);
        let field = |xn: f64, yn: f64| {
            0.02 + 0.01 * xn + 0.02 * yn - 0.015 * xn * xn + 0.005 * xn * yn + 0.01 * yn * yn
        };
        let s = field_summary(32, 32, (canvas.0, canvas.1), field);
        let masks = vec![vec![false; 32 * 32]];
        let phot = identity_phot(1, 1);

        let f = fit_flatten(&[s], &masks, &phot, None, canvas, 2, &PathBuf::new()).unwrap();
        assert_eq!(f.coeffs[0].len(), 6);
        for &(xn, yn) in &[(0.1, 0.2), (0.8, 0.9), (0.5, 0.1)] {
            let want = field(xn, yn) - field(0.5, 0.5);
            let got = f.delta(0, xn, yn);
            assert!(
                (got - want).abs() < 1e-6,
                "delta({xn},{yn}) {got} vs {want}"
            );
        }
    }

    #[test]
    fn bright_cells_do_not_steer_the_fit() {
        let canvas = (128u64, 96u64, 1u64);
        let field = |xn: f64, yn: f64| 0.03 + 0.02 * xn - 0.01 * yn;
        let mut s = field_summary(16, 12, (canvas.0, canvas.1), field);
        // A handful of nebula-bright cells, way above median + 3×MAD.
        for &i in &[17usize, 18, 33, 34, 100] {
            s.mean[i] = 0.8;
        }
        let masks = vec![vec![false; 16 * 12]];
        let phot = identity_phot(1, 1);

        let f = fit_flatten(&[s], &masks, &phot, None, canvas, 1, &PathBuf::new()).unwrap();
        for (got, want) in f.coeffs[0].iter().zip([0.03, 0.02, -0.01]) {
            assert!(
                (got - want).abs() < 1e-6,
                "bright cells bent the fit: coeff {got} vs {want}"
            );
        }
        assert!(f.bg_frac < 1.0, "the bright cells must have been excluded");
    }

    /// Mandatory refusal path: a signal-dominated (nebula-heavy) mosaic —
    /// star masks covering most of the covered cells — errors cleanly
    /// instead of fitting the nebula.
    #[test]
    fn refuses_signal_dominated_mosaic() {
        let canvas = (80u64, 80u64, 1u64);
        let s = field_summary(10, 10, (canvas.0, canvas.1), |_, _| 0.03);
        // 85 of 100 cells are structure.
        let mask: Vec<bool> = (0..100).map(|i| i < 85).collect();
        let phot = identity_phot(1, 1);

        let err = fit_flatten(&[s], &[mask], &phot, None, canvas, 1, &PathBuf::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("refusing to flatten") && err.contains("background"),
            "refusal must be clear about why: {err}"
        );
    }

    #[test]
    fn rejects_bad_order() {
        let canvas = (80u64, 80u64, 1u64);
        let s = field_summary(10, 10, (canvas.0, canvas.1), |_, _| 0.03);
        let masks = vec![vec![false; 100]];
        let phot = identity_phot(1, 1);
        for bad in [0u32, 3] {
            assert!(
                fit_flatten(
                    std::slice::from_ref(&s),
                    &masks,
                    &phot,
                    None,
                    canvas,
                    bad,
                    &PathBuf::new()
                )
                .is_err(),
                "order {bad} must be rejected"
            );
        }
    }

    /// Two overlapping panels with different corrected levels: the merged
    /// background is between them, weighted toward each panel's interior.
    #[test]
    fn merged_background_blends_covering_panels() {
        let canvas = (160u64, 40u64, 1u64);
        let (w8, h8) = (20u32, 5u32);
        let mk = |x_lo: u32, x_hi: u32, v: f32| {
            let mut s = L8Summary::zeroed(w8, h8, 1);
            for y in 0..h8 {
                for x in x_lo..x_hi {
                    let i = (y * w8 + x) as usize;
                    s.coverage[i] = 1.0;
                    s.mean[i] = v;
                }
            }
            s
        };
        // A = 0.02 on x8<12, B = 0.04 on x8≥8 (overlap x8∈[8,12)).
        let summaries = vec![mk(0, 12, 0.02), mk(8, 20, 0.04)];
        let masks = vec![vec![false; (w8 * h8) as usize]; 2];
        let phot = identity_phot(2, 1);

        let f = fit_flatten(&summaries, &masks, &phot, None, canvas, 1, &PathBuf::new()).unwrap();
        // The fitted plane rises from A's level to B's level along x.
        let left = f.eval(0, 0.15, 0.5);
        let right = f.eval(0, 0.85, 0.5);
        assert!(
            left < right,
            "plane must tilt from 0.02 toward 0.04: {left} vs {right}"
        );
        assert!(
            (0.01..=0.03).contains(&left),
            "left end near A's level: {left}"
        );
        assert!(
            (0.03..=0.05).contains(&right),
            "right end near B's level: {right}"
        );
    }
}
