//! Golden decode of synthesized XISF/FITS panels through the public read
//! path (`InputPanel` metadata + `PanelReader` rows).
//!
//! The formats consolidation (Phase 3) swaps the readers underneath for
//! `mpp-core`'s; this pins what mmm sees today so the swap is provably
//! behaviour-preserving. Regenerate deliberately with
//! `MMM_BLESS=1 cargo test --test decode_golden`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mmm_core::formats::{FitsKeyword, InputPanel, XisfProperty};
use mmm_core::panel_reader::PanelReader;
use mmm_core::synth::{write_fits, write_xisf, write_xisf_with_header_xml};

#[derive(Debug, serde::Serialize)]
struct Decoded {
    width: u64,
    height: u64,
    channels: u64,
    keywords: Vec<FitsKeyword>,
    properties: Vec<XisfProperty>,
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
        keywords: panel.fits_keywords().to_vec(),
        properties: panel.properties().to_vec(),
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

    // One property of each kind the reader distinguishes. The F64Matrix
    // attachment deliberately points at the start of the pixel block
    // (offset 4096): overlapping, but a valid read that pins attachment
    // decoding.
    let p = dir.join("xisf_properties.xisf");
    write_xisf_with_header_xml(
        &p,
        4,
        2,
        1,
        &ramp(4, 2, 1),
        concat!(
            r#"<Property id="Test:String" type="String">hello world</Property>"#,
            r#"<Property id="Test:F64" type="Float64" value="-0.5"/>"#,
            r#"<Property id="Test:I32" type="Int32" value="42"/>"#,
            r#"<Property id="Test:Vec" type="F64Vector" length="2" location="inline:base64">AAAAAAAA+D8AAAAAAAAAwA==</Property>"#,
            r#"<Property id="Test:Mat" type="F64Matrix" rows="2" columns="2" location="attachment:4096:32"/>"#,
            r#"<Property id="Test:Complex" type="Complex64" value="(1,2)"/>"#,
            r#"<Property id="Test:C64Vec" type="C64Vector" length="1" location="inline:base64">AAAAAAAA8D8AAAAAAAAAQA==</Property>"#,
        ),
    )
    .unwrap();
    cases.insert("xisf_properties".into(), decode(&p).0);

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
    let json = serde_json::to_string_pretty(&cases).unwrap();
    if std::env::var("MMM_BLESS").as_deref() == Ok("1") {
        std::fs::write(&path, format!("{json}\n")).unwrap();
    }
    let golden = std::fs::read_to_string(&path).expect("fixture missing: run with MMM_BLESS=1");
    assert_eq!(json, golden.trim_end(), "decoded panels changed");
}
