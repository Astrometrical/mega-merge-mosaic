//! Binary-level tests of the `mmm` CLI around the shared reference frame:
//! `frame` → `analyze --frame` → `blend` yields co-registered FITS outputs,
//! and `batch` does the same in one command.

use std::path::{Path, PathBuf};
use std::process::Command;

use mmm_core::astrometry::LinearWcs;
use mmm_core::formats::InputPanel;
use mmm_core::reference::ReferenceFrame;
use mmm_core::synth::{SynthWcs, write_xisf_solved};

const S: f64 = 1.0e-3;
const STAR: [f64; 2] = [66.0, 18.0];

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sky(ra: f64, dec: f64) -> f32 {
    let dx = (ra - STAR[0]) * STAR[1].to_radians().cos() / S;
    let dy = (dec - STAR[1]) / S;
    (0.05 + 0.8 * (-(dx * dx + dy * dy) / 8.0).exp()) as f32
}

fn offset_px(dx: f64, dy: f64) -> [f64; 2] {
    [
        STAR[0] + dx * S / STAR[1].to_radians().cos(),
        STAR[1] + dy * S,
    ]
}

fn write_raw_panel(path: &Path, w: u64, h: u64, crval: [f64; 2]) {
    let cd = [[-S, 0.0], [0.0, S]];
    let refimg = [w as f64 / 2.0, h as f64 / 2.0];
    let lin = LinearWcs {
        crval,
        crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
        cd,
        ctype: ["RA---TAN".into(), "DEC--TAN".into()],
        radesys: "ICRS".into(),
    };
    let mut plane = vec![0.0f32; (w * h) as usize];
    for j in 0..h {
        for i in 0..w {
            let (ra, dec) = lin.pixel_to_sky(i as f64 + 1.0, j as f64 + 1.0);
            plane[(j * w + i) as usize] = sky(ra, dec);
        }
    }
    write_xisf_solved(path, w, h, 1, &plane, &SynthWcs { crval, refimg, cd }).unwrap();
}

/// Two overlapping raw panels; `shift` moves the group on the sky (px), `w2`
/// sets the second panel's width so groups have different footprints.
fn write_group(dir: &Path, tag: &str, shift: (f64, f64), w2: u64) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let a = dir.join(format!("{tag}_00.xisf"));
    let b = dir.join(format!("{tag}_01.xisf"));
    write_raw_panel(&a, 160, 120, offset_px(-60.0 + shift.0, shift.1));
    write_raw_panel(&b, w2, 120, offset_px(60.0 + shift.0, shift.1));
    vec![a, b]
}

fn mmm() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mmm"));
    c.arg("--no-banner");
    c
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("mmm must start");
    assert!(
        out.status.success(),
        "mmm failed ({}):\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn geometry_and_wcs(fits: &Path) -> ((u64, u64), LinearWcs) {
    let p = InputPanel::open(fits).unwrap();
    (
        (p.width(), p.height()),
        p.linear_wcs().expect("blend output carries WCS cards"),
    )
}

#[test]
fn frame_analyze_blend_yield_co_registered_outputs() {
    let dir = tempdir("pipeline");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let frame = dir.join("ref.mmm-frame.json");

    let out = run(mmm().arg("frame").args(&a).args(&b).arg("-o").arg(&frame));
    assert!(out.contains("solved frame"), "{out}");
    assert!(matches!(
        ReferenceFrame::load(&frame).unwrap(),
        ReferenceFrame::Solved { .. }
    ));

    for (tag, panels) in [("A", &a), ("B", &b)] {
        let session = dir.join(format!("{tag}.mmm-session"));
        let out = run(mmm()
            .arg("analyze")
            .args(panels)
            .arg("-s")
            .arg(&session)
            .arg("--input")
            .arg("solved")
            .arg("--frame")
            .arg(&frame));
        assert!(
            out.contains("imposed"),
            "analyze must say the frame was imposed:\n{out}"
        );
        run(mmm()
            .arg("blend")
            .arg("-s")
            .arg(&session)
            .arg("-o")
            .arg(dir.join(format!("{tag}.fits")))
            .arg("--mode")
            .arg("feather")
            .arg("--feather")
            .arg("24"));
    }
    let (ga, wa) = geometry_and_wcs(&dir.join("A.fits"));
    let (gb, wb) = geometry_and_wcs(&dir.join("B.fits"));
    assert_eq!(ga, gb);
    assert_eq!(wa, wb);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn blend_extent_flag_overrides_the_default() {
    let dir = tempdir("extent-flag");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let session = dir.join("A.mmm-session");
    run(mmm()
        .arg("analyze")
        .args(&a)
        .arg("-s")
        .arg(&session)
        .arg("--input")
        .arg("solved"));
    run(mmm()
        .arg("blend")
        .arg("-s")
        .arg(&session)
        .arg("-o")
        .arg(dir.join("union.fits"))
        .arg("--mode")
        .arg("feather"));
    run(mmm()
        .arg("blend")
        .arg("-s")
        .arg(&session)
        .arg("-o")
        .arg(dir.join("canvas.fits"))
        .arg("--mode")
        .arg("feather")
        .arg("--extent")
        .arg("canvas"));
    let (gu, _) = geometry_and_wcs(&dir.join("union.fits"));
    let (gc, _) = geometry_and_wcs(&dir.join("canvas.fits"));
    assert!(
        gc.0 > gu.0 && gc.1 > gu.1,
        "canvas {gc:?} must exceed union {gu:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn analyze_with_missing_frame_file_fails_naming_it() {
    let dir = tempdir("missing-frame");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let out = mmm()
        .arg("analyze")
        .args(&a)
        .arg("-s")
        .arg(dir.join("A.mmm-session"))
        .arg("--frame")
        .arg(dir.join("absent.mmm-frame.json"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("absent.mmm-frame.json"), "{err}");
    assert!(
        !dir.join("A.mmm-session").join("session.json").exists(),
        "must fail before analyzing"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
