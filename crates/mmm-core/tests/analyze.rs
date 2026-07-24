//! Integration tests for the analyze stage: synthetic XISF panels in, session
//! directory with `session.json` + per-panel L8 summaries out.

use std::path::{Path, PathBuf};

use mmm_core::analyze::analyze;
use mmm_core::overlap::OverlapGraph;
use mmm_core::session::Session;
use mmm_core::summary::L8Summary;

/// Write a minimal monolithic XISF file (Float32, planar, little-endian,
/// uncompressed attachment). Mirrors the generator in `formats::xisf::tests`.
fn write_xisf(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32]) {
    assert_eq!(planes.len() as u64, w * h * ch);
    let data_offset = 4096u64;
    let data_size = w * h * ch * 4;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><xisf version="1.0" xmlns="http://www.pixinsight.com/xisf"><Image geometry="{w}:{h}:{ch}" sampleFormat="Float32" colorSpace="RGB" location="attachment:{data_offset}:{data_size}"/></xisf>"#
    );
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"XISF0100");
    bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(xml.as_bytes());
    bytes.resize(data_offset as usize, 0);
    for v in planes {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Build a full-canvas panel: `values[c]` in x ∈ [x_lo, x_hi) for all rows,
/// zeros elsewhere.
fn make_panel(path: &Path, w: u64, h: u64, values: &[f32], x_lo: u64, x_hi: u64) {
    let ch = values.len() as u64;
    let mut planes = vec![0.0f32; (w * h * ch) as usize];
    for (c, &v) in values.iter().enumerate() {
        for y in 0..h {
            for x in x_lo..x_hi {
                planes[(c as u64 * w * h + y * w + x) as usize] = v;
            }
        }
    }
    write_xisf(path, w, h, ch, &planes);
}

fn write_xisf_owned(path: &Path, w: u64, h: u64, ch: u64, planes: Vec<f32>) {
    write_xisf(path, w, h, ch, &planes);
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-analyze-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn analyze_two_overlapping_panels() {
    let dir = tempdir("two");
    let (w, h) = (64u64, 48u64);
    let a = dir.join("a.xisf");
    let b = dir.join("b.xisf");
    // Panel A covers x in [0,40), B covers x in [24,64) -> overlap [24,40).
    make_panel(&a, w, h, &[0.5, 0.4, 0.3], 0, 40);
    make_panel(&b, w, h, &[0.25, 0.2, 0.15], 24, 64);

    let session_dir = dir.join("s.mmm-session");
    let session = analyze(&[a.clone(), b.clone()], &session_dir).unwrap();

    // session.json persisted; canvas geometry recorded.
    assert!(session_dir.join("session.json").is_file());
    assert_eq!(session.canvas, (64, 48, 3));
    assert_eq!(session.panels.len(), 2);

    // Panel metadata: exact bboxes (x0,y0,x1,y1 exclusive), coverage fractions,
    // per-channel stats.
    let pa = &session.panels[0];
    assert_eq!(pa.id, 0);
    assert_eq!(pa.path, a);
    assert_eq!(pa.bbox, [0, 0, 40, 48]);
    assert!((pa.nonzero_frac - 40.0 / 64.0).abs() < 1e-12);
    assert_eq!(pa.ch_min, vec![0.5, 0.4, 0.3]);
    assert_eq!(pa.ch_max, vec![0.5, 0.4, 0.3]);
    assert!((pa.ch_mean[0] - 0.5).abs() < 1e-7);
    assert!((pa.ch_mean[2] - 0.3).abs() < 1e-7);

    let pb = &session.panels[1];
    assert_eq!(pb.id, 1);
    assert_eq!(pb.bbox, [24, 0, 64, 48]);
    assert!((pb.nonzero_frac - 40.0 / 64.0).abs() < 1e-12);
    assert!((pb.ch_mean[1] - 0.2).abs() < 1e-7);

    // L8 summaries on disk, readable, with exact coverage/means.
    let sa = L8Summary::read(&session.summary_path(0)).unwrap();
    assert_eq!((sa.w8, sa.h8, sa.channels), (8, 6, 3));
    for y8 in 0..6 {
        for x8 in 0..8u32 {
            let expect_cov = if x8 < 5 { 1.0 } else { 0.0 };
            assert_eq!(sa.cov(x8, y8), expect_cov, "panel A cov at ({x8},{y8})");
            let expect_mean = if x8 < 5 { 0.4 } else { 0.0 };
            assert_eq!(sa.cell(1, x8, y8), expect_mean, "panel A ch1 mean at ({x8},{y8})");
        }
    }
    let sb = L8Summary::read(&session.summary_path(1)).unwrap();
    for y8 in 0..6 {
        for x8 in 0..8u32 {
            let expect_cov = if x8 >= 3 { 1.0 } else { 0.0 };
            assert_eq!(sb.cov(x8, y8), expect_cov, "panel B cov at ({x8},{y8})");
            let expect_mean = if x8 >= 3 { 0.25 } else { 0.0 };
            assert_eq!(sb.cell(0, x8, y8), expect_mean, "panel B ch0 mean at ({x8},{y8})");
        }
    }

    // Session round-trips from disk with summary paths reconstructed.
    let reopened = Session::open(&session_dir).unwrap();
    assert_eq!(reopened.canvas, (64, 48, 3));
    assert_eq!(reopened.panels.len(), 2);
    assert_eq!(reopened.panels[1].bbox, [24, 0, 64, 48]);
    assert!(reopened.summary_path(0).is_file());
    assert!(reopened.summary_path(1).is_file());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn analyze_builds_and_saves_overlap_graph() {
    let dir = tempdir("graph");
    let (w, h) = (128u64, 128u64);
    let a = dir.join("a.xisf");
    let b = dir.join("b.xisf");
    // Overlap x ∈ [48,80) → L8 cells x8 ∈ [6,10), all 16 cell rows = 64 cells.
    make_panel(&a, w, h, &[0.5], 0, 80);
    make_panel(&b, w, h, &[0.25], 48, 128);

    let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
    let graph_path = session.overlap_graph_path();
    assert_eq!(graph_path, session.dir.join("analysis").join("overlap_graph.json"));
    assert!(graph_path.is_file(), "analyze must persist analysis/overlap_graph.json");

    let graph = OverlapGraph::load(&graph_path).unwrap();
    assert_eq!(graph.edges.len(), 1);
    let e = &graph.edges[0];
    assert_eq!((e.a, e.b), (0, 1));
    assert_eq!(e.n_cells, 64);
    assert_eq!(e.bbox8, [6, 0, 10, 16]);
    assert_eq!(graph.components(2), vec![vec![0, 1]]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pixel_with_one_zero_channel_is_uncovered() {
    let dir = tempdir("zero-ch");
    let (w, h, ch) = (64u64, 48u64, 3u64);
    let mut planes = vec![0.0f32; (w * h * ch) as usize];
    for c in 0..ch {
        for y in 0..h {
            for x in 0..40 {
                planes[(c * w * h + y * w + x) as usize] = 0.5;
            }
        }
    }
    // Kill channel 1 at pixel (0,0): the pixel is no longer covered.
    planes[(w * h) as usize] = 0.0;
    let p = dir.join("p.xisf");
    write_xisf_owned(&p, w, h, ch, planes);

    let session = analyze(&[p], &dir.join("s.mmm-session")).unwrap();
    let meta = &session.panels[0];
    // bbox unchanged (other covered pixels still touch x=0 and y=0)...
    assert_eq!(meta.bbox, [0, 0, 40, 48]);
    // ...but the covered fraction misses exactly one pixel.
    let expect = (40 * 48 - 1) as f64 / (64 * 48) as f64;
    assert!((meta.nonzero_frac - expect).abs() < 1e-12);

    // Coverage of cell (0,0) is 63/64; means still 0.5 over covered pixels.
    let s = L8Summary::read(&session.summary_path(0)).unwrap();
    assert!((s.cov(0, 0) - 63.0 / 64.0).abs() < 1e-6);
    assert!((s.cell(0, 0, 0) - 0.5).abs() < 1e-6);
    assert_eq!(s.cov(1, 0), 1.0);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn edge_cells_average_over_actual_pixel_counts() {
    // 10x6 canvas: w8 = 2 (cells of 8 and 2 columns), h8 = 1 (6 rows < 8).
    let dir = tempdir("edge");
    let p = dir.join("p.xisf");
    make_panel(&p, 10, 6, &[1.0], 0, 10);

    let session = analyze(&[p], &dir.join("s.mmm-session")).unwrap();
    let s = L8Summary::read(&session.summary_path(0)).unwrap();
    assert_eq!((s.w8, s.h8, s.channels), (2, 1, 1));
    // Fully covered panel: even edge cells (fewer pixels) must read 1.0.
    assert_eq!(s.cov(0, 0), 1.0);
    assert_eq!(s.cov(1, 0), 1.0);
    assert_eq!(s.cell(0, 1, 0), 1.0);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn partial_cell_coverage_fraction() {
    // Cover x in [0,4) of a 16x8 canvas: cell (0,0) is half covered.
    let dir = tempdir("partial");
    let p = dir.join("p.xisf");
    make_panel(&p, 16, 8, &[2.0], 0, 4);

    let session = analyze(&[p], &dir.join("s.mmm-session")).unwrap();
    let s = L8Summary::read(&session.summary_path(0)).unwrap();
    assert_eq!(s.cov(0, 0), 0.5);
    assert_eq!(s.cov(1, 0), 0.0);
    assert_eq!(s.cell(0, 0, 0), 2.0);
    assert_eq!(s.cell(0, 1, 0), 0.0);
    assert_eq!(session.panels[0].bbox, [0, 0, 4, 8]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mismatched_canvas_geometry_errors() {
    let dir = tempdir("mismatch");
    let a = dir.join("a.xisf");
    let b = dir.join("b.xisf");
    make_panel(&a, 64, 48, &[0.5, 0.5, 0.5], 0, 40);
    make_panel(&b, 32, 48, &[0.5, 0.5, 0.5], 0, 20);

    assert!(analyze(&[a, b], &dir.join("s.mmm-session")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn analyze_no_inputs_errors() {
    let dir = tempdir("empty");
    assert!(analyze(&[], &dir.join("s.mmm-session")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
