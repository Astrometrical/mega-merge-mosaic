//! Integration tests for the shared reference frame: header-only derivation,
//! analyze adopting an imposed frame (and refusing groups that do not fit),
//! and the two-group end-to-end guarantee that outputs share one pixel grid.

use std::path::{Path, PathBuf};

use mmm_core::Result;
use mmm_core::analyze::{InputSelect, analyze_full};
use mmm_core::astrometry::LinearWcs;
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend, union_bbox};
use mmm_core::formats::xisf::XisfPanel;
use mmm_core::formats::{FitsKeyword, InputPanel};
use mmm_core::ipc::protocol::PanelDesc;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::{GainMode, Photometry};
use mmm_core::reference::{ReferenceFrame, derive, derive_from_descs};
use mmm_core::session::{InputKind, Session};
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{
    SynthWcs, write_fits, write_xisf, write_xisf_solved, write_xisf_with_header_xml,
};

/// Pixel scale of every synthetic solution, degrees per pixel (3.6″).
const S: f64 = 1.0e-3;
/// Sky position (RA, Dec) of the planted star — also the layout centre.
const STAR: [f64; 2] = [66.0, 18.0];

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-reference-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Noiseless analytic sky: flat background 0.05 plus one Gaussian star
/// (σ = 2 px, amplitude 0.8) at [`STAR`]. Distances are measured in pixels
/// of scale [`S`] on the tangent plane about the star.
fn sky(ra: f64, dec: f64) -> f32 {
    let dx = (ra - STAR[0]) * STAR[1].to_radians().cos() / S;
    let dy = (dec - STAR[1]) / S;
    (0.05 + 0.8 * (-(dx * dx + dy * dy) / 8.0).exp()) as f32
}

/// Sky position `(dx, dy)` pixels from [`STAR`] (RA offset corrected for cos δ).
fn offset_px(dx: f64, dy: f64) -> [f64; 2] {
    [
        STAR[0] + dx * S / STAR[1].to_radians().cos(),
        STAR[1] + dy * S,
    ]
}

/// A north-up linear solution centred on `crval` for a `w`×`h` panel, in both
/// the FITS form the test samples with and the form the XISF writer takes.
fn north_up(crval: [f64; 2], w: u64, h: u64) -> (LinearWcs, SynthWcs) {
    let cd = [[-S, 0.0], [0.0, S]];
    let refimg = [w as f64 / 2.0, h as f64 / 2.0];
    (
        LinearWcs {
            crval,
            crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
            cd,
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        },
        SynthWcs { crval, refimg, cd },
    )
}

/// Write one raw plate-solved XISF panel (`ch` channels, all identical) of
/// `w`×`h` px centred on `crval`; pixel (i, j) carries the sky at FITS
/// coordinate (i + 1, j + 1) of its own solution.
fn write_raw_panel(path: &Path, w: u64, h: u64, ch: u64, crval: [f64; 2]) {
    let (lin, synth) = north_up(crval, w, h);
    let mut plane = vec![0.0f32; (w * h) as usize];
    for j in 0..h {
        for i in 0..w {
            let (ra, dec) = lin.pixel_to_sky(i as f64 + 1.0, j as f64 + 1.0);
            plane[(j * w + i) as usize] = sky(ra, dec);
        }
    }
    let planes: Vec<f32> = (0..ch).flat_map(|_| plane.iter().copied()).collect();
    write_xisf_solved(path, w, h, ch, &planes, &synth).unwrap();
}

/// A group of two overlapping raw mono panels around [`STAR`]: 160×120 at
/// −60 px and `w2`×120 at +60 px (≈ 40 px overlap containing the star).
/// `shift` displaces the whole group on the sky, in pixels, so two groups can
/// have different footprints.
fn write_solved_group(dir: &Path, tag: &str, shift: (f64, f64), w2: u64) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join(format!("{tag}_00.xisf"));
    let b = dir.join(format!("{tag}_01.xisf"));
    write_raw_panel(&a, 160, 120, 1, offset_px(-60.0 + shift.0, shift.1));
    write_raw_panel(&b, w2, 120, 1, offset_px(60.0 + shift.0, shift.1));
    vec![a, b]
}

/// Canvas geometry of every synthetic aligned (registered) frame.
const CANVAS: (u64, u64) = (240, 200);

/// Write one full-canvas mono frame carrying the canvas WCS centred on
/// `canvas_crval`, with `value` inside `window` `[x0, y0, x1, y1]` and zeros
/// elsewhere (a MosaicByCoordinates-style registered panel).
fn write_canvas_panel(path: &Path, canvas_crval: [f64; 2], window: [u64; 4], value: f32) {
    let (w, h) = CANVAS;
    let (_, synth) = north_up(canvas_crval, w, h);
    let mut plane = vec![0.0f32; (w * h) as usize];
    for y in window[1]..window[3] {
        for x in window[0]..window[2] {
            plane[(y * w + x) as usize] = value;
        }
    }
    write_xisf_solved(path, w, h, 1, &plane, &synth).unwrap();
}

/// A group of two registered 100×120 panels overlapping by 30 px, placed at
/// `(dx, dy)` on the shared canvas (coverage 25% each, so auto-detect reads
/// the set as aligned).
fn write_aligned_group(
    dir: &Path,
    tag: &str,
    canvas_crval: [f64; 2],
    dx: u64,
    dy: u64,
) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join(format!("{tag}_00.xisf"));
    let b = dir.join(format!("{tag}_01.xisf"));
    write_canvas_panel(&a, canvas_crval, [dx, dy, dx + 100, dy + 120], 0.4);
    write_canvas_panel(&b, canvas_crval, [dx + 70, dy, dx + 170, dy + 120], 0.4);
    vec![a, b]
}

// ---------------------------------------------------------------------------
// derive

#[test]
fn derive_mixed_geometries_is_solved_over_all_panels() {
    let dir = tempdir("derive-solved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let r = derive(&all, InputSelect::Auto).unwrap();
    let ReferenceFrame::Solved { frame } = &r else {
        panic!("expected a solved reference, got {r:?}");
    };
    // Union of both groups: x spans [−140, 170] px (310) plus 16 px margins.
    assert!(
        frame.width >= 310 + 32 && frame.width <= 310 + 36,
        "width {}",
        frame.width
    );
    assert!((frame.scale_deg - S).abs() < 1e-12);
    // Only the first group fits inside a frame derived from it alone.
    let only_a = derive(&a, InputSelect::Solved).unwrap();
    assert!(only_a.canvas().0 < frame.width);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_equal_geometries_is_aligned_with_canvas_wcs() {
    let dir = tempdir("derive-aligned");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let b = write_aligned_group(&dir.join("B"), "b", STAR, 30, 40);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let r = derive(&all, InputSelect::Auto).unwrap();
    match r {
        ReferenceFrame::Aligned {
            width,
            height,
            wcs: Some(wcs),
        } => {
            assert_eq!((width, height), CANVAS);
            assert!((wcs.crval[0] - STAR[0]).abs() < 1e-9 && (wcs.crval[1] - STAR[1]).abs() < 1e-9);
        }
        other => panic!("expected an aligned reference with WCS, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_honours_input_override_both_ways() {
    let dir = tempdir("derive-override");
    // Same-geometry raw panels: their solutions differ, so auto reads them
    // as solved; the aligned override still forces the canvas kind.
    let same = write_solved_group(&dir.join("same"), "s", (0.0, 0.0), 160);
    assert!(matches!(
        derive(&same, InputSelect::Auto).unwrap(),
        ReferenceFrame::Solved { .. }
    ));
    assert!(matches!(
        derive(&same, InputSelect::Aligned).unwrap(),
        ReferenceFrame::Aligned {
            width: 160,
            height: 120,
            ..
        }
    ));
    assert!(matches!(
        derive(&same, InputSelect::Solved).unwrap(),
        ReferenceFrame::Solved { .. }
    ));
    // Registered canvases carry solutions too: solved forces a fresh frame around the canvas.
    let canvases = write_aligned_group(&dir.join("canv"), "c", STAR, 10, 20);
    let forced = derive(&canvases, InputSelect::Solved).unwrap();
    assert!(matches!(forced, ReferenceFrame::Solved { .. }));
    let w = forced.canvas().0;
    assert!(
        w >= CANVAS.0 + 32 && w <= CANVAS.0 + 33,
        "canvas plus margins, got {w}"
    );
    assert!(matches!(
        derive(&canvases, InputSelect::Aligned).unwrap(),
        ReferenceFrame::Aligned { .. }
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_aligned_override_needs_one_geometry() {
    let dir = tempdir("derive-aligned-mismatch");
    let mixed = write_solved_group(&dir.join("m"), "m", (0.0, 0.0), 150);
    let err = derive(&mixed, InputSelect::Aligned)
        .unwrap_err()
        .to_string();
    assert!(err.contains("160x120") && err.contains("150x120"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_without_solution_lists_every_unsolved_file() {
    let dir = tempdir("derive-unsolved");
    let solved = write_solved_group(&dir.join("s"), "s", (0.0, 0.0), 150);
    let plain = dir.join("plain.xisf");
    write_xisf(&plain, 100, 80, 1, &vec![0.3f32; 100 * 80]).unwrap();
    let mut all = solved;
    all.push(plain);
    let err = derive(&all, InputSelect::Auto).unwrap_err().to_string();
    assert!(err.contains("astrometric solution in every panel"), "{err}");
    assert!(err.contains("plain.xisf"), "{err}");
    assert!(
        !err.contains("s_00.xisf"),
        "solved files must not be listed: {err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_allows_mixed_channel_counts() {
    let dir = tempdir("derive-channels");
    let mono = write_solved_group(&dir.join("mono"), "m", (0.0, 0.0), 150);
    let rgb = dir.join("rgb.xisf");
    write_raw_panel(&rgb, 140, 110, 3, offset_px(30.0, 10.0));
    let mut all = mono;
    all.push(rgb);
    let r = derive(&all, InputSelect::Auto).unwrap();
    assert!(matches!(r, ReferenceFrame::Solved { .. }), "{r:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn derive_refuses_empty_input() {
    let err = derive(&[], InputSelect::Auto).unwrap_err().to_string();
    assert!(err.contains("no input panels"), "{err}");
}

// ---------------------------------------------------------------------------
// analyze with an imposed frame

fn analyze(
    paths: &[PathBuf],
    dir: &Path,
    input: InputSelect,
    reference: Option<&ReferenceFrame>,
) -> Result<Session> {
    analyze_full(paths, dir, Some(2), GainMode::Fit, input, None, reference)
}

#[test]
fn solved_group_adopts_the_imposed_frame() {
    let dir = tempdir("adopt-solved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let reference = derive(&all, InputSelect::Auto).unwrap();
    let ReferenceFrame::Solved { frame } = &reference else {
        unreachable!()
    };

    let sa = analyze(
        &a,
        &dir.join("a.mmm-session"),
        InputSelect::Solved,
        Some(&reference),
    )
    .unwrap();
    assert_eq!(sa.input, InputKind::Solved);
    assert!(sa.frame_imposed);
    assert_eq!(sa.frame.as_ref(), Some(frame));
    assert_eq!(sa.canvas, (frame.width, frame.height, 1));
    // Persisted and read back.
    let reopened = Session::open(&dir.join("a.mmm-session")).unwrap();
    assert!(reopened.frame_imposed);
    assert_eq!(reopened.frame.as_ref(), Some(frame));

    // Without a reference the group gets its own, smaller frame.
    let own = analyze(&a, &dir.join("own.mmm-session"), InputSelect::Solved, None).unwrap();
    assert!(!own.frame_imposed);
    assert!(own.canvas.0 < sa.canvas.0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn panel_outside_the_imposed_frame_is_refused_by_name() {
    let dir = tempdir("footprint");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let only_a = derive(&a, InputSelect::Solved).unwrap();
    let err = analyze(
        &b,
        &dir.join("b.mmm-session"),
        InputSelect::Solved,
        Some(&only_a),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("b_01.xisf"), "{err}");
    assert!(err.contains("beyond the"), "{err}");
    assert!(err.contains("mmm frame"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aligned_group_adopts_a_matching_canvas() {
    let dir = tempdir("adopt-aligned");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let reference = derive(&a, InputSelect::Auto).unwrap();
    let s = analyze(
        &a,
        &dir.join("a.mmm-session"),
        InputSelect::Auto,
        Some(&reference),
    )
    .unwrap();
    assert_eq!(s.input, InputKind::Aligned);
    assert!(s.frame_imposed);
    assert!(s.frame.is_none(), "aligned sessions keep passthrough WCS");
    assert_eq!(s.canvas, (CANVAS.0, CANVAS.1, 1));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aligned_group_on_another_canvas_is_refused_with_hint() {
    let dir = tempdir("aligned-size");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let smaller = ReferenceFrame::Aligned {
        width: CANVAS.0 - 10,
        height: CANVAS.1,
        wcs: None,
    };
    let err = analyze(
        &a,
        &dir.join("a.mmm-session"),
        InputSelect::Auto,
        Some(&smaller),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not match the reference frame"), "{err}");
    assert!(
        err.contains("align every group to one common reference"),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn aligned_group_with_shifted_wcs_is_refused_quoting_pixels() {
    let dir = tempdir("aligned-wcs");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    // Same canvas, but this group's registration put the sky 2 px further north.
    let b = write_aligned_group(&dir.join("B"), "b", offset_px(0.0, 2.0), 10, 20);
    let reference = derive(&a, InputSelect::Auto).unwrap();
    let err = analyze(
        &b,
        &dir.join("b.mmm-session"),
        InputSelect::Auto,
        Some(&reference),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("displaced up to 2.00 px"), "{err}");
    assert!(
        err.contains("align every group to one common reference"),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reference_kind_must_match_the_input_kind() {
    let dir = tempdir("kind");
    let raw = write_solved_group(&dir.join("raw"), "r", (0.0, 0.0), 150);
    let registered = write_aligned_group(&dir.join("reg"), "g", STAR, 10, 20);
    let solved_ref = derive(&raw, InputSelect::Solved).unwrap();
    let aligned_ref = derive(&registered, InputSelect::Auto).unwrap();

    let err = analyze(
        &registered,
        &dir.join("x.mmm-session"),
        InputSelect::Auto,
        Some(&solved_ref),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("solved mosaic frame") && err.contains("--input solved"),
        "{err}"
    );

    let err = analyze(
        &raw,
        &dir.join("y.mmm-session"),
        InputSelect::Solved,
        Some(&aligned_ref),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("aligned canvas") && err.contains("--input aligned"),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// two groups → one grid

/// In-memory sink collecting the whole (small) blended output, planar.
struct MemSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl MemSink {
    fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            ch: 0,
            data: Vec::new(),
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
        Ok(())
    }
}

/// Blend a session with the given mode and the session-default extent.
fn blend_session(session: &Session, mode: BlendMode) -> MemSink {
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();
    let params = BlendParams {
        feather_px: 24.0,
        downsample: 1,
        band_rows: 64,
        mode,
        roi: None,
        defect_veto: true,
        flatten: None,
        extent: None,
    };
    let mut sink = MemSink::new();
    blend(session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    sink
}

/// Background-subtracted intensity-weighted centroid of the brightest spot
/// (±4 px window), channel 0, in output pixel coordinates.
fn star_centroid(sink: &MemSink) -> (f64, f64) {
    let mut best = (0usize, 0usize, f32::MIN);
    for y in 0..sink.h {
        for x in 0..sink.w {
            let v = sink.at(0, x, y);
            if v > best.2 {
                best = (x, y, v);
            }
        }
    }
    let (bx, by) = (best.0 as i64, best.1 as i64);
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for dy in -4..=4 {
        for dx in -4..=4 {
            let (x, y) = (bx + dx, by + dy);
            if x < 0 || y < 0 || x >= sink.w as i64 || y >= sink.h as i64 {
                continue;
            }
            let w = (f64::from(sink.at(0, x as usize, y as usize)) - 0.05).max(0.0);
            sx += w * x as f64;
            sy += w * y as f64;
            sw += w;
        }
    }
    (sx / sw, sy / sw)
}

#[test]
fn two_solved_groups_share_one_grid() {
    let dir = tempdir("two-solved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);

    // Sanity: left alone the groups land on different frames (the bug).
    let own_a = analyze(
        &a,
        &dir.join("own_a.mmm-session"),
        InputSelect::Solved,
        None,
    )
    .unwrap();
    let own_b = analyze(
        &b,
        &dir.join("own_b.mmm-session"),
        InputSelect::Solved,
        None,
    )
    .unwrap();
    assert_ne!(
        own_a.frame, own_b.frame,
        "fixture must make independently chosen frames differ"
    );

    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let reference = derive(&all, InputSelect::Auto).unwrap();
    let sa = analyze(
        &a,
        &dir.join("a.mmm-session"),
        InputSelect::Solved,
        Some(&reference),
    )
    .unwrap();
    let sb = analyze(
        &b,
        &dir.join("b.mmm-session"),
        InputSelect::Solved,
        Some(&reference),
    )
    .unwrap();
    assert_eq!(sa.frame, sb.frame);
    assert_eq!(sa.canvas, sb.canvas);
    assert_ne!(
        union_bbox(&sa).unwrap(),
        union_bbox(&sb).unwrap(),
        "coverage differs, grid must not"
    );

    for mode in [BlendMode::Feather, BlendMode::Pyramid] {
        let out_a = blend_session(&sa, mode);
        let out_b = blend_session(&sb, mode);
        assert_eq!((out_a.w, out_a.h), (out_b.w, out_b.h), "{mode:?}");
        assert_eq!(
            (out_a.w as u64, out_a.h as u64),
            (sa.canvas.0, sa.canvas.1),
            "{mode:?}: canvas extent"
        );
        assert!(
            out_a.data.iter().chain(&out_b.data).all(|v| v.is_finite()),
            "{mode:?}"
        );
        assert_eq!(
            out_a.at(0, 0, 0),
            0.0,
            "{mode:?}: frame margin is zero-filled"
        );
        let ca = star_centroid(&out_a);
        let cb = star_centroid(&out_b);
        assert!(
            (ca.0 - cb.0).abs() < 0.1 && (ca.1 - cb.1).abs() < 0.1,
            "{mode:?}: star at {ca:?} in group A vs {cb:?} in group B"
        );
        // The same WCS cards would be emitted: identical frames.
        assert_eq!(
            sa.frame.as_ref().unwrap().linear_wcs(),
            sb.frame.as_ref().unwrap().linear_wcs()
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn two_aligned_groups_share_the_full_canvas() {
    let dir = tempdir("two-aligned");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let b = write_aligned_group(&dir.join("B"), "b", STAR, 30, 40);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let reference = derive(&all, InputSelect::Auto).unwrap();
    let sa = analyze(
        &a,
        &dir.join("a.mmm-session"),
        InputSelect::Auto,
        Some(&reference),
    )
    .unwrap();
    let sb = analyze(
        &b,
        &dir.join("b.mmm-session"),
        InputSelect::Auto,
        Some(&reference),
    )
    .unwrap();
    assert_eq!(sa.input, InputKind::Aligned);
    assert_ne!(union_bbox(&sa).unwrap(), union_bbox(&sb).unwrap());
    let out_a = blend_session(&sa, BlendMode::Feather);
    let out_b = blend_session(&sb, BlendMode::Feather);
    assert_eq!((out_a.w as u64, out_a.h as u64), CANVAS);
    assert_eq!((out_b.w as u64, out_b.h as u64), CANVAS);
    // Content sits where the windows were, zero elsewhere.
    assert!((out_a.at(0, 50, 80) - 0.4).abs() < 1e-3);
    assert_eq!(out_a.at(0, 5, 5), 0.0);
    assert!((out_b.at(0, 70, 100) - 0.4).abs() < 1e-3);
    assert_eq!(out_b.at(0, 5, 5), 0.0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn header_knowable_mismatches_fail_before_any_scan() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let dir = tempdir("fail-fast");
    let a = write_aligned_group(&dir.join("A"), "a", STAR, 10, 20);
    let scanned = AtomicU64::new(0);
    let progress = |stage: &str, done: u64, _total: u64| {
        if stage == "analyze" && done > 0 {
            scanned.fetch_add(1, Ordering::Relaxed);
        }
    };

    // Wrong canvas geometry against an aligned reference: knowable from headers.
    let smaller = ReferenceFrame::Aligned {
        width: CANVAS.0 - 10,
        height: CANVAS.1,
        wcs: None,
    };
    let err = analyze_full(
        &a,
        &dir.join("x.mmm-session"),
        Some(2),
        GainMode::Fit,
        InputSelect::Auto,
        Some(&progress),
        Some(&smaller),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not match the reference frame"), "{err}");
    assert_eq!(
        scanned.load(Ordering::Relaxed),
        0,
        "a header-knowable mismatch must fail before scanning pixels"
    );

    // A solved reference forced onto aligned input: knowable without a scan.
    let raw = write_solved_group(&dir.join("raw"), "r", (0.0, 0.0), 150);
    let solved_ref = derive(&raw, InputSelect::Solved).unwrap();
    let err = analyze_full(
        &a,
        &dir.join("y.mmm-session"),
        Some(2),
        GainMode::Fit,
        InputSelect::Aligned,
        Some(&progress),
        Some(&solved_ref),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("solved mosaic frame"), "{err}");
    assert_eq!(scanned.load(Ordering::Relaxed), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// stage 2: descriptors and FILTER names

/// Wire descriptors for `paths`, exactly as a Views-mode host builds them
/// (geometry from the header, the astrometric properties verbatim).
fn descs_for(paths: &[PathBuf]) -> Vec<PanelDesc> {
    paths
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
        .collect()
}

#[test]
fn descriptors_derive_the_same_frame_as_files() {
    let dir = tempdir("descs");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let from_files = derive(&all, InputSelect::Auto).unwrap();
    let from_descs = derive_from_descs(&descs_for(&all), InputSelect::Auto).unwrap();
    assert_eq!(from_files, from_descs);

    let reg = write_aligned_group(&dir.join("R"), "r", STAR, 10, 20);
    let from_files = derive(&reg, InputSelect::Auto).unwrap();
    let from_descs = derive_from_descs(&descs_for(&reg), InputSelect::Auto).unwrap();
    assert_eq!(from_files, from_descs);
    assert!(matches!(
        from_descs,
        ReferenceFrame::Aligned { wcs: Some(_), .. }
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn descriptors_without_solution_are_named_by_panel_id() {
    let dir = tempdir("descs-unsolved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let mut descs = descs_for(&a);
    descs[1].properties.clear();
    let err = derive_from_descs(&descs, InputSelect::Solved)
        .unwrap_err()
        .to_string();
    assert!(err.contains("panel 1"), "{err}");
    assert!(!err.contains("panel 0"), "{err}");
    let err = derive_from_descs(&[], InputSelect::Auto)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no input panels"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn filter_name_strips_quotes_and_padding() {
    let dir = tempdir("filter-fits");
    let path = dir.join("ha.fits");
    let card = FitsKeyword {
        name: "FILTER".into(),
        value: "'Ha      '".into(),
        comment: String::new(),
    };
    write_fits(&path, 8, 6, 1, &[0.2f32; 48], -32, &[card]).unwrap();
    assert_eq!(
        InputPanel::open(&path).unwrap().filter_name().as_deref(),
        Some("Ha")
    );

    let plain = dir.join("plain.fits");
    write_fits(&plain, 8, 6, 1, &[0.2f32; 48], -32, &[]).unwrap();
    assert_eq!(InputPanel::open(&plain).unwrap().filter_name(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn filter_name_falls_back_to_xisf_property() {
    let dir = tempdir("filter-xisf");
    let by_card = dir.join("card.xisf");
    write_xisf_with_header_xml(
        &by_card,
        8,
        6,
        1,
        &[0.2f32; 48],
        r#"<FITSKeyword name="FILTER" value="'R'" comment=""/>"#,
    )
    .unwrap();
    assert_eq!(
        InputPanel::open(&by_card).unwrap().filter_name().as_deref(),
        Some("R")
    );

    let by_prop = dir.join("prop.xisf");
    write_xisf_with_header_xml(
        &by_prop,
        8,
        6,
        1,
        &[0.2f32; 48],
        r#"<Property id="Instrument:Filter:Name" type="String">OIII</Property>"#,
    )
    .unwrap();
    assert_eq!(
        InputPanel::open(&by_prop).unwrap().filter_name().as_deref(),
        Some("OIII")
    );

    let none = dir.join("none.xisf");
    write_xisf(&none, 8, 6, 1, &[0.2f32; 48]).unwrap();
    assert_eq!(InputPanel::open(&none).unwrap().filter_name(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Review fix: a one-panel group (e.g. a single Ha frame) in Files mode under
/// Auto must adopt an aligned reference whose canvas it matches, instead of
/// being read as a solved raw panel because Auto needs two panels to call a
/// set aligned.
#[test]
fn single_registered_panel_group_adopts_aligned_reference() {
    let dir = tempdir("single-aligned");
    let lrgb = write_aligned_group(&dir.join("L"), "l", STAR, 10, 20);
    let reference = derive(&lrgb, InputSelect::Auto).unwrap();
    assert!(matches!(reference, ReferenceFrame::Aligned { .. }));
    let ha = dir.join("ha.xisf");
    write_canvas_panel(&ha, STAR, [30, 40, 130, 160], 0.4);
    let s = analyze(
        &[ha],
        &dir.join("ha.mmm-session"),
        InputSelect::Auto,
        Some(&reference),
    )
    .unwrap();
    assert_eq!(s.input, InputKind::Aligned);
    assert!(s.frame_imposed);
    assert_eq!(s.canvas, (CANVAS.0, CANVAS.1, 1));
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// Auto rule: same-geometry panels are told apart by their canvas WCS

#[test]
fn auto_reads_same_size_raw_panels_as_solved() {
    let dir = tempdir("auto-raw");
    // One camera: both raw panels 160×120, each with its own solution.
    let raw = write_solved_group(&dir.join("raw"), "r", (0.0, 0.0), 160);
    let from_files = derive(&raw, InputSelect::Auto).unwrap();
    assert!(
        matches!(from_files, ReferenceFrame::Solved { .. }),
        "{from_files:?}"
    );
    let from_descs = derive_from_descs(&descs_for(&raw), InputSelect::Auto).unwrap();
    assert_eq!(from_files, from_descs);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn auto_keeps_registered_canvases_aligned() {
    let dir = tempdir("auto-registered");
    // Registered canvases share one identical canvas WCS.
    let reg = write_aligned_group(&dir.join("reg"), "g", STAR, 10, 20);
    assert!(matches!(
        derive(&reg, InputSelect::Auto).unwrap(),
        ReferenceFrame::Aligned { wcs: Some(_), .. }
    ));
    // Registered canvases without any WCS (PixInsight-exported FITS style).
    let a = dir.join("plain_a.xisf");
    let b = dir.join("plain_b.xisf");
    write_xisf(&a, 40, 30, 1, &[0.3f32; 1200]).unwrap();
    write_xisf(&b, 40, 30, 1, &[0.2f32; 1200]).unwrap();
    assert!(matches!(
        derive(&[a, b], InputSelect::Auto).unwrap(),
        ReferenceFrame::Aligned {
            width: 40,
            height: 30,
            wcs: None
        }
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn analyze_auto_dispatches_same_size_raw_panels_straight_to_solved() {
    use std::sync::Mutex;
    let dir = tempdir("auto-dispatch");
    let raw = write_solved_group(&dir.join("raw"), "r", (0.0, 0.0), 160);
    let stages = Mutex::new(Vec::<String>::new());
    let progress = |stage: &str, done: u64, _total: u64| {
        if done > 0 {
            stages.lock().unwrap().push(stage.to_string());
        }
    };
    let s = analyze_full(
        &raw,
        &dir.join("raw.mmm-session"),
        Some(2),
        GainMode::Fit,
        InputSelect::Auto,
        Some(&progress),
        None,
    )
    .unwrap();
    assert_eq!(s.input, InputKind::Solved);
    let stages = stages.into_inner().unwrap();
    assert_eq!(
        stages.first().map(String::as_str),
        Some("reproject"),
        "no aligned scan may precede the solved path: {stages:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
