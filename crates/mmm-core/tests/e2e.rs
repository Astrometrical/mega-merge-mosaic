//! End-to-end ground-truth test: synthesize a known sky cut into perturbed
//! overlapping panels, run the full pipeline (analyze → photometric solve →
//! full-res feather blend), and validate the merged output against the exact
//! truth.
//!
//! Assertions:
//! (a) the recovered global corrections invert the applied per-panel
//!     gain/offset perturbations, up to the solver's gauge (the reference
//!     panel is fixed at g=1, o=0), so compositions `recovered ∘ applied`
//!     must agree across panels;
//! (b) RMSE(merged, truth) per channel is < 2·noise_sigma over pixels at
//!     least 16 px inside the union of the panel windows — the merged output
//!     lives in the reference panel's photometric frame, so truth is mapped
//!     through the reference panel's applied transform first;
//! (c) no NaNs/Infs anywhere in the output.

use std::path::PathBuf;

use mmm_core::Result;
use mmm_core::analyze::{analyze, analyze_opts};
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend, union_bbox};
use mmm_core::formats::xisf::XisfPanel;
use mmm_core::linalg::solve_dense;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthSpec, generate};

/// In-memory sink collecting the whole (small) blended output, planar.
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

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Erode a boolean mask by `r` pixels (Chebyshev metric), separably:
/// horizontal min-filter of half-width `r`, then vertical.
fn erode(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut hpass = vec![false; w * h];
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            hpass[y * w + x] = x >= r && x + r < w && row[lo..=hi].iter().all(|&m| m);
        }
    }
    let mut out = vec![false; w * h];
    for y in r..h.saturating_sub(r) {
        for x in 0..w {
            out[y * w + x] = (y - r..=y + r).all(|yy| hpass[yy * w + x]);
        }
    }
    out
}

#[test]
fn full_pipeline_recovers_ground_truth() {
    let dir = tempdir("pipeline");
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 3,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 40,
        noise_sigma: 0.002,
        panel_gain_range: (0.7, 1.4),
        panel_offset_range: (-0.01, 0.02),
        panel_gradient_range: (0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();

    // Pipeline: analyze (runs overlap graph + photometric solve + surfaces)
    // → blend. With no injected gradients, applying the fitted surfaces must
    // be a no-op within the phase-1 RMSE bound (guard rail: nothing to
    // correct ⇒ nothing gets "corrected").
    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();

    let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 64, mode: BlendMode::Feather, roi: None, defect_veto: true };
    let mut sink = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);

    let n_panels = res.applied.len();
    let nch = spec.channels as usize;
    assert_eq!(phot.gains.len(), nch);
    assert_eq!(phot.offsets.len(), nch);

    // The gauge: exactly one panel is the fixed reference (g=1, o=0).
    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");

    // (a) Recovered corrections invert the applied perturbations up to the
    // gauge: composing recovered ∘ applied maps truth into the reference
    // panel's photometric frame, so the composition must agree across panels.
    for c in 0..nch {
        let compose = |p: usize| -> (f64, f64) {
            let (ga, oa) = res.applied[p];
            let (gr, or) = (phot.gains[c][p], phot.offsets[c][p]);
            (gr * ga as f64, gr * oa as f64 + or)
        };
        let (ref_gain, ref_offset) = compose(reference);
        let mut max_gain_err = 0.0f64;
        let mut max_offset_err = 0.0f64;
        for p in 0..n_panels {
            let (g, o) = compose(p);
            let gain_err = (g / ref_gain - 1.0).abs();
            let offset_err = (o - ref_offset).abs();
            max_gain_err = max_gain_err.max(gain_err);
            max_offset_err = max_offset_err.max(offset_err);
            assert!(
                gain_err <= 0.02,
                "ch {c} panel {p}: composed gain {g} vs reference {ref_gain} (>2% off)"
            );
            assert!(
                offset_err <= 2e-3,
                "ch {c} panel {p}: composed offset {o} vs reference {ref_offset}"
            );
        }
        eprintln!(
            "ch {c}: max composed gain error {max_gain_err:.2e} (bound 2e-2), \
             max offset error {max_offset_err:.2e} (bound 2e-3)"
        );
    }

    // (c) No NaNs/Infs anywhere in the output.
    assert!(sink.data.iter().all(|v| v.is_finite()), "output must contain no NaN/Inf");

    // (b) RMSE(merged, truth) per channel over pixels ≥ 16 px inside the
    // union of the panel windows. The blend crops to the union content bbox,
    // and its output is in the reference panel's photometric frame.
    let (w, h) = (spec.canvas.0 as usize, spec.canvas.1 as usize);
    let bbox = union_bbox(&session).unwrap();
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);
    assert_eq!((sink.w, sink.h, sink.ch), ((bbox[2] - bbox[0]) as usize, (bbox[3] - bbox[1]) as usize, nch));

    let mut mask = vec![false; w * h];
    for &[x0, y0, x1, y1] in &res.windows {
        for y in y0..y1 {
            for x in x0..x1 {
                mask[y as usize * w + x as usize] = true;
            }
        }
    }
    let interior = erode(&mask, w, h, 16);

    let (ref_gain, ref_offset) = res.applied[reference];
    let plane = w * h;
    for c in 0..nch {
        let truth = &res.truth[c * plane..(c + 1) * plane];
        let mut sum_sq = 0.0f64;
        let mut n = 0u64;
        for y in 0..h {
            for x in 0..w {
                if !interior[y * w + x] {
                    continue;
                }
                // Interior pixels must lie inside the blend's crop.
                assert!(x >= cx0 && x - cx0 < sink.w && y >= cy0 && y - cy0 < sink.h);
                let merged = sink.at(c, x - cx0, y - cy0);
                let expected = truth[y * w + x] * ref_gain + ref_offset;
                sum_sq += f64::from(merged - expected).powi(2);
                n += 1;
            }
        }
        assert!(n > 10_000, "interior region unexpectedly small: {n} px");
        let rmse = (sum_sq / n as f64).sqrt();
        let bound = 2.0 * spec.noise_sigma as f64;
        eprintln!("ch {c}: RMSE {rmse:.3e} vs bound {bound:.3e} over {n} interior px");
        assert!(rmse < bound, "ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6} over {n} px");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Fit an order-2 field `1, x, y, x², xy, y²` (normalized coords) to
/// `residual` over `mask` pixels and subtract it in place. Returns the max
/// |fitted field| over the masked pixels. The merged mosaic is only defined
/// up to one global smooth field (the gauge freedom of surface correction:
/// only *differences* s_i − s_j are constrained by data), so ground-truth
/// comparison removes that one field first.
fn remove_global_field(residual: &mut [f32], mask: &[bool], w: usize, h: usize) -> f64 {
    let basis = |x: usize, y: usize| -> [f64; 6] {
        let xn = x as f64 / w as f64;
        let yn = y as f64 / h as f64;
        [1.0, xn, yn, xn * xn, xn * yn, yn * yn]
    };
    let mut a = [0.0f64; 36];
    let mut b = [0.0f64; 6];
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let phi = basis(x, y);
            let r = residual[y * w + x] as f64;
            for i in 0..6 {
                for j in 0..6 {
                    a[i * 6 + j] += phi[i] * phi[j];
                }
                b[i] += phi[i] * r;
            }
        }
    }
    let c = solve_dense(&mut a, &mut b, 6).unwrap();
    let mut max_field = 0.0f64;
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let phi = basis(x, y);
            let f: f64 = (0..6).map(|i| c[i] * phi[i]).sum();
            residual[y * w + x] -= f as f32;
            max_field = max_field.max(f.abs());
        }
    }
    max_field
}

/// Mandatory phase-2 test 3: gradients + stars + noise through the full
/// pipeline (analyze → photometry → surfaces → blend). The final RMSE bound
/// is the phase-1 bound (2·noise_sigma), measured after removing one global
/// order-2 field — the unavoidable gauge freedom (adding the same smooth
/// field to every panel changes nothing the data can see). Raw residuals are
/// additionally bounded to catch runaway corrections.
#[test]
fn full_pipeline_with_gradients_recovers_ground_truth() {
    let dir = tempdir("gradients");
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 3,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 40,
        noise_sigma: 0.002,
        panel_gain_range: (0.7, 1.4),
        panel_offset_range: (-0.01, 0.02),
        panel_gradient_range: (-0.006, 0.006),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();

    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();
    assert_eq!(surf.order, 2);

    let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 64, mode: BlendMode::Feather, roi: None, defect_veto: true };
    let mut sink = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);
    assert!(sink.data.iter().all(|v| v.is_finite()), "output must contain no NaN/Inf");

    let n_panels = res.applied.len();
    let nch = spec.channels as usize;
    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");

    // Composed gains must still roughly agree across panels; the bound is much
    // looser than phase-1 because additive gradient planes bias the per-edge
    // gain fits (observed ~5% here) — the surfaces absorb the consequences,
    // and the RMSE assertion below is the real quality gate. Offsets are not
    // comparable at all: the injected gradients *are* spatially varying
    // offsets, absorbed by the surfaces rather than by o.
    for c in 0..nch {
        let compose_gain = |p: usize| phot.gains[c][p] * res.applied[p].0 as f64;
        let ref_gain = compose_gain(reference);
        for p in 0..n_panels {
            let gain_err = (compose_gain(p) / ref_gain - 1.0).abs();
            assert!(
                gain_err <= 0.08,
                "ch {c} panel {p}: composed gain off by {gain_err:.3} (>8%)"
            );
        }
    }

    // RMSE vs truth in the reference frame after removing one global order-2
    // field; also bound the raw residual and the removed field itself.
    let (w, h) = (spec.canvas.0 as usize, spec.canvas.1 as usize);
    let bbox = union_bbox(&session).unwrap();
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);
    let mut mask = vec![false; w * h];
    for &[x0, y0, x1, y1] in &res.windows {
        for y in y0..y1 {
            for x in x0..x1 {
                mask[y as usize * w + x as usize] = true;
            }
        }
    }
    let interior = erode(&mask, w, h, 16);

    let (ref_gain, ref_offset) = res.applied[reference];
    let plane = w * h;
    for c in 0..nch {
        let truth = &res.truth[c * plane..(c + 1) * plane];
        let mut residual = vec![0.0f32; plane];
        let mut n = 0u64;
        for y in 0..h {
            for x in 0..w {
                if !interior[y * w + x] {
                    continue;
                }
                let merged = sink.at(c, x - cx0, y - cy0);
                let expected = truth[y * w + x] * ref_gain + ref_offset;
                residual[y * w + x] = merged - expected;
                n += 1;
            }
        }
        assert!(n > 10_000, "interior region unexpectedly small: {n} px");

        let raw_rms = (residual
            .iter()
            .zip(&interior)
            .filter(|&(_, &m)| m)
            .map(|(&r, _)| f64::from(r) * f64::from(r))
            .sum::<f64>()
            / n as f64)
            .sqrt();
        assert!(raw_rms < 0.02, "ch {c}: raw residual RMS {raw_rms:.4} is runaway");

        let max_field = remove_global_field(&mut residual, &interior, w, h);
        assert!(
            max_field < 0.05,
            "ch {c}: removed global field {max_field:.4} exceeds sane gradient scale"
        );

        let rmse = (residual
            .iter()
            .zip(&interior)
            .filter(|&(_, &m)| m)
            .map(|(&r, _)| f64::from(r) * f64::from(r))
            .sum::<f64>()
            / n as f64)
            .sqrt();
        let bound = 2.0 * spec.noise_sigma as f64;
        eprintln!(
            "ch {c}: raw RMS {raw_rms:.3e}, field-removed RMSE {rmse:.3e} vs bound {bound:.3e}, \
             global field max {max_field:.3e}"
        );
        assert!(rmse < bound, "ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6}");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-2 test 4: `--surface off` bypasses cleanly — no
/// surfaces.json is written (a stale one is removed), and blending without
/// surfaces still works.
#[test]
fn surface_off_bypasses_cleanly() {
    let dir = tempdir("surfoff");
    let spec = SynthSpec {
        canvas: (256, 192),
        channels: 1,
        grid: (2, 1),
        overlap_frac: 0.3,
        n_stars: 10,
        noise_sigma: 0.001,
        panel_gain_range: (0.9, 1.1),
        panel_offset_range: (-0.005, 0.005),
        panel_gradient_range: (0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        seed: 3,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let sdir = dir.join("s.mmm-session");

    // Default analyze writes surfaces.json…
    let session = analyze(&res.panel_paths, &sdir).unwrap();
    assert!(session.surfaces_path().exists(), "default analyze must fit surfaces");

    // …and an explicit off re-analyze removes it again.
    let session = analyze_opts(&res.panel_paths, &sdir, None).unwrap();
    assert!(!session.surfaces_path().exists(), "--surface off must not leave surfaces.json");

    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 64, mode: BlendMode::Feather, roi: None, defect_veto: true };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);
    assert!(sink.data.iter().all(|v| v.is_finite()));

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-2B test 4: the full pipeline (analyze → photometry →
/// surfaces → TwoBand blend) still recovers ground truth within the phase-1
/// RMSE bound.
#[test]
fn full_pipeline_twoband_recovers_ground_truth() {
    let dir = tempdir("twoband");
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 3,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 40,
        noise_sigma: 0.002,
        panel_gain_range: (0.7, 1.4),
        panel_offset_range: (-0.01, 0.02),
        panel_gradient_range: (0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();

    let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 64, mode: BlendMode::TwoBand, roi: None, defect_veto: true };
    let mut sink = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);
    assert!(sink.data.iter().all(|v| v.is_finite()), "output must contain no NaN/Inf");

    let n_panels = res.applied.len();
    let nch = spec.channels as usize;
    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");

    let (w, h) = (spec.canvas.0 as usize, spec.canvas.1 as usize);
    let bbox = union_bbox(&session).unwrap();
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);
    let mut mask = vec![false; w * h];
    for &[x0, y0, x1, y1] in &res.windows {
        for y in y0..y1 {
            for x in x0..x1 {
                mask[y as usize * w + x as usize] = true;
            }
        }
    }
    let interior = erode(&mask, w, h, 16);

    let (ref_gain, ref_offset) = res.applied[reference];
    let plane = w * h;
    for c in 0..nch {
        let truth = &res.truth[c * plane..(c + 1) * plane];
        let mut sum_sq = 0.0f64;
        let mut n = 0u64;
        for y in 0..h {
            for x in 0..w {
                if !interior[y * w + x] {
                    continue;
                }
                let merged = sink.at(c, x - cx0, y - cy0);
                let expected = truth[y * w + x] * ref_gain + ref_offset;
                sum_sq += f64::from(merged - expected).powi(2);
                n += 1;
            }
        }
        assert!(n > 10_000, "interior region unexpectedly small: {n} px");
        let rmse = (sum_sq / n as f64).sqrt();
        let bound = 2.0 * spec.noise_sigma as f64;
        eprintln!("twoband ch {c}: RMSE {rmse:.3e} vs bound {bound:.3e} over {n} interior px");
        assert!(rmse < bound, "ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6}");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-2B test 3: base + detail must reconstruct exactly. A
/// single panel blended in TwoBand mode equals the corrected input away from
/// the coverage boundary (base cancels out of `base + (full − base)`).
#[test]
fn twoband_single_panel_reconstructs_input() {
    let dir = tempdir("recon");
    let spec = SynthSpec {
        canvas: (256, 192),
        channels: 2,
        grid: (1, 1),
        overlap_frac: 0.0,
        n_stars: 15,
        noise_sigma: 0.002,
        panel_gain_range: (1.0, 1.0),
        panel_offset_range: (0.0, 0.0),
        panel_gradient_range: (0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        seed: 7,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 32, mode: BlendMode::TwoBand, roi: None, defect_veto: true };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

    // The single panel is its own reference (g=1, o=0): output == input.
    let panel = XisfPanel::open(&res.panel_paths[0]).unwrap();
    let bbox = union_bbox(&session).unwrap();
    let [x0, y0, x1, y1] = res.windows[0];
    let mut max_diff = 0.0f32;
    for c in 0..spec.channels as u64 {
        let data = panel.channel(c);
        for y in y0 + 16..y1 - 16 {
            for x in x0 + 16..x1 - 16 {
                let merged =
                    sink.at(c as usize, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
                let input = data[(y * spec.canvas.0 + x) as usize];
                max_diff = max_diff.max((merged - input).abs());
            }
        }
    }
    eprintln!("twoband reconstruction max |merged − input| = {max_diff:.2e}");
    assert!(max_diff < 1e-5, "base+detail must reconstruct the input, max diff {max_diff}");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Local maxima above `floor` in `img` (w×h, channel 0 planar) whose ±(r+2)
/// neighbourhood is fully inside at least two panel windows — bright overlap
/// stars with both corrected panels defined around them.
fn bright_overlap_peaks(
    img: &[f32],
    w: usize,
    h: usize,
    floor: f32,
    r: usize,
    windows: &[[u64; 4]],
) -> Vec<(usize, usize, Vec<usize>)> {
    let m = r + 2;
    let mut peaks = Vec::new();
    for y in m..h - m {
        for x in m..w - m {
            let v = img[y * w + x];
            if v < floor {
                continue;
            }
            let is_max = (y - 3..=y + 3).all(|yy| {
                (x - 3..=x + 3).all(|xx| img[yy * w + xx] <= v || (xx == x && yy == y))
            });
            if !is_max {
                continue;
            }
            let covering: Vec<usize> = windows
                .iter()
                .enumerate()
                .filter(|&(_, &[wx0, wy0, wx1, wy1])| {
                    x >= wx0 as usize + m
                        && x + m < wx1 as usize
                        && y >= wy0 as usize + m
                        && y + m < wy1 as usize
                })
                .map(|(p, _)| p)
                .collect();
            if covering.len() >= 2 {
                peaks.push((x, y, covering));
            }
        }
    }
    peaks
}

/// Mandatory phase-2B test 2 (anti-pinching): with a 0.6 px star-only shift
/// on one panel, every bright overlap star's merged neighbourhood must match
/// ONE panel's corrected pixels in TwoBand mode — and the same check must
/// FAIL in Feather mode (which averages the two star positions), proving the
/// test can detect pinching.
#[test]
fn twoband_never_averages_misregistered_stars() {
    let dir = tempdir("pinch");
    // The canvas must be large enough that the grid-neighbour overlap bands
    // exceed seam::DP_MIN_LONG cells along their long axis — otherwise every
    // edge falls under the diagonal-corner rule and keeps Voronoi labels
    // (as it would on real data only for corner overlaps).
    let spec = SynthSpec {
        canvas: (1024, 768),
        channels: 1,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 120,
        noise_sigma: 0.004,
        panel_gain_range: (0.9, 1.2),
        panel_offset_range: (-0.005, 0.01),
        panel_gradient_range: (0.0, 0.0),
        // Panel 1 is misregistered by 0.6 px in x — stars only.
        panel_shift: vec![(0.0, 0.0), (0.6, 0.0), (0.0, 0.0), (0.0, 0.0)],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        seed: 1234,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    let run = |mode: BlendMode| -> MemSink {
        let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 64, mode, roi: None, defect_veto: true };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
        sink
    };
    let two = run(BlendMode::TwoBand);
    let fea = run(BlendMode::Feather);

    // Corrected panel pixels: g·v + o with the recovered corrections — the
    // frame the blend output lives in.
    let w = spec.canvas.0 as usize;
    let corrected: Vec<Vec<f32>> = res
        .panel_paths
        .iter()
        .enumerate()
        .map(|(p, path)| {
            let panel = XisfPanel::open(path).unwrap();
            let (g, o) = (phot.gains[0][p] as f32, phot.offsets[0][p] as f32);
            panel.channel(0).iter().map(|&v| v * g + o).collect()
        })
        .collect();

    let bbox = union_bbox(&session).unwrap();
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);
    const R: usize = 6; // neighbourhood half-width around each star peak
    let peaks = bright_overlap_peaks(&two.data, two.w, two.h, 0.15, R, &res.windows);
    assert!(peaks.len() >= 3, "need several bright overlap stars, found {}", peaks.len());
    let shifted_overlap = peaks.iter().any(|(_, _, covering)| covering.contains(&1));
    assert!(shifted_overlap, "at least one star must lie in an overlap of the shifted panel");

    // Per star and mode: distance to the *closest single panel* — max abs
    // diff over the neighbourhood, minimized over the covering panels.
    let one_panel_dist = |sink: &MemSink, px: usize, py: usize, covering: &[usize]| -> f32 {
        covering
            .iter()
            .map(|&p| {
                let mut d = 0.0f32;
                for y in py - R..=py + R {
                    for x in px - R..=px + R {
                        let merged = sink.at(0, x, y);
                        let corr = corrected[p][(y + cy0) * w + (x + cx0)];
                        d = d.max((merged - corr).abs());
                    }
                }
                d
            })
            .fold(f32::INFINITY, f32::min)
    };

    // TwoBand within ~noise of one panel; Feather's average is far from every
    // panel for at least one bright misregistered star.
    let thresh = 6.0 * spec.noise_sigma;
    let mut feather_fails = 0;
    for &(px, py, ref covering) in &peaks {
        let d_two = one_panel_dist(&two, px, py, covering);
        let d_fea = one_panel_dist(&fea, px, py, covering);
        eprintln!(
            "star at ({:4},{:4}) panels {:?}: twoband {:.4}, feather {:.4} (thresh {:.4})",
            px + cx0,
            py + cy0,
            covering,
            d_two,
            d_fea,
            thresh
        );
        assert!(
            d_two < thresh,
            "TwoBand: star at ({px},{py}) matches no single panel (min max-diff {d_two})"
        );
        if d_fea >= thresh {
            feather_fails += 1;
        }
    }
    assert!(
        feather_fails > 0,
        "Feather mode passed the one-panel check for all {} stars — the test has no teeth",
        peaks.len()
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
