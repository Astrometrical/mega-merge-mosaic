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
