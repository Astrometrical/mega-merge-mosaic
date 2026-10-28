//! Decode cache for XISF panels that cannot be served zero-copy.
//!
//! An uncompressed full-canvas Float32 XISF panel is read straight from its
//! own mmap. Any other XISF panel — compressed (WBPP writes compressed
//! masters), integer, big-endian, interleaved — is decoded **once** into a
//! raw planar f32 file under the cache directory (normally
//! `<session>/decoded/`), which is then memory-mapped and served exactly
//! like a reprojection cache (`panel_reader`'s `Backing::Cache`). Later
//! stages and re-runs find the file and skip the decode.
//!
//! Each decode is announced through the process-wide notifier (the CLI
//! prints a boxed notice on stderr; the PixInsight worker sends it to the
//! PixInsight console) because it costs time and disk: a 2 GB panel needs a
//! 2 GB cache file.
//!
//! The cache file name is a key over the source's canonical path, size,
//! modification time and block checksum, so a re-saved panel is decoded
//! afresh. Files are written under a unique temporary name and renamed into
//! place, so an interrupted decode never leaves a file that looks valid.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{Mmap, MmapMut};

use crate::formats::xisf::XisfPanel;
use crate::{Error, Result};

/// One decode about to happen, for the user-facing notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeNotice {
    /// The XISF panel being decoded.
    pub source: PathBuf,
    /// The cache file it is decoded into.
    pub cache: PathBuf,
    /// Size of the cache file in bytes.
    pub bytes: u64,
    /// The block's codec (e.g. `zstd+sh`), or `uncompressed`.
    pub codec: String,
}

impl DecodeNotice {
    /// The one-line notice text shown to the user.
    pub fn message(&self) -> String {
        let name = self.source.file_name().map_or_else(
            || self.source.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        format!(
            "Decoding {} XISF panel {name} to {} ({:.2} GB) — decoded once, re-used after",
            if self.codec == "uncompressed" {
                "non-mappable".to_string()
            } else {
                format!("compressed ({})", self.codec)
            },
            self.cache.display(),
            self.bytes as f64 / 1e9
        )
    }
}

type Notifier = Box<dyn Fn(&DecodeNotice) + Send + Sync>;

static CACHE_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
static NOTIFIER: RwLock<Option<Notifier>> = RwLock::new(None);
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Set where decoded panels are cached (normally `<session>/decoded`).
pub fn set_cache_dir(dir: PathBuf) {
    *CACHE_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(dir);
}

/// The cache directory: as set by [`set_cache_dir`], else
/// `<temp dir>/mmm-decoded`.
pub fn cache_dir() -> PathBuf {
    CACHE_DIR
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("mmm-decoded"))
}

/// Install the process-wide decode notifier (default: a `tracing` warning).
pub fn set_notifier(f: Box<dyn Fn(&DecodeNotice) + Send + Sync>) {
    *NOTIFIER.write().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

fn notify_global(n: &DecodeNotice) {
    match NOTIFIER.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(f) => f(n),
        None => tracing::warn!("{}", n.message()),
    }
}

/// The decoded planes of `img` (planar f32, `w·h·c` samples), from the
/// process-wide cache directory, decoding them there first if needed.
pub fn decoded_planes(img: &XisfPanel) -> Result<Mmap> {
    decoded_planes_in(img, &cache_dir(), &notify_global)
}

/// [`decoded_planes`] with an explicit cache directory and notifier.
pub fn decoded_planes_in(
    img: &XisfPanel,
    dir: &Path,
    notify: &dyn Fn(&DecodeNotice),
) -> Result<Mmap> {
    let path = img.path();
    let len = img.width() * img.height() * img.channels() * 4;
    let cache = dir.join(format!("{}.f32", cache_key(img)?));
    if std::fs::metadata(&cache).is_ok_and(|m| m.is_file() && m.len() == len) {
        return map(&cache);
    }
    let h = img.header();
    notify(&DecodeNotice {
        source: path.to_path_buf(),
        cache: cache.clone(),
        bytes: len,
        codec: h
            .block
            .compression
            .as_ref()
            .map_or_else(|| "uncompressed".to_string(), |c| c.codec_id()),
    });
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let tmp = dir.join(format!(
        "{}.{}.{}.tmp",
        cache.file_stem().unwrap().to_string_lossy(),
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> Result<()> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| Error::io(&tmp, e))?;
        file.set_len(len).map_err(|e| Error::io(&tmp, e))?;
        // SAFETY: the file was just created under a unique name; nothing else
        // maps or resizes it while this mapping is alive.
        let mut out = unsafe { MmapMut::map_mut(&file) }.map_err(|e| Error::io(&tmp, e))?;
        img.decode_into(bytemuck::cast_slice_mut(&mut out[..]))?;
        out.flush().map_err(|e| Error::io(&tmp, e))?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &cache) {
        // Another process may have won the race (Windows refuses to rename
        // over an existing file); its file is identical.
        let _ = std::fs::remove_file(&tmp);
        if !std::fs::metadata(&cache).is_ok_and(|m| m.len() == len) {
            return Err(Error::io(&cache, e));
        }
    }
    map(&cache)
}

fn map(path: &Path) -> Result<Mmap> {
    let f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    // SAFETY: cache files are only ever replaced by rename, never modified
    // in place.
    unsafe { Mmap::map(&f) }.map_err(|e| Error::io(path, e))
}

/// FNV-1a 64 over the source's canonical path, size, modification time
/// (ns), image id and block checksum attribute, as 16 hex digits.
fn cache_key(img: &XisfPanel) -> Result<String> {
    let path = img.path();
    let canon = std::fs::canonicalize(path).map_err(|e| Error::io(path, e))?;
    let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let h = img.header();
    let checksum = h
        .block
        .checksum
        .as_ref()
        .map(|c| format!("{}:{:02x?}", c.algorithm.name(), c.digest))
        .unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Field separator, so concatenations cannot collide.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    feed(canon.as_os_str().as_encoded_bytes());
    feed(&meta.len().to_le_bytes());
    feed(&mtime.to_le_bytes());
    feed(h.image_id.as_deref().unwrap_or("").as_bytes());
    feed(checksum.as_bytes());
    Ok(format!("{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpp_core::formats::xisf_codec::Codec;
    use mpp_core::testkit::{BlockSpec, ImageSpec, XisfBuilder};
    use std::sync::atomic::AtomicUsize;

    const W: u64 = 40;
    const H: u64 = 30;
    const C: u64 = 3;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-decoded-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pixels(seed: u32) -> Vec<f32> {
        (0..(W * H * C) as u32)
            .map(|i| ((i / 4 + seed) % 50) as f32 / 50.0)
            .collect()
    }

    /// A zstd+sh compressed Float32 RGB panel, and its uncompressed twin.
    fn write_pair(dir: &Path, seed: u32) -> (PathBuf, PathBuf) {
        let data: Vec<u8> = pixels(seed).iter().flat_map(|v| v.to_le_bytes()).collect();
        let spec = |block: BlockSpec| XisfBuilder {
            images: vec![ImageSpec {
                geometry: (W, H, C),
                sample_format: "Float32",
                data: data.clone(),
                attrs: r#" bounds="0:1" colorSpace="RGB""#.into(),
                children: String::new(),
                block,
            }],
            ..Default::default()
        };
        let (cmp, raw) = (dir.join("cmp.xisf"), dir.join("raw.xisf"));
        spec(BlockSpec {
            codec: Some((Codec::Zstd, Some(4), 1 << 20)),
            ..Default::default()
        })
        .write(&cmp);
        spec(BlockSpec::default()).write(&raw);
        (cmp, raw)
    }

    fn floats(m: &Mmap) -> Vec<f32> {
        bytemuck::cast_slice::<u8, f32>(&m[..]).to_vec()
    }

    #[test]
    fn notice_text_names_file_cache_and_size() {
        let n = DecodeNotice {
            source: "/data/masterLight_PANEL-4.xisf".into(),
            cache: "/s.mmm-session/decoded/0123456789abcdef.f32".into(),
            bytes: 2_033_508_600,
            codec: "zstd+sh".into(),
        };
        assert_eq!(
            n.message(),
            "Decoding compressed (zstd+sh) XISF panel masterLight_PANEL-4.xisf to \
             /s.mmm-session/decoded/0123456789abcdef.f32 (2.03 GB) — decoded once, re-used after"
        );
    }

    #[test]
    fn cache_is_reused_and_notified_once() {
        let dir = tmpdir("reuse");
        let (cmp, _) = write_pair(&dir, 0);
        let count = AtomicUsize::new(0);
        let note = |_: &DecodeNotice| {
            count.fetch_add(1, Ordering::Relaxed);
        };
        let img = XisfPanel::open(&cmp).unwrap();
        let a = floats(&decoded_planes_in(&img, &dir.join("cache"), &note).unwrap());
        let b = floats(&decoded_planes_in(&img, &dir.join("cache"), &note).unwrap());
        assert_eq!(a, pixels(0));
        assert_eq!(a, b);
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "second open reuses the cache"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_cache_is_not_reused() {
        let dir = tmpdir("stale");
        let (cmp, _) = write_pair(&dir, 0);
        let cache = dir.join("cache");
        let img = XisfPanel::open(&cmp).unwrap();
        assert_eq!(
            floats(&decoded_planes_in(&img, &cache, &|_| {}).unwrap()),
            pixels(0)
        );
        // Re-save with different pixels (same size) and a later mtime.
        write_pair(&dir, 7);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&cmp)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let count = AtomicUsize::new(0);
        let img = XisfPanel::open(&cmp).unwrap();
        let got = floats(
            &decoded_planes_in(&img, &cache, &|_| {
                count.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap(),
        );
        assert_eq!(got, pixels(7));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn truncated_cache_is_rebuilt() {
        let dir = tmpdir("trunc");
        let (cmp, _) = write_pair(&dir, 0);
        let cache = dir.join("cache");
        let img = XisfPanel::open(&cmp).unwrap();
        drop(decoded_planes_in(&img, &cache, &|_| {}).unwrap());
        let file = std::fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(100)
            .unwrap();
        std::fs::write(cache.join("leftover.123.0.tmp"), b"junk").unwrap();
        assert_eq!(
            floats(&decoded_planes_in(&img, &cache, &|_| {}).unwrap()),
            pixels(0)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parallel_decodes_do_not_collide() {
        let dir = tmpdir("parallel");
        let (cmp, _) = write_pair(&dir, 3);
        let cache = dir.join("cache");
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        let img = XisfPanel::open(&cmp).unwrap();
                        floats(&decoded_planes_in(&img, &cache, &|_| {}).unwrap())
                    })
                })
                .collect();
            for h in handles {
                assert_eq!(h.join().unwrap(), pixels(3));
            }
        });
        let leftovers: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temporary files left: {leftovers:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
