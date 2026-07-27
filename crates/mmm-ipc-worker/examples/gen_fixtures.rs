//! Deterministic fixture generator for the C++ `test_golden_aligned` and
//! `test_golden_solved` byte-identity tests
//! (`integration/pixinsight/host/test/test_golden_{aligned,solved}.cpp`).
//!
//! Emits, into the directory given as the sole CLI argument, a small aligned
//! two-panel scene in the exact shape as `end_to_end.rs::synth_two_panels`
//! (128×64×1: panel A = 0.2 over x∈[8,80), y∈[8,56); panel B = 0.4 over
//! x∈[48,120), y∈[16,64)):
//!
//! - `panel0.xisf`, `panel1.xisf` — full-canvas XISF frames (via
//!   [`mmm_core::synth::write_xisf`]); the worker reads these in Files mode to
//!   produce the reference blend.
//! - `panel0.bin`, `panel1.bin` — the *same* pixels as raw planar `f32`,
//!   native-endian; the C++ memory `PanelSource` serves these over shm in
//!   Aligned mode.
//! - `meta.json` — `{ "canvas":[w,h,ch], "panels":[{"id","w","h","ch"}...],
//!   "band_rows":16, "feather_px":16.0 }`.
//!
//! The `.xisf` (worker-read) and `.bin` (host-served) copies are written from
//! the same in-memory buffer, so any Aligned-vs-Files divergence the golden
//! test catches is a real transport/serve bug, not a fixture mismatch.
//!
//! Also emits the solved-mode fixtures `test_golden_solved` consumes,
//! mirroring `end_to_end.rs::write_two_solved_panels` exactly (same two
//! panels' geometry/WCS: panel 0 = 64×48, crval [10,0], rot 0°; panel 1 =
//! 60×52, offset crval, rot 6°; scale 1e-3 deg; ch=1):
//!
//! - `solved0.xisf`, `solved1.xisf` — raw (un-registered) plate-solved panels
//!   (via [`mmm_core::synth::write_xisf_solved`]); the worker reprojects these
//!   itself in Files(Solved) mode.
//! - `solved0.bin`, `solved1.bin` — the same pixels as raw planar `f32`,
//!   native-endian, each panel its own w×h; the C++ `MemSource` serves these
//!   over shm in `Solved` mode.
//! - `solved_props.json` — a JSON array of the two panels' `properties`
//!   arrays, read back via `XisfPanel::open(..).header().properties` (exactly
//!   what the worker itself parses) and serialized with serde_json, so the
//!   C++ test can splice each panel's plate solution into its `Init` JSON
//!   without hand-crafting `XisfProperty`/`PropertyValue` shapes.
//! - `solved_meta.json` — `{ "panels":[{"id","w","h","ch"}...], "ch":1,
//!   "band_rows":8, "feather_px":16.0 }`.

use std::path::Path;

use mmm_core::formats::xisf::XisfPanel;
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

/// Canvas dimensions shared by both panels (full-canvas aligned frames).
const W: u64 = 128;
const H: u64 = 64;
const CH: u64 = 1;

/// Fills the rectangle `x∈[x0,x1), y∈[y0,y1)` of a `W×H` single-channel
/// frame with `v`.
fn fill(frame: &mut [f32], v: f32, x0: u64, y0: u64, x1: u64, y1: u64) {
    for y in y0..y1 {
        for x in x0..x1 {
            frame[(y * W + x) as usize] = v;
        }
    }
}

/// Serializes a planar `f32` buffer to raw native-endian bytes.
fn to_ne_bytes(planes: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(planes.len() * 4);
    for &v in planes {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    bytes
}

/// Geometry/WCS spec for one raw solved panel, mirroring
/// `end_to_end.rs::write_two_solved_panels`'s private `Spec`.
struct SolvedSpec {
    w: u64,
    h: u64,
    crval: [f64; 2],
    rot_deg: f64,
}

/// Writes the two raw plate-solved panels `test_golden_solved` consumes,
/// with the *exact* geometry, rotation, sky offset, and deterministic pixel
/// pattern as `end_to_end.rs::write_two_solved_panels` — so the C++ golden
/// test exercises the same reprojection/blend behavior as the Rust e2e test
/// it mirrors. Returns each panel's `(w, h, ch, planar pixels)`.
fn write_two_solved_panels(out_dir: &Path) -> Vec<(u64, u64, u64, Vec<f32>)> {
    let scale_deg = 1.0e-3_f64;
    let ch = 1u64;
    let specs = [
        SolvedSpec {
            w: 64,
            h: 48,
            crval: [10.0, 0.0],
            rot_deg: 0.0,
        },
        SolvedSpec {
            w: 60,
            h: 52,
            // Offset a bit over half of panel 0's width east and a little
            // north, so the two footprints overlap by roughly 40%.
            crval: [10.0 + 64.0 * scale_deg * 0.55, 8.0 * scale_deg],
            rot_deg: 6.0,
        },
    ];

    let mut out = Vec::new();
    for (k, spec) in specs.iter().enumerate() {
        let (sr, cr) = spec.rot_deg.to_radians().sin_cos();
        let cd = [
            [-scale_deg * cr, -scale_deg * sr],
            [-scale_deg * sr, scale_deg * cr],
        ];
        let refimg = [spec.w as f64 / 2.0, spec.h as f64 / 2.0];
        let (w, h) = (spec.w, spec.h);
        let mut planes = vec![0f32; (w * h) as usize];
        for j in 0..h {
            for i in 0..w {
                planes[(j * w + i) as usize] =
                    (((i * 7 + j * 13 + k as u64 * 41) % 97) as f32) / 97.0 + 0.01;
            }
        }
        let wcs = SynthWcs {
            crval: spec.crval,
            refimg,
            cd,
        };
        let path = out_dir.join(format!("solved{k}.xisf"));
        write_xisf_solved(&path, w, h, ch, &planes, &wcs).expect("write_xisf_solved");
        let bin = out_dir.join(format!("solved{k}.bin"));
        std::fs::write(&bin, to_ne_bytes(&planes)).expect("write solved .bin");
        out.push((w, h, ch, planes));
    }
    out
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .expect("usage: gen_fixtures <out_dir>");
    let out_dir = Path::new(&out_dir);
    std::fs::create_dir_all(out_dir).expect("create out_dir");

    // Panel A = 0.2 over x∈[8,80), y∈[8,56).
    let mut a = vec![0f32; (W * H) as usize];
    fill(&mut a, 0.2, 8, 8, 80, 56);

    // Panel B = 0.4 over x∈[48,120), y∈[16,64).
    let mut b = vec![0f32; (W * H) as usize];
    fill(&mut b, 0.4, 48, 16, 120, 64);

    for (idx, planes) in [&a, &b].into_iter().enumerate() {
        let xisf = out_dir.join(format!("panel{idx}.xisf"));
        write_xisf(&xisf, W, H, CH, planes).expect("write_xisf");
        let bin = out_dir.join(format!("panel{idx}.bin"));
        std::fs::write(&bin, to_ne_bytes(planes)).expect("write .bin");
    }

    let meta = serde_json::json!({
        "canvas": [W, H, CH],
        "panels": [
            { "id": 0, "w": W, "h": H, "ch": CH },
            { "id": 1, "w": W, "h": H, "ch": CH },
        ],
        "band_rows": 16,
        "feather_px": 16.0,
    });
    let meta_path = out_dir.join("meta.json");
    std::fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap()).expect("write meta.json");

    // --- Solved-mode fixtures (Task 8). ---
    let solved_panels = write_two_solved_panels(out_dir);

    // Reopen each written panel exactly the way the worker itself does
    // (`XisfPanel::open(..).header().properties`), so `solved_props.json`
    // carries the plate solution the worker will actually parse rather than
    // a hand-maintained copy that could silently drift from the writer.
    let mut props_array = Vec::with_capacity(solved_panels.len());
    let mut solved_meta_panels = Vec::with_capacity(solved_panels.len());
    for (idx, (w, h, ch, _)) in solved_panels.iter().enumerate() {
        let path = out_dir.join(format!("solved{idx}.xisf"));
        let panel = XisfPanel::open(&path).expect("reopen solved panel");
        props_array.push(panel.header().properties.clone());
        solved_meta_panels.push(serde_json::json!({
            "id": idx as u32,
            "w": w,
            "h": h,
            "ch": ch,
        }));
    }
    let props_path = out_dir.join("solved_props.json");
    std::fs::write(
        &props_path,
        serde_json::to_vec_pretty(&props_array).unwrap(),
    )
    .expect("write solved_props.json");

    let solved_ch = solved_panels[0].2;
    let solved_meta = serde_json::json!({
        "panels": solved_meta_panels,
        "ch": solved_ch,
        "band_rows": 8,
        "feather_px": 16.0,
    });
    let solved_meta_path = out_dir.join("solved_meta.json");
    std::fs::write(
        &solved_meta_path,
        serde_json::to_vec_pretty(&solved_meta).unwrap(),
    )
    .expect("write solved_meta.json");

    eprintln!("gen_fixtures: wrote fixtures to {}", out_dir.display());
}
