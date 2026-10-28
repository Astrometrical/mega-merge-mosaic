//! Golden serde form of the value types that cross the PixInsight host ↔
//! worker IPC (`FitsKeyword`, `XisfProperty`, `PropertyValue`).
//!
//! These types move to `mpp-core` (formats consolidation, Phase 3); the C++
//! host and older module builds depend on their JSON shape, so it must not
//! change. mpp pins the same fixture byte-for-byte.
//!
//! Regenerate deliberately with `MMM_BLESS=1 cargo test --test wire_golden`.

use std::path::PathBuf;

use mmm_core::formats::{FitsKeyword, PropertyValue, XisfProperty};

#[derive(serde::Serialize)]
struct WireSample {
    keywords: Vec<FitsKeyword>,
    properties: Vec<XisfProperty>,
}

fn kw(name: &str, value: &str, comment: &str) -> FitsKeyword {
    FitsKeyword {
        name: name.into(),
        value: value.into(),
        comment: comment.into(),
    }
}

fn prop(id: &str, type_: &str, value: PropertyValue, location: Option<(u64, u64)>) -> XisfProperty {
    XisfProperty {
        id: id.into(),
        type_: type_.into(),
        value,
        location,
    }
}

fn sample() -> WireSample {
    WireSample {
        keywords: vec![
            kw("OBJECT", "'M42'", "target"),
            kw("CRVAL1", "84.199", ""),
            kw("COMMENT", "", "free text"),
        ],
        properties: vec![
            prop(
                "Instrument:Filter:Name",
                "String",
                PropertyValue::Str("Ha".into()),
                None,
            ),
            prop("Test:F64", "Float64", PropertyValue::F64(-0.5), None),
            prop("Test:I64", "Int32", PropertyValue::I64(-7), None),
            prop(
                "Test:Vector",
                "F64Vector",
                PropertyValue::F64Vec(vec![1.0, 2.5e-7, -3.0]),
                Some((4096, 24)),
            ),
            prop(
                "Test:Matrix",
                "F64Matrix",
                PropertyValue::F64Mat {
                    rows: 2,
                    cols: 2,
                    data: vec![1.0, 0.0, 0.0, -1.0],
                },
                None,
            ),
            prop("Test:Complex", "C64Vector", PropertyValue::Unread, None),
        ],
    }
}

#[test]
fn wire_golden_matches_fixture() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wire_golden.json");
    let json = serde_json::to_string_pretty(&sample()).unwrap();
    if std::env::var("MMM_BLESS").as_deref() == Ok("1") {
        std::fs::write(&path, format!("{json}\n")).unwrap();
    }
    let golden = std::fs::read_to_string(&path).expect("fixture missing: run with MMM_BLESS=1");
    assert_eq!(json, golden.trim_end(), "wire serde form changed");
}
