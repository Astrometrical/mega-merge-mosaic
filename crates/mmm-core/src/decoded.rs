//! Decode cache for XISF panels whose stored block is not readable in place.
//!
//! An uncompressed, planar, little-endian XISF panel is read straight from
//! its own mmap whatever its sample format, range or alignment: zero-copy
//! when it is Float32 in 0:1, else converted to f32 a band at a time
//! (`panel_reader`'s native backing), as a FITS panel is. Any other XISF
//! panel — compressed (WBPP writes compressed masters), big-endian or
//! interleaved — is decoded **once** into the native layout under the cache
//! directory (normally `<session>/decoded/`): planar and little-endian, but
//! in its own sample format, so a UInt16 panel's cache file is its
//! uncompressed size, half an f32 decode. The file is memory-mapped and
//! served like a reprojection cache when it holds Float32 in 0:1 (a WBPP
//! master), else converted on read like the panel itself. Later stages and
//! re-runs find the file and skip the decode.
//!
//! Each decode is announced through the process-wide notifier (the CLI
//! prints a boxed notice on stderr; the PixInsight worker sends it to the
//! PixInsight console) because it costs time and disk: a 2 GB Float32 panel
//! needs a 2 GB cache file.
//!
//! The cache file is `<key>.<format>` (`u16`, `f32`, …), the key over the
//! layout, the source's canonical path, size, modification time, image id
//! and block checksum, so a re-saved panel is decoded afresh and a file
//! 1.7.0 wrote (normalised f32) is never read. Files are written under a
//! unique temporary name and renamed into place, so an interrupted decode
//! never leaves a file that looks valid.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{Mmap, MmapMut};
use mpp_core::formats::SampleFormat;
use mpp_core::formats::xisf::PixelStorage;
use mpp_core::formats::xisf_block::ByteOrder;

use crate::formats::xisf::XisfPanel;
use crate::{Error, Result};

/// One decode about to happen, for the user-facing notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeNotice {
    /// The XISF panel being decoded.
    pub source: PathBuf,
    /// The cache file it is decoded into; `None` when it is decoded into
    /// memory (no cache directory set: a run without a session).
    pub cache: Option<PathBuf>,
    /// Size of the decoded samples in bytes: the native layout, in the
    /// stored sample format.
    pub bytes: u64,
    /// Why the stored block cannot be read in place: its codec
    /// (`zstd+sh compressed`), `big-endian`, `interleaved`, comma-joined.
    pub reason: String,
    /// The XISF sample format, kept by the decode (e.g. `UInt16`).
    pub sample_format: String,
}

impl DecodeNotice {
    /// The one-line notice text shown to the user.
    pub fn message(&self) -> String {
        let name = self.source.file_name().map_or_else(
            || self.source.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        // Plain ASCII: the PixInsight console reads the stage text as Latin-1.
        let (fmt, why) = (&self.sample_format, &self.reason);
        let gb = self.bytes as f64 / 1e9;
        match &self.cache {
            Some(cache) => format!(
                "Decoding {fmt} XISF panel {name} ({why}) to {} ({gb:.2} GB) - decoded once, \
                 re-used after",
                cache.display()
            ),
            None => format!("Decoding {fmt} XISF panel {name} ({why}) into memory ({gb:.2} GB)"),
        }
    }
}

type Notifier = Box<dyn Fn(&DecodeNotice) + Send + Sync>;

static CACHE_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
static NOTIFIER: RwLock<Option<Notifier>> = RwLock::new(None);
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
/// Serializes decodes process-wide: a decode holds a full-size scratch
/// buffer for byte-shuffled codecs (and a big-endian or interleaved
/// block's decompressed bytes) plus the dirtied output pages, so
/// decoding every panel of a mosaic at once (the analyze scan opens them in
/// parallel) would multiply peak memory by the panel count. mpp-core already
/// decodes one block's subblocks in parallel.
static DECODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
static IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static MAX_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Set where decoded panels are cached (normally `<session>/decoded`).
pub fn set_cache_dir(dir: PathBuf) {
    *CACHE_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(dir);
}

/// The cache directory as set by [`set_cache_dir`]; `None` when no session
/// provides one, in which case panels are decoded into memory and nothing is
/// left behind on disk.
pub fn cache_dir() -> Option<PathBuf> {
    CACHE_DIR.read().unwrap_or_else(|e| e.into_inner()).clone()
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

/// The samples of `img` in the native layout (planar, little-endian, the
/// stored sample format: [`XisfPanel::native_len`] bytes), from the
/// process-wide cache directory, decoding them there first if needed. Read
/// them with [`XisfPanel::native_planes_in`].
pub fn decoded_planes(img: &XisfPanel) -> Result<Mmap> {
    decoded_planes_in(img, cache_dir().as_deref(), &notify_global)
}

/// [`decoded_planes`] with an explicit cache directory (`None`: decode into
/// memory) and notifier.
pub fn decoded_planes_in(
    img: &XisfPanel,
    dir: Option<&Path>,
    notify: &dyn Fn(&DecodeNotice),
) -> Result<Mmap> {
    let path = img.path();
    let len = img.native_len();
    let cache = match dir {
        Some(d) => Some(d.join(format!(
            "{}.{}",
            cache_key(img)?,
            extension(img.header().sample_format)
        ))),
        None => None,
    };
    let valid = |c: &Path| std::fs::metadata(c).is_ok_and(|m| m.is_file() && m.len() == len);
    if let Some(c) = &cache
        && valid(c)
    {
        return map(c);
    }
    let _serial = DECODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Another thread may have produced the cache while this one waited.
    if let Some(c) = &cache
        && valid(c)
    {
        return map(c);
    }
    #[cfg(test)]
    {
        let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
        MAX_IN_FLIGHT.fetch_max(now, Ordering::SeqCst);
    }
    let result = decode_locked(img, path, len, cache.as_deref(), dir, notify);
    #[cfg(test)]
    IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    result
}

fn decode_locked(
    img: &XisfPanel,
    path: &Path,
    len: u64,
    cache: Option<&Path>,
    dir: Option<&Path>,
    notify: &dyn Fn(&DecodeNotice),
) -> Result<Mmap> {
    let h = img.header();
    notify(&DecodeNotice {
        source: path.to_path_buf(),
        cache: cache.map(Path::to_path_buf),
        bytes: len,
        reason: [
            h.block
                .compression
                .as_ref()
                .map(|c| format!("{} compressed", c.codec_id())),
            (h.block.byte_order == ByteOrder::Big).then(|| "big-endian".to_string()),
            (h.pixel_storage == PixelStorage::Normal).then(|| "interleaved".to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", "),
        sample_format: format!("{:?}", h.sample_format),
    });
    let (Some(cache), Some(dir)) = (cache, dir) else {
        // No session: decode into anonymous memory, gone when unmapped.
        let n = usize::try_from(len).map_err(|_| Error::format(path, "panel too large"))?;
        let mut out = MmapMut::map_anon(n).map_err(|e| Error::io(path, e))?;
        img.decode_native_into(&mut out[..])?;
        return out.make_read_only().map_err(|e| Error::io(path, e));
    };
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
        reserve(&file, len).map_err(|e| {
            if e.raw_os_error() == Some(libc_enospc()) {
                Error::format(
                    &tmp,
                    format!(
                        "not enough disk space for the decoded panel ({:.2} GB needed)",
                        len as f64 / 1e9
                    ),
                )
            } else {
                Error::io(&tmp, e)
            }
        })?;
        // SAFETY: the file was just created under a unique name; nothing else
        // maps or resizes it while this mapping is alive.
        let mut out = unsafe { MmapMut::map_mut(&file) }.map_err(|e| Error::io(&tmp, e))?;
        img.decode_native_into(&mut out[..])?;
        out.flush().map_err(|e| Error::io(&tmp, e))?;
        drop(out);
        file.sync_all().map_err(|e| Error::io(&tmp, e))?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, cache) {
        // Replacing a cache file another process has mapped can fail
        // (Windows: access denied); that file is identical, so use it.
        let _ = std::fs::remove_file(&tmp);
        if !std::fs::metadata(cache).is_ok_and(|m| m.len() == len) {
            return Err(Error::io(cache, e));
        }
    }
    map(cache)
}

/// Size `file` to `len` bytes with its disk space actually reserved, so a
/// full disk is an error here rather than SIGBUS on a store into the
/// mapping (a plain `set_len` makes a sparse file on unix). Linux & co use
/// `posix_fallocate`, macOS `F_PREALLOCATE` (it has no `posix_fallocate`);
/// filesystems without either fall back to `set_len`. On Windows `set_len`
/// itself allocates.
fn reserve(file: &std::fs::File, len: u64) -> std::io::Result<()> {
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        use std::os::fd::AsRawFd;
        let n = libc::off_t::try_from(len)
            .map_err(|_| std::io::Error::other("cache file too large"))?;
        // SAFETY: a valid open file descriptor; posix_fallocate only reads it.
        match unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, n) } {
            0 => return Ok(()),
            libc::EOPNOTSUPP | libc::EINVAL => {}
            err => return Err(std::io::Error::from_raw_os_error(err)),
        }
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        let n = libc::off_t::try_from(len)
            .map_err(|_| std::io::Error::other("cache file too large"))?;
        let mut store = libc::fstore_t {
            fst_flags: libc::F_ALLOCATEALL,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: n,
            fst_bytesalloc: 0,
        };
        // SAFETY: a valid open file descriptor and a live, initialised
        // fstore_t that fcntl may write `fst_bytesalloc` into.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut store) } == -1 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::ENOTSUP) | Some(libc::EINVAL) => {}
                _ => return Err(err),
            }
        }
        // F_PREALLOCATE reserves blocks but leaves the length; set it below.
    }
    file.set_len(len)
}

fn libc_enospc() -> i32 {
    #[cfg(unix)]
    {
        libc::ENOSPC
    }
    #[cfg(not(unix))]
    {
        112 // ERROR_DISK_FULL
    }
}

/// The cache file's extension: the sample format it holds.
fn extension(f: SampleFormat) -> &'static str {
    match f {
        SampleFormat::UInt8 => "u8",
        SampleFormat::UInt16 => "u16",
        SampleFormat::UInt32 => "u32",
        SampleFormat::Float32 => "f32",
        SampleFormat::Float64 => "f64",
        // Refused by mpp-core's open; never cached.
        _ => "raw",
    }
}

fn map(path: &Path) -> Result<Mmap> {
    let f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    // SAFETY: cache files are only ever replaced by rename, never modified
    // in place.
    unsafe { Mmap::map(&f) }.map_err(|e| Error::io(path, e))
}

/// What a cache file holds, fed to its key: a file written under another
/// rule (1.7.0 cached normalised f32) never matches.
const CACHE_LAYOUT: &[u8] = b"native planar little-endian v1";

/// FNV-1a 64 over [`CACHE_LAYOUT`], the source's canonical path, size,
/// modification time (ns), image id and block checksum attribute, as 16 hex
/// digits.
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
    feed(CACHE_LAYOUT);
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
    fn notice_text_names_file_format_reason_cache_and_size() {
        let mut n = DecodeNotice {
            source: "/data/masterLight_PANEL-4.xisf".into(),
            cache: Some("/s.mmm-session/decoded/0123456789abcdef.f32".into()),
            bytes: 2_033_508_600,
            reason: "zstd+sh compressed".into(),
            sample_format: "Float32".into(),
        };
        // Plain ASCII: the PixInsight console reads the stage text as Latin-1.
        assert_eq!(
            n.message(),
            "Decoding Float32 XISF panel masterLight_PANEL-4.xisf (zstd+sh compressed) to \
             /s.mmm-session/decoded/0123456789abcdef.f32 (2.03 GB) - decoded once, re-used after"
        );
        assert!(n.message().is_ascii());
        n.cache = None;
        assert_eq!(
            n.message(),
            "Decoding Float32 XISF panel masterLight_PANEL-4.xisf (zstd+sh compressed) into \
             memory (2.03 GB)"
        );
    }

    /// A zstd+sh compressed UInt16 RGB panel at `path`; its samples.
    fn write_uint16(path: &Path) -> Vec<u16> {
        let v: Vec<u16> = (0..(W * H * C) as u32)
            .map(|i| (i * 7919 % 65536) as u16)
            .collect();
        XisfBuilder {
            images: vec![ImageSpec {
                geometry: (W, H, C),
                sample_format: "UInt16",
                data: v.iter().flat_map(|s| s.to_le_bytes()).collect(),
                attrs: r#" colorSpace="RGB""#.into(),
                children: String::new(),
                block: BlockSpec {
                    codec: Some((Codec::Zstd, Some(2), 1 << 20)),
                    ..Default::default()
                },
            }],
            ..Default::default()
        }
        .write(path);
        v
    }

    #[test]
    fn a_uint16_panel_is_cached_in_its_own_sample_format() {
        let dir = tmpdir("uint16");
        let path = dir.join("u16.xisf");
        let v = write_uint16(&path);
        let img = XisfPanel::open(&path).unwrap();
        let seen = std::sync::Mutex::new(None);
        let m = decoded_planes_in(&img, Some(&dir.join("cache")), &|n| {
            *seen.lock().unwrap() = Some(n.clone())
        })
        .unwrap();
        let n = seen.lock().unwrap().clone().expect("announced");
        assert_eq!(n.sample_format, "UInt16");
        assert_eq!(n.reason, "zstd+sh compressed");
        assert_eq!(n.bytes, W * H * C * 2, "two bytes a sample, not four");
        assert_eq!(n.cache.unwrap().extension().unwrap(), "u16");
        let stored: Vec<u16> = m
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect();
        assert_eq!(stored, v, "the stored samples, unconverted");
        let mut row = vec![0f32; W as usize];
        img.native_planes_in(&m)
            .unwrap()
            .decode_rows(2, H - 1, 1, &mut row);
        for (x, got) in row.iter().enumerate() {
            let want = v[((2 * H + H - 1) * W) as usize + x] as f32 / 65535.0;
            assert!((got - want).abs() < 1e-6, "x {x}: {got} vs {want}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 1.7.0's cache key: [`cache_key`]'s fields with no layout tag, frozen.
    fn first_scheme_key(img: &XisfPanel) -> String {
        let canon = std::fs::canonicalize(img.path()).unwrap();
        let meta = std::fs::metadata(img.path()).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
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
            hash ^= 0xff;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        feed(canon.as_os_str().as_encoded_bytes());
        feed(&meta.len().to_le_bytes());
        feed(&mtime.to_le_bytes());
        feed(h.image_id.as_deref().unwrap_or("").as_bytes());
        feed(checksum.as_bytes());
        format!("{hash:016x}")
    }

    #[test]
    fn a_first_scheme_cache_file_is_never_read() {
        // 1.7.0 cached normalised f32 as `<key>.f32`. A Float32 panel whose
        // bounds are not 0:1 now caches its stored samples under the same
        // extension and length, so only the key tells the two apart.
        let dir = tmpdir("first");
        let path = dir.join("wide.xisf");
        let stored = pixels(0);
        XisfBuilder {
            images: vec![ImageSpec {
                geometry: (W, H, C),
                sample_format: "Float32",
                data: stored.iter().flat_map(|v| v.to_le_bytes()).collect(),
                attrs: r#" bounds="0:2" colorSpace="RGB""#.into(),
                children: String::new(),
                block: BlockSpec {
                    codec: Some((Codec::Zstd, Some(4), 1 << 20)),
                    ..Default::default()
                },
            }],
            ..Default::default()
        }
        .write(&path);
        let img = XisfPanel::open(&path).unwrap();
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let normalised: Vec<f32> = stored.iter().map(|v| v / 2.0).collect();
        std::fs::write(
            cache.join(format!("{}.f32", first_scheme_key(&img))),
            bytemuck::cast_slice::<f32, u8>(&normalised),
        )
        .unwrap();
        let m = decoded_planes_in(&img, Some(&cache), &|_| {}).unwrap();
        assert_eq!(floats(&m), stored, "the stored samples, not 1.7.0's file");
        std::fs::remove_dir_all(&dir).unwrap();
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
        let a = floats(&decoded_planes_in(&img, Some(&dir.join("cache")), &note).unwrap());
        let b = floats(&decoded_planes_in(&img, Some(&dir.join("cache")), &note).unwrap());
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
            floats(&decoded_planes_in(&img, Some(&cache), &|_| {}).unwrap()),
            pixels(0)
        );
        // Unmap the source first: Windows refuses to rewrite a mapped file.
        drop(img);
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
            &decoded_planes_in(&img, Some(&cache), &|_| {
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
        drop(decoded_planes_in(&img, Some(&cache), &|_| {}).unwrap());
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
            floats(&decoded_planes_in(&img, Some(&cache), &|_| {}).unwrap()),
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
                        floats(&decoded_planes_in(&img, Some(&cache), &|_| {}).unwrap())
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

    #[test]
    fn no_cache_dir_decodes_into_memory() {
        let dir = tmpdir("memory");
        let (cmp, _) = write_pair(&dir, 5);
        let img = XisfPanel::open(&cmp).unwrap();
        let seen = std::sync::Mutex::new(None);
        let m =
            decoded_planes_in(&img, None, &|n| *seen.lock().unwrap() = Some(n.clone())).unwrap();
        assert_eq!(floats(&m), pixels(5));
        let n = seen.lock().unwrap().clone().expect("announced");
        assert_eq!(n.cache, None);
        assert_eq!(n.reason, "zstd+sh compressed");
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            left.len(),
            2,
            "nothing written besides the two inputs: {left:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decodes_run_one_at_a_time() {
        let dir = tmpdir("serial");
        let paths: Vec<PathBuf> = (0..4u32)
            .map(|k| {
                let sub = dir.join(format!("p{k}"));
                std::fs::create_dir_all(&sub).unwrap();
                write_pair(&sub, k).0
            })
            .collect();
        std::thread::scope(|s| {
            for p in &paths {
                s.spawn(|| {
                    let img = XisfPanel::open(p).unwrap();
                    decoded_planes_in(&img, None, &|_| {}).unwrap();
                });
            }
        });
        assert_eq!(
            MAX_IN_FLIGHT.load(Ordering::Relaxed),
            1,
            "decodes overlapped"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
