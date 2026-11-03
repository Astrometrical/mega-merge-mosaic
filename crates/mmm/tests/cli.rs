//! Binary-level tests of the `mmm` CLI around the shared reference frame:
//! `frame` → `analyze --frame` → `blend` yields co-registered FITS outputs,
//! and `batch` does the same in one command; mosaic sessions (`init`, `set`,
//! `check`, `run`) do it from a persisted `mosaic.json`.

use std::path::{Path, PathBuf};
use std::process::Command;

use mmm_core::astrometry::LinearWcs;
use mmm_core::blend::BlendMode;
use mmm_core::formats::InputPanel;
use mmm_core::mosaic::MosaicConfig;
use mmm_core::reference::ReferenceFrame;
use mmm_core::synth::{
    SynthSpec, SynthWcs, generate, write_xisf_solved, write_xisf_with_header_xml,
};
use mmm_core::tiles::{TILE, TileGeometry, TileStore};

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

#[test]
fn batch_merges_every_group_onto_one_grid() {
    let dir = tempdir("batch");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let sessions = dir.join("sessions");
    let out_dir = dir.join("out");
    let out = run(mmm()
        .arg("batch")
        .arg("--group")
        .arg("L")
        .args(&a)
        .arg("--group")
        .arg("Ha")
        .args(&b)
        .arg("-s")
        .arg(&sessions)
        .arg("-o")
        .arg(&out_dir)
        .arg("--input")
        .arg("solved")
        .arg("--mode")
        .arg("feather")
        .arg("--feather")
        .arg("24"));
    assert!(out.contains("group L"), "{out}");
    assert!(out.contains("group Ha"), "{out}");
    assert!(matches!(
        ReferenceFrame::load(&sessions.join("reference.mmm-frame.json")).unwrap(),
        ReferenceFrame::Solved { .. }
    ));
    assert!(sessions.join("L.mmm-session").join("session.json").exists());
    assert!(
        sessions
            .join("Ha.mmm-session")
            .join("session.json")
            .exists()
    );
    let (gl, wl) = geometry_and_wcs(&out_dir.join("L.fits"));
    let (gh, wh) = geometry_and_wcs(&out_dir.join("Ha.fits"));
    assert_eq!(gl, gh);
    assert_eq!(wl, wh);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn batch_refuses_bad_group_names_before_any_work() {
    let dir = tempdir("batch-names");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let out = mmm()
        .arg("batch")
        .arg("--group")
        .arg("L")
        .args(&a)
        .arg("--group")
        .arg("L")
        .args(&a)
        .arg("-s")
        .arg(dir.join("sessions"))
        .arg("-o")
        .arg(dir.join("out"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("used twice"));
    assert!(!dir.join("sessions").exists(), "no work before validation");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Recompress the pixel block of a synthesized XISF file in place
/// (zstd+sh), the way PixInsight's WBPP writes masters.
fn recompress(path: &Path) {
    use mpp_core::formats::xisf_codec::{Codec, compress};
    let mut bytes = std::fs::read(path).unwrap();
    let hlen = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let xml = String::from_utf8(bytes[16..16 + hlen].to_vec()).unwrap();
    let start = xml.find(r#"location="attachment:"#).unwrap();
    let attr = &xml[start
        ..xml[start..]
            .find("\" ")
            .map_or(xml.len(), |e| start + e + 1)];
    let attr = attr.split('"').nth(1).unwrap().to_string(); // attachment:OFF:SIZE
    let mut it = attr.trim_start_matches("attachment:").split(':');
    let off: usize = it.next().unwrap().parse().unwrap();
    let size: usize = it.next().unwrap().parse().unwrap();
    let (stored, _) = compress(Codec::Zstd, Some(4), &bytes[off..off + size], 1 << 20);
    let xml = xml.replace(
        &format!(r#"location="{attr}""#),
        &format!(
            r#"compression="zstd+sh:{size}:4" location="attachment:{off}:{}""#,
            stored.len()
        ),
    );
    assert!(16 + xml.len() <= off, "header still fits before the block");
    bytes.truncate(16);
    bytes[8..12].copy_from_slice(&(xml.len() as u32).to_le_bytes());
    bytes.extend_from_slice(xml.as_bytes());
    bytes.resize(off, 0);
    bytes.extend_from_slice(&stored);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn analyze_reads_compressed_xisf_and_says_so() {
    let dir = tempdir("compressed");
    let panels = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    for p in &panels {
        recompress(p);
        assert!(
            !InputPanel::open(p).unwrap().as_xisf().is_some(),
            "not mappable"
        );
    }
    let session = dir.join("A.mmm-session");
    let out = mmm()
        .arg("analyze")
        .args(&panels)
        .arg("-s")
        .arg(&session)
        .arg("--input")
        .arg("solved")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "analyze failed:\n{stderr}");
    assert!(
        stderr.contains("Decoding compressed (zstd+sh) XISF panel a_00.xisf"),
        "the decode is announced:\n{stderr}"
    );
    let cached: Vec<_> = std::fs::read_dir(session.join("decoded"))
        .expect("decode cache inside the session")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "f32"))
        .collect();
    assert_eq!(cached.len(), 2, "one cache file per panel");
    // Blending re-uses the cache: no second announcement.
    let out = mmm()
        .arg("blend")
        .arg("-s")
        .arg(&session)
        .arg("-o")
        .arg(dir.join("A.fits"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "blend failed:\n{stderr}");
    assert!(
        !stderr.contains("Decoding compressed"),
        "cache re-used:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A 2×2 aligned synthetic panel set, each panel rewritten with a `FILTER`
/// header card; returns the tagged panel paths.
fn filter_group(root: &Path, filter: &str, seed: u64) -> Vec<PathBuf> {
    const CANVAS: (u64, u64) = (240, 180);
    let spec = SynthSpec {
        canvas: CANVAS,
        channels: 1,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 20,
        noise_sigma: 0.002,
        panel_gain_range: (0.8, 1.2),
        panel_offset_range: (0.0, 0.01),
        panel_gradient_range: (0.0, 0.0),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![],
        panel_spike_angle: vec![],
        panel_defects: vec![],
        mid_blobs: 0,
        shift_blobs: false,
        core: None,
        seed,
    };
    let res = generate(&spec, &root.join(format!("gen-{filter}"))).unwrap();
    let out = root.join("panels");
    std::fs::create_dir_all(&out).unwrap();
    let n = (CANVAS.0 * CANVAS.1) as usize;
    res.panel_paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // The synth writer stores the Float32 planes at offset 4096.
            let bytes = std::fs::read(p).unwrap();
            let planes: Vec<f32> = bytes[4096..4096 + n * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            let path = out.join(format!("{filter}_{i}.xisf"));
            write_xisf_with_header_xml(
                &path,
                CANVAS.0,
                CANVAS.1,
                1,
                &planes,
                &format!(r#"<FITSKeyword name="FILTER" value="'{filter}'" comment=""/>"#),
            )
            .unwrap();
            path
        })
        .collect()
}

/// Run `mmm` expecting failure; returns stderr.
fn run_fail(cmd: &mut Command) -> String {
    let out = cmd.output().expect("mmm must start");
    assert!(
        !out.status.success(),
        "mmm unexpectedly succeeded:\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `mmm init` over Ha + OIII panels grouped by `FILTER` into `dir/m`.
fn init_two_groups(dir: &Path) -> PathBuf {
    let mut panels = filter_group(dir, "Ha", 7);
    panels.extend(filter_group(dir, "OIII", 11));
    let m = dir.join("m");
    run(mmm()
        .arg("init")
        .args(&panels)
        .arg("-s")
        .arg(&m)
        .arg("--group-by")
        .arg("header:FILTER"));
    m
}

#[test]
fn init_run_two_groups() {
    let dir = tempdir("init-run");
    let m = init_two_groups(&dir);
    let out_dir = dir.join("out");
    let out = run(mmm().arg("run").arg("-s").arg(&m).arg("-o").arg(&out_dir));
    assert!(out.contains("group Ha"), "{out}");
    assert!(out.contains("group OIII"), "{out}");
    let cfg = MosaicConfig::load(&m).unwrap();
    let file = |g: &str| out_dir.join(format!("{}.fits", cfg.group_dirs[g]));
    let ha = InputPanel::open(&file("Ha")).unwrap();
    let oiii = InputPanel::open(&file("OIII")).unwrap();
    assert_eq!(
        (ha.width(), ha.height()),
        (oiii.width(), oiii.height()),
        "groups share one grid"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn set_group_override_persists() {
    let dir = tempdir("set-override");
    let m = init_two_groups(&dir);
    let out = run(mmm()
        .arg("set")
        .arg("-s")
        .arg(&m)
        .arg("--group")
        .arg("Ha")
        .arg("--mode")
        .arg("feather"));
    assert!(out.contains("reblend"), "impact is printed:\n{out}");
    let json: String = std::fs::read_to_string(m.join("mosaic.json")).unwrap();
    let cfg = MosaicConfig::load(&m).unwrap();
    assert_eq!(
        cfg.group_overrides["Ha"].blend.mode,
        Some(BlendMode::Feather),
        "{json}"
    );
    assert_eq!(cfg.blend.mode, BlendMode::Pyramid, "mosaic-wide unchanged");
    assert!(!cfg.group_overrides.contains_key("OIII"));

    // Mosaic-wide: only the flag passed changes.
    run(mmm()
        .arg("set")
        .arg("-s")
        .arg(&m)
        .arg("--feather")
        .arg("100"));
    let cfg2 = MosaicConfig::load(&m).unwrap();
    assert_eq!(cfg2.blend.feather_px, 100.0);
    assert_eq!(cfg2.blend.mode, BlendMode::Pyramid);
    assert_eq!(cfg2.analyze, cfg.analyze);
    assert_eq!(cfg2.group_overrides, cfg.group_overrides);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn set_mosaic_wide_is_atomic() {
    let dir = tempdir("set-atomic");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let m = dir.join("m");
    run(mmm().arg("init").args(&a).arg("-s").arg(&m));
    // A blend in progress holds a group's result store.
    let group = m.join("groups").join("mosaic");
    std::fs::create_dir_all(&group).unwrap();
    let g = TileGeometry {
        width: 64,
        height: 64,
        channels: 1,
        tile: TILE,
    };
    let held = TileStore::create(&group.join("result.mmmt"), g).unwrap();
    let before = std::fs::read_to_string(m.join("mosaic.json")).unwrap();
    let set_both = || {
        let mut c = mmm();
        c.arg("set")
            .arg("-s")
            .arg(&m)
            .arg("--surface")
            .arg("1")
            .arg("--feather")
            .arg("100");
        c
    };
    let err = run_fail(&mut set_both());
    assert!(err.contains("in use"), "{err}");
    assert_eq!(
        std::fs::read_to_string(m.join("mosaic.json")).unwrap(),
        before,
        "neither half of the edit is saved"
    );

    drop(held);
    let out = run(&mut set_both());
    assert!(out.contains("reanalyze"), "{out}");
    let cfg = MosaicConfig::load(&m).unwrap();
    assert_eq!(cfg.analyze.surface, Some(1));
    assert_eq!(cfg.blend.feather_px, 100.0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn check_lists_missing_distortion() {
    let dir = tempdir("check-distortion");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let m = dir.join("m");
    run(mmm().arg("init").args(&a).arg("-s").arg(&m));
    let out = run(mmm().arg("check").arg("-s").arg(&m));
    assert!(out.contains("mosaic: 2 panels"), "{out}");
    assert!(out.contains("problems (2)"), "{out}");
    assert!(
        out.contains("a_00.xisf: missing distortion model."),
        "{out}"
    );
    assert!(
        out.contains("Re-solve the panel with distortion correction enabled."),
        "{out}"
    );
    assert!(out.contains("passed:"), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn blend_on_mosaic_dir_points_to_run() {
    let dir = tempdir("mosaic-guard");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let m = dir.join("m");
    run(mmm().arg("init").args(&a).arg("-s").arg(&m));
    let err = run_fail(
        mmm()
            .arg("blend")
            .arg("-s")
            .arg(&m)
            .arg("-o")
            .arg(dir.join("x.fits")),
    );
    assert!(err.contains("mmm run"), "{err}");
    assert!(err.contains("mmm check"), "{err}");
    let err = run_fail(mmm().arg("analyze").args(&a).arg("-s").arg(&m));
    assert!(err.contains("mmm run"), "{err}");
    assert!(!m.join("session.json").exists(), "nothing analyzed");
    let err = run_fail(mmm().arg("report").arg("-s").arg(&m));
    assert!(err.contains("mmm check"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_refuses_existing() {
    let dir = tempdir("init-existing");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let m = dir.join("m");
    run(mmm().arg("init").args(&a).arg("-s").arg(&m));
    let before = std::fs::read_to_string(m.join("mosaic.json")).unwrap();
    let err = run_fail(mmm().arg("init").args(&a[..1]).arg("-s").arg(&m));
    assert!(err.contains("--force"), "{err}");
    assert_eq!(
        std::fs::read_to_string(m.join("mosaic.json")).unwrap(),
        before,
        "untouched"
    );
    run(mmm()
        .arg("init")
        .args(&a[..1])
        .arg("-s")
        .arg(&m)
        .arg("--force"));
    assert_eq!(MosaicConfig::load(&m).unwrap().panels.len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn set_rejects_non_finite_or_non_positive_feather() {
    let dir = tempdir("set-bad-feather");
    let a = write_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let m = dir.join("m");
    run(mmm().arg("init").args(&a).arg("-s").arg(&m));
    let before = std::fs::read_to_string(m.join("mosaic.json")).unwrap();
    for bad in ["nan", "inf", "0", "-5"] {
        let err = run_fail(
            mmm()
                .arg("set")
                .arg("-s")
                .arg(&m)
                .arg(format!("--feather={bad}")),
        );
        assert!(err.contains("--feather"), "{bad}: {err}");
        assert!(err.contains("greater than 0"), "{bad}: {err}");
        assert_eq!(
            std::fs::read_to_string(m.join("mosaic.json")).unwrap(),
            before,
            "{bad}: mosaic.json unchanged"
        );
    }
    // `init` refuses it too, before creating anything.
    let m2 = dir.join("m2");
    let err = run_fail(
        mmm()
            .arg("init")
            .args(&a)
            .arg("-s")
            .arg(&m2)
            .arg("--feather=nan"),
    );
    assert!(err.contains("--feather"), "{err}");
    assert!(!m2.exists());
    MosaicConfig::load(&m).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn check_reports_corrupt_group_analysis_and_continues() {
    let dir = tempdir("check-corrupt");
    let m = init_two_groups(&dir);
    run(mmm()
        .arg("run")
        .arg("-s")
        .arg(&m)
        .arg("-o")
        .arg(dir.join("out")));
    let cfg = MosaicConfig::load(&m).unwrap();
    let session = m
        .join("groups")
        .join(&cfg.group_dirs["Ha"])
        .join("session.json");
    let json = std::fs::read(&session).unwrap();
    std::fs::write(&session, &json[..json.len() / 2]).unwrap();

    let out = run(mmm().arg("check").arg("-s").arg(&m));
    assert!(
        out.contains("cannot read analysis of group Ha:"),
        "problem line for the corrupt group:\n{out}"
    );
    // The other group's checks and the summary still print.
    assert!(out.contains("passed:"), "{out}");
    assert!(!out.contains("group OIII:"), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn export_warns_on_stale() {
    use mmm_core::job::JobCtl;
    use mmm_core::mosaic::{Mosaic, analyze_all, blend_group};

    let dir = tempdir("export-stale");
    let m = init_two_groups(&dir);
    // Blend both groups into their tiled results (as the app does), then
    // stale Ha's only tile (the 240×180 canvas is one 256-px tile).
    let mut mosaic = Mosaic::open(&m).unwrap();
    analyze_all(&mut mosaic, &JobCtl::none()).unwrap();
    blend_group(&mut mosaic, "Ha", None, &JobCtl::none()).unwrap();
    blend_group(&mut mosaic, "OIII", None, &JobCtl::none()).unwrap();
    TileStore::open(&mosaic.result_path("Ha"))
        .unwrap()
        .invalidate_rect([0, 0, 1, 1]);

    let ha = dir.join("ha.fits");
    let out = mmm()
        .args(["export", "--group", "Ha", "-s"])
        .arg(&m)
        .arg("-o")
        .arg(&ha)
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("1 stale, 0 missing"), "{stdout}");
    assert!(
        stderr.contains("warning:") && stderr.contains("1 stale tile"),
        "{stderr}"
    );
    let p = InputPanel::open(&ha).unwrap();
    assert_eq!((p.width(), p.height()), (240, 180));

    // A fully blended group exports without a warning (TIFF here).
    let oiii = dir.join("oiii.tif");
    let out = mmm()
        .args(["export", "--group", "OIII", "--format", "tiff", "-s"])
        .arg(&m)
        .arg("-o")
        .arg(&oiii)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");
    assert!(oiii.is_file());
    // A bad format is refused before anything is written.
    let err = run_fail(
        mmm()
            .args(["export", "--group", "OIII", "--format", "png", "-s"])
            .arg(&m)
            .arg("-o")
            .arg(dir.join("x.png")),
    );
    assert!(err.contains("--format must be fits or tiff"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}
