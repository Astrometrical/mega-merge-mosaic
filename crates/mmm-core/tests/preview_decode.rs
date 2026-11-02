//! Previews of compressed XISF panels decode once into the mosaic's decode
//! cache on disk (own test binary: the decode cache directory is
//! process-wide).

use std::path::PathBuf;
use std::sync::Mutex;

use mmm_core::job::JobCtl;
use mmm_core::mosaic::{Mosaic, MosaicConfig};
use mmm_core::preview::{Preview, plan_previews, run_previews};
use mpp_core::formats::xisf_codec::Codec;
use mpp_core::testkit::{BlockSpec, ImageSpec, XisfBuilder};

#[test]
fn compressed_panels_decode_into_the_session_cache() {
    let dir = std::env::temp_dir().join(format!("mmm-preview-decode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(mmm_core::decoded::cache_dir().is_none());
    let (w, h) = (60u32, 40u32);
    let paths: Vec<PathBuf> = (0..2u32)
        .map(|k| {
            // Panel k covers its own half of the canvas with value 0.25 + k/10.
            let data: Vec<u8> = (0..w * h)
                .flat_map(|i| {
                    let x = i % w;
                    let v = if (x < w / 2) == (k == 0) {
                        0.25 + k as f32 / 10.0
                    } else {
                        0.0
                    };
                    v.to_le_bytes()
                })
                .collect();
            let path = dir.join(format!("p{k}.xisf"));
            XisfBuilder {
                images: vec![ImageSpec {
                    geometry: (w as u64, h as u64, 1),
                    sample_format: "Float32",
                    data,
                    attrs: r#" bounds="0:1" colorSpace="Gray""#.into(),
                    children: String::new(),
                    block: BlockSpec {
                        codec: Some((Codec::Zstd, Some(4), 1 << 20)),
                        ..Default::default()
                    },
                }],
                ..Default::default()
            }
            .write(&path);
            path
        })
        .collect();
    let m = Mosaic::create(&dir.join("mosaic"), MosaicConfig::new(paths.clone())).unwrap();
    let got = Mutex::new(Vec::<Preview>::new());
    let errs = run_previews(&plan_previews(&m), &JobCtl::none(), &|p| {
        got.lock().unwrap().push(p)
    })
    .unwrap();
    assert!(errs.is_empty(), "{errs:?}");
    let decoded = m.dir.join("decoded");
    assert_eq!(mmm_core::decoded::cache_dir(), Some(decoded.clone()));
    let files: Vec<_> = std::fs::read_dir(&decoded)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "f32"))
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    let mut got = got.into_inner().unwrap();
    got.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(got[0].rect, [0.0, 0.0, 30.0, 40.0]);
    assert_eq!(got[1].rect, [30.0, 0.0, 60.0, 40.0]);
    assert!(got[1].data.iter().all(|v| (v - 0.35).abs() < 1e-6));
    std::fs::remove_dir_all(&dir).ok();
}
