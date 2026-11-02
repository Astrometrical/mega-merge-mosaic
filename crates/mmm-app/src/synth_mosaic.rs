//! A small synthetic two-group (Ha + OIII) mosaic built with mmm-core's
//! `synth` (the approach of `crates/mmm-core/tests/mosaic.rs`), never
//! `test_data/`. Shared by the tests (`testutil`) and the
//! `make_selftest_session` example, so it depends on mmm-core only.

use std::path::{Path, PathBuf};

use mmm_core::mosaic::{GroupRule, Grouping, Mosaic, MosaicConfig};
use mmm_core::synth::{SynthSpec, generate, write_xisf_with_header_xml};

/// Synthetic canvas size.
pub const CANVAS: (u64, u64) = (600, 420);
const CH: u64 = 3;

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

/// A grouped Ha + OIII mosaic session in `root/m.mmm-session`, grouped by
/// the `FILTER` header, not yet analyzed.
pub fn two_group_mosaic(root: &Path) -> Mosaic {
    let mut paths = filter_group(root, "Ha", 7);
    paths.extend(filter_group(root, "OIII", 11));
    let mut cfg = MosaicConfig::new(paths);
    cfg.grouping = Grouping {
        enabled: true,
        rule: GroupRule::Header {
            key: "FILTER".into(),
        },
    };
    Mosaic::create(&root.join("m.mmm-session"), cfg).unwrap()
}
