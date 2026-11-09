//! Export of a group's tiled result to FITS and TIFF: full canvas, linear,
//! full precision, with WCS and provenance; stale tiles exported as they
//! are and missing ones as zeros, both counted.

use std::path::{Path, PathBuf};

use mmm_core::Error;
use mmm_core::Result;
use mmm_core::blend::{BlendParams, Extent, RowSink, blend};
use mmm_core::export::{
    CompositeChannels, ExportFormat, ExportReport, OnExisting, export_check, export_composite,
    export_composite_with, export_group, export_group_with,
};
use mmm_core::formats::fits::FitsPanel;
use mmm_core::formats::{FitsKeyword, InputPanel};
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::{
    GroupRule, Grouping, Mosaic, MosaicConfig, SINGLE_GROUP, analyze_all, blend_group,
};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::settings::BlendSettings;
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{
    SynthSpec, SynthWcs, generate, solved_fits_cards, write_fits, write_xisf_solved,
};
use mmm_core::tiles::{TILE, TileGeometry, TileStore};

const CANVAS: (u64, u64) = (600, 420);
const CH: u64 = 3;
const TOL: f32 = 1e-6;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mmm-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The canvas WCS every synthetic panel carries (aligned panels share it).
fn canvas_wcs() -> SynthWcs {
    SynthWcs {
        crval: [83.8, -5.4],
        refimg: [300.0, 210.0],
        cd: [[-1e-3, 0.0], [0.0, 1e-3]],
    }
}

/// A single-group mosaic of a 2×2 aligned synthetic panel set, each panel
/// carrying the canvas WCS; analysed, not blended.
fn analysed_mosaic(root: &Path) -> Mosaic {
    let spec = SynthSpec {
        canvas: CANVAS,
        channels: CH as u32,
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
        core: None,
        seed: 5,
    };
    let res = generate(&spec, &root.join("gen")).unwrap();
    let out = root.join("panels");
    std::fs::create_dir_all(&out).unwrap();
    let n = (CANVAS.0 * CANVAS.1 * CH) as usize;
    let paths: Vec<PathBuf> = res
        .panel_paths
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
            let path = out.join(format!("p{i}.xisf"));
            write_xisf_solved(&path, CANVAS.0, CANVAS.1, CH, &planes, &canvas_wcs()).unwrap();
            path
        })
        .collect();
    let mut m = Mosaic::create(&root.join("m"), MosaicConfig::new(paths)).unwrap();
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    m
}

struct MemSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl RowSink for MemSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        (self.w, self.h, self.ch) = (w as usize, h as usize, ch as usize);
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

/// Direct streaming whole-canvas blend of the group analysed in `dir`.
fn direct_blend(dir: &Path) -> MemSink {
    let session = Session::open(dir).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();
    let params = BlendParams {
        downsample: 1,
        extent: Some(Extent::Canvas),
        ..BlendSettings::default().to_params()
    };
    let mut sink = MemSink {
        w: 0,
        h: 0,
        ch: 0,
        data: Vec::new(),
    };
    blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
    sink
}

/// Planar samples of a FITS file, top-down.
fn read_fits(path: &Path) -> (u64, u64, u64, Vec<f32>) {
    let img = FitsPanel::open(path).unwrap();
    let p = InputPanel::open(path).unwrap();
    let (w, h, ch) = (p.width(), p.height(), p.channels());
    let plane = (w * h) as usize;
    let mut out = vec![0f32; plane * ch as usize];
    for c in 0..ch {
        img.decode_rows(c, 0, h as usize, &mut out[c as usize * plane..][..plane]);
    }
    (w, h, ch, out)
}

/// The FITS header cards of `path`, as 80-column strings (up to `END`).
fn header_cards(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let mut cards = Vec::new();
    for card in bytes.chunks(80) {
        let s = String::from_utf8_lossy(card).into_owned();
        let end = s.starts_with("END ");
        cards.push(s);
        if end {
            break;
        }
    }
    cards
}

/// Planar `f32` samples of a TIFF, top-down: `(w, h, channels, data)`.
fn tiff_read_planar(path: &Path) -> mpp_core::Result<(u64, u64, u64, Vec<f32>)> {
    use mpp_core::formats::tiff_read::{SampleFormat, TiffReader};
    let mut r = TiffReader::open(path)?;
    let info = r.info().clone();
    assert_eq!(info.sample_format, SampleFormat::F32);
    let (w, h, ch) = (info.width, info.height, info.total_channels() as u64);
    let plane = (w * h) as usize;
    let mut data = vec![f32::NAN; plane * ch as usize];
    r.read_bands(&mut |c, y0, rows, bytes| {
        let dst = &mut data[c as usize * plane + (y0 * w) as usize..][..rows as usize * w as usize];
        for (d, b) in dst.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *d = f32::from_ne_bytes(*b);
        }
        Ok(())
    })?;
    Ok((w, h, ch, data))
}

fn assert_close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!((x - y).abs() <= TOL, "sample {i}: {x} vs {y}");
    }
}

#[test]
fn fits_export_matches_streaming_blend() {
    let tmp = TempDir::new("fits");
    let mut m = analysed_mosaic(&tmp.0);
    blend_group(&mut m, SINGLE_GROUP, None, &JobCtl::none()).unwrap();
    let out = tmp.0.join("out.fits");
    let report = export_group(
        &m.dir,
        SINGLE_GROUP,
        &out,
        ExportFormat::Fits,
        &JobCtl::none(),
    )
    .unwrap();
    assert_eq!(
        report,
        ExportReport {
            stale_tiles: 0,
            missing_tiles: 0,
            width: CANVAS.0,
            height: CANVAS.1,
        }
    );

    let (w, h, ch, data) = read_fits(&out);
    assert_eq!((w, h, ch), (CANVAS.0, CANVAS.1, CH));
    let direct = direct_blend(&m.group_session_dir(SINGLE_GROUP));
    assert_eq!((direct.w as u64, direct.h as u64), (w, h));
    assert_close(&data, &direct.data);

    // WCS cards for the frame: the canvas solution, unshifted.
    let got = InputPanel::open(&out)
        .unwrap()
        .linear_wcs()
        .expect("WCS cards");
    let want = InputPanel::open(&m.config.panels[0].path)
        .unwrap()
        .linear_wcs()
        .unwrap();
    for i in 0..2 {
        assert!((got.crval[i] - want.crval[i]).abs() < 1e-9, "{got:?}");
        assert!((got.crpix[i] - want.crpix[i]).abs() < 1e-9, "{got:?}");
        for j in 0..2 {
            assert!((got.cd[i][j] - want.cd[i][j]).abs() < 1e-12, "{got:?}");
        }
    }

    // Provenance: the mmm version and the blend settings.
    let cards = header_cards(&out);
    let history: Vec<&String> = cards.iter().filter(|c| c.starts_with("HISTORY ")).collect();
    let text: String = history.iter().map(|c| c[8..].trim_end()).collect();
    assert!(
        text.contains(&format!("mmm {}", env!("CARGO_PKG_VERSION"))),
        "{history:?}"
    );
    assert!(text.contains("feather_px"), "{history:?}");
    assert!(!out.with_extension("fits.part").exists());
}

#[test]
fn tiff_export_dimensions_and_values() {
    let tmp = TempDir::new("tiff");
    let mut m = analysed_mosaic(&tmp.0);
    blend_group(&mut m, SINGLE_GROUP, None, &JobCtl::none()).unwrap();
    let out = tmp.0.join("out.tif");
    let progress = std::sync::Mutex::new(Vec::new());
    let observer = |stage: &str, done: u64, total: u64| {
        progress
            .lock()
            .unwrap()
            .push((stage.to_string(), done, total));
    };
    let ctl = JobCtl::new(CancelToken::new(), Some(&observer));
    let report = export_group(&m.dir, SINGLE_GROUP, &out, ExportFormat::Tiff, &ctl).unwrap();
    assert_eq!((report.width, report.height), CANVAS);
    assert_eq!((report.stale_tiles, report.missing_tiles), (0, 0));

    let (w, h, ch, data) = tiff_read_planar(&out).unwrap();
    assert_eq!((w, h, ch), (CANVAS.0, CANVAS.1, CH));
    let direct = direct_blend(&m.group_session_dir(SINGLE_GROUP));
    assert_close(&data, &direct.data);
    // Linear, unclamped: the blend is not confined to [0, 1] in general,
    // and the TIFF keeps exactly what the tiles hold.
    let store = TileStore::open(&m.result_path(SINGLE_GROUP)).unwrap();
    let t = TILE as usize;
    let mut tile = vec![0f32; CH as usize * t * t];
    assert!(store.read_tile(0, 1, 1, &mut tile));
    let (x, y) = (t + 7, t + 3);
    for c in 0..CH as usize {
        let plane = (w * h) as usize;
        assert_eq!(
            data[c * plane + y * w as usize + x].to_bits(),
            tile[(c * t + 3) * t + 7].to_bits(),
            "channel {c} bit-exact"
        );
    }

    let progress = progress.into_inner().unwrap();
    let last = progress.last().unwrap();
    assert_eq!(last.1, last.2, "progress ends complete: {progress:?}");
    // No scratch or partial file is left behind.
    let names: Vec<String> = std::fs::read_dir(&tmp.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("out"))
        .collect();
    assert_eq!(names, ["out.tif"]);
}

#[test]
fn export_reports_stale_and_missing() {
    let tmp = TempDir::new("stale");
    let mut m = analysed_mosaic(&tmp.0);
    // Two of the six level-0 tiles blended (600×420 in 256-px tiles is
    // 3×2), then one of them invalidated.
    blend_group(
        &mut m,
        SINGLE_GROUP,
        Some([0, 0, 512, 256]),
        &JobCtl::none(),
    )
    .unwrap();
    let result = m.result_path(SINGLE_GROUP);
    let t = TILE as usize;
    let mut before = vec![0f32; CH as usize * t * t];
    {
        let mut store = TileStore::open(&result).unwrap();
        assert!(store.read_tile(0, 0, 0, &mut before));
        store.invalidate_rect([0, 0, 1, 1]);
    }
    let want = ExportReport {
        stale_tiles: 1,
        missing_tiles: 4,
        width: CANVAS.0,
        height: CANVAS.1,
    };
    assert_eq!(export_check(&result).unwrap(), want);
    let warning = want.warning().expect("a warning");
    assert!(warning.contains("not fully merged"), "{warning}");
    assert!(!warning.to_lowercase().contains("blend"), "{warning}");

    let out = tmp.0.join("partial.fits");
    let report = export_group(
        &m.dir,
        SINGLE_GROUP,
        &out,
        ExportFormat::Fits,
        &JobCtl::none(),
    )
    .unwrap();
    assert_eq!(report, want);
    let (w, h, _, data) = read_fits(&out);
    let plane = (w * h) as usize;
    for c in 0..CH as usize {
        // The stale tile is exported as it is...
        for (y, x) in [(0, 0), (10, 200), (255, 255)] {
            assert_eq!(
                data[c * plane + y * w as usize + x],
                before[(c * t + y) * t + x],
                "stale ({x},{y}) ch {c}"
            );
        }
        // ...and a missing one as zeros.
        for (y, x) in [(300, 10), (419, 599), (100, 520)] {
            assert_eq!(
                data[c * plane + y * w as usize + x],
                0.0,
                "missing ({x},{y})"
            );
        }
    }
}

#[test]
fn export_cancels() {
    let tmp = TempDir::new("cancel");
    let mut m = analysed_mosaic(&tmp.0);
    blend_group(&mut m, SINGLE_GROUP, None, &JobCtl::none()).unwrap();
    for (format, name) in [
        (ExportFormat::Fits, "c.fits"),
        (ExportFormat::Tiff, "c.tif"),
    ] {
        let out = tmp.0.join(name);
        // Cancelled during the first progress report.
        let token = CancelToken::new();
        let t2 = token.clone();
        let observer = move |_: &str, _: u64, _: u64| t2.cancel();
        let ctl = JobCtl::new(token, Some(&observer));
        let err = export_group(&m.dir, SINGLE_GROUP, &out, format, &ctl).unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{format:?}: {err}");
        // Nothing written: no output, no partial or scratch file.
        let left: Vec<String> = std::fs::read_dir(&tmp.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("c."))
            .collect();
        assert!(left.is_empty(), "{format:?}: {left:?}");
    }
    // An existing file at the destination survives a cancelled export.
    let out = tmp.0.join("keep.fits");
    std::fs::write(&out, b"old").unwrap();
    let token = CancelToken::new();
    token.cancel();
    let ctl = JobCtl::new(token, None);
    assert!(export_group(&m.dir, SINGLE_GROUP, &out, ExportFormat::Fits, &ctl).is_err());
    assert_eq!(std::fs::read(&out).unwrap(), b"old");
}

#[test]
fn export_of_unblended_group_says_so() {
    let tmp = TempDir::new("unblended");
    let m = analysed_mosaic(&tmp.0);
    let out = tmp.0.join("x.fits");
    let err = export_group(
        &m.dir,
        SINGLE_GROUP,
        &out,
        ExportFormat::Fits,
        &JobCtl::none(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("not been merged"), "{err}");
    assert!(!err.to_lowercase().contains("blend"), "{err}");
    // (The message names the store, whose folder this test calls
    // "unblended".)
    let store = m.result_path(SINGLE_GROUP);
    let err = export_check(&store)
        .unwrap_err()
        .to_string()
        .replace(&store.display().to_string(), "the store");
    assert!(err.contains("not been merged"), "{err}");
    assert!(!err.to_lowercase().contains("blend"), "{err}");
    let err = export_group(&m.dir, "nope", &out, ExportFormat::Fits, &JobCtl::none())
        .unwrap_err()
        .to_string();
    assert!(err.contains("nope"), "{err}");
    assert!(!out.exists());
}

/// A grouped mosaic of 2×2 aligned, solved panels per `(name, channels,
/// seed)` group (assigned by hand), analysed and every group merged.
fn grouped_mosaic(root: &Path, groups: &[(&str, u64, u64)]) -> Mosaic {
    grouped_mosaic_of(root, groups, false)
}

/// [`grouped_mosaic`]; when `banded`, its panels are FITS files whose
/// headers name their group's band twice, `FILTER` and `BAND` (the grouping
/// rule's key), beside a session-level `OBJECT`.
fn grouped_mosaic_of(root: &Path, groups: &[(&str, u64, u64)], banded: bool) -> Mosaic {
    let mut paths = Vec::new();
    let mut names = Vec::new();
    for &(name, ch, seed) in groups {
        let spec = SynthSpec {
            canvas: CANVAS,
            channels: ch as u32,
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
            core: None,
            seed,
        };
        let res = generate(&spec, &root.join(format!("gen-{name}"))).unwrap();
        let out = root.join("panels");
        std::fs::create_dir_all(&out).unwrap();
        let n = (CANVAS.0 * CANVAS.1 * ch) as usize;
        for (i, p) in res.panel_paths.iter().enumerate() {
            let bytes = std::fs::read(p).unwrap();
            let planes: Vec<f32> = bytes[4096..4096 + n * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            let path = if banded {
                let card = |key: &str, value: &str| FitsKeyword {
                    name: key.into(),
                    value: format!("'{value}'"),
                    comment: String::new(),
                };
                let mut cards = solved_fits_cards(&canvas_wcs(), CANVAS.1, None);
                cards.extend([
                    card("OBJECT", "M42"),
                    card("FILTER", name),
                    card("BAND", name),
                ]);
                let path = out.join(format!("{name}_{i}.fits"));
                write_fits(&path, CANVAS.0, CANVAS.1, ch, &planes, -32, &cards).unwrap();
                path
            } else {
                let path = out.join(format!("{name}_{i}.xisf"));
                write_xisf_solved(&path, CANVAS.0, CANVAS.1, ch, &planes, &canvas_wcs()).unwrap();
                path
            };
            paths.push(path);
            names.push(name.to_string());
        }
    }
    let mut cfg = MosaicConfig::new(paths);
    cfg.grouping = Grouping {
        enabled: true,
        rule: if banded {
            GroupRule::Header { key: "BAND".into() }
        } else {
            GroupRule::Manual
        },
    };
    for (p, n) in cfg.panels.iter_mut().zip(names) {
        p.manual_group = Some(n);
    }
    let mut m = Mosaic::create(&root.join("m"), cfg).unwrap();
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    for &(name, ..) in groups {
        blend_group(&mut m, name, None, &JobCtl::none()).unwrap();
    }
    m
}

fn hoo<'a>() -> CompositeChannels<'a> {
    CompositeChannels {
        name: "HOO",
        r: Some("Ha"),
        g: Some("OIII"),
        b: Some("OIII"),
    }
}

/// Plane `p` of planar `data` (`plane` samples each).
fn plane_of(data: &[f32], plane: usize, p: usize) -> &[f32] {
    &data[p * plane..(p + 1) * plane]
}

#[test]
fn composite_export_maps_groups_to_planes() {
    let tmp = TempDir::new("comp-map");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)]);
    let out = tmp.0.join("hoo.fits");
    let report =
        export_composite(&m.dir, &hoo(), &out, ExportFormat::Fits, &JobCtl::none()).unwrap();
    assert_eq!(
        report,
        ExportReport {
            stale_tiles: 0,
            missing_tiles: 0,
            width: CANVAS.0,
            height: CANVAS.1,
        }
    );
    let (w, h, ch, data) = read_fits(&out);
    assert_eq!((w, h, ch), (CANVAS.0, CANVAS.1, 3));
    let plane = (w * h) as usize;
    let ha = direct_blend(&m.group_session_dir("Ha"));
    let oiii = direct_blend(&m.group_session_dir("OIII"));
    assert_close(plane_of(&data, plane, 0), &ha.data);
    assert_close(plane_of(&data, plane, 1), &oiii.data);
    assert_close(plane_of(&data, plane, 2), &oiii.data);
    assert!(
        plane_of(&data, plane, 0)
            .iter()
            .zip(plane_of(&data, plane, 1))
            .any(|(a, b)| a != b),
        "the groups differ"
    );

    // WCS and provenance.
    assert!(InputPanel::open(&out).unwrap().linear_wcs().is_some());
    let text: String = header_cards(&out)
        .iter()
        .filter(|c| c.starts_with("HISTORY "))
        .map(|c| c[8..].trim_end().to_string() + "\n")
        .collect();
    for want in [
        "composite 'HOO'",
        "R: group 'Ha'",
        "G: group 'OIII'",
        "B: group 'OIII'",
        "Group 'Ha':",
        "Group 'OIII':",
        "feather_px",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    assert!(!out.with_extension("fits.part").exists());

    // An unmapped channel is zeros.
    let out = tmp.0.join("ha-only.fits");
    let one = CompositeChannels {
        name: "Ha only",
        r: Some("Ha"),
        g: None,
        b: None,
    };
    export_composite(&m.dir, &one, &out, ExportFormat::Fits, &JobCtl::none()).unwrap();
    let (_, _, ch, data) = read_fits(&out);
    assert_eq!(ch, 3);
    assert_close(plane_of(&data, plane, 0), &ha.data);
    assert!(data[plane..].iter().all(|&v| v == 0.0));
    let text = header_cards(&out).concat();
    assert!(text.contains("G: none"), "{text}");
}

#[test]
fn composite_export_averages_an_osc_member() {
    let tmp = TempDir::new("comp-osc");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("RGB", 3, 9)]);
    let out = tmp.0.join("mix.fits");
    let channels = CompositeChannels {
        name: "Mix",
        r: Some("Ha"),
        g: Some("RGB"),
        b: None,
    };
    export_composite(&m.dir, &channels, &out, ExportFormat::Fits, &JobCtl::none()).unwrap();
    let (w, h, ch, data) = read_fits(&out);
    assert_eq!((w, h, ch), (CANVAS.0, CANVAS.1, 3));
    let plane = (w * h) as usize;
    let ha = direct_blend(&m.group_session_dir("Ha"));
    let rgb = direct_blend(&m.group_session_dir("RGB"));
    assert_eq!(rgb.ch, 3);
    let mean: Vec<f32> = (0..plane)
        .map(|i| (rgb.data[i] + rgb.data[plane + i] + rgb.data[2 * plane + i]) / 3.0)
        .collect();
    assert_close(plane_of(&data, plane, 0), &ha.data);
    assert_close(plane_of(&data, plane, 1), &mean);
    assert!(plane_of(&data, plane, 2).iter().all(|&v| v == 0.0));
}

/// The header cards of `path` named `name` (columns 1–8).
fn cards_named(path: &Path, name: &str) -> Vec<String> {
    header_cards(path)
        .into_iter()
        .filter(|c| c.get(..8).is_some_and(|n| n.trim_end() == name))
        .collect()
}

#[test]
fn a_composite_claims_no_one_filter() {
    let tmp = TempDir::new("comp-filter");
    let m = grouped_mosaic_of(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)], true);
    // A group's export passes its panels' band through.
    let ha = tmp.0.join("ha.fits");
    export_group(&m.dir, "Ha", &ha, ExportFormat::Fits, &JobCtl::none()).unwrap();
    for name in ["FILTER", "BAND"] {
        let cards = cards_named(&ha, name);
        assert_eq!(cards.len(), 1, "{name}: {cards:?}");
        assert!(cards[0].contains("'Ha'"), "{}", cards[0]);
    }
    assert_eq!(cards_named(&ha, "OBJECT").len(), 1);
    // A composite's names no single panel's band: neither FILTER nor the
    // card its groups are made by. The session's cards and the WCS stay,
    // and the HISTORY says which group each channel holds.
    let out = tmp.0.join("hoo.fits");
    export_composite(&m.dir, &hoo(), &out, ExportFormat::Fits, &JobCtl::none()).unwrap();
    assert_eq!(cards_named(&out, "FILTER"), Vec::<String>::new());
    assert_eq!(cards_named(&out, "BAND"), Vec::<String>::new());
    assert_eq!(cards_named(&out, "OBJECT").len(), 1);
    assert!(InputPanel::open(&out).unwrap().linear_wcs().is_some());
    let text = header_cards(&out).concat();
    for want in ["R: group 'Ha'", "G: group 'OIII'", "B: group 'OIII'"] {
        assert!(text.contains(want), "{want}: {text}");
    }
}

#[test]
fn composite_export_refuses_different_canvases() {
    let tmp = TempDir::new("comp-canvas");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)]);
    // OIII's result becomes a store of another size.
    let wrong = TileGeometry {
        width: 300,
        height: 200,
        channels: 1,
        tile: TILE,
    };
    drop(TileStore::create(&m.result_path("OIII"), wrong).unwrap());
    let out = tmp.0.join("x.fits");
    let err = export_composite(&m.dir, &hoo(), &out, ExportFormat::Fits, &JobCtl::none())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("the composite's groups have different canvases"),
        "{err}"
    );
    assert!(!out.exists());
    assert!(!out.with_extension("fits.part").exists());
}

#[test]
fn composite_export_names_its_problems() {
    let tmp = TempDir::new("comp-errors");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)]);
    let out = tmp.0.join("x.fits");
    let go = |c: &CompositeChannels| {
        export_composite(&m.dir, c, &out, ExportFormat::Fits, &JobCtl::none())
            .unwrap_err()
            .to_string()
    };
    let none = CompositeChannels {
        name: "Empty",
        r: None,
        g: None,
        b: None,
    };
    assert!(go(&none).contains("the composite maps no groups"));
    let ghost = CompositeChannels {
        r: Some("Ghost"),
        ..none
    };
    assert!(go(&ghost).contains("'Ghost'"));
    // A group whose result is gone reads as not merged yet.
    std::fs::remove_file(m.result_path("OIII")).unwrap();
    let err = go(&hoo());
    assert!(
        err.contains("group 'OIII' has not been merged yet"),
        "{err}"
    );
    assert!(!out.exists());
}

#[test]
fn composite_export_cancels_and_leaves_the_destination() {
    let tmp = TempDir::new("comp-cancel");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)]);
    for (format, name) in [
        (ExportFormat::Fits, "c.fits"),
        (ExportFormat::Tiff, "c.tif"),
    ] {
        let out = tmp.0.join(name);
        let token = CancelToken::new();
        let t2 = token.clone();
        let observer = move |_: &str, _: u64, _: u64| t2.cancel();
        let ctl = JobCtl::new(token, Some(&observer));
        let err = export_composite(&m.dir, &hoo(), &out, format, &ctl).unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{format:?}: {err}");
        let left: Vec<String> = std::fs::read_dir(&tmp.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("c."))
            .collect();
        assert!(left.is_empty(), "{format:?}: {left:?}");
    }
    let out = tmp.0.join("keep.fits");
    std::fs::write(&out, b"old").unwrap();
    let token = CancelToken::new();
    token.cancel();
    let ctl = JobCtl::new(token, None);
    assert!(export_composite(&m.dir, &hoo(), &out, ExportFormat::Fits, &ctl).is_err());
    assert_eq!(std::fs::read(&out).unwrap(), b"old");
}

#[test]
fn composite_tiff_export_is_three_channel() {
    let tmp = TempDir::new("comp-tiff");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)]);
    let out = tmp.0.join("hoo.tif");
    let report =
        export_composite(&m.dir, &hoo(), &out, ExportFormat::Tiff, &JobCtl::none()).unwrap();
    assert_eq!((report.width, report.height), CANVAS);
    assert!(report.is_complete());
    let (w, h, ch, data) = tiff_read_planar(&out).unwrap();
    assert_eq!((w, h, ch), (CANVAS.0, CANVAS.1, 3));
    let plane = (w * h) as usize;
    let ha = direct_blend(&m.group_session_dir("Ha"));
    let oiii = direct_blend(&m.group_session_dir("OIII"));
    assert_close(plane_of(&data, plane, 0), &ha.data);
    assert_close(plane_of(&data, plane, 1), &oiii.data);
    assert_close(plane_of(&data, plane, 2), &oiii.data);
    let names: Vec<String> = std::fs::read_dir(&tmp.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("hoo"))
        .collect();
    assert_eq!(names, ["hoo.tif"]);
}

/// The files in `dir` whose names start with `prefix`, sorted.
fn files_starting(dir: &Path, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix))
        .collect();
    names.sort();
    names
}

#[test]
fn export_never_replaces_unless_asked() {
    let tmp = TempDir::new("keep");
    let mut m = analysed_mosaic(&tmp.0);
    blend_group(&mut m, SINGLE_GROUP, None, &JobCtl::none()).unwrap();
    let keep = OnExisting::Keep;
    // There when the export starts: refused before anything is written.
    let out = tmp.0.join("k.fits");
    std::fs::write(&out, b"old").unwrap();
    let err = export_group_with(
        &m.dir,
        SINGLE_GROUP,
        &out,
        ExportFormat::Fits,
        keep,
        &JobCtl::none(),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "k.fits appeared meanwhile and was not replaced"
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"old");
    assert_eq!(files_starting(&tmp.0, "k."), ["k.fits"]);
    // Appearing while the export runs (once the file is complete, before
    // it is put in place): refused, and the newcomer kept.
    for (format, name) in [
        (ExportFormat::Fits, "late.fits"),
        (ExportFormat::Tiff, "late.tif"),
    ] {
        let out = tmp.0.join(name);
        let appear = out.clone();
        let observer = move |_: &str, done: u64, total: u64| {
            if done == total && !appear.exists() {
                std::fs::write(&appear, b"new").unwrap();
            }
        };
        let ctl = JobCtl::new(CancelToken::new(), Some(&observer));
        let err = export_group_with(&m.dir, SINGLE_GROUP, &out, format, keep, &ctl).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{name} appeared meanwhile and was not replaced"),
            "{format:?}"
        );
        assert_eq!(std::fs::read(&out).unwrap(), b"new", "{format:?}");
        assert_eq!(files_starting(&tmp.0, "late."), [name], "no partial file");
        std::fs::remove_file(&out).unwrap();
    }
    // Not there: written as ever.
    let fresh = tmp.0.join("fresh.fits");
    export_group_with(
        &m.dir,
        SINGLE_GROUP,
        &fresh,
        ExportFormat::Fits,
        keep,
        &JobCtl::none(),
    )
    .unwrap();
    assert_eq!(read_fits(&fresh).0, CANVAS.0);
    // Asked to (and export_group, as `mmm export` uses it): replaced.
    export_group(
        &m.dir,
        SINGLE_GROUP,
        &out,
        ExportFormat::Fits,
        &JobCtl::none(),
    )
    .unwrap();
    assert_eq!(read_fits(&out).0, CANVAS.0);
    std::fs::write(&out, b"old").unwrap();
    let replace = OnExisting::Replace;
    export_group_with(
        &m.dir,
        SINGLE_GROUP,
        &out,
        ExportFormat::Fits,
        replace,
        &JobCtl::none(),
    )
    .unwrap();
    assert_eq!(read_fits(&out).0, CANVAS.0);
}

#[test]
fn composite_export_never_replaces_unless_asked() {
    let tmp = TempDir::new("comp-keep");
    let m = grouped_mosaic(&tmp.0, &[("Ha", 1, 3), ("OIII", 1, 4)]);
    let out = tmp.0.join("hoo.fits");
    std::fs::write(&out, b"old").unwrap();
    let keep = OnExisting::Keep;
    let ctl = JobCtl::none();
    let err =
        export_composite_with(&m.dir, &hoo(), &out, ExportFormat::Fits, keep, &ctl).unwrap_err();
    assert_eq!(
        err.to_string(),
        "hoo.fits appeared meanwhile and was not replaced"
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"old");
    export_composite(&m.dir, &hoo(), &out, ExportFormat::Fits, &ctl).unwrap();
    assert_eq!(read_fits(&out).2, 3, "replaced by default");
}
