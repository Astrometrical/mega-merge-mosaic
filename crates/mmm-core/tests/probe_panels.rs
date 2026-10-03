//! Tests for [`mmm_core::analyze::probe_panels`] — the Files-mode metadata
//! probe the IPC worker exposes as `--probe-panels` (PROTOCOL.md §11).

use std::path::{Path, PathBuf};

use mmm_core::analyze::{InputSelect, probe_panels, solved_frame};
use mmm_core::formats::xisf::XisfPanel;
use mmm_core::ipc::protocol::PanelDesc;
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-probe-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two small plate-solved raw panels (mirrors the ipc-worker end-to-end
/// fixture shape: overlapping footprints, differing geometries).
fn write_solved(dir: &Path) -> Vec<PathBuf> {
    let scale_deg = 1.0e-3_f64;
    let mut paths = Vec::new();
    for (k, (w, h, crval)) in [
        (64u64, 48u64, [10.0, 0.0]),
        (60, 52, [10.0 + 64.0 * scale_deg * 0.55, 8.0 * scale_deg]),
    ]
    .into_iter()
    .enumerate()
    {
        let planes = vec![0.5f32; (w * h) as usize];
        let wcs = SynthWcs {
            crval,
            refimg: [w as f64 / 2.0, h as f64 / 2.0],
            cd: [[-scale_deg, 0.0], [0.0, scale_deg]],
        };
        let path = dir.join(format!("solved_{k}.xisf"));
        write_xisf_solved(&path, w, h, 1, &planes, &wcs).unwrap();
        paths.push(path);
    }
    paths
}

#[test]
fn solved_panels_report_geometry_and_frame() {
    let dir = tmpdir("solved");
    let paths = write_solved(&dir);

    // Expected frame straight from solved_frame over the file headers.
    let descs: Vec<PanelDesc> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let x = XisfPanel::open(p).unwrap();
            PanelDesc {
                panel_id: i as u32,
                width: x.width(),
                height: x.height(),
                channels: x.channels(),
                properties: x.header().properties.clone(),
            }
        })
        .collect();
    let (_, frame, ch) = solved_frame(&descs).unwrap();

    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert_eq!(reply.panels.len(), 2);
    assert_eq!(
        (
            reply.panels[0].width,
            reply.panels[0].height,
            reply.panels[0].channels
        ),
        (64, 48, 1)
    );
    assert_eq!((reply.panels[1].width, reply.panels[1].height), (60, 52));
    assert_eq!(reply.frame, Some([frame.width, frame.height, ch]));

    // Explicit Solved gives the same frame; explicit Aligned suppresses it.
    assert_eq!(
        probe_panels(&paths, InputSelect::Solved).unwrap().frame,
        reply.frame
    );
    assert_eq!(
        probe_panels(&paths, InputSelect::Aligned).unwrap().frame,
        None
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unsolved_panels_have_no_frame_in_auto_and_fail_in_solved() {
    let dir = tmpdir("unsolved");
    let mut paths = Vec::new();
    for k in 0..2u64 {
        let path = dir.join(format!("plain_{k}.xisf"));
        write_xisf(&path, 8, 6, 1, &[0.25f32; 48]).unwrap();
        paths.push(path);
    }

    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert_eq!(reply.frame, None);
    assert_eq!(reply.panels.len(), 2);
    assert_eq!(
        (
            reply.panels[0].width,
            reply.panels[0].height,
            reply.panels[0].channels
        ),
        (8, 6, 1)
    );

    let err = probe_panels(&paths, InputSelect::Solved).unwrap_err();
    assert!(err.to_string().contains("astrometric"), "got: {err}");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_file_errors_with_path() {
    let dir = tmpdir("missing");
    let good = dir.join("good.xisf");
    write_xisf(&good, 4, 4, 1, &[0.1f32; 16]).unwrap();
    let bad = dir.join("nope.xisf");
    let err = probe_panels(&[good, bad.clone()], InputSelect::Auto).unwrap_err();
    assert!(err.to_string().contains("nope.xisf"), "got: {err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn empty_paths_error() {
    assert!(probe_panels(&[], InputSelect::Auto).is_err());
}

/// The Files-mode probe refuses a mono/colour mix before a run starts: the
/// PixInsight host sizes its shm slots from `panels[0].channels`, so a mixed
/// set must never reach the run stage.
#[test]
fn mixed_channel_panels_are_refused() {
    let dir = tmpdir("mixed-types");
    let mono = dir.join("mono.xisf");
    let rgb = dir.join("rgb.xisf");
    write_xisf(&mono, 8, 6, 1, &[0.1f32; 48]).unwrap();
    write_xisf(&rgb, 8, 6, 3, &[0.1f32; 144]).unwrap();
    let paths = vec![mono, rgb];

    for input in [InputSelect::Auto, InputSelect::Aligned, InputSelect::Solved] {
        let err = probe_panels(&paths, input).unwrap_err().to_string();
        assert!(err.contains("same number of channels"), "{input:?}: {err}");
        assert!(err.contains("mono.xisf"), "{input:?}: {err}");
        assert!(err.contains("rgb.xisf"), "{input:?}: {err}");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn standard_rev1_solved_panels_probe_like_legacy_ones() {
    use mmm_core::synth::write_xisf_solved_standard;
    let dir = tmpdir("solved-standard");
    let scale_deg = 1.0e-3_f64;
    let (w, h) = (64u64, 48u64);
    let planes = vec![0.5f32; (w * h) as usize];
    let wcs = SynthWcs {
        crval: [10.0, 0.0],
        refimg: [32.0, 24.0],
        cd: [[-scale_deg, 0.0], [0.0, scale_deg]],
    };
    let legacy = dir.join("legacy.xisf");
    let standard = dir.join("standard.xisf");
    write_xisf_solved(&legacy, w, h, 1, &planes, &wcs).unwrap();
    write_xisf_solved_standard(&standard, w, h, 1, &planes, &wcs).unwrap();
    let ids: Vec<String> = XisfPanel::open(&standard)
        .unwrap()
        .header()
        .properties
        .iter()
        .map(|p| p.id.clone())
        .collect();
    assert!(ids.iter().any(|i| i == "AstrometricSolution:Version"));
    assert!(!ids.iter().any(|i| i.starts_with("PCL:")));
    let a = probe_panels(&[legacy], InputSelect::Auto).unwrap();
    let b = probe_panels(&[standard], InputSelect::Auto).unwrap();
    assert!(a.frame.is_some());
    assert_eq!(a.frame, b.frame, "same solution, same frame");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// FITS panels probe exactly like XISF ones: geometry from the primary
/// header, a solved frame from the standard WCS cards, and the same
/// mono/colour refusal.
#[test]
fn fits_panels_probe_like_xisf() {
    use mmm_core::formats::FitsKeyword;
    use mmm_core::synth::write_fits;
    let dir = tmpdir("fits");
    let kw = |n: &str, v: &str| FitsKeyword {
        name: n.into(),
        value: v.into(),
        comment: String::new(),
    };
    let (w, h) = (120u64, 90u64);
    let mut paths = Vec::new();
    for k in 0..2u64 {
        let planes = vec![0.4f32; (w * h) as usize];
        let cards = vec![
            kw("CTYPE1", "'RA---TAN'"),
            kw("CTYPE2", "'DEC--TAN'"),
            kw("CRVAL1", &format!("{}", 80.0 + 0.05 * k as f64)),
            kw("CRVAL2", "-5.0"),
            kw("CRPIX1", "60.5"),
            kw("CRPIX2", "45.5"),
            kw("CD1_1", "-1.0E-3"),
            kw("CD1_2", "0"),
            kw("CD2_1", "0"),
            kw("CD2_2", "1.0E-3"),
        ];
        let p = dir.join(format!("p{k}.fits"));
        write_fits(&p, w, h, 1, &planes, 16, &cards).unwrap();
        paths.push(p);
    }
    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert_eq!(reply.panels.len(), 2);
    assert_eq!(
        (
            reply.panels[0].width,
            reply.panels[0].height,
            reply.panels[0].channels
        ),
        (w, h, 1)
    );
    let frame = reply.frame.expect("solved FITS panels yield a frame");
    assert!(frame[0] >= w && frame[1] >= h && frame[2] == 1, "{frame:?}");
    // Aligned select never reports a frame; a mono/colour mix is refused.
    assert!(
        probe_panels(&paths, InputSelect::Aligned)
            .unwrap()
            .frame
            .is_none()
    );
    let rgb = dir.join("rgb.fits");
    write_fits(&rgb, w, h, 3, &vec![0.4f32; (w * h * 3) as usize], -32, &[]).unwrap();
    let e = probe_panels(&[paths[0].clone(), rgb], InputSelect::Auto)
        .unwrap_err()
        .to_string();
    assert!(e.contains("channel"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn probe_reports_filter_names_and_the_reference() {
    use mmm_core::formats::FitsKeyword;
    use mmm_core::reference::{ReferenceFrame, derive};
    use mmm_core::synth::write_fits;
    let dir = tmpdir("filter-ref");
    let paths = write_solved(&dir);
    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert!(reply.panels.iter().all(|p| p.filter.is_none()));
    assert_eq!(
        reply.reference,
        Some(derive(&paths, InputSelect::Auto).unwrap())
    );
    assert!(matches!(
        reply.reference,
        Some(ReferenceFrame::Solved { .. })
    ));

    let ha = dir.join("ha.fits");
    write_fits(
        &ha,
        8,
        6,
        1,
        &[0.2f32; 48],
        -32,
        &[FitsKeyword {
            name: "FILTER".into(),
            value: "'Ha'".into(),
            comment: String::new(),
        }],
    )
    .unwrap();
    let plain = dir.join("plain.fits");
    write_fits(&plain, 8, 6, 1, &[0.2f32; 48], -32, &[]).unwrap();
    let reply = probe_panels(&[ha, plain], InputSelect::Aligned).unwrap();
    assert_eq!(reply.panels[0].filter.as_deref(), Some("Ha"));
    assert_eq!(reply.panels[1].filter, None);
    assert!(matches!(
        reply.reference,
        Some(ReferenceFrame::Aligned {
            width: 8,
            height: 6,
            ..
        })
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn probe_reference_is_none_when_underivable() {
    let dir = tmpdir("no-ref");
    let a = dir.join("a.xisf");
    let b = dir.join("b.xisf");
    write_xisf(&a, 10, 8, 1, &[0.1f32; 80]).unwrap();
    write_xisf(&b, 12, 8, 1, &[0.1f32; 96]).unwrap();
    // Mixed geometry, no solutions, Auto: neither aligned nor solvable.
    let reply = probe_panels(&[a, b], InputSelect::Auto).unwrap();
    assert_eq!(reply.frame, None);
    assert_eq!(reply.reference, None);
    std::fs::remove_dir_all(&dir).unwrap();
}
