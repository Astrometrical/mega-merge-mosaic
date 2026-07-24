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
    pub canvas: (u64, u64),
    pub channels: u32,
    /// Panels across x down.
    pub grid: (u32, u32),
    /// Fraction of a grid cell added as overlap between neighbours (e.g. 0.25).
    pub overlap_frac: f64,
    pub n_stars: usize,
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
    pub seed: u64,
}

/// Output of [`generate`]: the exact truth image plus the written panels and
/// the perturbations applied to each.
#[derive(Debug)]
pub struct SynthResult {
    /// Planar channel planes, canvas-sized (keep the canvas small: <= 1024^2).
    pub truth: Vec<f32>,
    pub panel_paths: Vec<PathBuf>,
    /// Per-panel (gain, offset) actually applied.
    pub applied: Vec<(f32, f32)>,
    /// Per-panel gradient plane coefficients `(a, b, c)` actually applied:
    /// `a + b·x/w + c·y/h` over normalized canvas coords.
    pub applied_grad: Vec<(f32, f32, f32)>,
    /// Per-panel content window on the canvas: [x0, y0, x1, y1], exclusive.
    pub windows: Vec<[u64; 4]>,
}

/// Positive floor applied to truth (and to any covered panel pixel that would
/// otherwise land on exactly 0.0): zero is the no-data sentinel and must never
/// occur inside a panel's window.
const VALUE_FLOOR: f32 = 1e-4;

/// xorshift64* PRNG with a Box-Muller Gaussian tap. Deterministic for a seed;
/// no external dependency.
struct Rng {
    state: u64,
    spare_gauss: Option<f64>,
}

impl Rng {
    fn new(seed: u64) -> Self {
        // xorshift state must be nonzero.
        let state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
        Self { state, spare_gauss: None }
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
        return Err(Error::format(dir, "SynthSpec canvas/channels/grid must all be nonzero"));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;

    let ch = spec.channels as usize;
    let plane = (w * h) as usize;
    let mut rng = Rng::new(spec.seed);

    // Stars: positions and sigma from the RNG, amplitudes log-spaced from the
    // brightest (~1.0) down to a faint floor, shared across channels.
    let mut star_plane = vec![0.0f32; plane];
    for i in 0..spec.n_stars {
        let sx = rng.range_f64(0.0, w as f64);
        let sy = rng.range_f64(0.0, h as f64);
        let sigma = rng.range_f64(1.0, 3.0);
        let t = if spec.n_stars > 1 { i as f64 / (spec.n_stars - 1) as f64 } else { 0.0 };
        let amp = 1.0 * (0.02f64 / 1.0).powf(t); // log-spaced 1.0 .. 0.02

        let r = (4.0 * sigma).ceil() as i64;
        let (cxp, cyp) = (sx.round() as i64, sy.round() as i64);
        let inv_2s2 = 1.0 / (2.0 * sigma * sigma);
        for y in (cyp - r).max(0)..=(cyp + r).min(h as i64 - 1) {
            for x in (cxp - r).max(0)..=(cxp + r).min(w as i64 - 1) {
                let dx = x as f64 - sx;
                let dy = y as f64 - sy;
                let v = amp * (-(dx * dx + dy * dy) * inv_2s2).exp();
                star_plane[(y as u64 * w + x as u64) as usize] += v as f32;
            }
        }
    }

    // Truth = background + stars + Gaussian noise, floored positive.
    let mut truth = vec![0.0f32; ch * plane];
    for c in 0..spec.channels {
        let out = &mut truth[c as usize * plane..(c as usize + 1) * plane];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let noise = spec.noise_sigma * rng.next_gaussian() as f32;
                let v = background(x, y, c, w, h) + star_plane[i] + noise;
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
    let mut prng = Rng::new(spec.seed ^ 0xC0FF_EE00_D15E_A5E5);
    let mut panel_paths = Vec::with_capacity(n_panels);
    let mut applied = Vec::with_capacity(n_panels);
    let mut applied_grad = Vec::with_capacity(n_panels);
    let mut windows = Vec::with_capacity(n_panels);
    let mut frame = vec![0.0f32; ch * plane];

    for cy in 0..spec.grid.1 {
        for cx in 0..spec.grid.0 {
            let id = (cy * spec.grid.0 + cx) as usize;
            let gain =
                prng.range_f64(spec.panel_gain_range.0 as f64, spec.panel_gain_range.1 as f64)
                    as f32;
            let offset =
                prng.range_f64(spec.panel_offset_range.0 as f64, spec.panel_offset_range.1 as f64)
                    as f32;
            let (glo, ghi) =
                (spec.panel_gradient_range.0 as f64, spec.panel_gradient_range.1 as f64);
            let ga = prng.range_f64(glo, ghi) as f32;
            let gb = prng.range_f64(glo, ghi) as f32;
            let gc = prng.range_f64(glo, ghi) as f32;
            let [x0, y0, x1, y1] = panel_window(spec, cx, cy);

            frame.fill(0.0);
            for c in 0..ch {
                let src = &truth[c * plane..(c + 1) * plane];
                let dst = &mut frame[c * plane..(c + 1) * plane];
                for y in y0..y1 {
                    let grad_y = ga + gc * (y as f32 / h as f32);
                    for x in x0..x1 {
                        let i = (y * w + x) as usize;
                        let grad = grad_y + gb * (x as f32 / w as f32);
                        let mut v = src[i] * gain + offset + grad;
                        if v == 0.0 {
                            // Covered pixels must never be exactly 0 (no-data
                            // sentinel); nudge by a value below test tolerance.
                            v = 1e-6;
                        }
                        dst[i] = v;
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

    Ok(SynthResult { truth, panel_paths, applied, applied_grad, windows })
}

/// Minimal monolithic XISF writer (Float32, planar, little-endian,
/// uncompressed attachment at offset 4096). Round-trips through
/// [`crate::formats::xisf::XisfPanel`].
pub fn write_xisf(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32]) -> Result<()> {
    let n = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(ch))
        .ok_or_else(|| Error::format(path, "geometry overflow"))? as usize;
    if planes.len() != n {
        return Err(Error::format(
            path,
            format!("planes length {} does not match geometry {w}x{h}x{ch} ({n})", planes.len()),
        ));
    }

    const DATA_OFFSET: usize = 4096;
    let data_size = n as u64 * 4;
    let color_space = if ch == 1 { "Gray" } else { "RGB" };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><xisf version="1.0" xmlns="http://www.pixinsight.com/xisf"><Image geometry="{w}:{h}:{ch}" sampleFormat="Float32" colorSpace="{color_space}" pixelStorage="Planar" byteOrder="little" location="attachment:{DATA_OFFSET}:{data_size}"><FITSKeyword name="CREATOR" value="'mmm-synth'" comment="synthetic ground-truth frame"/></Image></xisf>"#
    );
    if 16 + xml.len() > DATA_OFFSET {
        return Err(Error::format(path, "XISF header does not fit before the 4096-byte attachment"));
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
            assert_eq!(panel.channel(c), &planes[c as usize * plane..(c as usize + 1) * plane]);
        }
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
            assert!(v > 0.0, "truth must never be exactly 0 or negative, got {v}");
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
        assert!(bx0 < ax1 && ax0 < bx1, "horizontally adjacent panels must overlap");

        // Default gradient range (0,0) is a strict no-op.
        for &(a, b, c) in &res.applied_grad {
            assert_eq!((a, b, c), (0.0, 0.0, 0.0), "gradient range (0,0) must draw zeros");
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
                        let grad =
                            ga + gb * (x as f32 / w as f32) + gc * (y as f32 / h as f32);
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
        assert_eq!(bytes_a, bytes_b, "panel files must be byte-identical for a fixed seed");

        std::fs::remove_dir_all(&dir_a).unwrap();
        std::fs::remove_dir_all(&dir_b).unwrap();
    }
}
