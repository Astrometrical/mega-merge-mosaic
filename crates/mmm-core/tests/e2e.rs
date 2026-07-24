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
use mmm_core::analyze::analyze;
use mmm_core::blend::{BlendParams, RowSink, blend, union_bbox};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
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
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();

    // Pipeline: analyze (runs overlap graph + photometric solve) → blend.
    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    let params = BlendParams { feather_px: 24.0, downsample: 1, band_rows: 64 };
    let mut sink = MemSink::new();
    blend(&session, &phot, &graph, &params, &mut sink).unwrap();
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
