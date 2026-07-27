//! Deterministic fixture generator for the C++ `test_golden_aligned` byte-identity
//! test (`integration/pixinsight/host/test/test_golden_aligned.cpp`).
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

use std::path::Path;

use mmm_core::synth::write_xisf;

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

    eprintln!("gen_fixtures: wrote fixtures to {}", out_dir.display());
}
