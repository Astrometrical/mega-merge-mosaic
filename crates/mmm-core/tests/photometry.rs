//! Integration test for the photometric solve: synthetic ground-truth panels
//! with known per-panel gain/offset perturbations go through `analyze` (which
//! runs the solve and persists `analysis/photometry.json`); the recovered
//! corrections must invert the applied transforms.

use std::path::PathBuf;

use mmm_core::analyze::analyze;
use mmm_core::photometry::Photometry;
use mmm_core::synth::{SynthSpec, generate};

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-phot-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn analyze_recovers_applied_panel_transforms() {
    let dir = tempdir("recover");
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
        seed: 7,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();

    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();
    let phot_path = session.photometry_path();
    assert!(
        phot_path.is_file(),
        "analyze must persist analysis/photometry.json"
    );
    let phot = Photometry::load(&phot_path).unwrap();

    // 2×2 grid with 25% overlap: 4 side edges + 2 diagonal edges, 3 channels.
    assert!(
        phot.edge_fits.len() >= 4 * 3,
        "expected ≥ 12 edge fits, got {}",
        phot.edge_fits.len()
    );
    assert_eq!(phot.gains.len(), 3);
    assert_eq!(phot.offsets.len(), 3);
    for c in 0..3 {
        assert_eq!(phot.gains[c].len(), 4);
        assert_eq!(phot.offsets[c].len(), 4);
    }
    for f in &phot.edge_fits {
        assert!(f.gain.is_finite() && f.offset.is_finite() && f.rms.is_finite());
        assert!(f.n >= 16, "edge {}–{} kept only {} pairs", f.a, f.b, f.n);
    }

    // The reference panel carries the exact gauge (g=1, o=0).
    let r = (0..4)
        .find(|&p| phot.gains[0][p] == 1.0 && phot.offsets[0][p] == 0.0)
        .expect("one panel must be the gauge reference");
    let (gr, or) = res.applied[r];

    // Every panel's correction must invert its applied transform, mapping it
    // into the reference panel's frame: g_p = g_r/g_p^a, o_p = o_r − g_p·o_p^a.
    for c in 0..3usize {
        for p in 0..4usize {
            let (gp, op) = res.applied[p];
            let eg = gr as f64 / gp as f64;
            let eo = or as f64 - eg * op as f64;
            let (g, o) = (phot.gains[c][p], phot.offsets[c][p]);
            assert!(
                (g - eg).abs() <= 0.02 * eg,
                "ch {c} panel {p}: gain {g} vs expected {eg}"
            );
            assert!(
                (o - eo).abs() <= 2e-3,
                "ch {c} panel {p}: offset {o} vs expected {eo}"
            );
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
