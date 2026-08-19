//! Synthetic ground-truth generator for tests and benches.
//!
//! Builds a known "sky" (smooth gradient background + Gaussian-PSF stars +
//! Gaussian noise), cuts it into overlapping grid panels, applies per-panel
//! gain/offset perturbations, and writes each panel as a full-canvas
//! monolithic XISF frame (zeros outside the panel window — the no-data
//! sentinel). The pipeline can then be validated against the exact truth.
//!
//! No external RNG dependency: xorshift64* + Box-Muller, deterministic for a
//! fixed seed.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Specification of the synthetic mosaic to generate.
#[derive(Debug, Clone)]
pub struct SynthSpec {
    /// Canvas dimensions `(width, height)` in pixels.
    pub canvas: (u64, u64),
    /// Number of channels (1 = mono, 3 = RGB-like).
    pub channels: u32,
    /// Panels across x down.
    pub grid: (u32, u32),
    /// Fraction of a grid cell added as overlap between neighbours (e.g. 0.25).
    pub overlap_frac: f64,
    /// Number of stars, amplitudes log-spaced from ~1.0 down to 0.02.
    pub n_stars: usize,
    /// Per-pixel Gaussian noise sigma (0.0 = noiseless).
    pub noise_sigma: f32,
    /// Per-panel gain drawn uniformly from this range, e.g. (0.7, 1.4).
    pub panel_gain_range: (f32, f32),
    /// Per-panel offset drawn uniformly from this range, e.g. (-0.01, 0.02).
    pub panel_offset_range: (f32, f32),
    /// Per-panel additive gradient plane `a + b·x/w + c·y/h` (normalized
    /// canvas coords) with `a`, `b`, `c` each drawn uniformly from this range;
    /// applied inside the window on top of gain/offset. `(0.0, 0.0)` is a
    /// strict no-op (phase-1 behavior).
    pub panel_gradient_range: (f32, f32),
    /// Common additive gradient plane `a + b·x/w + c·y/h` (normalized canvas
    /// coords) added to *every* panel inside its window, after gain/offset —
    /// a sky gradient shared by all panels (identical in overlaps, so the
    /// photometric solve and per-panel surfaces cannot see it; only the
    /// global flatten can remove it). NOT included in the returned `truth`.
    /// `(0.0, 0.0, 0.0)` is a strict no-op.
    pub global_gradient: (f32, f32, f32),
    /// Per-panel sub-pixel shift `(dx, dy)` of the *star positions only* —
    /// background and noise stay put. Simulates residual misregistration.
    /// Empty = no shift; otherwise must have one entry per panel.
    pub panel_shift: Vec<(f32, f32)>,
    /// Per-panel rotation offset (radians) of 4-armed diffraction spikes
    /// drawn on the brightest stars (amplitude ≥ [`SPIKE_MIN_AMP`]; arm
    /// length ∝ amplitude, ~1 px wide, additive). Empty = no spikes anywhere;
    /// otherwise one entry per panel. The truth carries spikes at angle 0;
    /// differing per-panel offsets simulate per-session camera rotation.
    pub panel_spike_angle: Vec<f32>,
    /// Number of mid-frequency Gaussian blobs (σ = [`BLOB_SIGMA_PX`] px ≈ 3
    /// L8 cells) added to the truth sky — extended structure between the
    /// detail band and the feather scale, faint enough to stay below the star
    /// mask, for the pyramid blend's ghost-reduction tests. Positions and
    /// amplitudes ([`BLOB_AMP_RANGE`]) come from an RNG stream independent of
    /// the star/noise/perturbation draws, so `0` is byte-identical to a spec
    /// without blobs.
    pub mid_blobs: usize,
    /// Apply `panel_shift` to the blob centers too (not only stars) —
    /// simulates misregistration of extended mid-scale structure.
    pub shift_blobs: bool,
    /// Optional giant textured core `(x, y, radius_px, amplitude)` added to
    /// the truth sky — a deterministic stand-in for a bright nebular core
    /// (M42): a dense cluster of compact Gaussians (σ = radius/8, spacing
    /// ~σ) under a broad envelope, so the whole region carries cell-scale
    /// detail energy and the star/structure mask floods across it. Placed in
    /// unshifted canvas coordinates, identical in every panel. `None` is
    /// byte-identical to a spec without a core.
    pub core: Option<(f64, f64, f64, f64)>,
    /// Single-panel defects `(panel, x, y, length_px, amplitude)`: a bright
    /// 1-px-wide *horizontal* line segment starting at `(x, y)` (length 1 =
    /// cosmic ray, longer = a satellite-trail piece), added to all channels
    /// *after* gain/offset/gradient, clipped to the panel's window. Simulates
    /// transients that survive stacking in exactly one panel.
    pub panel_defects: Vec<(usize, u64, u64, u32, f32)>,
    /// RNG seed; every output is deterministic in the spec including this.
    pub seed: u64,
}

/// Output of [`generate`]: the exact truth image plus the written panels and
/// the perturbations applied to each.
#[derive(Debug)]
pub struct SynthResult {
    /// Planar channel planes, canvas-sized (keep the canvas small: <= 1024^2).
    pub truth: Vec<f32>,
    /// The written panel files, in panel-id order.
    pub panel_paths: Vec<PathBuf>,
    /// Per-panel (gain, offset) actually applied.
    pub applied: Vec<(f32, f32)>,
    /// Per-panel gradient plane coefficients `(a, b, c)` actually applied:
    /// `a + b·x/w + c·y/h` over normalized canvas coords.
    pub applied_grad: Vec<(f32, f32, f32)>,
    /// Per-panel content window on the canvas: [x0, y0, x1, y1], exclusive.
    pub windows: Vec<[u64; 4]>,
    /// Stars that received diffraction spikes (when `panel_spike_angle` is
    /// non-empty): `(x, y, arm_length_px)` in unshifted canvas coordinates.
    pub spiked: Vec<(f64, f64, f64)>,
    /// Mid-frequency blobs `(x, y, amplitude)` in unshifted canvas
    /// coordinates (σ is always [`BLOB_SIGMA_PX`]).
    pub blobs: Vec<(f64, f64, f64)>,
}

/// Positive floor applied to truth (and to any covered panel pixel that would
/// otherwise land on exactly 0.0): zero is the no-data sentinel and must never
/// occur inside a panel's window.
const VALUE_FLOOR: f32 = 1e-4;

/// Stars at least this bright (peak amplitude) receive diffraction spikes
/// when `panel_spike_angle` is set.
pub const SPIKE_MIN_AMP: f64 = 0.25;

/// Spike arm length in pixels per unit of star amplitude.
pub const SPIKE_LEN_PER_AMP: f64 = 48.0;

/// Peak spike brightness relative to the star's amplitude; brightness falls
/// linearly to zero at the arm tip.
const SPIKE_REL_AMP: f64 = 0.35;

/// Gaussian half-width of a spike arm across its axis, in pixels (~1 px wide).
const SPIKE_SIGMA: f64 = 0.6;

/// Mid-frequency blob width: σ = 3 L8 cells — structure between the detail
/// band (< 8 px) and typical feather scales.
pub const BLOB_SIGMA_PX: f64 = 24.0;

/// Mid-frequency blob peak amplitudes are drawn uniformly from this range:
/// bright against the sky, but with cell-scale detail energy near the noise
/// median so the star mask leaves blobs in the base band.
pub const BLOB_AMP_RANGE: (f64, f64) = (0.06, 0.12);

/// xorshift64* PRNG with a Box-Muller Gaussian tap. Deterministic for a seed;
/// no external dependency.
struct Rng {
    state: u64,
    spare_gauss: Option<f64>,
}

impl Rng {
    fn new(seed: u64) -> Self {
        // xorshift state must be nonzero.
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self {
            state,
            spare_gauss: None,
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in [lo, hi).
    fn range_f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// Standard normal via Box-Muller (pairs; the second value is cached).
    fn next_gaussian(&mut self) -> f64 {
        if let Some(z) = self.spare_gauss.take() {
            return z;
        }
        let u1 = self.next_f64().max(f64::MIN_POSITIVE); // avoid ln(0)
        let u2 = self.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (std::f64::consts::TAU * u2).sin_cos();
        self.spare_gauss = Some(r * s);
        r * c
    }
}

/// Smooth gradient background: plane + low-frequency sine, per channel.
/// Values stay in ~0.014..0.046 (sane linear sky range).
fn background(x: u64, y: u64, c: u32, w: u64, h: u64) -> f32 {
    let fx = x as f64 / w as f64;
    let fy = y as f64 / h as f64;
    let phase = c as f64 * 0.7;
    let v = 0.02
        + 0.012 * fx
        + 0.008 * fy
        + 0.006
            * (std::f64::consts::TAU * (1.3 * fx + 0.15) + phase).sin()
            * (std::f64::consts::TAU * (0.9 * fy + 0.35)).sin();
    v as f32
}

/// Panel content window on the canvas: grid cell expanded by
/// `overlap_frac/2` of a cell on each side (adjacent windows overlap by
/// `overlap_frac` of a cell), clamped to the canvas. Exclusive upper bounds.
fn panel_window(spec: &SynthSpec, cx: u32, cy: u32) -> [u64; 4] {
    let (w, h) = spec.canvas;
    let cell_w = w as f64 / spec.grid.0 as f64;
    let cell_h = h as f64 / spec.grid.1 as f64;
    let pad_x = spec.overlap_frac * cell_w / 2.0;
    let pad_y = spec.overlap_frac * cell_h / 2.0;
    let x0 = (cx as f64 * cell_w - pad_x).floor().max(0.0) as u64;
    let x1 = (((cx + 1) as f64 * cell_w + pad_x).ceil() as u64).min(w);
    let y0 = (cy as f64 * cell_h - pad_y).floor().max(0.0) as u64;
    let y1 = (((cy + 1) as f64 * cell_h + pad_y).ceil() as u64).min(h);
    [x0, y0, x1, y1]
}

/// Generate the truth image and panel files into `dir`.
pub fn generate(spec: &SynthSpec, dir: &Path) -> Result<SynthResult> {
    let (w, h) = spec.canvas;
    if w == 0 || h == 0 || spec.channels == 0 || spec.grid.0 == 0 || spec.grid.1 == 0 {
        return Err(Error::format(
            dir,
            "SynthSpec canvas/channels/grid must all be nonzero",
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;

    let ch = spec.channels as usize;
    let plane = (w * h) as usize;
    let mut rng = Rng::new(spec.seed);

    // Stars: positions and sigma from the RNG, amplitudes log-spaced from the
    // brightest (~1.0) down to a faint floor, shared across channels.
    let stars: Vec<(f64, f64, f64, f64)> = (0..spec.n_stars)
        .map(|i| {
            let sx = rng.range_f64(0.0, w as f64);
            let sy = rng.range_f64(0.0, h as f64);
            let sigma = rng.range_f64(1.0, 3.0);
            let t = if spec.n_stars > 1 {
                i as f64 / (spec.n_stars - 1) as f64
            } else {
                0.0
            };
            let amp = 1.0 * (0.02f64 / 1.0).powf(t); // log-spaced 1.0 .. 0.02
            (sx, sy, sigma, amp)
        })
        .collect();

    /// Render the stars into a canvas-sized plane, centers shifted by `(dx, dy)`.
    fn render_stars(stars: &[(f64, f64, f64, f64)], w: u64, h: u64, dx: f64, dy: f64) -> Vec<f32> {
        let mut plane = vec![0.0f32; (w * h) as usize];
        for &(sx0, sy0, sigma, amp) in stars {
            let (sx, sy) = (sx0 + dx, sy0 + dy);
            let r = (4.0 * sigma).ceil() as i64;
            let (cxp, cyp) = (sx.round() as i64, sy.round() as i64);
            let inv_2s2 = 1.0 / (2.0 * sigma * sigma);
            for y in (cyp - r).max(0)..=(cyp + r).min(h as i64 - 1) {
                for x in (cxp - r).max(0)..=(cxp + r).min(w as i64 - 1) {
                    let ddx = x as f64 - sx;
                    let ddy = y as f64 - sy;
                    let v = amp * (-(ddx * ddx + ddy * ddy) * inv_2s2).exp();
                    plane[(y as u64 * w + x as u64) as usize] += v as f32;
                }
            }
        }
        plane
    }
    let star_plane = render_stars(&stars, w, h, 0.0, 0.0);
    // The giant core mimics a saturated nebular core (M42): a *flat*
    // saturated plateau (detail-free, so the detail-energy star mask leaves
    // it unmasked — an enclosed pocket) surrounded by a wide annulus of
    // compact Gaussian clumps whose cell-scale detail floods the mask across
    // the whole ring. This is the geometry behind the real-data base-fill
    // flood: the plateau pocket acts as a trusted fill source inside an
    // otherwise masked complex.
    let core_plane = spec.core.map(|(cx, cy, radius, amp)| {
        let sigma = 8.0f64.min(radius / 12.0).max(2.0);
        let step = sigma * 1.25;
        let n = (radius / step).ceil() as i64;
        let (r_in, skirt) = (0.25 * radius, 0.08 * radius);
        let mut clumps = Vec::new();
        for gy in -n..=n {
            for gx in -n..=n {
                // Offset alternate rows for a hex-ish packing.
                let x = cx + (gx as f64 + 0.5 * (gy.rem_euclid(2)) as f64) * step;
                let y = cy + gy as f64 * step * 0.87;
                let r2 = ((x - cx) * (x - cx) + (y - cy) * (y - cy)) / (radius * radius);
                if r2 > 1.0 || r2 < (r_in / radius) * (r_in / radius) {
                    continue;
                }
                clumps.push((x, y, amp * (-2.0 * r2).exp(), sigma));
            }
        }
        let mut plane = render_stars(&clumps, w, h, 0.0, 0.0);
        // Flat plateau with a short linear skirt (the skirt's cliff carries
        // detail and is masked; the flat interior is not).
        for y in 0..h {
            for x in 0..w {
                let r = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
                if r < r_in + skirt {
                    let t = ((r_in + skirt - r) / skirt).min(1.0);
                    plane[(y * w + x) as usize] += (amp * t) as f32;
                }
            }
        }
        plane
    });

    /// Render 4-armed diffraction spikes for the brightest stars: arms at
    /// `angle + k·π/2`, length [`SPIKE_LEN_PER_AMP`] × amplitude, Gaussian
    /// cross-section of width [`SPIKE_SIGMA`], brightness falling linearly to
    /// zero at the tip; star centers shifted by `(dx, dy)`. Additive.
    fn render_spikes(
        stars: &[(f64, f64, f64, f64)],
        w: u64,
        h: u64,
        dx: f64,
        dy: f64,
        angle: f64,
    ) -> Vec<f32> {
        let mut plane = vec![0.0f32; (w * h) as usize];
        let inv_2s2 = 1.0 / (2.0 * SPIKE_SIGMA * SPIKE_SIGMA);
        for &(sx0, sy0, _, amp) in stars {
            if amp < SPIKE_MIN_AMP {
                continue;
            }
            let (sx, sy) = (sx0 + dx, sy0 + dy);
            let len = SPIKE_LEN_PER_AMP * amp;
            let r = (len + 3.0).ceil() as i64;
            let (cxp, cyp) = (sx.round() as i64, sy.round() as i64);
            for y in (cyp - r).max(0)..=(cyp + r).min(h as i64 - 1) {
                for x in (cxp - r).max(0)..=(cxp + r).min(w as i64 - 1) {
                    let (px, py) = (x as f64 - sx, y as f64 - sy);
                    let mut v = 0.0f64;
                    for k in 0..4 {
                        let (s, c) = (angle + k as f64 * std::f64::consts::FRAC_PI_2).sin_cos();
                        let t = px * c + py * s; // along the arm
                        let d = py * c - px * s; // across the arm
                        if t < 0.0 || t > len {
                            continue;
                        }
                        v += amp * SPIKE_REL_AMP * (1.0 - t / len) * (-d * d * inv_2s2).exp();
                    }
                    plane[(y as u64 * w + x as u64) as usize] += v as f32;
                }
            }
        }
        plane
    }
    // Mid-frequency blobs: rendered exactly like (wide, faint) stars, from a
    // dedicated RNG stream so enabling them never perturbs the star, noise or
    // panel-perturbation draws (mid_blobs == 0 is byte-identical output).
    let mut brng = Rng::new(spec.seed ^ 0xB10B_B10B_B10B_B10B);
    let blobs: Vec<(f64, f64, f64)> = (0..spec.mid_blobs)
        .map(|_| {
            let bx = brng.range_f64(0.0, w as f64);
            let by = brng.range_f64(0.0, h as f64);
            let amp = brng.range_f64(BLOB_AMP_RANGE.0, BLOB_AMP_RANGE.1);
            (bx, by, amp)
        })
        .collect();
    let blob_stars = |dx: f64, dy: f64| -> Vec<(f64, f64, f64, f64)> {
        blobs
            .iter()
            .map(|&(bx, by, amp)| (bx + dx, by + dy, BLOB_SIGMA_PX, amp))
            .collect()
    };
    let blob_plane =
        (!blobs.is_empty()).then(|| render_stars(&blob_stars(0.0, 0.0), w, h, 0.0, 0.0));

    let has_spikes = !spec.panel_spike_angle.is_empty();
    let truth_spikes = has_spikes.then(|| render_spikes(&stars, w, h, 0.0, 0.0, 0.0));
    let spiked: Vec<(f64, f64, f64)> = if has_spikes {
        stars
            .iter()
            .filter(|s| s.3 >= SPIKE_MIN_AMP)
            .map(|s| (s.0, s.1, s.3 * SPIKE_LEN_PER_AMP))
            .collect()
    } else {
        Vec::new()
    };

    // Truth = background + stars + Gaussian noise, floored positive.
    let mut truth = vec![0.0f32; ch * plane];
    for c in 0..spec.channels {
        let out = &mut truth[c as usize * plane..(c as usize + 1) * plane];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let noise = spec.noise_sigma * rng.next_gaussian() as f32;
                let sp = truth_spikes.as_ref().map_or(0.0, |p| p[i]);
                let bl = blob_plane.as_ref().map_or(0.0, |p| p[i]);
                let co = core_plane.as_ref().map_or(0.0, |p| p[i]);
                let v = background(x, y, c, w, h) + star_plane[i] + sp + bl + co + noise;
                out[i] = v.max(VALUE_FLOOR);
            }
        }
    }

    // Panels: full-canvas frames, truth·gain + offset + gradient plane inside
    // the window, hard zeros elsewhere. Perturbations come from a *separate*
    // RNG stream so the drawn gain/offset/gradient values are invariant to
    // `n_stars`/noise draws — tests compare star vs star-free generations of
    // the same seed and need identical perturbations.
    let n_panels = (spec.grid.0 * spec.grid.1) as usize;
    if !spec.panel_shift.is_empty() && spec.panel_shift.len() != n_panels {
        return Err(Error::format(
            dir,
            format!(
                "panel_shift has {} entries but the grid has {n_panels} panels",
                spec.panel_shift.len()
            ),
        ));
    }
    if has_spikes && spec.panel_spike_angle.len() != n_panels {
        return Err(Error::format(
            dir,
            format!(
                "panel_spike_angle has {} entries but the grid has {n_panels} panels",
                spec.panel_spike_angle.len()
            ),
        ));
    }
    for &(dp, _, _, len, _) in &spec.panel_defects {
        if dp >= n_panels {
            return Err(Error::format(
                dir,
                format!("panel_defects names panel {dp} but the grid has {n_panels} panels"),
            ));
        }
        if len == 0 {
            return Err(Error::format(dir, "panel_defects length must be ≥ 1"));
        }
    }
    let mut prng = Rng::new(spec.seed ^ 0xC0FF_EE00_D15E_A5E5);
    let mut panel_paths = Vec::with_capacity(n_panels);
    let mut applied = Vec::with_capacity(n_panels);
    let mut applied_grad = Vec::with_capacity(n_panels);
    let mut windows = Vec::with_capacity(n_panels);
    let mut frame = vec![0.0f32; ch * plane];

    for cy in 0..spec.grid.1 {
        for cx in 0..spec.grid.0 {
            let id = (cy * spec.grid.0 + cx) as usize;
            let gain = prng.range_f64(
                spec.panel_gain_range.0 as f64,
                spec.panel_gain_range.1 as f64,
            ) as f32;
            let offset = prng.range_f64(
                spec.panel_offset_range.0 as f64,
                spec.panel_offset_range.1 as f64,
            ) as f32;
            let (glo, ghi) = (
                spec.panel_gradient_range.0 as f64,
                spec.panel_gradient_range.1 as f64,
            );
            let ga = prng.range_f64(glo, ghi) as f32;
            let gb = prng.range_f64(glo, ghi) as f32;
            let gc = prng.range_f64(glo, ghi) as f32;
            let [x0, y0, x1, y1] = panel_window(spec, cx, cy);

            // Star-only misregistration and per-panel spike rotation: replace
            // the truth's star (+spike) field with this panel's rendering
            // (background and noise are untouched).
            let shift = spec.panel_shift.get(id).copied().unwrap_or((0.0, 0.0));
            let angle = spec.panel_spike_angle.get(id).copied().unwrap_or(0.0);
            let star_delta: Option<Vec<f32>> = if shift == (0.0, 0.0) && angle == 0.0 {
                None
            } else {
                let shifted = render_stars(&stars, w, h, shift.0 as f64, shift.1 as f64);
                let mut d: Vec<f32> = shifted
                    .iter()
                    .zip(&star_plane)
                    .map(|(&s, &u)| s - u)
                    .collect();
                if let Some(tsp) = &truth_spikes {
                    let psp =
                        render_spikes(&stars, w, h, shift.0 as f64, shift.1 as f64, angle as f64);
                    for ((dv, &p), &t) in d.iter_mut().zip(&psp).zip(tsp) {
                        *dv += p - t;
                    }
                }
                if spec.shift_blobs
                    && shift != (0.0, 0.0)
                    && let Some(tbl) = &blob_plane
                {
                    let pbl =
                        render_stars(&blob_stars(shift.0 as f64, shift.1 as f64), w, h, 0.0, 0.0);
                    for ((dv, &p), &t) in d.iter_mut().zip(&pbl).zip(tbl) {
                        *dv += p - t;
                    }
                }
                Some(d)
            };

            let (gga, ggb, ggc) = spec.global_gradient;
            frame.fill(0.0);
            for c in 0..ch {
                let src = &truth[c * plane..(c + 1) * plane];
                let dst = &mut frame[c * plane..(c + 1) * plane];
                for y in y0..y1 {
                    let grad_y = ga + gga + (gc + ggc) * (y as f32 / h as f32);
                    for x in x0..x1 {
                        let i = (y * w + x) as usize;
                        let grad = grad_y + (gb + ggb) * (x as f32 / w as f32);
                        let star = star_delta.as_ref().map_or(0.0, |d| d[i]);
                        let mut v = (src[i] + star) * gain + offset + grad;
                        if v == 0.0 {
                            // Covered pixels must never be exactly 0 (no-data
                            // sentinel); nudge by a value below test tolerance.
                            v = 1e-6;
                        }
                        dst[i] = v;
                    }
                }
            }

            // Single-panel defects: additive on top of gain/offset/gradient
            // (a transient in the light frames survives the stack's scaling),
            // clipped to the window so no-data pixels stay exactly 0.
            for &(dp, defx, defy, len, amp) in &spec.panel_defects {
                if dp != id || defy < y0 || defy >= y1 {
                    continue;
                }
                for x in defx..defx.saturating_add(len as u64) {
                    if x < x0 || x >= x1 {
                        continue;
                    }
                    let i = (defy * w + x) as usize;
                    for c in 0..ch {
                        frame[c * plane + i] += amp;
                    }
                }
            }

            let path = dir.join(format!("panel_{id:02}.xisf"));
            write_xisf(&path, w, h, spec.channels as u64, &frame)?;
            panel_paths.push(path);
            applied.push((gain, offset));
            applied_grad.push((ga, gb, gc));
            windows.push([x0, y0, x1, y1]);
        }
    }

    Ok(SynthResult {
        truth,
        panel_paths,
        applied,
        applied_grad,
        windows,
        spiked,
        blobs,
    })
}

/// Linear astrometric solution attached to a synthetic *solved* panel, in
/// exactly the PixInsight property form [`crate::astrometry`] parses.
#[derive(Debug, Clone)]
pub struct SynthWcs {
    /// Reference sky coordinates `[RA, Dec]`, degrees.
    pub crval: [f64; 2],
    /// Reference point in PixInsight image coordinates (0-based, top-down,
    /// pixel k spanning `[k, k+1]`).
    pub refimg: [f64; 2],
    /// 2×2 linear transformation, deg/px, applied to top-down image offsets
    /// from the reference point (row 0 → ξ, row 1 → η).
    pub cd: [[f64; 2]; 2],
}

/// Standard-alphabet base64 with padding (the encoding PixInsight uses for
/// `location="inline:base64"` property payloads).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(ALPHA[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// f64 slice → little-endian bytes → base64 (XISF inline payload form).
fn b64_f64s(vals: &[f64]) -> String {
    let bytes: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
    base64_encode(&bytes)
}

/// `<Property>` elements for a linear Gnomonic astrometric solution — the
/// exact ids/types/encodings `astrometry::wcs_from_properties` requires.
fn wcs_property_xml(wcs: &SynthWcs) -> String {
    let cd = [wcs.cd[0][0], wcs.cd[0][1], wcs.cd[1][0], wcs.cd[1][1]];
    format!(
        concat!(
            r#"<Property id="PCL:AstrometricSolution:ReferenceCelestialCoordinates" type="F64Vector" length="2" location="inline:base64">{crval}</Property>"#,
            r#"<Property id="PCL:AstrometricSolution:ReferenceImageCoordinates" type="F64Vector" length="2" location="inline:base64">{refimg}</Property>"#,
            r#"<Property id="PCL:AstrometricSolution:LinearTransformationMatrix" type="F64Matrix" rows="2" columns="2" location="inline:base64">{cd}</Property>"#,
            r#"<Property id="PCL:AstrometricSolution:ProjectionSystem" type="String">Gnomonic</Property>"#,
        ),
        crval = b64_f64s(&wcs.crval),
        refimg = b64_f64s(&wcs.refimg),
        cd = b64_f64s(&cd),
    )
}

/// Minimal monolithic XISF writer (Float32, planar, little-endian,
/// uncompressed attachment at offset 4096). Round-trips through
/// [`crate::formats::xisf::XisfPanel`].
pub fn write_xisf(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32]) -> Result<()> {
    write_xisf_impl(path, w, h, ch, planes, "")
}

/// [`write_xisf`] plus a linear astrometric solution as inline-base64 XISF
/// `<Property>` elements — a synthetic stand-in for a plate-solved raw panel
/// (`analyze --input solved` consumes these).
pub fn write_xisf_solved(
    path: &Path,
    w: u64,
    h: u64,
    ch: u64,
    planes: &[f32],
    wcs: &SynthWcs,
) -> Result<()> {
    write_xisf_impl(path, w, h, ch, planes, &wcs_property_xml(wcs))
}

fn write_xisf_impl(
    path: &Path,
    w: u64,
    h: u64,
    ch: u64,
    planes: &[f32],
    extra_xml: &str,
) -> Result<()> {
    let n = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(ch))
        .ok_or_else(|| Error::format(path, "geometry overflow"))? as usize;
    if planes.len() != n {
        return Err(Error::format(
            path,
            format!(
                "planes length {} does not match geometry {w}x{h}x{ch} ({n})",
                planes.len()
            ),
        ));
    }

    const DATA_OFFSET: usize = 4096;
    let data_size = n as u64 * 4;
    let color_space = if ch == 1 { "Gray" } else { "RGB" };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><xisf version="1.0" xmlns="http://www.pixinsight.com/xisf"><Image geometry="{w}:{h}:{ch}" sampleFormat="Float32" colorSpace="{color_space}" pixelStorage="Planar" byteOrder="little" location="attachment:{DATA_OFFSET}:{data_size}"><FITSKeyword name="CREATOR" value="'mmm-synth'" comment="synthetic ground-truth frame"/>{extra_xml}</Image></xisf>"#
    );
    if 16 + xml.len() > DATA_OFFSET {
        return Err(Error::format(
            path,
            "XISF header does not fit before the 4096-byte attachment",
        ));
    }

    let mut bytes = Vec::with_capacity(DATA_OFFSET + n * 4);
    bytes.extend_from_slice(b"XISF0100");
    bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved
    bytes.extend_from_slice(xml.as_bytes());
    bytes.resize(DATA_OFFSET, 0);
    for v in planes {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::xisf::XisfPanel;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-synth-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_spec() -> SynthSpec {
        SynthSpec {
            canvas: (96, 80),
            channels: 3,
            grid: (2, 2),
            overlap_frac: 0.25,
            n_stars: 12,
            noise_sigma: 0.002,
            panel_gain_range: (0.7, 1.4),
            panel_offset_range: (-0.01, 0.02),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![],
            panel_spike_angle: vec![],
            panel_defects: vec![],
            mid_blobs: 0,
            shift_blobs: false,
            core: None,
            seed: 42,
        }
    }

    #[test]
    fn write_xisf_round_trips_through_reader() {
        let dir = tmpdir("roundtrip");
        let (w, h, ch) = (5u64, 4u64, 2u64);
        let planes: Vec<f32> = (0..(w * h * ch)).map(|i| i as f32 * 0.5 + 0.25).collect();
        let path = dir.join("rt.xisf");
        write_xisf(&path, w, h, ch, &planes).unwrap();

        let panel = XisfPanel::open(&path).unwrap();
        assert_eq!(panel.width(), w);
        assert_eq!(panel.height(), h);
        assert_eq!(panel.channels(), ch);
        assert_eq!(panel.header().data_offset, 4096);
        assert!(
            !panel.header().fits_keywords.is_empty(),
            "writer must emit at least one FITSKeyword"
        );
        let plane = (w * h) as usize;
        for c in 0..ch {
            assert_eq!(
                panel.channel(c),
                &planes[c as usize * plane..(c as usize + 1) * plane]
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The solved writer's properties must round-trip through the XISF reader
    /// and the astrometry extractor into exactly the written solution — the
    /// contract the solved-input e2e tests build on.
    #[test]
    fn write_xisf_solved_round_trips_wcs_properties() {
        let dir = tmpdir("solved");
        let (w, h, ch) = (24u64, 16u64, 2u64);
        let planes: Vec<f32> = (0..(w * h * ch)).map(|i| i as f32 * 0.25 + 0.5).collect();
        // Non-diagonal matrix (rotation) so element ordering is pinned.
        let wcs = SynthWcs {
            crval: [84.25, -3.5],
            refimg: [12.25, 8.75],
            cd: [[-4.0e-4, 0.5e-4], [0.5e-4, 4.0e-4]],
        };
        let path = dir.join("solved.xisf");
        write_xisf_solved(&path, w, h, ch, &planes, &wcs).unwrap();

        let panel = XisfPanel::open(&path).unwrap();
        assert_eq!(
            (panel.width(), panel.height(), panel.channels()),
            (w, h, ch)
        );
        let plane = (w * h) as usize;
        assert_eq!(
            panel.channel(1),
            &planes[plane..2 * plane],
            "pixel data intact"
        );

        let hdr = panel.header();
        let model = crate::astrometry::WcsModel::from_properties(&hdr.properties, w, h)
            .expect("written properties must form a valid solution");
        assert!(!model.is_spline(), "linear-only solution");
        assert_eq!(model.linear.crval, wcs.crval);
        // FITS = PixInsight image coords + 0.5 on both axes.
        assert_eq!(
            model.linear.crpix,
            [wcs.refimg[0] + 0.5, wcs.refimg[1] + 0.5]
        );
        assert_eq!(model.linear.cd, wcs.cd);
        assert_eq!(model.linear.ctype[0], "RA---TAN");

        // The reference image coordinate maps to the reference sky position.
        let (ra, dec) = model.pixel_to_sky(wcs.refimg[0], wcs.refimg[1]);
        assert!((ra - wcs.crval[0]).abs() < 1e-9 && (dec - wcs.crval[1]).abs() < 1e-9);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_xisf_rejects_wrong_plane_len() {
        let dir = tmpdir("badlen");
        let err = write_xisf(&dir.join("bad.xisf"), 4, 4, 1, &[0.0; 15]);
        assert!(err.is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn generate_panels_match_truth_with_gain_offset() {
        let dir = tmpdir("gen");
        let spec = test_spec();
        let res = generate(&spec, &dir).unwrap();

        let (w, h) = spec.canvas;
        let ch = spec.channels as u64;
        let plane = (w * h) as usize;
        let n_panels = (spec.grid.0 * spec.grid.1) as usize;

        assert_eq!(res.truth.len(), plane * ch as usize);
        assert_eq!(res.panel_paths.len(), n_panels);
        assert_eq!(res.applied.len(), n_panels);
        assert_eq!(res.windows.len(), n_panels);

        // Truth sanity: finite, strictly positive (zero is the no-data
        // sentinel), background in a sane linear range, at least one bright star.
        let mut max_v = f32::MIN;
        for &v in &res.truth {
            assert!(v.is_finite());
            assert!(
                v > 0.0,
                "truth must never be exactly 0 or negative, got {v}"
            );
            max_v = max_v.max(v);
        }
        assert!(max_v > 0.3, "expected a bright star, max was {max_v}");

        for (p, path) in res.panel_paths.iter().enumerate() {
            let (gain, offset) = res.applied[p];
            assert!((spec.panel_gain_range.0..=spec.panel_gain_range.1).contains(&gain));
            assert!((spec.panel_offset_range.0..=spec.panel_offset_range.1).contains(&offset));

            let panel = XisfPanel::open(path).unwrap();
            assert_eq!(panel.width(), w, "panel frames must be full-canvas");
            assert_eq!(panel.height(), h);
            assert_eq!(panel.channels(), ch);

            let [x0, y0, x1, y1] = res.windows[p];
            assert!(x0 < x1 && x1 <= w && y0 < y1 && y1 <= h);
            for c in 0..ch {
                let data = panel.channel(c);
                let truth_plane = &res.truth[c as usize * plane..(c as usize + 1) * plane];
                for y in 0..h {
                    for x in 0..w {
                        let i = (y * w + x) as usize;
                        let got = data[i];
                        if x >= x0 && x < x1 && y >= y0 && y < y1 {
                            let expected = truth_plane[i] * gain + offset;
                            let tol = 1e-5 + expected.abs() * 1e-5;
                            assert!(
                                (got - expected).abs() <= tol,
                                "panel {p} ch {c} ({x},{y}): got {got}, expected {expected}"
                            );
                            assert!(got != 0.0, "covered pixel must not be exactly 0");
                        } else {
                            assert!(
                                got == 0.0,
                                "panel {p} ch {c} ({x},{y}) outside window must be 0, got {got}"
                            );
                        }
                    }
                }
            }
        }

        // Adjacent panel windows must actually overlap.
        let [ax0, _, ax1, _] = res.windows[0];
        let [bx0, _, bx1, _] = res.windows[1];
        assert!(
            bx0 < ax1 && ax0 < bx1,
            "horizontally adjacent panels must overlap"
        );

        // Default gradient range (0,0) is a strict no-op.
        for &(a, b, c) in &res.applied_grad {
            assert_eq!(
                (a, b, c),
                (0.0, 0.0, 0.0),
                "gradient range (0,0) must draw zeros"
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn generate_applies_per_panel_gradient_planes() {
        let dir = tmpdir("grad");
        let mut spec = test_spec();
        spec.panel_gradient_range = (-0.01, 0.02);
        let res = generate(&spec, &dir).unwrap();

        let (w, h) = spec.canvas;
        let plane = (w * h) as usize;
        for (p, path) in res.panel_paths.iter().enumerate() {
            let (gain, offset) = res.applied[p];
            let (ga, gb, gc) = res.applied_grad[p];
            assert!((-0.01..=0.02).contains(&ga));
            assert!((-0.01..=0.02).contains(&gb));
            assert!((-0.01..=0.02).contains(&gc));

            let panel = XisfPanel::open(path).unwrap();
            let [x0, y0, x1, y1] = res.windows[p];
            for c in 0..spec.channels as u64 {
                let data = panel.channel(c);
                let truth_plane = &res.truth[c as usize * plane..(c as usize + 1) * plane];
                for y in (y0..y1).step_by(3) {
                    for x in (x0..x1).step_by(3) {
                        let i = (y * w + x) as usize;
                        let grad = ga + gb * (x as f32 / w as f32) + gc * (y as f32 / h as f32);
                        let expected = truth_plane[i] * gain + offset + grad;
                        let got = data[i];
                        let tol = 1e-5 + expected.abs() * 1e-5;
                        assert!(
                            (got - expected).abs() <= tol,
                            "panel {p} ch {c} ({x},{y}): got {got}, expected {expected}"
                        );
                    }
                }
            }
        }

        // The same seed with stars added must draw identical perturbations
        // (perturbation RNG is independent of star/noise draws).
        let mut spec_stars = spec.clone();
        spec_stars.n_stars = 30;
        let dir2 = tmpdir("grad-stars");
        let res2 = generate(&spec_stars, &dir2).unwrap();
        assert_eq!(res.applied, res2.applied);
        assert_eq!(res.applied_grad, res2.applied_grad);

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&dir2).unwrap();
    }

    /// The common global gradient is added to every panel's window (after
    /// gain/offset, identically across panels) and is NOT in the truth.
    #[test]
    fn generate_applies_common_global_gradient() {
        let dir_clean = tmpdir("gg-clean");
        let clean = generate(&test_spec(), &dir_clean).unwrap();

        let mut spec = test_spec();
        spec.global_gradient = (0.01, 0.05, -0.03);
        let dir = tmpdir("gg");
        let res = generate(&spec, &dir).unwrap();

        // Same seed → identical truth (the gradient lives in the panels only).
        assert_eq!(res.truth, clean.truth);

        let (w, h) = spec.canvas;
        let (ga, gb, gc) = spec.global_gradient;
        for (p, path) in res.panel_paths.iter().enumerate() {
            let a = XisfPanel::open(path).unwrap();
            let b = XisfPanel::open(&clean.panel_paths[p]).unwrap();
            let [x0, y0, x1, y1] = res.windows[p];
            for c in 0..spec.channels as u64 {
                let (da, db) = (a.channel(c), b.channel(c));
                for y in (y0..y1).step_by(3) {
                    for x in (x0..x1).step_by(3) {
                        let i = (y * w + x) as usize;
                        let expect = ga + gb * (x as f32 / w as f32) + gc * (y as f32 / h as f32);
                        let diff = da[i] - db[i];
                        assert!(
                            (diff - expect).abs() < 1e-5,
                            "panel {p} ch {c} ({x},{y}): diff {diff} vs global gradient {expect}"
                        );
                    }
                }
            }
        }

        std::fs::remove_dir_all(&dir_clean).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Defects are injected after gain/offset (additive, all channels),
    /// clipped to the panel's window, and only into the named panel.
    #[test]
    fn panel_defects_inject_line_segments() {
        let dir_clean = tmpdir("defects-clean");
        let spec_clean = test_spec();
        let clean = generate(&spec_clean, &dir_clean).unwrap();

        let mut spec = test_spec();
        let [x0, y0, x1, _y1] = clean.windows[1];
        // A 5-px trail inside panel 1's window, a cosmic ray (len 1), and a
        // segment overhanging the window's right edge (must be clipped).
        let trail = (1usize, x0 + 4, y0 + 6, 5u32, 0.5f32);
        let ray = (1usize, x0 + 10, y0 + 12, 1u32, 0.25f32);
        let overhang = (1usize, x1 - 2, y0 + 3, 6u32, 0.125f32);
        spec.panel_defects = vec![trail, ray, overhang];
        let dir = tmpdir("defects");
        let res = generate(&spec, &dir).unwrap();

        let (w, _) = spec.canvas;
        let plane = (spec.canvas.0 * spec.canvas.1) as usize;
        // Same seed → identical truth/perturbations; the panel-1 frames must
        // differ by exactly the defect amplitudes, on every channel.
        for p in 0..res.panel_paths.len() {
            let a = XisfPanel::open(&res.panel_paths[p]).unwrap();
            let b = XisfPanel::open(&clean.panel_paths[p]).unwrap();
            for c in 0..spec.channels as u64 {
                let (da, db) = (a.channel(c), b.channel(c));
                for i in 0..plane {
                    let (x, y) = (i as u64 % w, i as u64 / w);
                    let mut expect = 0.0f32;
                    if p == 1 {
                        for &(_, dx, dy, len, amp) in &spec.panel_defects {
                            if y == dy && x >= dx && x < dx + len as u64 && x < x1 {
                                expect += amp;
                            }
                        }
                    }
                    let diff = da[i] - db[i];
                    assert!(
                        (diff - expect).abs() < 1e-6,
                        "panel {p} ch {c} ({x},{y}): diff {diff} vs expected {expect}"
                    );
                }
            }
        }

        // Out-of-range panel index errors.
        let mut bad = test_spec();
        bad.panel_defects = vec![(9, 0, 0, 1, 0.1)];
        assert!(generate(&bad, &tmpdir("defects-bad")).is_err());

        std::fs::remove_dir_all(&dir_clean).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spike_angle_len_mismatch_errors() {
        let dir = tmpdir("spikelen");
        let mut spec = test_spec(); // 2×2 grid = 4 panels
        spec.panel_spike_angle = vec![0.0, 0.1];
        assert!(generate(&spec, &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Spikes are rendered on the brightest stars at angle 0 in the truth,
    /// rotated per panel by the given offset: a π/2 offset maps the 4-armed
    /// cross onto itself (panel identical to no-offset), while a π/4 offset
    /// must visibly move spike flux inside the panel window.
    #[test]
    fn spikes_add_arms_and_rotate_per_panel() {
        let base = SynthSpec {
            canvas: (256, 192),
            channels: 1,
            grid: (2, 1),
            overlap_frac: 0.25,
            n_stars: 12,
            noise_sigma: 0.001,
            panel_gain_range: (1.0, 1.0),
            panel_offset_range: (0.0, 0.0),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![],
            panel_spike_angle: vec![],
            panel_defects: vec![],
            mid_blobs: 0,
            shift_blobs: false,
            core: None,
            seed: 42,
        };
        let dir_plain = tmpdir("spikes-plain");
        let plain = generate(&base, &dir_plain).unwrap();
        assert!(
            plain.spiked.is_empty(),
            "no spikes requested → none reported"
        );

        let mut spec = base.clone();
        spec.panel_spike_angle = vec![0.0, std::f64::consts::FRAC_PI_2 as f32];
        let dir_sp = tmpdir("spikes-on");
        let sp = generate(&spec, &dir_sp).unwrap();
        assert!(!sp.spiked.is_empty(), "the amp-1.0 star must be spiked");

        // Truth difference = exactly the spike plane (same seed → same stars
        // and noise): zero far from spiked stars, elevated along the +x arm.
        let (w, h) = (base.canvas.0 as usize, base.canvas.1 as usize);
        let diff: Vec<f32> = sp.truth[..w * h]
            .iter()
            .zip(&plain.truth[..w * h])
            .map(|(a, b)| a - b)
            .collect();
        let &(sx, sy, len) = sp.spiked.iter().max_by(|a, b| a.2.total_cmp(&b.2)).unwrap();
        for &(_, _, l) in &sp.spiked {
            assert!(
                l >= SPIKE_MIN_AMP * SPIKE_LEN_PER_AMP,
                "arm length ∝ amplitude"
            );
        }
        let mut max_far = 0.0f32;
        for y in 0..h {
            for x in 0..w {
                let near = sp.spiked.iter().any(|&(cx, cy, l)| {
                    (x as f64 - cx).abs().max((y as f64 - cy).abs()) <= l + 3.0
                });
                if !near {
                    max_far = max_far.max(diff[y * w + x].abs());
                }
            }
        }
        assert!(
            max_far < 1e-6,
            "spike flux only near spiked stars, saw {max_far}"
        );
        // Sample the +x arm of the brightest spiked star (angle 0 in truth).
        let t = (len * 0.25).round();
        let (ax, ay) = ((sx + t).round() as usize, sy.round() as usize);
        if ax < w && ay < h {
            assert!(
                diff[ay * w + ax] > 0.02,
                "+x arm sample at ({ax},{ay}) too faint: {}",
                diff[ay * w + ax]
            );
        }

        // Panel 1's π/2 rotation is a symmetry of the cross: its frame must
        // equal gain·truth + offset (gain 1, offset 0 here) inside its window.
        let panel = XisfPanel::open(&sp.panel_paths[1]).unwrap();
        let data = panel.channel(0);
        let [x0, y0, x1, y1] = sp.windows[1];
        let mut max_dev = 0.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * base.canvas.0 + x) as usize;
                max_dev = max_dev.max((data[i] - sp.truth[i]).abs());
            }
        }
        assert!(
            max_dev < 1e-5,
            "π/2-rotated spikes must be identical, max dev {max_dev}"
        );

        // A π/4 rotation is not: flux moves off the truth's arms.
        let mut spec4 = base.clone();
        spec4.panel_spike_angle = vec![0.0, std::f64::consts::FRAC_PI_4 as f32];
        let dir4 = tmpdir("spikes-rot");
        let r4 = generate(&spec4, &dir4).unwrap();
        let panel4 = XisfPanel::open(&r4.panel_paths[1]).unwrap();
        let data4 = panel4.channel(0);
        let [x0, y0, x1, y1] = r4.windows[1];
        let mut max_dev4 = 0.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * base.canvas.0 + x) as usize;
                max_dev4 = max_dev4.max((data4[i] - r4.truth[i]).abs());
            }
        }
        assert!(
            max_dev4 > 0.02,
            "π/4-rotated spikes must differ from the truth's, max dev {max_dev4}"
        );

        std::fs::remove_dir_all(&dir_plain).unwrap();
        std::fs::remove_dir_all(&dir_sp).unwrap();
        std::fs::remove_dir_all(&dir4).unwrap();
    }

    /// Mid-frequency blobs land in the truth near their centers only, follow
    /// `panel_shift` when `shift_blobs` is set, and stay put otherwise. Same
    /// seed without blobs draws identical stars/noise/perturbations (the blob
    /// RNG stream is independent).
    #[test]
    fn mid_blobs_add_shifted_midscale_structure() {
        let base = SynthSpec {
            canvas: (256, 192),
            channels: 1,
            grid: (2, 1),
            overlap_frac: 0.25,
            n_stars: 0, // isolate the blobs: panel_shift then only moves them
            noise_sigma: 0.001,
            panel_gain_range: (1.0, 1.0),
            panel_offset_range: (0.0, 0.0),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![(0.0, 0.0), (3.0, 0.0)],
            panel_spike_angle: vec![],
            mid_blobs: 0,
            shift_blobs: false,
            core: None,
            panel_defects: vec![],
            seed: 42,
        };
        let dir_clean = tmpdir("blobs-clean");
        let clean = generate(&base, &dir_clean).unwrap();
        assert!(clean.blobs.is_empty());

        let mut spec = base.clone();
        spec.mid_blobs = 4;
        spec.shift_blobs = true;
        let dir = tmpdir("blobs");
        let res = generate(&spec, &dir).unwrap();
        assert_eq!(res.blobs.len(), 4);
        for &(_, _, amp) in &res.blobs {
            assert!((BLOB_AMP_RANGE.0..BLOB_AMP_RANGE.1).contains(&amp));
        }
        // Independent blob stream: perturbation draws are unchanged.
        assert_eq!(res.applied, clean.applied);

        // Truth diff = exactly the blob plane: zero far from every blob,
        // ≈ amp at each blob center (up to neighbouring blob tails).
        let (w, h) = (base.canvas.0 as usize, base.canvas.1 as usize);
        let diff: Vec<f32> = res.truth[..w * h]
            .iter()
            .zip(&clean.truth[..w * h])
            .map(|(a, b)| a - b)
            .collect();
        let r = 4.0 * BLOB_SIGMA_PX;
        let mut max_far = 0.0f32;
        for y in 0..h {
            for x in 0..w {
                let near = res.blobs.iter().any(|&(bx, by, _)| {
                    (x as f64 - bx).abs().max((y as f64 - by).abs()) <= r + 1.0
                });
                if !near {
                    max_far = max_far.max(diff[y * w + x].abs());
                }
            }
        }
        assert!(
            max_far < 1e-6,
            "blob flux only near blob centers, saw {max_far}"
        );
        for &(bx, by, amp) in &res.blobs {
            let (cx, cy) = (bx.round() as usize, by.round() as usize);
            if cx < w && cy < h {
                assert!(
                    diff[cy * w + cx] >= 0.9 * amp as f32,
                    "blob at ({bx:.0},{by:.0}) too faint in truth: {}",
                    diff[cy * w + cx]
                );
            }
        }

        // shift_blobs: panel 1's frame differs from the truth inside its
        // window by the blob displacement (no stars → nothing else moves)…
        let [x0, y0, x1, y1] = res.windows[1];
        let panel = XisfPanel::open(&res.panel_paths[1]).unwrap();
        let data = panel.channel(0);
        let mut max_dev = 0.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * base.canvas.0 + x) as usize;
                max_dev = max_dev.max((data[i] - res.truth[i]).abs());
            }
        }
        // Only meaningful when a blob actually reaches panel 1's window.
        let blob_in_window = res.blobs.iter().any(|&(bx, by, _)| {
            bx + r >= x0 as f64 && bx - r < x1 as f64 && by + r >= y0 as f64 && by - r < y1 as f64
        });
        assert!(
            blob_in_window,
            "seed must place a blob touching panel 1's window"
        );
        assert!(
            max_dev > 5e-3,
            "shifted blobs must move flux in panel 1, max dev {max_dev}"
        );

        // …and with shift_blobs = false the same panel matches the truth.
        let mut spec_ns = spec.clone();
        spec_ns.shift_blobs = false;
        let dir_ns = tmpdir("blobs-noshift");
        let res_ns = generate(&spec_ns, &dir_ns).unwrap();
        let panel_ns = XisfPanel::open(&res_ns.panel_paths[1]).unwrap();
        let data_ns = panel_ns.channel(0);
        let mut max_dev_ns = 0.0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * base.canvas.0 + x) as usize;
                max_dev_ns = max_dev_ns.max((data_ns[i] - res_ns.truth[i]).abs());
            }
        }
        assert!(
            max_dev_ns < 1e-6,
            "unshifted blobs must leave the panel equal to the truth, max dev {max_dev_ns}"
        );

        std::fs::remove_dir_all(&dir_clean).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&dir_ns).unwrap();
    }

    #[test]
    fn generate_is_deterministic_for_seed() {
        let dir_a = tmpdir("det-a");
        let dir_b = tmpdir("det-b");
        let spec = test_spec();
        let a = generate(&spec, &dir_a).unwrap();
        let b = generate(&spec, &dir_b).unwrap();

        assert_eq!(a.truth, b.truth);
        assert_eq!(a.applied, b.applied);
        assert_eq!(a.windows, b.windows);
        let bytes_a = std::fs::read(&a.panel_paths[0]).unwrap();
        let bytes_b = std::fs::read(&b.panel_paths[0]).unwrap();
        assert_eq!(
            bytes_a, bytes_b,
            "panel files must be byte-identical for a fixed seed"
        );

        std::fs::remove_dir_all(&dir_a).unwrap();
        std::fs::remove_dir_all(&dir_b).unwrap();
    }
}
