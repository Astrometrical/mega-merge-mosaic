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

use std::path::{Path, PathBuf};

use mmm_core::Result;
use mmm_core::analyze::{InputSelect, analyze, analyze_gain, analyze_input, analyze_opts};
use mmm_core::astrometry::LinearWcs;
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend, union_bbox};
use mmm_core::formats::xisf::XisfPanel;
use mmm_core::linalg::solve_dense;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::{GainMode, Photometry};
use mmm_core::session::{InputKind, Session};
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthSpec, SynthWcs, generate, write_xisf, write_xisf_solved};

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
        Self {
            w: 0,
            h: 0,
            ch: 0,
            data: Vec::new(),
            finished: false,
        }
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
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
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

    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
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
    assert!(
        sink.data.iter().all(|v| v.is_finite()),
        "output must contain no NaN/Inf"
    );

    // (b) RMSE(merged, truth) per channel over pixels ≥ 16 px inside the
    // union of the panel windows. The blend crops to the union content bbox,
    // and its output is in the reference panel's photometric frame.
    let (w, h) = (spec.canvas.0 as usize, spec.canvas.1 as usize);
    let bbox = union_bbox(&session).unwrap();
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);
    assert_eq!(
        (sink.w, sink.h, sink.ch),
        (
            (bbox[2] - bbox[0]) as usize,
            (bbox[3] - bbox[1]) as usize,
            nch
        )
    );

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
        assert!(
            rmse < bound,
            "ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6} over {n} px"
        );
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
fn unity_gain_mode_threads_through_analyze() {
    // `analyze` with GainMode::Unity must persist unity gains in
    // photometry.json, record the mode in session.json, and fit surfaces on
    // top of the unity corrections.
    let dir = tempdir("unitymode");
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 1,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 40,
        noise_sigma: 0.002,
        panel_gain_range: (1.0, 1.0),
        panel_offset_range: (-0.01, 0.02),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 5,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();

    let session = analyze_gain(
        &res.panel_paths,
        &dir.join("s.mmm-session"),
        Some(2),
        GainMode::Unity,
    )
    .unwrap();
    assert_eq!(session.gain_mode, GainMode::Unity);
    let reopened = Session::open(&dir.join("s.mmm-session")).unwrap();
    assert_eq!(
        reopened.gain_mode,
        GainMode::Unity,
        "gain mode round-trips through session.json"
    );

    let phot = Photometry::load(&session.photometry_path()).unwrap();
    assert!(
        phot.gains[0].iter().all(|&g| g == 1.0),
        "unity mode pins every gain at 1: {:?}",
        phot.gains[0]
    );
    // Offsets recover the applied level differences up to the gauge.
    let reference = (0..res.applied.len())
        .find(|&p| phot.offsets[0][p] == 0.0)
        .expect("a gauge panel");
    let (_, ref_o) = res.applied[reference];
    for (p, &(_, o)) in res.applied.iter().enumerate() {
        let expect = ref_o as f64 - o as f64;
        assert!(
            (phot.offsets[0][p] - expect).abs() < 2e-3,
            "panel {p}: offset {} vs applied-difference {expect}",
            phot.offsets[0][p]
        );
    }
    assert!(session.surfaces_path().exists(), "surfaces fitted on top");

    std::fs::remove_dir_all(&dir).unwrap();
}

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
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();

    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();
    assert_eq!(surf.order, 2);

    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);
    assert!(
        sink.data.iter().all(|v| v.is_finite()),
        "output must contain no NaN/Inf"
    );

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
        assert!(
            raw_rms < 0.02,
            "ch {c}: raw residual RMS {raw_rms:.4} is runaway"
        );

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
        assert!(
            rmse < bound,
            "ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6}"
        );
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
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 3,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let sdir = dir.join("s.mmm-session");

    // Default analyze writes surfaces.json…
    let session = analyze(&res.panel_paths, &sdir).unwrap();
    assert!(
        session.surfaces_path().exists(),
        "default analyze must fit surfaces"
    );

    // …and an explicit off re-analyze removes it again.
    let session = analyze_opts(&res.panel_paths, &sdir, None).unwrap();
    assert!(
        !session.surfaces_path().exists(),
        "--surface off must not leave surfaces.json"
    );

    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);
    assert!(sink.data.iter().all(|v| v.is_finite()));

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-2B test 4 (+ phase-4 test 3): the full pipeline (analyze
/// → photometry → surfaces → blend) still recovers ground truth within the
/// phase-1 RMSE bound, in TwoBand *and* Pyramid mode.
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
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();

    let n_panels = res.applied.len();
    let nch = spec.channels as usize;

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

    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");
    let (ref_gain, ref_offset) = res.applied[reference];
    let plane = w * h;

    for mode in [BlendMode::TwoBand, BlendMode::Pyramid] {
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
        assert!(sink.finished);
        assert!(
            sink.data.iter().all(|v| v.is_finite()),
            "output must contain no NaN/Inf"
        );

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
            eprintln!("{mode:?} ch {c}: RMSE {rmse:.3e} vs bound {bound:.3e} over {n} interior px");
            assert!(
                rmse < bound,
                "{mode:?} ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6}"
            );
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-2B test 3 (+ phase-4 test 3): base + detail must
/// reconstruct exactly. A single panel blended in TwoBand or Pyramid mode
/// equals the corrected input away from the coverage boundary (in Pyramid
/// mode the single-contributor pyramid collapses back to the panel's own
/// base plane, so the cancellation still holds).
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
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 7,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    // The single panel is its own reference (g=1, o=0): output == input.
    let panel = XisfPanel::open(&res.panel_paths[0]).unwrap();
    let bbox = union_bbox(&session).unwrap();
    let [x0, y0, x1, y1] = res.windows[0];
    for mode in [BlendMode::TwoBand, BlendMode::Pyramid] {
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 32,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

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
        eprintln!("{mode:?} reconstruction max |merged − input| = {max_diff:.2e}");
        assert!(
            max_diff < 1e-5,
            "{mode:?}: base+detail must reconstruct the input, max diff {max_diff}"
        );
    }

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
            let is_max = (y - 3..=y + 3)
                .all(|yy| (x - 3..=x + 3).all(|xx| img[yy * w + xx] <= v || (xx == x && yy == y)));
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

/// Mandatory phase-2B test 2 (anti-pinching, + phase-4 test 3): with a
/// 0.6 px star-only shift on one panel, every bright overlap star's merged
/// neighbourhood must match ONE panel's corrected pixels in TwoBand *and*
/// Pyramid mode — and the same check must FAIL in Feather mode (which
/// averages the two star positions), proving the test can detect pinching.
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
        global_gradient: (0.0, 0.0, 0.0),
        // Panel 1 is misregistered by 0.6 px in x — stars only.
        panel_shift: vec![(0.0, 0.0), (0.6, 0.0), (0.0, 0.0), (0.0, 0.0)],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 1234,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    let run = |mode: BlendMode| -> MemSink {
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
        sink
    };
    let two = run(BlendMode::TwoBand);
    let pyr = run(BlendMode::Pyramid);
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
    assert!(
        peaks.len() >= 3,
        "need several bright overlap stars, found {}",
        peaks.len()
    );
    let shifted_overlap = peaks.iter().any(|(_, _, covering)| covering.contains(&1));
    assert!(
        shifted_overlap,
        "at least one star must lie in an overlap of the shifted panel"
    );

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

    // TwoBand and Pyramid within ~noise of one panel; Feather's average is
    // far from every panel for at least one bright misregistered star.
    let thresh = 6.0 * spec.noise_sigma;
    let mut feather_fails = 0;
    for &(px, py, ref covering) in &peaks {
        let d_two = one_panel_dist(&two, px, py, covering);
        let d_pyr = one_panel_dist(&pyr, px, py, covering);
        let d_fea = one_panel_dist(&fea, px, py, covering);
        eprintln!(
            "star at ({:4},{:4}) panels {:?}: twoband {:.4}, pyramid {:.4}, feather {:.4} \
             (thresh {:.4})",
            px + cx0,
            py + cy0,
            covering,
            d_two,
            d_pyr,
            d_fea,
            thresh
        );
        assert!(
            d_two < thresh,
            "TwoBand: star at ({px},{py}) matches no single panel (min max-diff {d_two})"
        );
        assert!(
            d_pyr < thresh,
            "Pyramid: star at ({px},{py}) matches no single panel (min max-diff {d_pyr})"
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

/// Mandatory phase-4 test 2 (mid-frequency ghost reduction): two panels share
/// Gaussian blobs (σ ≈ 24 px — structure between the detail band and the
/// feather scale) displaced 3 px between the panels. TwoBand's feathered base
/// averages the two displaced copies over the whole overlap, so the merged
/// blob matches neither panel; the pyramid base seam-switches each frequency
/// band over a distance matched to its scale, so blobs away from the seam
/// come from one panel. Metric per blob: RMS of (merged − closest single
/// corrected panel) over the blob region; Pyramid must beat TwoBand by ≥30%.
/// Additionally the Pyramid profile across the seam shows no step: where the
/// panels disagree near the boundary, the blend fraction moves monotonically
/// (within noise) from one panel to the other, without overshooting either.
#[test]
fn pyramid_reduces_midfrequency_ghosting() {
    let dir = tempdir("ghost");
    let spec = SynthSpec {
        canvas: (768, 512),
        channels: 1,
        grid: (2, 1),
        overlap_frac: 0.6, // wide band: windows [0,500) and [268,768)
        n_stars: 20,
        noise_sigma: 0.002,
        panel_gain_range: (0.95, 1.05),
        panel_offset_range: (-0.003, 0.003),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        // Stars AND blobs shift 3 px in panel 1 (shift_blobs) — mid-scale
        // misregistration the detail band cannot own (blobs live in the base).
        panel_shift: vec![(0.0, 0.0), (3.0, 0.0)],
        panel_spike_angle: vec![],
        mid_blobs: 20,
        shift_blobs: true,
        core: None,
        panel_defects: vec![],
        seed: 37,
    };
    let feather = 64.0f32;
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    let run = |mode: BlendMode| -> MemSink {
        let params = BlendParams {
            feather_px: feather,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
        sink
    };
    let two = run(BlendMode::TwoBand);
    let pyr = run(BlendMode::Pyramid);

    // Corrected panels (the frame the blend output lives in) and the owner
    // map / star masks the blend used (same feather).
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
    let (summaries, owner) =
        mmm_core::diag::load_owner_map(&session, &graph, &phot, None, feather).unwrap();
    let masks: Vec<Vec<bool>> = summaries.iter().map(mmm_core::seam::star_mask).collect();
    let bbox = union_bbox(&session).unwrap();
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);

    // Seam x at a row: first cell of the band owned by panel 1 whose left
    // neighbour is owned by panel 0 (in px).
    let seam_x = |y: u64| -> Option<f64> {
        let y8 = (y / 8) as u32;
        (1..owner.w8).find_map(|x8| {
            (owner.at(x8 - 1, y8) == 0 && owner.at(x8, y8) == 1).then_some((x8 as f64) * 8.0)
        })
    };

    // Blobs whose metric region (±R px) is fully inside BOTH windows, is
    // ≥ 48 px clear of the seam (so hard switching could in principle match
    // one panel), and whose cells the star mask left in the base band.
    const R: i64 = 32;
    let [w0, w1] = [res.windows[0], res.windows[1]];
    let inside = |win: [u64; 4], bx: f64, by: f64| {
        bx - R as f64 >= win[0] as f64
            && bx + R as f64 <= win[2] as f64 - 1.0
            && by - R as f64 >= win[1] as f64
            && by + R as f64 <= win[3] as f64 - 1.0
    };
    let unmasked = |bx: f64, by: f64| {
        let (x8, y8) = ((bx / 8.0) as u32, (by / 8.0) as u32);
        let i = y8 as usize * owner.w8 as usize + x8 as usize;
        !masks[0][i] && !masks[1][i]
    };
    let rms_vs_closest = |sink: &MemSink, bx: f64, by: f64| -> f32 {
        (0..2)
            .map(|p| {
                let (mut ss, mut n) = (0.0f64, 0u64);
                for y in (by as i64 - R)..=(by as i64 + R) {
                    for x in (bx as i64 - R)..=(bx as i64 + R) {
                        let (x, y) = (x as usize, y as usize);
                        let m = sink.at(0, x - cx0, y - cy0);
                        ss += f64::from(m - corrected[p][y * w + x]).powi(2);
                        n += 1;
                    }
                }
                (ss / n as f64).sqrt() as f32
            })
            .fold(f32::INFINITY, f32::min)
    };

    let mut n_metric = 0;
    let (mut sum_two, mut sum_pyr) = (0.0f64, 0.0f64);
    for &(bx, by, amp) in &res.blobs {
        if !inside(w0, bx, by) || !inside(w1, bx, by) || !unmasked(bx, by) {
            continue;
        }
        // No seam on the row, or too close to it: the profile check's job.
        let near_seam = seam_x(by as u64).is_none_or(|sx| (bx - sx).abs() < 48.0);
        let sx = seam_x(by as u64).unwrap_or(f64::NAN);
        if near_seam {
            continue;
        }
        let d_two = rms_vs_closest(&two, bx, by);
        let d_pyr = rms_vs_closest(&pyr, bx, by);
        eprintln!(
            "blob at ({bx:5.1},{by:5.1}) amp {amp:.3}, seam at {sx:5.1}: \
             twoband RMS {d_two:.5}, pyramid RMS {d_pyr:.5}"
        );
        sum_two += d_two as f64;
        sum_pyr += d_pyr as f64;
        n_metric += 1;
    }
    assert!(
        n_metric >= 2,
        "need ≥ 2 clear overlap blobs, got {n_metric} (reseed)"
    );
    let ratio = sum_pyr / sum_two;
    eprintln!(
        "ghost metric over {n_metric} blobs: twoband {:.5}, pyramid {:.5}, ratio {ratio:.3}",
        sum_two / n_metric as f64,
        sum_pyr / n_metric as f64
    );
    assert!(
        ratio <= 0.7,
        "pyramid must reduce mid-frequency ghosting by ≥ 30%, got ratio {ratio:.3}"
    );

    // No step at the seam: around the blob nearest the seam, the row-averaged
    // Pyramid blend fraction α(x) = (merged − c0)/(c1 − c0) rises monotonely
    // (within noise) from the panel-0 side to the panel-1 side wherever the
    // panels disagree enough to measure, and merged never overshoots the
    // envelope of the two panels.
    let near = res
        .blobs
        .iter()
        .filter(|&&(bx, by, _)| inside(w0, bx, by) && inside(w1, bx, by) && unmasked(bx, by))
        .min_by(|a, b| {
            let d = |&(bx, by, _): &(f64, f64, f64)| {
                seam_x(by as u64).map_or(f64::INFINITY, |sx| (bx - sx).abs())
            };
            d(a).total_cmp(&d(b))
        })
        .copied()
        .expect("at least one clear overlap blob");
    let (bx, by, _) = near;
    let sx = seam_x(by as u64).unwrap();
    eprintln!(
        "profile blob at ({bx:.1},{by:.1}), seam at {sx:.1} (dist {:.1})",
        (bx - sx).abs()
    );
    let rows: Vec<usize> = ((by as i64 - 16)..=(by as i64 + 16))
        .map(|y| y as usize)
        .collect();
    // Clamp the profile to the shared-coverage band (16 px margin): outside
    // it one of the "panels" is uncovered and its corrected value means
    // nothing, and panel-rim cells are the feather's job, not the seam's.
    let x_lo = ((sx.min(bx) - 40.0) as usize).max(w1[0] as usize + 16);
    let x_hi = ((sx.max(bx) + 40.0) as usize).min(w0[2] as usize - 17);
    let row_mean = |img: &dyn Fn(usize, usize) -> f32, x: usize| -> f64 {
        rows.iter().map(|&y| img(x, y) as f64).sum::<f64>() / rows.len() as f64
    };
    let merged = |x: usize, y: usize| pyr.at(0, x - cx0, y - cy0);
    let c0 = |x: usize, y: usize| corrected[0][y * w + x];
    let c1 = |x: usize, y: usize| corrected[1][y * w + x];
    let sigma_row = spec.noise_sigma as f64 / (rows.len() as f64).sqrt();
    let mut alphas: Vec<(usize, f64)> = Vec::new();
    for x in x_lo..=x_hi {
        let m = row_mean(&merged, x);
        let a = row_mean(&c0, x);
        let b = row_mean(&c1, x);
        // Envelope: merged stays between the two source panels (within noise).
        let (lo, hi) = (a.min(b), a.max(b));
        assert!(
            m >= lo - 6.0 * sigma_row && m <= hi + 6.0 * sigma_row,
            "merged overshoots the source envelope at x={x}: {m} vs [{lo}, {hi}]"
        );
        if (b - a).abs() > 10.0 * sigma_row {
            alphas.push((x, (m - a) / (b - a)));
        }
    }
    eprintln!(
        "profile: {} measurable columns; α = {:?}",
        alphas.len(),
        alphas
            .iter()
            .map(|&(_, a)| (a * 100.0).round() / 100.0)
            .collect::<Vec<_>>()
    );
    for pair in alphas.windows(2) {
        let (&(x0, a0), &(x1, a1)) = (&pair[0], &pair[1]);
        assert!(
            a1 >= a0 - 0.2,
            "blend fraction steps backwards across the seam: α({x0})={a0:.2} → α({x1})={a1:.2}"
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// FNV-1a hash of a sink's geometry + f32 output bits — a bit-exactness
/// fingerprint for the regression guard below.
fn output_hash(sink: &MemSink) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut upd = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for d in [sink.w as u64, sink.h as u64, sink.ch as u64] {
        d.to_le_bytes().into_iter().for_each(&mut upd);
    }
    for v in &sink.data {
        v.to_le_bytes().into_iter().for_each(&mut upd);
    }
    h
}

/// Mandatory phase-4 test 4 (regression guard): Feather and TwoBand outputs
/// on a fixed-seed synthetic mosaic (exercising shifts, spikes, defects and
/// surfaces) are bit-identical to their pre-pyramid baselines. The literals
/// were captured from the phase-3 code immediately before the pyramid work;
/// the pipeline is deterministic (fixed-order accumulation, rayon only across
/// independent rows), so any change to these paths trips the hashes.
#[test]
fn feather_and_twoband_outputs_are_bit_stable() {
    let dir = tempdir("bitstable");
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 3,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 40,
        noise_sigma: 0.002,
        panel_gain_range: (0.7, 1.4),
        panel_offset_range: (-0.01, 0.02),
        panel_gradient_range: (-0.004, 0.004),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![(0.0, 0.0), (0.6, 0.0), (0.0, 0.3), (0.0, 0.0)],
        panel_spike_angle: vec![0.0, 0.02, 0.0, 0.01],
        panel_defects: vec![(1, 300, 150, 4, 0.03)],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 77,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();

    let run = |mode: BlendMode| -> u64 {
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
        output_hash(&sink)
    };
    let feather = run(BlendMode::Feather);
    let twoband = run(BlendMode::TwoBand);
    eprintln!("feather hash {feather:#018x}, twoband hash {twoband:#018x}");
    // Recaptured for the photometric-solve rework (gain-collapse fix): the
    // per-edge fit became a detrended symmetric (Deming) fit with an
    // identifiability guard, and the global solve now chains fitted lines
    // (cell-count-weighted gain ratios + mean-level rows) instead of raw
    // second moments — gains/offsets shift by ~1e-3 on this spec, moving
    // every output byte. Previous values: feather 0x4e5d_7ebe_25f4_b6e9
    // (dark-moat recapture; phase-3 capture 0x536f_0323_8796_27da before
    // that), twoband 0xa1a7_7370_c8d8_e7b7.
    assert_eq!(
        feather, 0x8762_daa0_68ed_e1a4,
        "Feather output changed — must stay bit-identical"
    );
    assert_eq!(
        twoband, 0xf626_d1ed_2fd6_afee,
        "TwoBand output changed — must stay bit-identical"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Synthetic case for the global-flatten tests: unit gains, zero offsets and
/// a large common sky gradient added to every panel (invisible to photometry
/// and per-panel surfaces — identical in overlaps).
fn global_gradient_spec() -> SynthSpec {
    SynthSpec {
        canvas: (512, 384),
        channels: 1,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 30,
        noise_sigma: 0.002,
        panel_gain_range: (1.0, 1.0),
        panel_offset_range: (0.0, 0.0),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.3, 0.25),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 21,
    }
}

/// Fit a plane `c0 + c1·xn + c2·yn` to `(xn, yn, v)` samples and return the
/// amplitude of its varying part about the canvas center:
/// `max |c1·(xn−0.5) + c2·(yn−0.5)|` over the samples.
fn plane_amp_about_center(samples: &[(f64, f64, f64)]) -> f64 {
    let mut a = [0.0f64; 9];
    let mut b = [0.0f64; 3];
    for &(x, y, v) in samples {
        let phi = [1.0, x, y];
        for i in 0..3 {
            for j in 0..3 {
                a[i * 3 + j] += phi[i] * phi[j];
            }
            b[i] += phi[i] * v;
        }
    }
    let c = solve_dense(&mut a, &mut b, 3).unwrap();
    samples
        .iter()
        .map(|&(x, y, _)| (c[1] * (x - 0.5) + c[2] * (y - 0.5)).abs())
        .fold(0.0f64, f64::max)
}

/// Background residual samples `(xn, yn, merged − truth_in_ref_frame)` over
/// interior background pixels (16 px inside the window union, star pixels
/// excluded by a truth threshold).
fn background_residual_samples(
    sink: &MemSink,
    res: &mmm_core::synth::SynthResult,
    spec: &SynthSpec,
    bbox: [u64; 4],
    ref_gain: f32,
    ref_offset: f32,
) -> Vec<(f64, f64, f64)> {
    let (w, h) = (spec.canvas.0 as usize, spec.canvas.1 as usize);
    let mut mask = vec![false; w * h];
    for &[x0, y0, x1, y1] in &res.windows {
        for y in y0..y1 {
            for x in x0..x1 {
                mask[y as usize * w + x as usize] = true;
            }
        }
    }
    let interior = erode(&mask, w, h, 16);
    let (cx0, cy0) = (bbox[0] as usize, bbox[1] as usize);
    let mut samples = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let truth = res.truth[y * w + x];
            if !interior[y * w + x] || truth >= 0.12 {
                continue; // star cores/wings must not steer the plane fit
            }
            let merged = sink.at(0, x - cx0, y - cy0);
            let r = merged - (truth * ref_gain + ref_offset);
            samples.push((x as f64 / w as f64, y as f64 / h as f64, r as f64));
        }
    }
    assert!(
        samples.len() > 10_000,
        "background sample unexpectedly small"
    );
    samples
}

/// Mandatory phase-3H test 1: a sky gradient common to ALL panels survives
/// photometry (it cancels in every overlap) but `flatten: Some(1)` removes
/// it — the post-flatten background residual's plane-fit amplitude is < 10%
/// of the injected amplitude, measured against the truth mapped through the
/// reference panel like the other e2e tests. The downsampled preview path
/// must subtract the same field.
#[test]
fn flatten_removes_common_sky_gradient() {
    let dir = tempdir("flatten-on");
    let spec = global_gradient_spec();
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let bbox = union_bbox(&session).unwrap();

    let run = |flatten: Option<u32>, downsample: u32| -> MemSink {
        let params = BlendParams {
            feather_px: 24.0,
            downsample,
            band_rows: 64,
            mode: BlendMode::TwoBand,
            roi: None,
            defect_veto: true,
            flatten,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
        sink
    };
    let flat = run(Some(1), 1);
    assert!(
        flat.data.iter().all(|v| v.is_finite()),
        "no NaN/Inf in flattened output"
    );

    let n_panels = res.applied.len();
    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");
    let (ref_gain, ref_offset) = res.applied[reference];

    let samples = background_residual_samples(&flat, &res, &spec, bbox, ref_gain, ref_offset);
    let amp_flat = plane_amp_about_center(&samples);

    // Injected amplitude over the same pixels.
    let (_, gb, gc) = spec.global_gradient;
    let injected: Vec<(f64, f64, f64)> = samples
        .iter()
        .map(|&(x, y, _)| (x, y, gb as f64 * x + gc as f64 * y))
        .collect();
    let amp_inj = plane_amp_about_center(&injected);
    assert!(
        amp_inj > 0.2,
        "injected gradient amplitude sanity: {amp_inj:.3}"
    );

    eprintln!("flatten on: residual plane amp {amp_flat:.4} vs injected {amp_inj:.4}");
    assert!(
        amp_flat < 0.1 * amp_inj,
        "post-flatten background plane amplitude {amp_flat:.4} >= 10% of injected {amp_inj:.4}"
    );

    // Downsample path: the L8 preview subtracts the same global field — the
    // (off − on) preview difference is that field, a plane whose amplitude
    // matches the injected gradient (the fit also absorbs the truth's own
    // mild background trend, hence the generous band).
    let l8_on = run(Some(1), 8);
    let l8_off = run(None, 8);
    assert_eq!((l8_on.w, l8_on.h), (l8_off.w, l8_off.h));
    let (w, h) = (spec.canvas.0 as f64, spec.canvas.1 as f64);
    let (gx0, gy0) = (bbox[0] / 8, bbox[1] / 8);
    let mut diffs = Vec::new();
    for cy in 0..l8_on.h {
        for cx in 0..l8_on.w {
            let (von, voff) = (l8_on.at(0, cx, cy), l8_off.at(0, cx, cy));
            if von == 0.0 || voff == 0.0 {
                continue; // uncovered cell
            }
            let xn = (gx0 as f64 + cx as f64 + 0.5) * 8.0 / w;
            let yn = (gy0 as f64 + cy as f64 + 0.5) * 8.0 / h;
            diffs.push((xn, yn, (voff - von) as f64));
        }
    }
    assert!(
        diffs.len() > 500,
        "too few covered preview cells: {}",
        diffs.len()
    );
    let amp_l8 = plane_amp_about_center(&diffs);
    eprintln!("L8 preview subtracted-field amp {amp_l8:.4} vs injected {amp_inj:.4}");
    assert!(
        amp_l8 > 0.7 * amp_inj && amp_l8 < 1.3 * amp_inj,
        "L8 preview must subtract the same field: amp {amp_l8:.4} vs injected {amp_inj:.4}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-3H test 2: with flatten off (the default), the common sky
/// gradient passes through intact.
#[test]
fn flatten_off_keeps_common_sky_gradient() {
    let dir = tempdir("flatten-off");
    let spec = global_gradient_spec();
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let bbox = union_bbox(&session).unwrap();

    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::TwoBand,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

    let n_panels = res.applied.len();
    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");
    let (ref_gain, ref_offset) = res.applied[reference];

    let samples = background_residual_samples(&sink, &res, &spec, bbox, ref_gain, ref_offset);
    let amp_off = plane_amp_about_center(&samples);
    let (_, gb, gc) = spec.global_gradient;
    let injected: Vec<(f64, f64, f64)> = samples
        .iter()
        .map(|&(x, y, _)| (x, y, gb as f64 * x + gc as f64 * y))
        .collect();
    let amp_inj = plane_amp_about_center(&injected);

    eprintln!("flatten off: residual plane amp {amp_off:.4} vs injected {amp_inj:.4}");
    assert!(
        amp_off > 0.6 * amp_inj,
        "with flatten off the gradient must survive: amp {amp_off:.4} vs injected {amp_inj:.4}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// Phase 5: unaligned solved-panel input
// ---------------------------------------------------------------------------

/// xorshift64* + Box-Muller, deterministic — local so the tests do not depend
/// on synth's private RNG.
struct MiniRng(u64, Option<f64>);

impl MiniRng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9 } else { seed }, None)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    fn gaussian(&mut self) -> f64 {
        if let Some(z) = self.1.take() {
            return z;
        }
        let u1 = self.next_f64().max(f64::MIN_POSITIVE);
        let u2 = self.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (std::f64::consts::TAU * u2).sin_cos();
        self.1 = Some(r * s);
        r * c
    }
}

/// Analytic sky for the solved-input tests: a smooth background plus Gaussian
/// stars, defined on a *truth* TAN frame so every panel (and the merged
/// output) can be compared against the exact noiseless value at any sky
/// position. Purely analytic — no interpolation error enters the ground
/// truth.
struct Scene {
    /// The truth frame (FITS convention).
    wcs: LinearWcs,
    /// Truth-region extent in truth-frame pixels (background normalization).
    w: f64,
    h: f64,
    /// Stars `(u, v, sigma, amp)` in truth-frame FITS coordinates.
    stars: Vec<(f64, f64, f64, f64)>,
}

impl Scene {
    fn new(crval: [f64; 2], scale_deg: f64, w: f64, h: f64, n_stars: usize, seed: u64) -> Scene {
        let wcs = LinearWcs {
            crval,
            crpix: [w / 2.0, h / 2.0],
            cd: [[-scale_deg, 0.0], [0.0, scale_deg]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        };
        let mut rng = MiniRng::new(seed);
        let stars = (0..n_stars)
            .map(|i| {
                let u = rng.range(16.0, w - 16.0);
                let v = rng.range(16.0, h - 16.0);
                let sigma = rng.range(1.4, 2.4);
                let t = if n_stars > 1 {
                    i as f64 / (n_stars - 1) as f64
                } else {
                    0.0
                };
                let amp = 0.7 * (0.03f64 / 0.7).powf(t); // log-spaced 0.7 .. 0.03
                (u, v, sigma, amp)
            })
            .collect();
        Scene { wcs, w, h, stars }
    }

    /// Noiseless value at truth-frame FITS coordinates.
    fn at_uv(&self, u: f64, v: f64, c: usize) -> f64 {
        let tau = std::f64::consts::TAU;
        let mut val = 0.02
            + 0.012 * u / self.w
            + 0.008 * v / self.h
            + 0.004
                * (tau * (1.3 * u / self.w + 0.1 * c as f64)).sin()
                * (tau * (0.9 * v / self.h + 0.2)).sin();
        for &(su, sv, sig, amp) in &self.stars {
            let d2 = (u - su).powi(2) + (v - sv).powi(2);
            if d2 < (6.0 * sig).powi(2) {
                val += amp * (-d2 / (2.0 * sig * sig)).exp();
            }
        }
        val
    }

    /// Noiseless value at a sky position.
    fn value(&self, ra: f64, dec: f64, c: usize) -> f64 {
        let (u, v) = self.wcs.sky_to_pixel(ra, dec);
        self.at_uv(u, v, c)
    }
}

/// One synthetic solved panel: its own geometry, rotation, photometric
/// perturbation, and window center on the truth frame.
struct SolvedPanelSpec {
    w: u64,
    h: u64,
    /// Field rotation in degrees (matrix = R(rot)·diag(−s, +s)).
    rot_deg: f64,
    /// Panel center in truth-frame FITS coordinates.
    center_uv: (f64, f64),
    gain: f32,
    offset: f32,
}

/// Render and write solved panels for `scene`: pixel (i, j) carries the exact
/// analytic scene at the sky its own linear WCS assigns to span coordinate
/// (i + 0.5, j + 0.5), times gain plus offset plus per-panel Gaussian noise.
fn write_solved_panels(
    dir: &Path,
    scene: &Scene,
    specs: &[SolvedPanelSpec],
    channels: usize,
    scale_deg: f64,
    noise_sigma: f64,
    seed: u64,
) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let mut paths = Vec::new();
    for (k, spec) in specs.iter().enumerate() {
        let (cra, cdec) = scene.wcs.pixel_to_sky(spec.center_uv.0, spec.center_uv.1);
        let (sr, cr) = spec.rot_deg.to_radians().sin_cos();
        // R(rot)·diag(−s, +s): the standard astro E-W mirror is preserved.
        let cd = [
            [-scale_deg * cr, -scale_deg * sr],
            [-scale_deg * sr, scale_deg * cr],
        ];
        let refimg = [spec.w as f64 / 2.0, spec.h as f64 / 2.0];
        let lin = LinearWcs {
            crval: [cra, cdec],
            crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
            cd,
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        };
        let (w, h) = (spec.w as usize, spec.h as usize);
        let mut rng = MiniRng::new(seed ^ (0xABCD << k));
        let mut planes = vec![0.0f32; channels * w * h];
        for j in 0..h {
            for i in 0..w {
                // Raw panels honor the span convention: array pixel (i, j)
                // has its center at solution coordinate (i+0.5, j+0.5), i.e.
                // FITS (i+1, j+1).
                let (ra, dec) = lin.pixel_to_sky(i as f64 + 1.0, j as f64 + 1.0);
                let (u, v) = scene.wcs.sky_to_pixel(ra, dec);
                for c in 0..channels {
                    let v0 = scene.at_uv(u, v, c) * spec.gain as f64
                        + spec.offset as f64
                        + noise_sigma * rng.gaussian();
                    planes[(c * h + j) * w + i] = (v0 as f32).max(1e-4);
                }
            }
        }
        let path = dir.join(format!("solved_{k:02}.xisf"));
        write_xisf_solved(
            &path,
            spec.w,
            spec.h,
            channels as u64,
            &planes,
            &SynthWcs {
                crval: [cra, cdec],
                refimg,
                cd,
            },
        )
        .unwrap();
        paths.push(path);
    }
    paths
}

/// Mandatory phase-5 test 1: solved-input e2e. A known analytic sky is cut
/// into four panels at their own offset geometries with correct linear
/// solutions including modest per-panel rotations; `analyze --input solved`
/// chooses a frame, reprojects, and the blended mosaic must match the
/// analytic truth within the phase-1 RMSE bound (2·noise_sigma, noiseless
/// truth in the reference panel's photometric frame).
#[test]
fn solved_input_pipeline_recovers_ground_truth() {
    let dir = tempdir("solved");
    const S: f64 = 1.0e-3; // 3.6″/px
    let noise_sigma = 0.002f64;
    let nch = 3usize;
    let scene = Scene::new([66.0, 18.0], S, 360.0, 280.0, 30, 4242);
    let gains = [1.0f32, 0.85, 1.2, 1.05];
    let offsets = [0.0f32, 0.01, -0.005, 0.004];
    let specs: Vec<SolvedPanelSpec> =
        [(100.0, 80.0), (260.0, 80.0), (100.0, 200.0), (260.0, 200.0)]
            .iter()
            .zip([0.0f64, 2.0, -1.5, 3.0])
            .enumerate()
            .map(|(k, (&center_uv, rot_deg))| SolvedPanelSpec {
                w: 210 + 2 * k as u64, // every panel its own geometry
                h: 170 - 2 * k as u64,
                rot_deg,
                center_uv,
                gain: gains[k],
                offset: offsets[k],
            })
            .collect();
    let paths = write_solved_panels(&dir.join("panels"), &scene, &specs, nch, S, noise_sigma, 7);

    let session = analyze_input(
        &paths,
        &dir.join("s.mmm-session"),
        Some(2),
        InputSelect::Solved,
    )
    .unwrap();
    assert_eq!(session.input, InputKind::Solved);
    let frame = session
        .frame
        .clone()
        .expect("solved session must persist its frame");
    assert_eq!(session.canvas, (frame.width, frame.height, nch as u64));
    assert!((frame.scale_deg - S).abs() < 1e-12, "median scale");
    for p in &session.panels {
        assert!(
            p.source.is_some(),
            "reprojected panels record their source file"
        );
    }

    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();

    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    assert!(sink.finished);
    assert!(
        sink.data.iter().all(|v| v.is_finite()),
        "output must contain no NaN/Inf"
    );

    // The gauge panel fixes the output's photometric frame.
    let n_panels = specs.len();
    let reference = (0..n_panels)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must carry the gauge (g=1, o=0)");
    for c in 0..nch {
        let compose_gain = |p: usize| phot.gains[c][p] * gains[p] as f64;
        let ref_gain = compose_gain(reference);
        for p in 0..n_panels {
            let gain_err = (compose_gain(p) / ref_gain - 1.0).abs();
            assert!(
                gain_err <= 0.02,
                "ch {c} panel {p}: composed gain off by {gain_err:.4} (>2%)"
            );
        }
    }

    // Coverage mask from the output itself (rotated footprints make window
    // rectangles wrong; zero = no-data in the blend too), eroded 16 px.
    let (w, h) = (sink.w, sink.h);
    let mask: Vec<bool> = (0..w * h)
        .map(|i| (0..nch).all(|c| sink.data[c * w * h + i] != 0.0))
        .collect();
    let interior = erode(&mask, w, h, 16);

    let bbox = union_bbox(&session).unwrap();
    let flin = frame.linear_wcs();
    let (ref_gain, ref_offset) = (gains[reference] as f64, offsets[reference] as f64);
    for c in 0..nch {
        let mut sum_sq = 0.0f64;
        let mut n = 0u64;
        for y in 0..h {
            for x in 0..w {
                if !interior[y * w + x] {
                    continue;
                }
                // Placement law: output array pixel (cx, cy) carries the sky
                // of frame FITS coordinate (cx + 0.5, cy + 0.5).
                let (cx, cy) = (bbox[0] as f64 + x as f64, bbox[1] as f64 + y as f64);
                let (ra, dec) = flin.pixel_to_sky(cx + 0.5, cy + 0.5);
                let expected = scene.value(ra, dec, c) * ref_gain + ref_offset;
                let merged = f64::from(sink.at(c, x, y));
                sum_sq += (merged - expected).powi(2);
                n += 1;
            }
        }
        assert!(n > 10_000, "interior region unexpectedly small: {n} px");
        let rmse = (sum_sq / n as f64).sqrt();
        let bound = 2.0 * noise_sigma;
        eprintln!("solved e2e ch {c}: RMSE {rmse:.3e} vs bound {bound:.3e} over {n} interior px");
        assert!(
            rmse < bound,
            "ch {c}: RMSE {rmse:.6} exceeds bound {bound:.6}"
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-5 test 2: auto-detection picks the right mode on both
/// input kinds, applies the coverage rule to same-geometry raw panels, and
/// unsolvable inputs fail with a per-file error naming what is missing.
#[test]
fn auto_detects_input_kind() {
    let dir = tempdir("autodetect");

    // (a) Aligned: same-geometry full-canvas frames, panels cover < 50%.
    let spec = SynthSpec {
        canvas: (256, 192),
        channels: 1,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 10,
        noise_sigma: 0.001,
        panel_gain_range: (0.9, 1.1),
        panel_offset_range: (0.0, 0.0),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed: 11,
    };
    let res = generate(&spec, &dir.join("aligned-panels")).unwrap();
    let session = analyze_input(
        &res.panel_paths,
        &dir.join("a.mmm-session"),
        None,
        InputSelect::Auto,
    )
    .unwrap();
    assert_eq!(
        session.input,
        InputKind::Aligned,
        "full-canvas frames must scan as aligned"
    );
    assert!(session.frame.is_none());
    assert_eq!(session.canvas, (256, 192, 1));

    // (b) Solved: mixed geometries with solutions.
    const S: f64 = 1.0e-3;
    let scene = Scene::new([120.0, -25.0], S, 260.0, 140.0, 8, 99);
    let mixed = [
        SolvedPanelSpec {
            w: 150,
            h: 120,
            rot_deg: 0.0,
            center_uv: (80.0, 70.0),
            gain: 1.0,
            offset: 0.0,
        },
        SolvedPanelSpec {
            w: 154,
            h: 118,
            rot_deg: 2.0,
            center_uv: (180.0, 70.0),
            gain: 1.1,
            offset: 0.002,
        },
    ];
    let paths = write_solved_panels(&dir.join("mixed"), &scene, &mixed, 1, S, 0.001, 3);
    let session =
        analyze_input(&paths, &dir.join("b.mmm-session"), None, InputSelect::Auto).unwrap();
    assert_eq!(
        session.input,
        InputKind::Solved,
        "mixed geometries must go solved"
    );
    assert!(session.frame.is_some());

    // (c) Same-geometry raw solved panels (full coverage): the geometry
    // signal alone says aligned, the ≥ 50%-coverage rule re-dispatches.
    let same = [
        SolvedPanelSpec {
            w: 150,
            h: 120,
            rot_deg: 0.0,
            center_uv: (80.0, 70.0),
            gain: 1.0,
            offset: 0.0,
        },
        SolvedPanelSpec {
            w: 150,
            h: 120,
            rot_deg: 0.0,
            center_uv: (140.0, 70.0),
            gain: 0.95,
            offset: 0.001,
        },
    ];
    let paths = write_solved_panels(&dir.join("same"), &scene, &same, 1, S, 0.001, 4);
    let session =
        analyze_input(&paths, &dir.join("c.mmm-session"), None, InputSelect::Auto).unwrap();
    assert_eq!(
        session.input,
        InputKind::Solved,
        "same-geometry full-coverage panels must re-dispatch to solved"
    );
    assert!(session.frame.is_some());

    // (d) Solved-looking input without solutions: per-file error naming the
    // missing properties.
    let p1 = dir.join("nowcs_a.xisf");
    let p2 = dir.join("nowcs_b.xisf");
    write_xisf(&p1, 40, 30, 1, &vec![0.5; 40 * 30]).unwrap();
    write_xisf(&p2, 42, 30, 1, &vec![0.5; 42 * 30]).unwrap();
    let err = analyze_input(
        &[p1.clone(), p2.clone()],
        &dir.join("d.mmm-session"),
        None,
        InputSelect::Auto,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("nowcs_a.xisf") && err.contains("nowcs_b.xisf"),
        "err: {err}"
    );
    assert!(
        err.contains("ReferenceCelestialCoordinates"),
        "err must name what is missing: {err}"
    );

    // (e) --input solved on unsolved aligned frames errors the same way.
    let err = analyze_input(
        &res.panel_paths,
        &dir.join("e.mmm-session"),
        None,
        InputSelect::Solved,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("astrometric solution"), "err: {err}");

    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- real-data acceptance (manual; multi-GB gitignored test_data) ---------

/// Minimal reader for this project's own FITS output (BITPIX −32, planar,
/// big-endian, rows stored bottom-up with matching bottom-up WCS cards):
/// stored index (x, r) of a channel plane is FITS pixel (x + 1, r + 1).
struct FitsImage {
    w: usize,
    h: usize,
    ch: usize,
    wcs: LinearWcs,
    mmap: memmap2::Mmap,
    data_start: usize,
}

impl FitsImage {
    fn open(path: &Path) -> FitsImage {
        let file =
            std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        let mmap = unsafe { memmap2::Mmap::map(&file) }.unwrap();
        let mut cards = std::collections::HashMap::new();
        let mut off = 0;
        'blocks: loop {
            for i in 0..36 {
                let card = &mmap[off + i * 80..off + (i + 1) * 80];
                let name = String::from_utf8_lossy(&card[..8]).trim().to_string();
                if name == "END" {
                    off += 2880;
                    break 'blocks;
                }
                if &card[8..10] == b"= " {
                    let val = String::from_utf8_lossy(&card[10..]);
                    let val = val.split(" / ").next().unwrap().trim().to_string();
                    cards.insert(name, val);
                }
            }
            off += 2880;
        }
        let num = |n: &str| cards[n].parse::<f64>().unwrap();
        let quoted = |n: &str| cards[n].trim_matches('\'').trim().to_string();
        assert_eq!(cards["BITPIX"], "-32");
        assert_eq!(quoted("ROWORDER"), "BOTTOM-UP");
        let wcs = LinearWcs {
            crval: [num("CRVAL1"), num("CRVAL2")],
            crpix: [num("CRPIX1"), num("CRPIX2")],
            cd: [[num("CD1_1"), num("CD1_2")], [num("CD2_1"), num("CD2_2")]],
            ctype: [quoted("CTYPE1"), quoted("CTYPE2")],
            radesys: quoted("RADESYS"),
        };
        FitsImage {
            w: num("NAXIS1") as usize,
            h: num("NAXIS2") as usize,
            ch: num("NAXIS3") as usize,
            wcs,
            mmap,
            data_start: off,
        }
    }

    fn value(&self, c: usize, x: usize, r: usize) -> f32 {
        let i = self.data_start + ((c * self.h + r) * self.w + x) * 4;
        f32::from_be_bytes(self.mmap[i..i + 4].try_into().unwrap())
    }

    /// One channel plane decoded to native f32 (row r = stored order).
    fn plane(&self, c: usize) -> Vec<f32> {
        let start = self.data_start + c * self.w * self.h * 4;
        self.mmap[start..start + self.w * self.h * 4]
            .chunks_exact(4)
            .map(|b| f32::from_be_bytes(b.try_into().unwrap()))
            .collect()
    }

    /// Bilinear sample of channel `c` at stored-index coordinates; `None` if
    /// any tap is outside or zero (no-data).
    fn sample(&self, c: usize, x: f64, r: f64) -> Option<f64> {
        let (x0, r0) = (x.floor() as i64, r.floor() as i64);
        if x0 < 0 || r0 < 0 || x0 + 1 >= self.w as i64 || r0 + 1 >= self.h as i64 {
            return None;
        }
        let (fx, fr) = (x - x0 as f64, r - r0 as f64);
        let mut acc = 0.0;
        for (dj, wj) in [(0, 1.0 - fr), (1, fr)] {
            for (di, wi) in [(0, 1.0 - fx), (1, fx)] {
                let v = self.value(c, (x0 + di) as usize, (r0 + dj) as usize);
                if v == 0.0 {
                    return None;
                }
                acc += wj * wi * f64::from(v);
            }
        }
        Some(acc)
    }
}

/// Bright, sharp, isolated peaks with local centroids: local maxima above
/// `floor` whose peak stands well above the surrounding ring (rejects
/// extended nebulosity), centroided over a ±5 px window above the ring
/// background, thinned to a ≥ `min_sep` px spacing (brightest first).
fn detect_stars(img: &[f32], w: usize, h: usize, floor: f32, min_sep: f64) -> Vec<(f64, f64, f32)> {
    let m = 12usize;
    let mut cands: Vec<(f64, f64, f32)> = Vec::new();
    for y in m..h - m {
        for x in m..w - m {
            let v = img[y * w + x];
            if v < floor {
                continue;
            }
            let is_max = (y - 4..=y + 4)
                .all(|yy| (x - 4..=x + 4).all(|xx| img[yy * w + xx] <= v || (xx == x && yy == y)));
            if !is_max {
                continue;
            }
            // Ring background at Chebyshev radius 8..=10.
            let mut ring: Vec<f32> = Vec::with_capacity(160);
            for yy in y - 10..=y + 10 {
                for xx in x - 10..=x + 10 {
                    let d = yy.abs_diff(y).max(xx.abs_diff(x));
                    if (8..=10).contains(&d) {
                        ring.push(img[yy * w + xx]);
                    }
                }
            }
            ring.sort_by(f32::total_cmp);
            let bg = ring[ring.len() / 2];
            if v - bg < 0.6 * v {
                continue; // not a sharp point source over its surroundings
            }
            // Centroid over ±5 px, weights (value − bg) clamped at 0.
            let (mut sw, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
            for yy in y - 5..=y + 5 {
                for xx in x - 5..=x + 5 {
                    let wgt = f64::from((img[yy * w + xx] - bg).max(0.0));
                    sw += wgt;
                    sx += wgt * xx as f64;
                    sy += wgt * yy as f64;
                }
            }
            cands.push((sx / sw, sy / sw, v - bg));
        }
    }
    cands.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut picked: Vec<(f64, f64, f32)> = Vec::new();
    for c in cands {
        if picked
            .iter()
            .all(|p| (p.0 - c.0).hypot(p.1 - c.1) >= min_sep)
        {
            picked.push(c);
        }
        if picked.len() >= 30 {
            break;
        }
    }
    picked
}

/// Phase-5 acceptance on the real 12-panel Orion set: full pipeline on the
/// RAW solved panels (analyze auto → align → blend full-res), compared
/// against the registered-input phase-4 output. ≥ 10 bright stars detected in
/// BOTH outputs via local centroid and matched by WCS sky position must agree
/// to < 1 px median; the overall difference is characterized via sky-mapped
/// samples (the frames differ slightly in size/center, so raw indices are
/// never compared). Timings reported (align target < 60 s cold).
///
/// Run (repo root, release):
/// ```sh
/// MMM_REAL_OUT=/path/for/outputs \
/// MMM_REG_SESSION=/path/orion.mmm-session \
/// cargo test -p mmm-core --release --test e2e real_solved -- --ignored --nocapture
/// ```
/// `MMM_REG_SESSION` points at an existing registered-input session (its
/// panel paths must still resolve); the phase-4 comparison output is rebuilt
/// from it unless `MMM_REG_FITS` names one directly.
#[test]
#[ignore = "needs multi-GB test_data/orion_mosaic_raw_panels (gitignored); run manually"]
fn real_solved_pipeline_matches_registered() {
    use mmm_core::astrometry::{wcs_cards, wcs_from_properties};
    use mmm_core::output::fits::{FitsSink, keywords_for_output};
    use mmm_core::session::Session;

    let raw_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_data/orion_mosaic_raw_panels");
    let paths: Vec<PathBuf> = (1..=12)
        .map(|n| {
            raw_dir.join(format!(
                "masterLight_BIN-1_4944x3284_EXPOSURE-30.00s_FILTER-NoFilter_RGB_PANEL-{n}_autocrop.xisf"
            ))
        })
        .collect();
    let out_root = std::env::var_os("MMM_REAL_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&out_root).unwrap();

    // --- raw pipeline: analyze (auto must pick solved) + blend full-res ---
    let session_dir = out_root.join("orion_raw.mmm-session");
    let _ = std::fs::remove_dir_all(&session_dir);
    let t0 = std::time::Instant::now();
    let session = analyze_input(&paths, &session_dir, Some(2), InputSelect::Auto).unwrap();
    let analyze_secs = t0.elapsed().as_secs_f64();
    assert_eq!(
        session.input,
        InputKind::Solved,
        "auto-detect must pick solved"
    );
    let frame = session.frame.clone().unwrap();
    eprintln!(
        "raw analyze: {analyze_secs:.1}s total, align stage {:.1}s; frame {}x{} @ {:.4}\"/px",
        session.align_secs.unwrap(),
        frame.width,
        frame.height,
        frame.scale_deg * 3600.0
    );

    let blend_fits = |session: &Session, wcs: &LinearWcs, out: &Path| -> f64 {
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        let phot = Photometry::load(&session.photometry_path()).unwrap();
        let surf = Surfaces::load(&session.surfaces_path()).ok();
        let params = BlendParams {
            feather_px: 256.0,
            downsample: 1,
            mode: BlendMode::Pyramid,
            roi: None,
            defect_veto: true,
            flatten: None,
            ..Default::default()
        };
        let bbox = mmm_core::blend::output_bbox(session, &params).unwrap();
        let keywords = wcs_cards(wcs, (bbox[0], bbox[1]), bbox[3] - bbox[1]);
        let t = std::time::Instant::now();
        let mut sink = FitsSink::create(out, keywords).unwrap();
        blend(session, &phot, surf.as_ref(), &graph, &params, &mut sink).unwrap();
        t.elapsed().as_secs_f64()
    };

    let raw_fits = out_root.join("orion_raw_full.fits");
    let secs = blend_fits(&session, &frame.linear_wcs(), &raw_fits);
    eprintln!("raw blend full-res: {secs:.1}s -> {}", raw_fits.display());

    // --- registered phase-4 output (given, or rebuilt from its session) ---
    let reg_fits = match std::env::var_os("MMM_REG_FITS") {
        Some(p) => PathBuf::from(p),
        None => {
            let reg_session_dir = PathBuf::from(
                std::env::var_os("MMM_REG_SESSION")
                    .expect("set MMM_REG_SESSION (registered-input session) or MMM_REG_FITS"),
            );
            let mut reg_session = Session::open(&reg_session_dir).unwrap();
            // Sessions created from the repo root store relative panel
            // paths; the test runs from the crate dir — re-anchor them.
            let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            for p in &mut reg_session.panels {
                if p.path.is_relative() {
                    p.path = repo_root.join(&p.path);
                }
            }
            let p0 = XisfPanel::open(&reg_session.panels[0].path).unwrap();
            let wcs = wcs_from_properties(&p0.header().properties).unwrap();
            let _ = &keywords_for_output; // registered passthrough not needed for the comparison
            let out = out_root.join("orion_reg_full.fits");
            let secs = blend_fits(&reg_session, &wcs, &out);
            eprintln!("registered blend full-res: {secs:.1}s -> {}", out.display());
            out
        }
    };

    // --- compare: star positions via WCS sky matching --------------------
    let a = FitsImage::open(&raw_fits);
    let b = FitsImage::open(&reg_fits);
    eprintln!(
        "raw output {}x{}x{}, registered {}x{}x{}",
        a.w, a.h, a.ch, b.w, b.h, b.ch
    );

    let t = std::time::Instant::now();
    let plane_a = a.plane(0);
    let plane_b = b.plane(0);
    let stars_a = detect_stars(&plane_a, a.w, a.h, 0.1, 400.0);
    let stars_b = detect_stars(&plane_b, b.w, b.h, 0.1, 400.0);
    eprintln!(
        "star detection: {} raw / {} registered candidates in {:.1}s",
        stars_a.len(),
        stars_b.len(),
        t.elapsed().as_secs_f64()
    );
    assert!(
        stars_a.len() >= 10 && stars_b.len() >= 10,
        "need ≥ 10 bright stars in both"
    );

    let mut residuals: Vec<f64> = Vec::new();
    for &(ax, ay, amp) in &stars_a {
        let (ra, dec) = a.wcs.pixel_to_sky(ax + 1.0, ay + 1.0);
        let (px, py) = b.wcs.sky_to_pixel(ra, dec);
        let (px, py) = (px - 1.0, py - 1.0); // stored-index space
        let near = stars_b
            .iter()
            .map(|&(bx, by, _)| ((bx - px).hypot(by - py), bx, by))
            .min_by(|u, v| u.0.total_cmp(&v.0));
        if let Some((d, _, _)) = near
            && d < 4.0
        {
            eprintln!(
                "star raw({ax:7.1},{ay:7.1}) amp {amp:.2} -> reg predicted ({px:7.1},{py:7.1}) \
                 residual {d:.3} px"
            );
            residuals.push(d);
        }
    }
    assert!(
        residuals.len() >= 10,
        "only {} stars matched across the outputs",
        residuals.len()
    );
    residuals.sort_by(f64::total_cmp);
    let median = residuals[residuals.len() / 2];
    let max = residuals[residuals.len() - 1];
    eprintln!(
        "star match: {} stars, median residual {median:.3} px, max {max:.3} px",
        residuals.len()
    );
    assert!(
        median < 1.0,
        "median star residual {median:.3} px must be < 1 px"
    );

    // --- overall difference, sampled through the sky mapping --------------
    let mut n = 0u64;
    let mut diffs: Vec<f64> = Vec::new();
    let (mut sum_ratio, mut n_ratio) = (0.0f64, 0u64);
    for r in (0..a.h).step_by(97) {
        for x in (0..a.w).step_by(97) {
            let va: Vec<f32> = (0..a.ch).map(|c| a.value(c, x, r)).collect();
            if va.contains(&0.0) {
                continue;
            }
            let (ra, dec) = a.wcs.pixel_to_sky(x as f64 + 1.0, r as f64 + 1.0);
            let (px, py) = b.wcs.sky_to_pixel(ra, dec);
            let Some(vb) = b.sample(0, px - 1.0, py - 1.0) else {
                continue;
            };
            let d = f64::from(va[0]) - vb;
            diffs.push(d.abs());
            sum_ratio += f64::from(va[0]) / vb;
            n_ratio += 1;
            n += 1;
        }
    }
    diffs.sort_by(f64::total_cmp);
    let p = |q: f64| diffs[((diffs.len() - 1) as f64 * q) as usize];
    eprintln!(
        "value diff over {n} sky-matched samples (ch0): median {:.2e}, p90 {:.2e}, p99 {:.2e}; \
         mean raw/registered ratio {:.4}",
        p(0.5),
        p(0.9),
        p(0.99),
        sum_ratio / n_ratio as f64
    );
    assert!(n > 5_000, "too few comparable samples: {n}");
}

/// Deep-single-coverage identity (regression for the user-reported dark
/// streak, 2026-07-25): two panels whose *corrected* backgrounds genuinely
/// differ — a hand-built photometry lifts panel 0 and darkens panel 1
/// instead of reconciling them, emulating real data where the global solve
/// cannot fully match backgrounds — must still satisfy: in panel 0's
/// single-coverage zone deeper than the largest base transition width from
/// the overlap, the Pyramid output equals panel 0's corrected input within
/// 1e-5.
///
/// Pre-fix, `mask_pyramid` smoothed each panel's ownership mask with no
/// regard to the panel's validity, so at coarse levels the partner's mask
/// support extended hundreds of px past its geometric coverage — where its
/// data pyramid holds the normalized-convolution 0.0 sentinel (validity
/// zero) or wild extrapolation. Blending those with real weight dragged the
/// base toward zero: a broad dark streak in the neighbour's single-coverage
/// territory (observed at ~1e-4 depth over hundreds of px on the Orion
/// mosaic). This test fails on that implementation by >1e-3.
#[test]
fn pyramid_deep_single_coverage_matches_panel() {
    let dir = tempdir("deepsingle");
    let spec = SynthSpec {
        canvas: (4608, 384),
        channels: 1,
        grid: (2, 1),
        overlap_frac: 0.05,
        n_stars: 60,
        noise_sigma: 0.002,
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
    let feather = 256.0f32; // real-data config: 5 pyramid levels
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    // Identity gains, deliberately unreconciled offsets: panel 0 bright,
    // panel 1 darker by 0.13 — the cross-panel base difference the pyramid
    // must confine to the overlap's transition zone.
    let phot = Photometry {
        edge_fits: vec![],
        gains: vec![vec![1.0, 1.0]],
        offsets: vec![vec![0.1, -0.03]],
    };

    let params = BlendParams {
        feather_px: feather,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::Pyramid,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
    let bbox = union_bbox(&session).unwrap();

    // Deep zone: inside panel 0's window, ≥ 64 px from its own rims, and
    // ≥ DEEP px west of panel 1's coverage. DEEP = the largest transition
    // width (level-5 scale: clamped mask support reaches ≤ ~2 level-5 cells
    // = 512 px past coverage; measured influence ends by 454 px) + margin.
    // Pre-fix the bleed reached ~1000 px (1.6e-3 at 646 px — the RED proof).
    const DEEP: u64 = 640;
    let [x0_b, _, _, _] = res.windows[1];
    let [ax0, ay0, ax1, ay1] = res.windows[0];
    let (zx0, zx1) = (ax0 + 64, x0_b - DEEP);
    let (zy0, zy1) = (ay0 + 64, ay1 - 64);
    assert!(zx1 > zx0 + 512, "zone unexpectedly small: [{zx0},{zx1})");
    assert!(ax1 > x0_b, "windows must overlap");

    let panel = XisfPanel::open(&res.panel_paths[0]).unwrap();
    let data = panel.channel(0);
    let (g, o) = (phot.gains[0][0] as f32, phot.offsets[0][0] as f32);
    let mut max_diff = 0.0f32;
    let mut worst = (0u64, 0u64);
    for y in zy0..zy1 {
        for x in zx0..zx1 {
            let merged = sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
            let input = data[(y * spec.canvas.0 + x) as usize] * g + o;
            let d = (merged - input).abs();
            if d > max_diff {
                max_diff = d;
                worst = (x, y);
            }
        }
    }
    eprintln!(
        "deep single-coverage zone x[{zx0},{zx1}) y[{zy0},{zy1}): \
         max |merged − corrected input| = {max_diff:.2e} at {worst:?}"
    );
    assert!(
        max_diff < 1e-5,
        "partner influence bled {max_diff:.2e} into deep single coverage at {worst:?}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
