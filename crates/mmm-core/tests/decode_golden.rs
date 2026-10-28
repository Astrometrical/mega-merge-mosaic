//! Golden decode of synthesized XISF/FITS panels through the public read
//! path (`InputPanel` metadata + `PanelReader` rows).
//!
//! The formats consolidation (Phase 3) swaps the readers underneath for
//! `mpp-core`'s; this pins what mmm sees today so the swap is provably
//! behaviour-preserving. Regenerate deliberately with
//! `MMM_BLESS=1 cargo test --test decode_golden`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mmm_core::formats::{FitsKeyword, InputPanel};
use mmm_core::panel_reader::PanelReader;
use mmm_core::synth::{write_fits, write_xisf, write_xisf_with_header_xml};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Decoded {
    width: u64,
    height: u64,
    channels: u64,
    keywords: Vec<[String; 2]>,
    property_ids: Vec<String>,
    pixel_fnv: String,
}

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("mmm-decode-golden-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn kw(name: &str, value: &str) -> FitsKeyword {
    FitsKeyword {
        name: name.into(),
        value: value.into(),
        comment: String::new(),
    }
}

/// Deterministic ramp in [0, 1] with per-channel offsets.
fn ramp(w: u64, h: u64, ch: u64) -> Vec<f32> {
    let n = (w * h * ch) as usize;
    (0..n).map(|i| ((i * 37) % 101) as f32 / 100.0).collect()
}

/// FNV-1a 64 over the little-endian bytes of every decoded sample, channel
/// then row order; a missing row hashes as `width` zeros.
fn decode(path: &Path) -> (Decoded, Vec<f32>) {
    let panel = InputPanel::open(path).unwrap();
    let reader = PanelReader::open_file(path).unwrap();
    let (w, h, ch) = (panel.width(), panel.height(), panel.channels());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut all = Vec::new();
    let zeros = vec![0f32; w as usize];
    for c in 0..ch {
        for y in 0..h {
            let row = match reader.row(c, y) {
                Some((0, r)) => r,
                Some((x0, _)) => panic!("unexpected row offset {x0}"),
                None => &zeros,
            };
            for v in row {
                for b in v.to_le_bytes() {
                    hash ^= b as u64;
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
                all.push(*v);
            }
        }
    }
    let d = Decoded {
        width: w,
        height: h,
        channels: ch,
        keywords: panel
            .fits_keywords()
            .iter()
            .map(|k| [k.name.clone(), k.value.clone()])
            .collect(),
        property_ids: panel.properties().iter().map(|p| p.id.clone()).collect(),
        pixel_fnv: format!("{hash:016x}"),
    };
    (d, all)
}

#[test]
fn decode_golden_matches_fixture() {
    let dir = tmpdir();
    let mut cases: BTreeMap<String, Decoded> = BTreeMap::new();

    let p = dir.join("xisf_rgb.xisf");
    write_xisf(&p, 7, 5, 3, &ramp(7, 5, 3)).unwrap();
    cases.insert("xisf_rgb".into(), decode(&p).0);

    let p = dir.join("xisf_mono_with_keywords.xisf");
    write_xisf_with_header_xml(
        &p,
        6,
        4,
        1,
        &ramp(6, 4, 1),
        r#"<FITSKeyword name="OBJECT" value="'M42'" comment=""/><FITSKeyword name="EXPTIME" value="30." comment="s"/>"#,
    )
    .unwrap();
    cases.insert("xisf_mono_with_keywords".into(), decode(&p).0);

    let p = dir.join("fits_f32_bottomup.fits");
    write_fits(&p, 7, 5, 3, &ramp(7, 5, 3), -32, &[]).unwrap();
    cases.insert("fits_f32_bottomup".into(), decode(&p).0);

    let p = dir.join("fits_i16_topdown.fits");
    write_fits(
        &p,
        6,
        4,
        1,
        &ramp(6, 4, 1),
        16,
        &[kw("ROWORDER", "'TOP-DOWN'")],
    )
    .unwrap();
    cases.insert("fits_i16_topdown".into(), decode(&p).0);

    let p = dir.join("fits_u8.fits");
    write_fits(&p, 5, 3, 1, &ramp(5, 3, 1), 8, &[]).unwrap();
    cases.insert("fits_u8".into(), decode(&p).0);

    let p = dir.join("fits_f32_nonfinite.fits");
    let mut planes = ramp(4, 3, 1);
    planes[1] = f32::NAN;
    planes[5] = f32::INFINITY;
    planes[7] = f32::NEG_INFINITY;
    write_fits(&p, 4, 3, 1, &planes, -32, &[]).unwrap();
    let (d, px) = decode(&p);
    assert!(
        px.iter().all(|v| v.is_finite() && *v >= 0.0),
        "non-finite samples must decode to finite non-negative values: {px:?}"
    );
    cases.insert("fits_f32_nonfinite".into(), d);

    let _ = std::fs::remove_dir_all(&dir);

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/decode_golden.json");
    if std::env::var_os("MMM_BLESS").is_some() {
        let json = serde_json::to_string_pretty(&cases).unwrap();
        std::fs::write(&path, format!("{json}\n")).unwrap();
    }
    let golden: BTreeMap<String, Decoded> = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("fixture missing: run with MMM_BLESS=1"),
    )
    .unwrap();
    assert_eq!(cases, golden, "decoded panels changed");
}
