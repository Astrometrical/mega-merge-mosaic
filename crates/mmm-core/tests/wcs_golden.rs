//! Golden pixel→sky evaluation for every solution kind mmm reads: legacy
//! (`PCL:AstrometricSolution:*`) and standard (`AstrometricSolution:*`) XISF
//! properties, FITS TAN and TAN-SIP cards.
//!
//! The formats consolidation (Phase 4) replaces mmm's solution decoders
//! with `mpp-core`'s; this pins today's results. Regenerate deliberately
//! with `MMM_BLESS=1 cargo test --test wcs_golden`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mmm_core::formats::InputPanel;
use mmm_core::synth::{
    SynthSip, SynthWcs, write_fits_solved, write_xisf_solved, write_xisf_solved_standard,
};

const W: u64 = 64;
const H: u64 = 48;
const TOL_DEG: f64 = 1e-9;

fn wcs() -> SynthWcs {
    SynthWcs {
        crval: [84.199, -3.240],
        refimg: [32.0, 24.0],
        cd: [[-4.4368e-4, 1e-5], [1e-5, 4.4368e-4]],
    }
}

fn sip() -> SynthSip {
    SynthSip {
        order: 2,
        a: vec![(2, 0, 1.5e-4), (1, 1, -2.0e-4), (0, 2, 1.0e-4)],
        b: vec![(2, 0, -1.0e-4), (1, 1, 1.2e-4), (0, 2, 2.0e-4)],
        ap: vec![(2, 0, -1.5e-4), (1, 1, 2.0e-4), (0, 2, -1.0e-4)],
        bp: vec![(2, 0, 1.0e-4), (1, 1, -1.2e-4), (0, 2, -2.0e-4)],
    }
}

/// `[x, y, ra, dec]` on a 5×5 grid of pixel corners/centres spanning the image.
fn evaluate(path: &Path) -> Vec<[f64; 4]> {
    let model = InputPanel::open(path).unwrap().wcs_model().unwrap();
    let mut out = Vec::new();
    for j in 0..5 {
        for i in 0..5 {
            let (x, y) = (W as f64 * i as f64 / 4.0, H as f64 * j as f64 / 4.0);
            let (ra, dec) = model.pixel_to_sky(x, y);
            out.push([x, y, ra, dec]);
        }
    }
    out
}

#[test]
fn wcs_golden_matches_fixture() {
    let dir = std::env::temp_dir().join(format!("mmm-wcs-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let planes = vec![0.5f32; (W * H) as usize];
    let mut cases: BTreeMap<String, Vec<[f64; 4]>> = BTreeMap::new();

    let p = dir.join("legacy.xisf");
    write_xisf_solved(&p, W, H, 1, &planes, &wcs()).unwrap();
    cases.insert("xisf_legacy".into(), evaluate(&p));

    let p = dir.join("standard.xisf");
    write_xisf_solved_standard(&p, W, H, 1, &planes, &wcs()).unwrap();
    cases.insert("xisf_standard".into(), evaluate(&p));

    let p = dir.join("tan.fits");
    write_fits_solved(&p, W, H, 1, &planes, &wcs(), None, -32).unwrap();
    cases.insert("fits_tan".into(), evaluate(&p));

    let p = dir.join("tan_sip.fits");
    write_fits_solved(&p, W, H, 1, &planes, &wcs(), Some(&sip()), -32).unwrap();
    cases.insert("fits_tan_sip".into(), evaluate(&p));

    let _ = std::fs::remove_dir_all(&dir);

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wcs_golden.json");
    if std::env::var("MMM_BLESS").as_deref() == Ok("1") {
        let json = serde_json::to_string_pretty(&cases).unwrap();
        std::fs::write(&path, format!("{json}\n")).unwrap();
    }
    let golden: BTreeMap<String, Vec<[f64; 4]>> = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("fixture missing: run with MMM_BLESS=1"),
    )
    .unwrap();
    assert_eq!(
        cases.keys().collect::<Vec<_>>(),
        golden.keys().collect::<Vec<_>>()
    );
    for (case, got) in &cases {
        let want = &golden[case];
        assert_eq!(got.len(), want.len(), "{case}: grid size");
        for (g, w) in got.iter().zip(want) {
            for k in 0..4 {
                assert!(
                    (g[k] - w[k]).abs() <= TOL_DEG,
                    "{case}: at ({}, {}) component {k}: got {} want {}",
                    g[0],
                    g[1],
                    g[k],
                    w[k]
                );
            }
        }
    }
    // The SIP case must actually differ from plain TAN, or it pins nothing.
    let tan = &cases["fits_tan"];
    let sip = &cases["fits_tan_sip"];
    assert!(
        tan.iter().zip(sip).any(|(a, b)| (a[2] - b[2]).abs() > 1e-7),
        "SIP terms had no effect"
    );
}
