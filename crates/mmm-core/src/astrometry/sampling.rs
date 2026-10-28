//! Sampling of an astrometric model onto lookup grids, and the decode-time
//! consistency check of a PixInsight projective/spline solution.
//!
//! The solutions themselves are decoded by mpp-core
//! ([`mpp_core::astrometry`]); `WcsModel` samples each direction once onto a
//! [`Grid2D`] (bicubic lookups are what the reprojection inner loop can
//! afford) and validates the solution against its own linear layer.

use mpp_core::astrometry::PiSolution;
use rayon::prelude::*;

use super::{Grid2D, expected_nodes};

/// Grid spacing in image pixels for the image→projection sampling. PixInsight
/// 1.9.5's own grid cache defaults to 16 px (1.9.4 files carried 8 px grids);
/// the distortion field is smooth enough that bicubic interpolation on a
/// 16 px lattice reproduces the model to well under 0.01″ (verified against
/// PixInsight's 8 px cache on the real Orion panels), at a quarter of the
/// spline evaluations.
pub(crate) const IMAGE_DELTA_PX: f64 = 16.0;

/// Decode-time validation of a projective/spline solution against its own
/// linear layer, evaluated directly on the model (a handful of evaluations;
/// the grids are sampled lazily later): the reference pixel must map to
/// ≈ (0, 0) and back, and the image corners must agree with the linear
/// solution within the distortion allowance.
pub(crate) fn validate_model(sol: &PiSolution, width: u64, height: u64) -> bool {
    let refimg = sol.reference_image;
    let (xi, eta) = sol.image_to_projection(refimg[0], refimg[1]);
    if !xi.is_finite() || !eta.is_finite() || xi.hypot(eta) > 0.01 {
        return false;
    }
    let (ix, iy) = sol.projection_to_image(0.0, 0.0);
    if !ix.is_finite() || !iy.is_finite() || (ix - refimg[0]).hypot(iy - refimg[1]) > 5.0 {
        return false;
    }
    let (w, h) = (width as f64, height as f64);
    let m = sol.linear;
    for (cx, cy) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
        let (gx, gy) = sol.image_to_projection(cx, cy);
        let (dx, dy) = (cx - refimg[0], cy - refimg[1]);
        let (lx, ly) = (m[0][0] * dx + m[0][1] * dy, m[1][0] * dx + m[1][1] * dy);
        if !gx.is_finite() || !gy.is_finite() || (gx - lx).hypot(gy - ly) > 0.05 {
            return false;
        }
    }
    true
}

/// Sample the image → projection direction onto a lookup grid over the image
/// bounds. `None` when a value is not finite.
pub(crate) fn sample_image_to_native(
    sol: &PiSolution,
    width: u64,
    height: u64,
    parallel: bool,
) -> Option<Grid2D> {
    sample_fn(
        |x, y| sol.image_to_projection(x, y),
        [0.0, 0.0, width as f64, height as f64],
        IMAGE_DELTA_PX,
        parallel,
    )
}

/// Sample the projection → image direction onto a lookup grid over the
/// projection-plane bounding box of the mapped image corners and edge
/// midpoints, padded by one cell. `None` when the linear scale is degenerate
/// or a value is not finite.
pub(crate) fn sample_native_to_image(
    sol: &PiSolution,
    width: u64,
    height: u64,
    parallel: bool,
) -> Option<Grid2D> {
    let delta = IMAGE_DELTA_PX * matrix_scale(sol.linear)?;
    let rect = native_rect(|x, y| sol.image_to_projection(x, y), width, height, delta);
    sample_fn(
        |xi, eta| sol.projection_to_image(xi, eta),
        rect,
        delta,
        parallel,
    )
}

/// The native-plane rectangle covering the image's forward-mapped corners and
/// edge midpoints, padded by one `delta`.
pub(crate) fn native_rect<F>(forward: F, width: u64, height: u64, delta: f64) -> [f64; 4]
where
    F: Fn(f64, f64) -> (f64, f64),
{
    let (w, h) = (width as f64, height as f64);
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in [
        (0.0, 0.0),
        (w, 0.0),
        (0.0, h),
        (w, h),
        (w / 2.0, 0.0),
        (w / 2.0, h),
        (0.0, h / 2.0),
        (w, h / 2.0),
    ] {
        let (xi, eta) = forward(x, y);
        x0 = x0.min(xi);
        y0 = y0.min(eta);
        x1 = x1.max(xi);
        y1 = y1.max(eta);
    }
    [x0 - delta, y0 - delta, x1 + delta, y1 + delta]
}

/// Pixel scale (deg/px) of a matrix: `sqrt(|det|)`; `None` unless positive and
/// finite.
pub(crate) fn matrix_scale(m: [[f64; 2]; 2]) -> Option<f64> {
    let s = (m[0][0] * m[1][1] - m[0][1] * m[1][0]).abs().sqrt();
    (s.is_finite() && s > 0.0).then_some(s)
}

/// Sample one 2-D map onto a grid. `parallel` uses the rayon pool (one row
/// per task); callers that may already be running *inside* a rayon worker
/// must pass `false` — a nested parallel sampling from a worker that other
/// workers are blocked waiting on (lazy `OnceLock` init) lets the
/// initializing thread steal one of those blocked jobs and deadlock.
///
/// `None` when the rectangle is degenerate or any sampled value is not finite.
pub(crate) fn sample_fn<F>(f: F, rect: [f64; 4], delta: f64, parallel: bool) -> Option<Grid2D>
where
    F: Fn(f64, f64) -> (f64, f64) + Sync,
{
    let cols = expected_nodes(rect[2] - rect[0], delta)?;
    let rows = expected_nodes(rect[3] - rect[1], delta)?;
    let n = rows as usize * cols as usize;
    let mut gx = vec![0.0; n];
    let mut gy = vec![0.0; n];
    let row = |r: usize, rx: &mut [f64], ry: &mut [f64]| {
        let y = rect[1] + r as f64 * delta;
        for (c, (ox, oy)) in rx.iter_mut().zip(ry.iter_mut()).enumerate() {
            let x = rect[0] + c as f64 * delta;
            let (vx, vy) = f(x, y);
            *ox = vx;
            *oy = vy;
        }
    };
    if parallel {
        gx.par_chunks_mut(cols as usize)
            .zip(gy.par_chunks_mut(cols as usize))
            .enumerate()
            .for_each(|(r, (rx, ry))| row(r, rx, ry));
    } else {
        for (r, (rx, ry)) in gx
            .chunks_mut(cols as usize)
            .zip(gy.chunks_mut(cols as usize))
            .enumerate()
        {
            row(r, rx, ry);
        }
    }
    if gx.iter().chain(&gy).any(|v| !v.is_finite()) {
        return None;
    }
    Some(Grid2D {
        rect,
        delta,
        rows,
        cols,
        gx,
        gy,
    })
}
