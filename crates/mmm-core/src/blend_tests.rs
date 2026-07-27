use super::*;
use crate::analyze::analyze;
use crate::synth::write_xisf;
use std::path::{Path, PathBuf};

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

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-blend-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two constant single-channel panels on a 128×64 canvas:
/// A = 0.2 over x∈[8,80), y∈[8,56); B = 0.4 over x∈[48,120), y∈[16,64).
/// Union bbox = [8,8,120,64) → output 112×56.
fn make_panels(dir: &Path) -> (Session, OverlapGraph) {
    let (w, h) = (128u64, 64u64);
    let mut frame = vec![0f32; (w * h) as usize];
    let fill = |frame: &mut [f32], v: f32, x0: u64, y0: u64, x1: u64, y1: u64| {
        for y in y0..y1 {
            for x in x0..x1 {
                frame[(y * w + x) as usize] = v;
            }
        }
    };
    fill(&mut frame, 0.2, 8, 8, 80, 56);
    let a = dir.join("a.xisf");
    write_xisf(&a, w, h, 1, &frame).unwrap();

    frame.fill(0.0);
    fill(&mut frame, 0.4, 48, 16, 120, 64);
    let b = dir.join("b.xisf");
    write_xisf(&b, w, h, 1, &frame).unwrap();

    let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    (session, graph)
}

fn identity_phot(n_panels: usize, ch: usize) -> Photometry {
    Photometry {
        edge_fits: vec![],
        gains: vec![vec![1.0; n_panels]; ch],
        offsets: vec![vec![0.0; n_panels]; ch],
    }
}

#[test]
fn union_bbox_covers_all_panels() {
    let dir = tmpdir("bbox");
    let (session, _) = make_panels(&dir);
    assert_eq!(union_bbox(&session).unwrap(), [8, 8, 120, 64]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn feather_blend_two_overlapping_panels() {
    let dir = tmpdir("feather");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

    // Output is the cropped union bbox, and every pixel was written.
    assert_eq!((sink.w, sink.h, sink.ch), (112, 56, 1));
    assert!(sink.finished);
    assert!(
        sink.data.iter().all(|v| v.is_finite()),
        "no NaN/Inf anywhere"
    );

    // Canvas (x, y) → output (x−8, y−8).
    let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);

    // Zero-covered region untouched (only B covers x≥80, but B starts y=16).
    assert_eq!(at(100, 10), 0.0);

    // Single-coverage interiors: weights normalize out exactly.
    assert!(
        (at(24, 32) - 0.2).abs() < 1e-6,
        "A interior: {}",
        at(24, 32)
    );
    assert!(
        (at(104, 40) - 0.4).abs() < 1e-6,
        "B interior: {}",
        at(104, 40)
    );

    // Overlap midpoint: both panels at full weight → exact average.
    assert!(
        (at(64, 36) - 0.3).abs() < 1e-6,
        "overlap midpoint: {}",
        at(64, 36)
    );

    // Monotone ramp from A's value to B's value across the overlap.
    let mut prev = f32::NEG_INFINITY;
    for x in 48..80 {
        let v = at(x, 36);
        assert!(v >= prev - 1e-5, "ramp not monotone at x={x}: {v} < {prev}");
        prev = v;
    }
    assert!(
        at(48, 36) < 0.3 && at(79, 36) > 0.3,
        "ramp spans the average"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Injecting the default [`crate::ipc::source::FileSource`] through
/// `blend_with_source` must be byte-for-byte identical to the plain
/// `blend` entry point — proves the `PanelSource` indirection changes
/// nothing about the file-backed path. Exercises the two-band
/// (`blend_twoband_impl`) `open_readers` choke-point, the default mode.
#[test]
fn blend_with_file_source_matches_plain_blend() {
    let dir = tmpdir("filesrc");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Pyramid,
        roi: None,
        defect_veto: true,
        flatten: None,
    };

    let mut a = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut a).unwrap();

    let mut b = MemSink::new();
    blend_with_source(
        &session,
        &phot,
        None,
        &graph,
        &params,
        &crate::ipc::source::FileSource,
        &mut b,
    )
    .unwrap();

    assert_eq!(
        (a.w, a.h, a.ch),
        (b.w, b.h, b.ch),
        "geometry must match between blend and blend_with_source"
    );
    assert_eq!(a.finished, b.finished);
    assert_eq!(
        a.data, b.data,
        "injecting the default FileSource must not change output"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn roi_matches_full_blend_subregion() {
    let dir = tmpdir("roi");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    let full = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut full_sink = MemSink::new();
    blend(&session, &phot, None, &graph, &full, &mut full_sink).unwrap();

    // ROI spanning the overlap: canvas [40,100)x[20,60), clipped to union y<64.
    let roi = BlendParams {
        roi: Some([40, 20, 100, 60]),
        ..full.clone()
    };
    let mut roi_sink = MemSink::new();
    blend(&session, &phot, None, &graph, &roi, &mut roi_sink).unwrap();

    assert_eq!((roi_sink.w, roi_sink.h), (60, 40));
    for y in 0..roi_sink.h {
        for x in 0..roi_sink.w {
            // ROI output (x,y) = canvas (40+x, 20+y) = full output (32+x, 12+y).
            let (a, b) = (roi_sink.at(0, x, y), full_sink.at(0, x + 32, y + 12));
            assert!(
                (a - b).abs() < 1e-6,
                "mismatch at roi ({x},{y}): {a} vs {b}"
            );
        }
    }

    // Disjoint ROI errors.
    let miss = BlendParams {
        roi: Some([0, 0, 4, 4]),
        ..full.clone()
    };
    assert!(blend(&session, &phot, None, &graph, &miss, &mut MemSink::new()).is_err());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn photometric_corrections_are_applied() {
    let dir = tmpdir("phot");
    let (session, graph) = make_panels(&dir);
    let phot = Photometry {
        edge_fits: vec![],
        gains: vec![vec![2.0, 1.0]],
        offsets: vec![vec![0.01, 0.0]],
    };
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

    let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);
    // A interior: 0.2·2 + 0.01 = 0.41; B untouched; overlap = mean of both.
    assert!((at(24, 32) - 0.41).abs() < 1e-6);
    assert!((at(104, 40) - 0.4).abs() < 1e-6);
    assert!((at(64, 36) - 0.405).abs() < 1e-6);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn surfaces_are_applied_during_accumulation() {
    let dir = tmpdir("surf");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    // Panel A gets s(x,y) = 0.05 + 0.1·x − 0.02·y + 0.2·x² (normalized
    // canvas coords, canvas 128×64); panel B gets zero.
    let surf = crate::surfaces::Surfaces {
        order: 2,
        coeffs: vec![vec![vec![0.05, 0.1, -0.02, 0.2, 0.0, 0.0], vec![0.0; 6]]],
        max_abs_s: vec![],
        bg_mad: vec![],
    };
    let s_at = |x: f64, y: f64| {
        let (xn, yn) = (x / 128.0, y / 64.0);
        0.05 + 0.1 * xn - 0.02 * yn + 0.2 * xn * xn
    };

    let params = BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);

    // A interior: 0.2 + s(x,y); B interior unchanged; overlap midpoint
    // (both full weight): mean of corrected values.
    let (ax, ay) = (24u64, 32u64);
    let expect_a = 0.2 + s_at(ax as f64, ay as f64) as f32;
    assert!(
        (at(ax, ay) - expect_a).abs() < 1e-5,
        "A interior {} vs {expect_a}",
        at(ax, ay)
    );
    assert!(
        (at(104, 40) - 0.4).abs() < 1e-6,
        "B interior must stay uncorrected"
    );
    let expect_mid = 0.5 * ((0.2 + s_at(64.0, 36.0) as f32) + 0.4);
    assert!(
        (at(64, 36) - expect_mid).abs() < 1e-5,
        "overlap midpoint {} vs {expect_mid}",
        at(64, 36)
    );

    // Same corrections must hold on the L8 preview path (cell centers).
    let params8 = BlendParams {
        feather_px: 16.0,
        downsample: 8,
        band_rows: 4,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink8 = MemSink::new();
    blend(&session, &phot, Some(&surf), &graph, &params8, &mut sink8).unwrap();
    // Canvas cell (3,4) is A-only; center pixel (28, 36).
    let got = sink8.at(0, 3 - 1, 4 - 1);
    let expect_cell = 0.2 + s_at(28.0, 36.0) as f32;
    assert!(
        (got - expect_cell).abs() < 1e-5,
        "L8 A-only cell {got} vs {expect_cell}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn downsample_blends_from_l8_summaries() {
    let dir = tmpdir("l8");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 8,
        band_rows: 3,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

    // L8 grid cropped to union bbox / 8: x8∈[1,15), y8∈[1,8) → 14×7.
    assert_eq!((sink.w, sink.h, sink.ch), (14, 7, 1));
    assert!(sink.data.iter().all(|v| v.is_finite()));

    // Canvas cell (x8, y8) → output (x8−1, y8−1).
    let at = |x8: usize, y8: usize| sink.at(0, x8 - 1, y8 - 1);
    assert!((at(3, 4) - 0.2).abs() < 1e-6, "A-only cell: {}", at(3, 4));
    assert!((at(8, 4) - 0.3).abs() < 1e-6, "overlap cell: {}", at(8, 4));
    assert_eq!(at(13, 1), 0.0, "cell covered by neither panel");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-3E test 3 (spike integrity, end-to-end): two panels
/// share bright spiked stars in their overlap; panel 1 is misregistered
/// by 0.6 px and its spikes are rotated 0.02 rad (per-session camera
/// rotation). Every pixel along each merged arm must match ONE panel's
/// corrected pixels — and the same check must FAIL with the star masks
/// disabled, proving the mask (not luck) protects the arms.
#[test]
fn twoband_spike_arms_match_one_panel() {
    use crate::analyze::analyze_opts;
    use crate::synth::{SynthSpec, generate};

    let dir = tmpdir("spikes");
    let spec = SynthSpec {
        canvas: (768, 640),
        channels: 1,
        grid: (2, 1),
        overlap_frac: 0.25,
        n_stars: 90,
        noise_sigma: 0.002,
        panel_gain_range: (0.95, 1.1),
        panel_offset_range: (-0.003, 0.005),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![(0.0, 0.0), (0.8, 0.0)],
        panel_spike_angle: vec![0.0, 0.02],
        panel_defects: vec![],
        // Seed chosen (deterministically) so a bright spiked star lands on
        // the band's midline, where the mask-disabled seam runs within
        // ramp reach of its arms — the kink scenario observed on real
        // data. Verified: mask on ≤ 0.0004 on all arms; mask off 0.0203
        // on one arm (thresh 0.0120).
        mid_blobs: 0,
        shift_blobs: false,
        seed: 9,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode: BlendMode::TwoBand,
        roi: None,
        // This test isolates the star mask's protection: in the run(false)
        // teeth check every arm cell is unmasked and thus veto-eligible,
        // and the veto could partially repair the arm mismatch the check
        // must detect. The veto has its own tests.
        defect_veto: false,
        flatten: None,
    };

    let run = |use_mask: bool, mode: BlendMode| -> MemSink {
        let params = BlendParams {
            mode,
            ..params.clone()
        };
        let mut sink = MemSink::new();
        blend_twoband_impl(
            &session,
            &phot,
            None,
            &graph,
            &params,
            &crate::ipc::source::FileSource,
            &mut sink,
            use_mask,
        )
        .unwrap();
        sink
    };
    let with_mask = run(true, BlendMode::TwoBand);
    let with_mask_pyr = run(true, BlendMode::Pyramid);
    let no_mask = run(false, BlendMode::TwoBand);

    // Corrected panel pixels in the blend's photometric frame.
    let w = spec.canvas.0 as usize;
    let corrected: Vec<Vec<f32>> = res
        .panel_paths
        .iter()
        .enumerate()
        .map(|(p, path)| {
            let panel = crate::formats::xisf::XisfPanel::open(path).unwrap();
            let (g, o) = (phot.gains[0][p] as f32, phot.offsets[0][p] as f32);
            panel.channel(0).iter().map(|&v| v * g + o).collect()
        })
        .collect();
    let bbox = union_bbox(&session).unwrap();

    // Spiked stars whose full spike footprint (arm length + margin) lies
    // inside BOTH panel windows — arms crossing the overlap band.
    let overlap_stars: Vec<(f64, f64, f64)> = res
        .spiked
        .iter()
        .copied()
        .filter(|&(sx, sy, len)| {
            res.windows.iter().all(|&[x0, y0, x1, y1]| {
                sx - len >= x0 as f64 + 4.0
                    && sx + len < x1 as f64 - 4.0
                    && sy - len >= y0 as f64 + 4.0
                    && sy + len < y1 as f64 - 4.0
            })
        })
        .collect();
    assert!(
        !overlap_stars.is_empty(),
        "seed must place at least one spiked star fully inside the overlap"
    );

    // Pixels of one arm: 3×3 neighbourhoods along the arm at BOTH panels'
    // spike angles (the merged arm may come from either panel), skipping
    // the core where the panels nearly agree anyway.
    let arm_pixels = |sx: f64, sy: f64, len: f64, arm: usize| -> Vec<(usize, usize)> {
        let mut px = Vec::new();
        for &a0 in &spec.panel_spike_angle {
            let th = a0 as f64 + arm as f64 * std::f64::consts::FRAC_PI_2;
            let (s, c) = th.sin_cos();
            let mut t = 3.0;
            while t <= len {
                let (x, y) = ((sx + t * c).round() as i64, (sy + t * s).round() as i64);
                for yy in y - 1..=y + 1 {
                    for xx in x - 1..=x + 1 {
                        px.push((xx as usize, yy as usize));
                    }
                }
                t += 1.0;
            }
        }
        px
    };
    // Distance of the merged arm to the closest single corrected panel.
    let one_panel_dist = |sink: &MemSink, pixels: &[(usize, usize)]| -> f32 {
        (0..2)
            .map(|p| {
                pixels
                    .iter()
                    .map(|&(x, y)| {
                        let merged = sink.at(0, x - bbox[0] as usize, y - bbox[1] as usize);
                        (merged - corrected[p][y * w + x]).abs()
                    })
                    .fold(0.0f32, f32::max)
            })
            .fold(f32::INFINITY, f32::min)
    };

    let thresh = 6.0 * spec.noise_sigma;
    let mut no_mask_fails = 0;
    let mut arms = 0;
    for &(sx, sy, len) in &overlap_stars {
        for arm in 0..4 {
            let pixels = arm_pixels(sx, sy, len, arm);
            let d_mask = one_panel_dist(&with_mask, &pixels);
            let d_pyr = one_panel_dist(&with_mask_pyr, &pixels);
            let d_none = one_panel_dist(&no_mask, &pixels);
            eprintln!(
                "spiked star ({sx:6.1},{sy:6.1}) arm {arm}: \
                 masked {d_mask:.4}, pyramid {d_pyr:.4}, unmasked {d_none:.4} \
                 (thresh {thresh:.4})"
            );
            assert!(
                d_mask < thresh,
                "arm {arm} of star at ({sx},{sy}) matches no single panel: {d_mask}"
            );
            assert!(
                d_pyr < thresh,
                "Pyramid: arm {arm} of star at ({sx},{sy}) matches no single panel: {d_pyr}"
            );
            if d_none >= thresh {
                no_mask_fails += 1;
            }
            arms += 1;
        }
    }
    assert!(
        no_mask_fails > 0,
        "mask-disabled blend passed the one-panel check on all {arms} arms — \
         the mask is not biting"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-3F test 1: a satellite-trail piece and a cosmic ray in
/// ONE panel's overlap region are vetoed — the merged pixels match the
/// clean panel within noise — while `defect_veto: false` leaves them at
/// full strength (proving both directions).
#[test]
fn twoband_defect_veto_suppresses_overlap_defects() {
    use crate::analyze::analyze_opts;
    use crate::seam::compute_owner_map;
    use crate::synth::{SynthSpec, generate};

    let dir = tmpdir("veto");
    // 2×1 grid on 768×480: windows [0,432) and [336,768), overlap band
    // x∈[336,432). The band is 12×60 cells — under seam::DP_MIN_LONG on
    // both axes — so ownership keeps the deterministic Voronoi midline at
    // x≈384 and the defects (x ≥ 414) are owned by panel 1, the panel
    // that carries them: without the veto they show at full strength.
    let noise = 0.0025f32;
    // Amplitudes thread the veto's needle deliberately: |Δdetail| ≈ 0.97
    // amp must exceed 6× the clean panel's cell RMS (≈ noise), while the
    // defect cell's own RMS (0.17×/0.12× amp for 2 px/1 px per cell) must
    // stay under the star mask's 3×-median seed threshold — like the
    // real defects that matter, bright but not star-bright. The trail
    // starts at x=414 (cell phase 6) so its 4 px straddle two cells.
    let trail = (1usize, 414u64, 268u64, 4u32, 0.032f32);
    let ray = (1usize, 421u64, 156u64, 1u32, 0.040f32);
    let spec = SynthSpec {
        canvas: (768, 480),
        channels: 1,
        grid: (2, 1),
        overlap_frac: 0.25,
        n_stars: 30,
        noise_sigma: noise,
        panel_gain_range: (0.95, 1.1),
        panel_offset_range: (-0.003, 0.005),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![trail, ray],
        mid_blobs: 0,
        shift_blobs: false,
        seed: 11,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

    // Preconditions (defects must sit where the veto is allowed to act):
    // their cells star-mask-clear in both panels and owned by panel 1.
    let summaries: Vec<L8Summary> = session
        .panels
        .iter()
        .map(|p| L8Summary::read(&session.summary_path(p.id)).unwrap())
        .collect();
    let masks: Vec<Vec<bool>> = summaries.iter().map(crate::seam::star_mask).collect();
    let owner = compute_owner_map(&summaries, &graph, &phot, None, session.canvas, 24.0);
    for &(_, dx, dy, len, _) in &spec.panel_defects {
        for x in dx..dx + len as u64 {
            let cell =
                (dy / BLOCK as u64) as usize * owner.w8 as usize + (x / BLOCK as u64) as usize;
            assert!(
                !masks[0][cell] && !masks[1][cell],
                "defect cell at ({x},{dy}) must be star-mask-clear (move it or reseed)"
            );
            assert_eq!(
                owner.owner[cell], 1,
                "defect cell at ({x},{dy}) must be owned by the defect panel"
            );
        }
    }

    let run = |veto: bool, mode: BlendMode| -> MemSink {
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: veto,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
        sink
    };
    let on = run(true, BlendMode::TwoBand);
    let off = run(false, BlendMode::TwoBand);
    let on_pyr = run(true, BlendMode::Pyramid);
    let off_pyr = run(false, BlendMode::Pyramid);

    // Clean panel (0), corrected into the blend's photometric frame.
    let w = spec.canvas.0 as usize;
    let clean = crate::formats::xisf::XisfPanel::open(&res.panel_paths[0]).unwrap();
    let (g0, o0) = (phot.gains[0][0] as f32, phot.offsets[0][0] as f32);
    let corrected0: Vec<f32> = clean.channel(0).iter().map(|&v| v * g0 + o0).collect();
    let bbox = union_bbox(&session).unwrap();

    // Max |merged − clean panel| over the defect pixels ±1.
    let defect_dist = |sink: &MemSink, d: (usize, u64, u64, u32, f32)| -> f32 {
        let (_, dx, dy, len, _) = d;
        let mut worst = 0.0f32;
        for y in dy - 1..=dy + 1 {
            for x in dx - 1..dx + len as u64 + 1 {
                let merged = sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
                worst = worst.max((merged - corrected0[y as usize * w + x as usize]).abs());
            }
        }
        worst
    };

    let thresh = 6.0 * noise;
    for (mode, on, off) in [
        (BlendMode::TwoBand, &on, &off),
        (BlendMode::Pyramid, &on_pyr, &off_pyr),
    ] {
        for (name, d) in [("trail", trail), ("ray", ray)] {
            let d_on = defect_dist(on, d);
            let d_off = defect_dist(off, d);
            eprintln!(
                "{mode:?} {name}: veto on {d_on:.4}, veto off {d_off:.4} (thresh {thresh:.4})"
            );
            assert!(
                d_on < thresh,
                "{mode:?} {name}: veto ON must match the clean panel within noise, got {d_on}"
            );
            assert!(
                d_off > thresh,
                "{mode:?} {name}: veto OFF must show the defect (teeth), got {d_off}"
            );
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Mandatory phase-3F test 3: a defect *outside* any overlap has nothing
/// to be compared against — the owner's value stands untouched, veto ON.
#[test]
fn defect_outside_overlap_is_untouched() {
    use crate::analyze::analyze_opts;
    use crate::synth::{SynthSpec, generate};

    let dir = tmpdir("veto-single");
    // Windows [0,288) and [224,512): x=400 is covered by panel 1 alone.
    let defect = (1usize, 400u64, 190u64, 4u32, 0.03f32);
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 1,
        grid: (2, 1),
        overlap_frac: 0.25,
        n_stars: 20,
        noise_sigma: 0.0025,
        panel_gain_range: (0.95, 1.1),
        panel_offset_range: (-0.003, 0.005),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![defect],
        mid_blobs: 0,
        shift_blobs: false,
        seed: 5,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

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

    let w = spec.canvas.0 as usize;
    let panel = crate::formats::xisf::XisfPanel::open(&res.panel_paths[1]).unwrap();
    let (g1, o1) = (phot.gains[0][1] as f32, phot.offsets[0][1] as f32);
    let corrected1: Vec<f32> = panel.channel(0).iter().map(|&v| v * g1 + o1).collect();
    let bbox = union_bbox(&session).unwrap();
    let (_, dx, dy, len, amp) = defect;

    // Single coverage: base + (full − base) reconstructs the corrected
    // input exactly — including the defect, at full strength.
    let mut worst = 0.0f32;
    for y in dy - 2..=dy + 2 {
        for x in dx - 2..dx + len as u64 + 2 {
            let merged = sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
            worst = worst.max((merged - corrected1[y as usize * w + x as usize]).abs());
        }
    }
    eprintln!("outside-overlap defect: max |merged − corrected input| = {worst:.2e}");
    assert!(
        worst < 1e-4,
        "owner value must stand untouched, max diff {worst}"
    );

    // And the defect really is present in the output (not vetoed away):
    // the defect pixel towers over the row 4 px below by ~the amplitude.
    let at = |x: u64, y: u64| sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
    let step = at(dx + 1, dy) - at(dx + 1, dy + 4);
    assert!(
        step > 0.5 * amp * g1,
        "defect must survive in single coverage: step {step} vs amp {amp}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Deterministic per-pixel "noise" (splitmix-style hash), identical for
/// every generation of the same canvas — it cancels exactly out of the
/// cross-panel and cross-run differences the structure tests measure,
/// while giving the star mask a realistic nonzero median detail floor.
fn hash_noise(x: u64, y: u64) -> f32 {
    let mut s = (y.wrapping_mul(0x1_0000_0001) ^ x).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    s ^= s >> 33;
    s = s.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    s ^= s >> 33;
    (((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2.0 * 0.002) as f32
}

/// Two panels reproducing the M42-staircase geometry: a vertical overlap
/// band x∈[224,288) over the full 640-px height, crossed by a bright
/// *horizontal* ridge of extended structure (σ=50 px around y=320) with
/// cell-scale texture and one embedded bright star — so the connected
/// star mask floods across the whole band at structure latitudes exactly
/// as it does over the real M42 core. Panel B carries a residual
/// mismatch proportional to the structure brightness (peak `mismatch`)
/// plus a small background anchor that keeps the DP seam mid-band in the
/// unmasked rows. Panel A's exact pixel values are returned so tests can
/// measure the merged output's deviation profile `D = out − A`.
fn structure_band_panels(dir: &Path, mismatch: f32) -> (Session, OverlapGraph, Vec<f32>) {
    std::fs::create_dir_all(dir).unwrap();
    let (w, h) = (512u64, 640u64);
    let ridge = |y: f64| 0.3 * (-((y - 320.0) * (y - 320.0)) / (2.0 * 50.0 * 50.0)).exp();
    let sky_a = |x: u64, y: u64| -> f32 {
        let (xf, yf) = (x as f64, y as f64);
        let s = ridge(yf);
        let tex = s * 0.15 * (1.19 * xf).sin() * (1.33 * yf).sin();
        let dx = xf - 256.0;
        let dy = yf - 320.0;
        let star = 1.0 * (-(dx * dx + dy * dy) / (2.0 * 2.0 * 2.0)).exp();
        0.05 + (s + tex + star) as f32 + hash_noise(x, y)
    };
    // Residual inter-panel mismatch ∝ structure brightness, plus a mild
    // background anchor (cheapest at the band's midline) so the seam has
    // a preferred mid-band line in the unmasked background rows.
    let delta = |x: u64, y: u64| -> f32 {
        let anchor = 0.002 * (((x as f64 - 256.0) / 32.0).powi(2)).min(4.0);
        (mismatch as f64 * ridge(y as f64) / 0.3 + anchor) as f32
    };

    // The full-canvas A-frame sky — the reference D is measured against
    // (defined everywhere, unlike panel A's zero-padded window).
    let truth_a: Vec<f32> = (0..w * h).map(|i| sky_a(i % w, i / w)).collect();

    let mut frame = vec![0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..288u64 {
            frame[(y * w + x) as usize] = truth_a[(y * w + x) as usize];
        }
    }
    let a = dir.join("a.xisf");
    write_xisf(&a, w, h, 1, &frame).unwrap();

    frame.fill(0.0);
    for y in 0..h {
        for x in 224..512u64 {
            frame[(y * w + x) as usize] = truth_a[(y * w + x) as usize] + delta(x, y);
        }
    }
    let b = dir.join("b.xisf");
    write_xisf(&b, w, h, 1, &frame).unwrap();

    let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    (session, graph, truth_a)
}

/// Mandatory M42-staircase-fix test 2 (seam-through-structure
/// continuity): where the connected star mask floods across bright
/// extended structure spanning the whole overlap band, the merged output
/// must stay continuous across the owner boundary despite a residual
/// inter-panel mismatch — the mismatch must ramp, not snap.
///
/// Metric: D(x, y) = merged − panel A's exact pixels. Away from the seam
/// D is ~0 (A side) or ~the mismatch (B side); the max adjacent-pixel
/// jump of D along rows crossing the band measures how abruptly the
/// transition happens. A no-mismatch control run bounds the machinery's
/// baseline jump. Teeth: with star-lock snapping on the full flooded
/// mask this measured a max jump of 0.0283 ≈ the full band mismatch in
/// both TwoBand and Pyramid mode (control 0.0103) — the L8-quantized
/// staircase; the bound below fails loudly on that behaviour. The ramp
/// alone still left 0.0147 ≈ *half* the mismatch, stepping at panel A's
/// coverage rim (x=287) where the DP hugs the band edge and the ramp is
/// truncated by coverage — the rim fade in the detail mix removes that
/// too. Current: TwoBand 0.0015 (control 0.0003), Pyramid 0.0015
/// (control 0.0006), with the base excluding the full star mask.
#[test]
fn seam_through_structure_ramps_the_mismatch() {
    use crate::seam::split_mask_components;

    let dir = tmpdir("structseam");
    let mismatch = 0.03f32;
    let (session, graph, truth_a) = structure_band_panels(&dir.join("mm"), mismatch);
    let (ctrl_session, ctrl_graph, ctrl_truth) = structure_band_panels(&dir.join("ctrl"), 0.0);
    let phot = identity_phot(2, 1);

    // Preconditions — the scenario must reproduce the M42 diagnosis:
    // at structure latitudes the connected mask covers the entire band
    // in both panels (flooded through bright structure), and every one
    // of those cells classifies as extended structure, not compact.
    let summaries = load_summaries(&session).unwrap();
    let (w8, h8) = (summaries[0].w8, summaries[0].h8);
    let masks: Vec<Vec<bool>> = summaries.iter().map(star_mask).collect();
    let splits: Vec<(Vec<bool>, Vec<bool>)> = masks
        .iter()
        .map(|m| split_mask_components(m, w8, h8))
        .collect();
    for y8 in 34..46u32 {
        for x8 in 28..36u32 {
            let i = (y8 * w8 + x8) as usize;
            for p in 0..2 {
                assert!(
                    masks[p][i],
                    "band cell ({x8},{y8}) must be mask-flooded in panel {p}"
                );
                assert!(
                    splits[p].1[i] && !splits[p].0[i],
                    "band cell ({x8},{y8}) must classify as structure in panel {p}"
                );
            }
        }
    }

    // The owner boundary at structure latitudes must run through
    // structure-masked cells (the seam has no mask-free corridor —
    // exactly the M42 situation), wherever the DP put it.
    let owner = crate::seam::compute_owner_map_masked(
        &summaries,
        &graph,
        &phot,
        None,
        session.canvas,
        64.0,
        &masks,
    );
    for y8 in 34..46u32 {
        let bx = (1..w8)
            .find(|&x8| owner.at(x8 - 1, y8) == 0 && owner.at(x8, y8) == 1)
            .expect("each band row must have an A→B owner boundary");
        let i = (y8 * w8 + bx) as usize;
        assert!(
            splits[0].1[i] || splits[1].1[i],
            "boundary cell ({bx},{y8}) must be structure-masked"
        );
    }

    let run = |session: &Session, graph: &OverlapGraph, mode: BlendMode| -> MemSink {
        let params = BlendParams {
            feather_px: 64.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(session, &phot, None, graph, &params, &mut sink).unwrap();
        sink
    };

    // Max adjacent-pixel jump of D = merged − truth_A along band-crossing
    // rows at structure latitudes (output coords == canvas coords: both
    // panels' content starts at the origin).
    let jump = |sink: &MemSink, truth: &[f32]| -> f32 {
        let mut worst = 0.0f32;
        for y in (248..392u64).step_by(4) {
            let d = |x: u64| sink.at(0, x as usize, y as usize) - truth[(y * 512 + x) as usize];
            // Whole shared band incl. its edge cells: the DP may hug a
            // band edge in fully-masked rows, putting the transition at
            // x=224/288 — the panel rims, where the ramp is truncated by
            // coverage and only the rim fade keeps D continuous;
            // single-coverage pixels either side reconstruct their panel
            // exactly, so D stays well-defined throughout.
            for x in 220..292u64 {
                worst = worst.max((d(x + 1) - d(x)).abs());
            }
        }
        worst
    };

    for mode in [BlendMode::TwoBand, BlendMode::Pyramid] {
        let out = run(&session, &graph, mode);
        let ctrl = run(&ctrl_session, &ctrl_graph, mode);

        // The mismatch really is in the data and survives to the far
        // side: in B-only coverage well beyond the feather scale the
        // output reconstructs B, D = mismatch (peak row) + anchor.
        let d_far = out.at(0, 420, 320) - truth_a[320 * 512 + 420];
        assert!(
            d_far > 0.8 * mismatch,
            "far-side D must carry the mismatch, got {d_far}"
        );

        let j_mm = jump(&out, &truth_a);
        let j_ctrl = jump(&ctrl, &ctrl_truth);
        eprintln!("{mode:?}: max D-jump {j_mm:.4} (control {j_ctrl:.4}, mismatch {mismatch})");
        assert!(
            j_mm < (2.0 * j_ctrl).max(0.008),
            "{mode:?}: seam through structure steps by {j_mm} \
             (control baseline {j_ctrl}) — the mismatch must ramp, not snap"
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Two panels reproducing the bright-star reflection-halo geometry behind
/// the user-reported dark moat: a vertical overlap band x∈[160,480) on a
/// 640×640 canvas, one bright star at (320,320) whose broad reflection
/// halo *differs between the panels* (halos are internal reflections —
/// optics- and position-dependent — so overlapping panels genuinely
/// disagree there: A carries 0.4·G(σ=24 px), B carries 0.3·G(σ=34 px)
/// centered 3 px off). The halo floods each panel's connected star mask
/// well past every compactness bound, so the star+halo component
/// classifies as extended structure in both panels — the exact trigger
/// of the regression. Shared deterministic noise gives the mask a
/// realistic median-detail floor and cancels out of cross-panel diffs.
fn halo_moat_panels(dir: &Path) -> (Session, OverlapGraph) {
    std::fs::create_dir_all(dir).unwrap();
    let (w, h) = (640u64, 640u64);
    let (sx, sy) = (320.0f64, 320.0f64);
    let value = |x: u64, y: u64, amp: f64, sigma: f64, halo_dx: f64| -> f32 {
        let (dx, dy) = (x as f64 - sx, y as f64 - sy);
        let core = 2.0 * (-(dx * dx + dy * dy) / (2.0 * 1.5 * 1.5)).exp();
        let hx = dx - halo_dx;
        let halo = amp * (-(hx * hx + dy * dy) / (2.0 * sigma * sigma)).exp();
        0.05 + (core + halo) as f32 + hash_noise(x, y)
    };
    let mut frame = vec![0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..480u64 {
            frame[(y * w + x) as usize] = value(x, y, 0.3, 26.0, 0.0);
        }
    }
    let a = dir.join("a.xisf");
    write_xisf(&a, w, h, 1, &frame).unwrap();

    frame.fill(0.0);
    for y in 0..h {
        for x in 160..640u64 {
            frame[(y * w + x) as usize] = value(x, y, 0.8, 40.0, 4.0);
        }
    }
    let b = dir.join("b.xisf");
    write_xisf(&b, w, h, 1, &frame).unwrap();

    let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    (session, graph)
}

/// Moat regression (user report on real Orion data, e.g. canvas
/// (2376,13912)): when base exclusion acted on *compact* mask components
/// only, a bright star's reflection halo — flooded into an
/// extended-structure component — entered the pyramid base band. Near
/// ownership/mask transitions the per-level masked blend then mixes the
/// panels' disagreeing halos with level-dependent weights, so the halo's
/// Laplacian negative lobes no longer cancel: a wide dark annulus
/// (clearly below background) around the star+halo. The base must
/// exclude the FULL star mask (compact ∪ structure) as in phase 3/4;
/// this test pins that: no annulus pixel between the halo edge and 1.5×
/// the halo radius may fall below the local background, in either
/// panel's single-owner zone. Teeth (measured red on the compact-only
/// base): Pyramid-mode B-owned annulus min 0.0361 vs background 0.05
/// (moat trough 0.0354 at r ≈ 125 px — a 30% deficit); after the fix
/// both sides sit at 0.0476+, within the ±0.002 noise floor. TwoBand's
/// per-pixel feather base is a convex combination and never dug a moat;
/// it is asserted too as a cheap guard.
#[test]
fn bright_star_halo_leaves_no_dark_moat() {
    use crate::seam::split_mask_components;

    let dir = tmpdir("halomoat");
    let (session, graph) = halo_moat_panels(&dir);
    let phot = identity_phot(2, 1);

    // Preconditions — the mechanism's trigger must be present: in both
    // panels the star's mask component exceeds every compactness bound
    // and classifies as extended structure.
    let summaries = load_summaries(&session).unwrap();
    let (w8, h8) = (summaries[0].w8, summaries[0].h8);
    let masks: Vec<Vec<bool>> = summaries.iter().map(star_mask).collect();
    let star_cell = (40 * w8 + 40) as usize;
    for (p, mask) in masks.iter().enumerate() {
        let (compact, structure) = split_mask_components(mask, w8, h8);
        assert!(mask[star_cell], "panel {p}: star cell must be masked");
        assert!(
            structure[star_cell] && !compact[star_cell],
            "panel {p}: the halo-flooded component must classify as \
             structure — the moat mechanism's trigger"
        );
    }

    // The owner map the blend will use (same inputs ⇒ identical),
    // to attribute annulus pixels to single-owner zones.
    let owner = crate::seam::compute_owner_map_masked(
        &summaries,
        &graph,
        &phot,
        None,
        session.canvas,
        256.0,
        &masks,
    );

    // Annulus between the halo edge (both halos below the noise floor,
    // r ≈ 110 px) and 1.5× that radius. Background is 0.05 ± 0.002
    // (deterministic noise); tolerance 2× the noise amplitude.
    let (r_lo, r_hi) = (140.0f64, 210.0f64);
    let (bg, tol) = (0.05f32, 0.004f32);
    for mode in [BlendMode::TwoBand, BlendMode::Pyramid] {
        let params = BlendParams {
            feather_px: 256.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        // Output coords == canvas coords (union bbox is the full canvas).
        assert_eq!((sink.w, sink.h), (640, 640));
        // Diagnostic radial profile: min per 10 px bin, r ∈ [80, 250).
        let mut bins = [f32::INFINITY; 17];
        for y in 0..640u64 {
            for x in 0..640u64 {
                let r = ((x as f64 - 320.0).powi(2) + (y as f64 - 320.0).powi(2)).sqrt();
                if (80.0..250.0).contains(&r) {
                    let b = ((r - 80.0) / 10.0) as usize;
                    bins[b] = bins[b].min(sink.at(0, x as usize, y as usize));
                }
            }
        }
        eprintln!("{mode:?} radial min profile (r=80..250, 10px bins): {bins:.4?}");
        let mut mins = [f32::INFINITY; 2];
        for y in 0..640u64 {
            for x in 0..640u64 {
                let r = ((x as f64 - 320.0).powi(2) + (y as f64 - 320.0).powi(2)).sqrt();
                if !(r_lo..=r_hi).contains(&r) {
                    continue;
                }
                let o = owner.at((x / 8) as u32, (y / 8) as u32);
                if o < 2 {
                    let v = sink.at(0, x as usize, y as usize);
                    mins[o as usize] = mins[o as usize].min(v);
                }
            }
        }
        eprintln!(
            "{mode:?}: annulus min A-owned {:.4}, B-owned {:.4} (background {bg}, tol {tol})",
            mins[0], mins[1]
        );
        for (side, min) in ["A", "B"].iter().zip(mins) {
            assert!(
                min.is_finite(),
                "{mode:?}: annulus must contain {side}-owned pixels"
            );
            assert!(
                min >= bg - tol,
                "{mode:?}: dark moat in the {side}-owned annulus: min {min} \
                 < background {bg} − tol {tol} — the halo's base content is \
                 bleeding Laplacian lobes into the blend"
            );
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejects_unsupported_downsample() {
    let dir = tmpdir("badds");
    let (session, graph) = make_panels(&dir);
    let phot = identity_phot(2, 1);
    let params = BlendParams {
        feather_px: 16.0,
        downsample: 4,
        band_rows: 16,
        mode: BlendMode::Feather,
        roi: None,
        defect_veto: true,
        flatten: None,
    };
    let mut sink = MemSink::new();
    assert!(blend(&session, &phot, None, &graph, &params, &mut sink).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
