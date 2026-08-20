//! Aligned-input regression guard for the PanelReader refactor (phase 5, S2).
//!
//! Locks the aligned-input pipeline byte-for-byte: a deterministic synthetic
//! session is analyzed and blended, and FNV-1a hashes of every analyze
//! artifact and of the blended outputs are asserted against constants captured
//! at the commit *before* the refactor (`0e9dbf6`, "feat: full astrometric
//! model with PI spline grid interpolation"). Aligned full-canvas input must
//! remain byte-identical through the PanelReader indirection.

use std::path::PathBuf;

use mmm_core::Result;
use mmm_core::analyze::analyze;
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthSpec, generate};

/// FNV-1a 64-bit over a byte stream (no new deps; stable across platforms).
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Fnv::new();
    h.update(bytes);
    h.finish()
}

/// Sink hashing the exact byte stream of the blended f32 rows (LE), plus the
/// output geometry.
struct HashSink {
    hasher: Fnv,
}

impl HashSink {
    fn new() -> Self {
        Self { hasher: Fnv::new() }
    }
}

impl RowSink for HashSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.hasher.update(&w.to_le_bytes());
        self.hasher.update(&h.to_le_bytes());
        self.hasher.update(&ch.to_le_bytes());
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        self.hasher.update(&y0.to_le_bytes());
        for v in rows {
            self.hasher.update(&v.to_le_bytes());
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Hashes captured at commit 0e9dbf6 (pre-refactor HEAD) by running this very
/// test; the pipeline must reproduce them exactly after the PanelReader
/// refactor.
const META_HASH: u64 = 0x73da_64fc_4fd9_c599;
const SUMMARY_HASHES: [u64; 4] = [
    0x1232_39a6_6f22_d92a,
    0x2d7f_da00_265c_a36d,
    0x7d1b_4516_1330_0112,
    0xd2f3_b0ce_f095_70a1,
];
const GRAPH_HASH: u64 = 0x27e6_9998_d3cc_c51c;
/// Photometry, surfaces and both blends recaptured for the photometric-solve
/// rework (gain-collapse fix): the per-edge fit became a detrended symmetric
/// (Deming) fit with a correlation identifiability guard (plus the new
/// `gain_identifiable` field in photometry.json), and the global solve became
/// a robust L1 (IRLS) log-gain potential solve over cell-count-weighted
/// gain-ratio rows, offsets following linearly — raw second moments are no
/// longer chained. Scan artifacts (META/SUMMARY/GRAPH) remain byte-identical
/// to the original 0e9dbf6 capture. Previous values: photometry
/// 0x65a5_2f09_d3a9_7e4c, surfaces 0x49cc_d828_e18f_3455, feather
/// 0x2e65_c7ff_0be0_c3de.
const PHOTOMETRY_HASH: u64 = 0xafa1_fed4_7c07_4ea8;
const SURFACES_HASH: u64 = 0x0987_e407_21c0_9291;
/// Both blends recaptured for the [0, 1] output clamp (out-of-range samples
/// from this spec's defect/bright stars now clamp; analyze artifacts
/// unchanged; previous values: feather 0x107f_46aa_73a4_e0c8, pyramid
/// 0xa247_5289_0a4b_0861).
const BLEND_FEATHER_HASH: u64 = 0xc3d6_5fa8_266b_c0cd;
/// Recaptured twice, deliberately:
/// - M42-staircase fix (`seam::split_mask_components`): this spec's dense
///   star field floods into filled mask components that classify as
///   extended structure, whose detail transitions ramp (±32 px) instead of
///   snapping (original 0e9dbf6 value: 0xdd24_79de_b5b5_3fb7, then
///   0x6a78_dfa4_bb05_6c08).
/// - Dark-moat fix (`blend::tests::bright_star_halo_leaves_no_dark_moat`):
///   base exclusion reverted to the FULL star mask — structure components
///   (e.g. reflection halos) must not enter the pyramid base, whose
///   level-dependent mask weights stop their Laplacian lobes cancelling —
///   and the detail mix gained the ±32 px coverage-rim fade that keeps the
///   ramped transition continuous where a seam rides a panel rim. Base
///   content away from ramp zones is back to the phase-4 behaviour. All
///   analyze artifacts and the Feather blend remain byte-identical to the
///   original 0e9dbf6 capture throughout.
/// - Dark-streak fix (`pyramid::mask_pyramid` validity clamp): ownership
///   masks no longer carry weight where the panel's downsampled validity
///   vanishes, so a partner's sentinel/extrapolated base can't bleed into a
///   panel's deep single-coverage zone (previous value:
///   0x514b_6e63_ed0d_f737). Base values change only near coverage edges
///   and in bled zones; Feather stays byte-identical.
/// - Photometric-solve rework (see PHOTOMETRY_HASH above): gains/offsets
///   shift on this spec, moving every blended byte (previous value:
///   0x1860_7b7e_788f_918d).
/// - Masked-base detail reference (M42 4-corner fix): deep inside (and in a
///   feather-scaled halo around) masked complexes thicker than the fill's
///   local layers, the detail band references the *blended* base instead of
///   each panel's own fill — long-range fills are fiction and their
///   disagreement used to print onto the output — and the onion fill's
///   long-range passes propagate only background-level values. This spec's
///   flooded mask components form such deep zones, moving the Pyramid
///   output; Feather and TwoBand (and all analyze artifacts) remain
///   byte-identical (previous value: 0xd416_4a6b_faa5_b9d8).
const BLEND_PYRAMID_HASH: u64 = 0x2393_cb56_692a_fd72;

#[test]
fn aligned_pipeline_is_byte_identical_to_pre_refactor_head() {
    let dir = std::env::temp_dir().join(format!("mmm-regguard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Deterministic synthetic mosaic exercising gains/offsets, per-panel
    // gradients, star shifts + spikes (seam/star-mask paths), and a defect
    // (veto path).
    let spec = SynthSpec {
        canvas: (512, 384),
        channels: 3,
        grid: (2, 2),
        overlap_frac: 0.25,
        n_stars: 40,
        noise_sigma: 0.002,
        panel_gain_range: (0.7, 1.4),
        panel_offset_range: (-0.01, 0.02),
        panel_gradient_range: (-0.004, 0.004),
        global_gradient: (0.0, 0.0, 0.0),
        panel_shift: vec![(0.0, 0.0), (0.4, -0.3), (-0.2, 0.5), (0.3, 0.3)],
        panel_spike_angle: vec![0.0, 0.01, -0.01, 0.02],
        panel_defects: vec![(1, 300, 120, 12, 0.5)],
        mid_blobs: 6,
        shift_blobs: false,
        core: None,
        seed: 42,
    };
    let res = generate(&spec, &dir.join("panels")).unwrap();
    let session = analyze(&res.panel_paths, &dir.join("s.mmm-session")).unwrap();

    // Panel metadata (paths excluded — the temp dir varies): canonical text.
    let mut meta_txt = String::new();
    for p in &session.panels {
        meta_txt.push_str(&format!(
            "{} {:?} {} {:?} {:?} {:?}\n",
            p.id, p.bbox, p.nonzero_frac, p.ch_min, p.ch_max, p.ch_mean
        ));
    }
    let meta_hash = fnv(meta_txt.as_bytes());

    let file_hash = |path: PathBuf| fnv(&std::fs::read(&path).unwrap());
    let summary_hashes: Vec<u64> = (0..4)
        .map(|id| file_hash(session.summary_path(id)))
        .collect();
    let graph_hash = file_hash(session.overlap_graph_path());
    let phot_hash = file_hash(session.photometry_path());
    let surf_hash = file_hash(session.surfaces_path());

    let phot = Photometry::load(&session.photometry_path()).unwrap();
    let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
    let surf = Surfaces::load(&session.surfaces_path()).unwrap();

    let mut blend_hashes = Vec::new();
    for mode in [BlendMode::Feather, BlendMode::Pyramid] {
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = HashSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
        blend_hashes.push(sink.hasher.finish());
    }

    eprintln!("META_HASH: {meta_hash:#018x}");
    eprintln!("SUMMARY_HASHES: {summary_hashes:#018x?}");
    eprintln!("GRAPH_HASH: {graph_hash:#018x}");
    eprintln!("PHOTOMETRY_HASH: {phot_hash:#018x}");
    eprintln!("SURFACES_HASH: {surf_hash:#018x}");
    eprintln!("BLEND_FEATHER_HASH: {:#018x}", blend_hashes[0]);
    eprintln!("BLEND_PYRAMID_HASH: {:#018x}", blend_hashes[1]);

    assert_eq!(meta_hash, META_HASH, "panel metadata changed");
    assert_eq!(summary_hashes, SUMMARY_HASHES, "L8 summaries changed");
    assert_eq!(graph_hash, GRAPH_HASH, "overlap graph changed");
    assert_eq!(phot_hash, PHOTOMETRY_HASH, "photometry changed");
    assert_eq!(surf_hash, SURFACES_HASH, "surfaces changed");
    assert_eq!(
        blend_hashes[0], BLEND_FEATHER_HASH,
        "feather blend output changed"
    );
    assert_eq!(
        blend_hashes[1], BLEND_PYRAMID_HASH,
        "pyramid blend output changed"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
