//! Multi-group mosaic orchestration: `analyze_all` over header-defined
//! groups, `blend_group` into each group's tiled result, and settings edits
//! staling exactly the stores they affect.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mmm_core::Result;
use mmm_core::blend::{BlendMode, BlendParams, Extent, RowSink, blend};
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::{
    FRAME_FILE, GROUPS_DIR, GroupRule, Grouping, Mosaic, MosaicConfig, RESULT_FILE, SINGLE_GROUP,
    analyze_all, blend_group, set_analyze_settings, set_blend_settings, set_group_override,
};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::settings::{
    AnalyzeOverride, BlendOverride, BlendSettings, ChangeImpact, GroupOverride,
};
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthSpec, generate, write_xisf_with_header_xml};
use mmm_core::tiles::{TILE, TileGeometry, TileStore};

/// Comparison tolerance (matches `tiles_blend.rs`).
const TOL: f32 = 1e-6;
const CANVAS: (u64, u64) = (600, 420);
const CH: u64 = 3;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mmm-mosaic-run-{tag}-{}", std::process::id()));
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

/// Synthesize a 2×2 aligned panel set and rewrite each panel with a
/// `FILTER` header card; returns the tagged panel paths.
fn filter_group(root: &Path, filter: &str, seed: u64) -> Vec<PathBuf> {
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
        seed,
    };
    let res = generate(&spec, &root.join(format!("gen-{filter}"))).unwrap();
    let out = root.join("panels");
    std::fs::create_dir_all(&out).unwrap();
    let n = (CANVAS.0 * CANVAS.1 * CH) as usize;
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
                CH,
                &planes,
                &format!(r#"<FITSKeyword name="FILTER" value="'{filter}'" comment=""/>"#),
            )
            .unwrap();
            path
        })
        .collect()
}

/// A grouped Ha + OIII mosaic in `root/m`.
fn two_group_mosaic(root: &Path) -> Mosaic {
    let mut paths = filter_group(root, "Ha", 7);
    paths.extend(filter_group(root, "OIII", 11));
    let mut cfg = MosaicConfig::new(paths);
    cfg.grouping = Grouping {
        enabled: true,
        rule: GroupRule::Header {
            key: "FILTER".into(),
        },
    };
    Mosaic::create(&root.join("m"), cfg).unwrap()
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

/// Direct whole-canvas blend of the session in `dir` with default settings.
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

fn valid0(path: &Path) -> usize {
    TileStore::open(path).unwrap().valid_count(0)
}

#[test]
fn two_group_mosaic_end_to_end() {
    let tmp = TempDir::new("e2e");
    let mut m = two_group_mosaic(&tmp.0);
    let groups = analyze_all(&mut m, &JobCtl::none()).unwrap();
    assert_eq!(
        groups.groups.keys().cloned().collect::<Vec<_>>(),
        ["Ha", "OIII"]
    );
    assert!(groups.ungrouped.is_empty());
    assert!(m.dir.join(GROUPS_DIR).join("Ha/session.json").is_file());
    assert!(m.dir.join(GROUPS_DIR).join("OIII/session.json").is_file());
    assert!(m.dir.join(FRAME_FILE).is_file());
    assert_eq!(m.group_session_dir("Ha"), m.dir.join("groups/Ha"));
    assert_eq!(
        m.result_path("Ha"),
        m.dir.join("groups/Ha").join(RESULT_FILE)
    );
    assert!(m.config.panels.iter().all(|p| p.fingerprint.is_some()));

    let r = blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    assert_eq!(r, [0, 0, CANVAS.0, CANVAS.1]);

    let full = direct_blend(&m.group_session_dir("Ha"));
    let store = TileStore::open(&m.result_path("Ha")).unwrap();
    let g = store.geometry();
    assert_eq!(
        (g.width, g.height, g.channels as u64),
        (CANVAS.0, CANVAS.1, CH)
    );
    let (nx, ny) = g.tiles(0);
    assert_eq!(store.valid_count(0), (nx * ny) as usize);
    let t = g.tile as usize;
    let mut buf = vec![0.0f32; CH as usize * t * t];
    for ty in 0..ny {
        for tx in 0..nx {
            assert!(store.read_tile(0, tx, ty, &mut buf));
            for c in 0..CH as usize {
                for y in 0..t {
                    for x in 0..t {
                        let (cx, cy) = (tx as usize * t + x, ty as usize * t + y);
                        let a = buf[(c * t + y) * t + x];
                        let b = if cx < full.w && cy < full.h {
                            full.data[(c * full.h + cy) * full.w + cx]
                        } else {
                            0.0
                        };
                        assert!((a - b).abs() < TOL, "({cx},{cy}) ch {c}: {a} vs {b}");
                    }
                }
            }
        }
    }

    // Reopened from disk, the mosaic sees the same groups.
    let reopened = Mosaic::open(&m.dir).unwrap();
    assert_eq!(reopened.config, m.config);
    assert_eq!(reopened.groups().unwrap(), groups);
}

#[test]
fn grouping_off_uses_single_group() {
    let tmp = TempDir::new("single");
    let paths = filter_group(&tmp.0, "Ha", 7);
    let mut m = Mosaic::create(&tmp.0.join("m"), MosaicConfig::new(paths)).unwrap();
    let groups = analyze_all(&mut m, &JobCtl::none()).unwrap();
    assert_eq!(
        groups.groups.keys().cloned().collect::<Vec<_>>(),
        [SINGLE_GROUP]
    );
    assert_eq!(groups.groups[SINGLE_GROUP].len(), 4);
    assert!(
        m.dir
            .join(GROUPS_DIR)
            .join(SINGLE_GROUP)
            .join("session.json")
            .is_file()
    );
}

#[test]
fn override_only_stales_its_group() {
    let tmp = TempDir::new("override");
    let mut m = two_group_mosaic(&tmp.0);
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    blend_group(&mut m, "OIII", None, &JobCtl::none()).unwrap();
    let (ha, oiii) = (m.result_path("Ha"), m.result_path("OIII"));
    let oiii_before = valid0(&oiii);
    assert!(valid0(&ha) > 0 && oiii_before > 0);

    let o = GroupOverride {
        blend: BlendOverride {
            mode: Some(BlendMode::Feather),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        set_group_override(&mut m, "Ha", o.clone()).unwrap(),
        ChangeImpact::Reblend
    );
    assert_eq!(valid0(&ha), 0);
    assert_eq!(valid0(&oiii), oiii_before);
    assert_eq!(MosaicConfig::load(&m.dir).unwrap().group_overrides["Ha"], o);
    assert_eq!(m.config.settings_for("Ha").1.mode, BlendMode::Feather);

    // Reblending reuses the store; an empty override removes the entry.
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    assert!(valid0(&ha) > 0);
    assert_eq!(
        set_group_override(&mut m, "Ha", GroupOverride::default()).unwrap(),
        ChangeImpact::Reblend
    );
    assert!(m.config.group_overrides.is_empty());
    assert!(
        MosaicConfig::load(&m.dir)
            .unwrap()
            .group_overrides
            .is_empty()
    );
    assert_eq!(valid0(&ha), 0);
    assert_eq!(valid0(&oiii), oiii_before);
}

#[test]
fn blend_settings_change_stales_all() {
    let tmp = TempDir::new("blendset");
    let mut m = two_group_mosaic(&tmp.0);
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    blend_group(&mut m, "OIII", None, &JobCtl::none()).unwrap();

    let same = m.config.blend.clone();
    assert_eq!(
        set_blend_settings(&mut m, same).unwrap(),
        ChangeImpact::None
    );
    assert!(valid0(&m.result_path("Ha")) > 0);

    let new = BlendSettings {
        feather_px: m.config.blend.feather_px + 10.0,
        ..m.config.blend.clone()
    };
    assert_eq!(
        set_blend_settings(&mut m, new.clone()).unwrap(),
        ChangeImpact::Reblend
    );
    assert_eq!(valid0(&m.result_path("Ha")), 0);
    assert_eq!(valid0(&m.result_path("OIII")), 0);
    assert_eq!(MosaicConfig::load(&m.dir).unwrap().blend, new);
}

#[test]
fn provenance_records_runs() {
    let tmp = TempDir::new("prov");
    let mut m = two_group_mosaic(&tmp.0);
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    blend_group(&mut m, "OIII", Some([10, 10, 100, 100]), &JobCtl::none()).unwrap();
    let cfg = MosaicConfig::load(&m.dir).unwrap();
    let actions: Vec<_> = cfg.provenance.iter().map(|p| p.action.as_str()).collect();
    assert_eq!(actions, ["analyze", "blend"]);
    for p in &cfg.provenance {
        assert_eq!(p.mmm_version, env!("CARGO_PKG_VERSION"));
    }
    let analyze = &cfg.provenance[0];
    assert!(analyze.detail["groups"]["Ha"].is_object());
    assert!(analyze.detail["groups"]["OIII"].is_object());
    let blend = &cfg.provenance[1];
    assert_eq!(blend.group.as_deref(), Some("OIII"));
    assert_eq!(blend.detail["group"], "OIII");
    assert_eq!(blend.detail["rect"], serde_json::json!([0, 0, 256, 256]));
    assert!(blend.detail["settings"].is_object());
}

#[test]
fn cancel_analyze_all_is_cancelled() {
    let tmp = TempDir::new("cancel");
    let mut m = two_group_mosaic(&tmp.0);
    let token = CancelToken::new();
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let obs = |s: &str, _: u64, _: u64| {
        stages.lock().unwrap().push(s.to_string());
        if s.starts_with("OIII: ") {
            token.cancel();
        }
    };
    let ctl = JobCtl::new(token.clone(), Some(&obs));
    let err = analyze_all(&mut m, &ctl).unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    let stages = stages.into_inner().unwrap();
    assert!(stages.iter().any(|s| s == "Ha: analyze"), "{stages:?}");
    assert!(
        stages
            .iter()
            .all(|s| s.starts_with("Ha: ") || s.starts_with("OIII: "))
    );
    assert!(m.dir.join(GROUPS_DIR).join("Ha/session.json").is_file());
    assert!(!m.dir.join(GROUPS_DIR).join("OIII/session.json").exists());
    assert!(MosaicConfig::load(&m.dir).unwrap().provenance.is_empty());
}

/// Every level-0 tile of the store at `path` is valid and the geometry is
/// the canvas.
fn assert_fully_valid(path: &Path) {
    let store = TileStore::open(path).unwrap();
    let g = store.geometry();
    assert_eq!(
        (g.width, g.height, g.channels as u64),
        (CANVAS.0, CANVAS.1, CH)
    );
    let (nx, ny) = g.tiles(0);
    assert_eq!(store.valid_count(0), (nx * ny) as usize);
}

#[test]
fn blend_group_replaces_mismatched_or_corrupt_store() {
    let tmp = TempDir::new("replace");
    let mut m = two_group_mosaic(&tmp.0);
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    let path = m.result_path("Ha");

    // A store of another geometry (dropped before blending).
    let wrong = TileGeometry {
        width: 100,
        height: 80,
        channels: 1,
        tile: TILE,
    };
    drop(TileStore::create(&path, wrong).unwrap());
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    assert_fully_valid(&path);

    // A corrupt, short file.
    std::fs::write(&path, b"not a tile store").unwrap();
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    assert_fully_valid(&path);
}

#[test]
fn group_dirs_stay_stable_when_names_collide() {
    let tmp = TempDir::new("dirs");
    let mut cfg = MosaicConfig::new(vec!["a.xisf".into()]);
    cfg.grouping.enabled = true;
    cfg.panels[0].manual_group = Some("Ha_3nm".into());
    let mut m = Mosaic::create(&tmp.0.join("m"), cfg).unwrap();
    let groups = m.groups().unwrap();
    assert!(m.assign_group_dirs(&groups));
    assert!(!m.assign_group_dirs(&groups));
    let first = m.group_session_dir("Ha_3nm");
    assert_eq!(first, m.dir.join(GROUPS_DIR).join("Ha_3nm"));

    // "Ha 3nm" sanitises to the same name and sorts first, yet the existing
    // assignment keeps its directory.
    m.config.panels.push(mmm_core::mosaic::PanelEntry {
        path: "b.xisf".into(),
        fingerprint: None,
        manual_group: Some("Ha 3nm".into()),
    });
    let groups = m.groups().unwrap();
    assert!(m.assign_group_dirs(&groups));
    assert_eq!(m.group_session_dir("Ha_3nm"), first);
    let second = m.group_session_dir("Ha 3nm");
    assert_ne!(second, first);
    assert_eq!(second, m.dir.join(GROUPS_DIR).join("Ha_3nm-2"));

    // Persisted: a reopened mosaic maps both the same way.
    m.save().unwrap();
    let reopened = Mosaic::open(&m.dir).unwrap();
    assert_eq!(reopened.group_session_dir("Ha_3nm"), first);
    assert_eq!(reopened.group_session_dir("Ha 3nm"), second);
}

#[test]
fn analyze_settings_change_blocks_blend_until_reanalyzed() {
    let tmp = TempDir::new("anaset");
    let mut m = two_group_mosaic(&tmp.0);
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    blend_group(&mut m, "OIII", None, &JobCtl::none()).unwrap();

    let same = m.config.analyze.clone();
    assert_eq!(
        set_analyze_settings(&mut m, same).unwrap(),
        ChangeImpact::None
    );
    assert!(m.config.needs_analyze.is_empty());

    let mut new = m.config.analyze.clone();
    new.surface = Some(1);
    assert_eq!(
        set_analyze_settings(&mut m, new.clone()).unwrap(),
        ChangeImpact::Reanalyze
    );
    assert_eq!(valid0(&m.result_path("Ha")), 0);
    assert_eq!(valid0(&m.result_path("OIII")), 0);
    let saved = MosaicConfig::load(&m.dir).unwrap();
    assert_eq!(saved.analyze, new);
    assert_eq!(
        saved.needs_analyze.iter().cloned().collect::<Vec<_>>(),
        ["Ha", "OIII"]
    );

    let err = blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap_err();
    assert!(err.to_string().contains("analyze first"), "{err}");

    analyze_all(&mut m, &JobCtl::none()).unwrap();
    assert!(m.config.needs_analyze.is_empty());
    assert!(MosaicConfig::load(&m.dir).unwrap().needs_analyze.is_empty());
    blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap();
    assert_fully_valid(&m.result_path("Ha"));
}

#[test]
fn analyze_override_blocks_only_its_group() {
    let tmp = TempDir::new("anaover");
    let mut m = two_group_mosaic(&tmp.0);
    analyze_all(&mut m, &JobCtl::none()).unwrap();
    let o = GroupOverride {
        analyze: AnalyzeOverride {
            surface: Some(Some(1)),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        set_group_override(&mut m, "Ha", o).unwrap(),
        ChangeImpact::Reanalyze
    );
    assert!(m.config.needs_analyze.contains("Ha"));
    assert!(!m.config.needs_analyze.contains("OIII"));
    let err = blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap_err();
    assert!(err.to_string().contains("analyze first"), "{err}");
    blend_group(&mut m, "OIII", None, &JobCtl::none()).unwrap();
}

#[test]
fn blend_before_analyze_is_refused() {
    let tmp = TempDir::new("noana");
    let mut m = two_group_mosaic(&tmp.0);
    let err = blend_group(&mut m, "Ha", None, &JobCtl::none()).unwrap_err();
    assert!(err.to_string().contains("analyze first"), "{err}");
}
