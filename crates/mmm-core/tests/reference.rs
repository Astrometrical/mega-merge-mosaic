//! Integration tests for the shared reference frame: header-only derivation,
//! analyze adopting an imposed frame (and refusing groups that do not fit),
//! and the two-group end-to-end guarantee that outputs share one pixel grid.

use std::path::{Path, PathBuf};

use mmm_core::analyze::InputSelect;
use mmm_core::astrometry::LinearWcs;
use mmm_core::reference::{ReferenceFrame, derive};
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

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
    // Same-geometry raw panels: auto reads them as aligned (documented), solved forces a frame.
    let same = write_solved_group(&dir.join("same"), "s", (0.0, 0.0), 160);
    assert!(matches!(
        derive(&same, InputSelect::Auto).unwrap(),
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
