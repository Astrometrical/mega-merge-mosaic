//! Memory-mapped tiled result store: the blended mosaic as a multi-level tile
//! pyramid with a per-tile sequence lock and validity bit.
//!
//! A background blend writes tiles through a [`TileStore`] while a viewer
//! reads them through one or more [`TileReader`]s — separate mappings of the
//! same file, sharing the OS page cache, possibly on different threads. The
//! storage layer knows nothing about how tiles are produced; [`blend_tiles`]
//! (with [`TileSink`] and [`build_parents`]) fills it from a blend.
//!
//! # File format (version 1)
//!
//! All integers and samples are little endian.
//!
//! 1. **Header**, 4096 bytes: magic `b"MMMTILE\0"`, `u32` version (`1`),
//!    `u32` tile size, `u64` width, `u64` height, `u32` channels, `u32`
//!    level count; the rest is zero padding.
//! 2. **Tile state table**: one `u32` per tile, level 0 first, each level
//!    row-major. Bit 31 is the *valid* bit; bits 0–30 are the tile's
//!    sequence number (odd = being written, even = stable), wrapping within
//!    31 bits. A zeroed entry is "never written, invalid". Zero-padded to a
//!    multiple of 4096 bytes.
//! 3. **Tile data**, level by level, each level row-major: every tile is
//!    `channels × tile × tile` `f32` samples, planar (channel 0's rows, then
//!    channel 1's, ...). Edge tiles are stored full size; samples beyond the
//!    image edge are zero ([`TileStore::write_tile`] zeroes them whatever the
//!    caller passed).
//!
//! **Levels**: level 0 is full size; level `l` is `ceil(w / 2^l) ×
//! ceil(h / 2^l)`; levels are added until both dimensions fit in one tile.
//!
//! # Torn-read protection
//!
//! Each tile's state entry is a seqlock, accessed as [`AtomicU32`] through
//! the mapping. The (single) writer bumps the sequence to odd and clears the
//! valid bit (Release), writes the samples, then bumps the sequence to even
//! and sets the valid bit (Release). Invalidation clears the valid bit
//! without touching the samples. A reader loads the entry (Relaxed, then an
//! Acquire fence: read-only mappings only support Relaxed loads) — it
//! must be even and valid — copies the samples, then (after an Acquire
//! fence) loads the entry again; the copy counts only if the second load
//! equals the first. Because every write and invalidation changes the entry,
//! a copy that straddles any of them, including a complete rewrite, is
//! rejected (barring exactly 2^30 rewrites during one copy). The writing
//! value is forced odd and the final value even, so an entry left odd by a
//! crash mid-write is simply invalid and the next write recovers it.
//!
//! `open`/`create` reject geometries whose layout would overflow (tile edge
//! over 65536, a tile grid wider than `u32`, or a file size beyond `isize`).
//!
//! Samples are themselves read and written as relaxed 32-bit atomics, so a
//! racing copy is a well-defined (if possibly stale) read, never UB.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering, fence};

use memmap2::{Mmap, MmapMut};

use crate::error::{Error, Result};

mod sink;
pub use sink::{TileSink, blend_tiles, build_parents};

/// The default tile edge, in pixels.
pub const TILE: u32 = 256;

const MAGIC: &[u8; 8] = b"MMMTILE\0";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 4096;
const PAGE: usize = 4096;
/// Valid bit of a tile state entry.
const VALID: u32 = 1 << 31;
/// Sequence bits of a tile state entry.
const SEQ: u32 = !VALID;
/// Largest supported tile edge.
const MAX_TILE: u32 = 65536;

/// Shape of a tile pyramid: the level-0 image size, channel count, and tile
/// edge. All rectangles are exclusive `[x0, y0, x1, y1]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileGeometry {
    /// Level-0 image width in pixels.
    pub width: u64,
    /// Level-0 image height in pixels.
    pub height: u64,
    /// Samples per pixel (planar within a tile).
    pub channels: u32,
    /// Tile edge in pixels (tiles are square).
    pub tile: u32,
}

impl TileGeometry {
    /// Number of pyramid levels: level 0 plus halvings until both dimensions
    /// fit in one tile.
    pub fn levels(&self) -> u32 {
        let t = self.tile as u64;
        let (mut w, mut h, mut n) = (self.width, self.height, 1);
        while w > t || h > t {
            w = w.div_ceil(2);
            h = h.div_ceil(2);
            n += 1;
        }
        n
    }

    /// Image size at `level`: `ceil(w / 2^level) × ceil(h / 2^level)`.
    pub fn level_dims(&self, level: u32) -> (u64, u64) {
        (ceil_shr(self.width, level), ceil_shr(self.height, level))
    }

    /// Tile grid size `(columns, rows)` at `level`.
    pub fn tiles(&self, level: u32) -> (u32, u32) {
        let (w, h) = self.level_dims(level);
        let t = self.tile as u64;
        (w.div_ceil(t) as u32, h.div_ceil(t) as u32)
    }

    /// Expand `r` outward to level-0 tile boundaries, then clamp to the
    /// canvas.
    pub fn align_rect(&self, r: [u64; 4]) -> [u64; 4] {
        let t = self.tile as u64;
        [
            (r[0] / t * t).min(self.width),
            (r[1] / t * t).min(self.height),
            (r[2].div_ceil(t) * t).min(self.width),
            (r[3].div_ceil(t) * t).min(self.height),
        ]
    }

    /// Tiles `(tx, ty)` at `level` that intersect `r` (given in level-0
    /// coordinates), row-major. Empty if `r` is empty or off the canvas.
    pub fn tiles_in_rect(&self, level: u32, r: [u64; 4]) -> Vec<(u32, u32)> {
        let (w, h) = self.level_dims(level);
        let x0 = (r[0] >> level).min(w);
        let y0 = (r[1] >> level).min(h);
        let x1 = ceil_shr(r[2], level).min(w);
        let y1 = ceil_shr(r[3], level).min(h);
        if x0 >= x1 || y0 >= y1 {
            return Vec::new();
        }
        let t = self.tile as u64;
        let (tx0, tx1) = ((x0 / t) as u32, x1.div_ceil(t) as u32);
        let (ty0, ty1) = ((y0 / t) as u32, y1.div_ceil(t) as u32);
        (ty0..ty1)
            .flat_map(|ty| (tx0..tx1).map(move |tx| (tx, ty)))
            .collect()
    }

    /// Samples in one tile (`channels × tile × tile`).
    fn tile_len(&self) -> usize {
        self.channels as usize * self.tile as usize * self.tile as usize
    }
}

fn ceil_shr(v: u64, s: u32) -> u64 {
    if s >= 64 {
        return (v > 0) as u64;
    }
    (v >> s) + ((v & ((1u64 << s) - 1)) != 0) as u64
}

/// Byte layout of a store file, derived from its geometry.
#[derive(Clone, Debug)]
struct Layout {
    g: TileGeometry,
    levels: u32,
    /// Global index of each level's first tile.
    level_start: Vec<usize>,
    total_tiles: usize,
    data_off: usize,
    file_len: usize,
}

impl Layout {
    /// Validate `g` and derive its layout; `Err` explains an unsupported
    /// geometry (zero sizes, a tile edge over `MAX_TILE`, a tile grid that
    /// does not fit `u32`, or a file size that overflows `usize`).
    fn new(g: TileGeometry) -> std::result::Result<Self, String> {
        if g.tile == 0 || g.width == 0 || g.height == 0 || g.channels == 0 {
            return Err("zero tile size, dimension, or channel count".into());
        }
        if g.tile > MAX_TILE {
            return Err(format!("tile size {} exceeds {MAX_TILE}", g.tile));
        }
        let t = g.tile as u64;
        // Level 0 has the largest grid; later levels only shrink.
        if g.width.div_ceil(t) > u32::MAX as u64 || g.height.div_ceil(t) > u32::MAX as u64 {
            return Err("tile grid does not fit in u32".into());
        }
        let overflow = || "geometry too large for this platform".to_string();
        let tile_bytes = (g.channels as usize)
            .checked_mul(g.tile as usize * g.tile as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(overflow)?;
        let levels = g.levels();
        let mut level_start = Vec::with_capacity(levels as usize);
        let mut total = 0usize;
        for l in 0..levels {
            level_start.push(total);
            let (tx, ty) = g.tiles(l);
            total = (tx as usize)
                .checked_mul(ty as usize)
                .and_then(|n| n.checked_add(total))
                .ok_or_else(overflow)?;
        }
        let table = total
            .checked_mul(4)
            .and_then(|n| n.checked_next_multiple_of(PAGE))
            .ok_or_else(overflow)?;
        let data_off = HEADER_LEN.checked_add(table).ok_or_else(overflow)?;
        let file_len = total
            .checked_mul(tile_bytes)
            .and_then(|n| n.checked_add(data_off))
            .filter(|&n| n <= isize::MAX as usize)
            .ok_or_else(overflow)?;
        Ok(Layout {
            g,
            levels,
            level_start,
            total_tiles: total,
            data_off,
            file_len,
        })
    }

    /// Global tile index; panics on an out-of-range level or tile.
    fn index(&self, level: u32, tx: u32, ty: u32) -> usize {
        assert!(level < self.levels, "tile level {level} out of range");
        let (nx, ny) = self.g.tiles(level);
        assert!(
            tx < nx && ty < ny,
            "tile ({tx},{ty}) out of range at level {level}"
        );
        self.level_start[level as usize] + ty as usize * nx as usize + tx as usize
    }

    fn header(&self) -> [u8; 40] {
        let mut h = [0u8; 40];
        h[0..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&VERSION.to_le_bytes());
        h[12..16].copy_from_slice(&self.g.tile.to_le_bytes());
        h[16..24].copy_from_slice(&self.g.width.to_le_bytes());
        h[24..32].copy_from_slice(&self.g.height.to_le_bytes());
        h[32..36].copy_from_slice(&self.g.channels.to_le_bytes());
        h[36..40].copy_from_slice(&self.levels.to_le_bytes());
        h
    }

    /// Parse and validate a mapped file.
    fn parse(path: &Path, bytes: &[u8]) -> Result<Self> {
        let bad = |why: &str| Error::format(path, format!("tile store: {why}"));
        if bytes.len() < HEADER_LEN {
            return Err(bad("file shorter than its header"));
        }
        if &bytes[0..8] != MAGIC {
            return Err(bad("bad magic"));
        }
        let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
        let version = u32_at(8);
        if version != VERSION {
            return Err(bad(&format!("unsupported version {version}")));
        }
        let g = TileGeometry {
            tile: u32_at(12),
            width: u64_at(16),
            height: u64_at(24),
            channels: u32_at(32),
        };
        let layout = Layout::new(g).map_err(|why| bad(&why))?;
        if u32_at(36) != layout.levels {
            return Err(bad("level count does not match geometry"));
        }
        if bytes.len() != layout.file_len {
            return Err(bad(&format!(
                "file is {} bytes, geometry needs {}",
                bytes.len(),
                layout.file_len
            )));
        }
        Ok(layout)
    }
}

/// Raw view over a mapped store: base pointer plus layout. Shared by the
/// writer and reader so both use one access protocol.
struct Raw {
    base: *mut u8,
    layout: Layout,
}

impl Raw {
    fn state(&self, idx: usize) -> &AtomicU32 {
        debug_assert!(idx < self.layout.total_tiles);
        // SAFETY: `idx < total_tiles`, so the 4 bytes at `HEADER_LEN + 4 idx`
        // lie inside the state table of the mapping, which outlives `self`
        // (the owning struct keeps the map alive). The offset is a multiple
        // of 4 from a page-aligned base, so the AtomicU32 is aligned, and
        // every access to these bytes goes through atomics.
        unsafe { &*(self.base.add(HEADER_LEN + idx * 4) as *const AtomicU32) }
    }

    fn samples(&self, idx: usize) -> &[AtomicU32] {
        let n = self.layout.g.tile_len();
        let off = self.layout.data_off + idx * n * 4;
        // SAFETY: `off + 4n <= file_len` (Layout::new computes file_len with
        // checked arithmetic, parse checks the file is exactly that long and
        // create sizes it so), `off` is a multiple of 4 from a page-aligned base, so
        // the AtomicU32s are aligned and in bounds; the mapping outlives
        // `self`. All sample accesses go through atomics.
        unsafe { std::slice::from_raw_parts(self.base.add(off) as *const AtomicU32, n) }
    }

    fn is_valid(&self, idx: usize) -> bool {
        let e = self.state(idx).load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        stable_valid(e)
    }

    fn read_tile(&self, idx: usize, out: &mut [f32]) -> bool {
        self.read_tile_with(idx, out, || {})
    }

    /// [`Self::read_tile`] with `mid` run between the copy and the
    /// re-check (a test hook for simulating a racing writer).
    fn read_tile_with(&self, idx: usize, out: &mut [f32], mid: impl FnOnce()) -> bool {
        let n = self.layout.g.tile_len();
        assert_eq!(
            out.len(),
            n,
            "tile buffer must hold channels × tile × tile samples"
        );
        let state = self.state(idx);
        // Relaxed load + Acquire fence rather than an Acquire load: std only
        // guarantees Relaxed atomic loads on read-only (reader) mappings.
        let first = state.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        if !stable_valid(first) {
            return false;
        }
        for (o, s) in out.iter_mut().zip(self.samples(idx)) {
            *o = f32::from_bits(u32::from_le(s.load(Ordering::Relaxed)));
        }
        mid();
        // Keep the sample loads above from sinking below the re-check.
        fence(Ordering::Acquire);
        state.load(Ordering::Relaxed) == first
    }

    /// Clear tile `idx`'s valid bit (writer side only).
    fn invalidate(&self, idx: usize) {
        let state = self.state(idx);
        let e = state.load(Ordering::Relaxed);
        if e & VALID != 0 {
            state.store(e & SEQ, Ordering::Release);
        }
    }
}

/// True if a state entry is stable (even sequence) and valid.
fn stable_valid(e: u32) -> bool {
    e & VALID != 0 && e & 1 == 0
}

fn map_err(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::io(path, e)
}

/// Read-write tile store (the blend side).
pub struct TileStore {
    path: PathBuf,
    raw: Raw,
    // Keeps the mapping that `raw.base` points into alive.
    map: MmapMut,
}

// SAFETY: `raw.base` points into `map`, which moves with the struct (the
// mapped memory itself never moves). All shared-memory access goes through
// atomics, and mutation requires `&mut self`.
unsafe impl Send for TileStore {}
// SAFETY: as above; `&self` methods only perform atomic loads.
unsafe impl Sync for TileStore {}

impl TileStore {
    /// Create (or truncate) a store at `path` for geometry `g`, with every
    /// tile invalid. The file is sized with `set_len` (sparse where the
    /// filesystem supports it).
    ///
    /// Callers must not recreate a store that readers still have mapped:
    /// truncating the file under a live mapping makes their accesses fault
    /// (`SIGBUS` on Unix). Drop every [`TileReader`] first.
    pub fn create(path: &Path, g: TileGeometry) -> Result<Self> {
        let layout = Layout::new(g).map_err(|why| Error::compute(format!("tile store: {why}")))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(map_err(path))?;
        file.set_len(layout.file_len as u64)
            .map_err(map_err(path))?;
        let mut store = Self::map(path, &file, layout)?;
        let header = store.raw.layout.header();
        store.map[..header.len()].copy_from_slice(&header);
        Ok(store)
    }

    /// Open an existing store read-write, validating its header and size.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(map_err(path))?;
        // SAFETY: see `map`; the read-only map is dropped before remapping.
        let probe = unsafe { Mmap::map(&file) }.map_err(map_err(path))?;
        let layout = Layout::parse(path, &probe)?;
        drop(probe);
        Self::map(path, &file, layout)
    }

    fn map(path: &Path, file: &File, layout: Layout) -> Result<Self> {
        // SAFETY: the file is our tile store; other processes/threads may map
        // it too, but every access to shared bytes is atomic (see the module
        // docs) and nobody resizes it while mapped.
        let mut map = unsafe { MmapMut::map_mut(file) }.map_err(map_err(path))?;
        let raw = Raw {
            base: map.as_mut_ptr(),
            layout,
        };
        Ok(TileStore {
            path: path.to_path_buf(),
            raw,
            map,
        })
    }

    /// The store's geometry.
    pub fn geometry(&self) -> TileGeometry {
        self.raw.layout.g
    }

    /// True if tile `(tx, ty)` at `level` holds complete data.
    ///
    /// # Panics
    /// If the level or tile is out of range.
    pub fn is_valid(&self, level: u32, tx: u32, ty: u32) -> bool {
        self.raw.is_valid(self.raw.layout.index(level, tx, ty))
    }

    /// Write tile `(tx, ty)` at `level` from `data` (`channels × tile × tile`
    /// planar samples) and mark it valid. Samples beyond the image edge at
    /// that level are stored as zero regardless of `data`.
    ///
    /// # Panics
    /// If the level or tile is out of range.
    pub fn write_tile(&mut self, level: u32, tx: u32, ty: u32, data: &[f32]) -> Result<()> {
        let g = self.raw.layout.g;
        if data.len() != g.tile_len() {
            return Err(Error::compute(format!(
                "tile data has {} samples, expected {}",
                data.len(),
                g.tile_len()
            )));
        }
        let idx = self.raw.layout.index(level, tx, ty);
        let t = g.tile as u64;
        let (lw, lh) = g.level_dims(level);
        let vw = (lw - tx as u64 * t).min(t) as usize;
        let vh = (lh - ty as u64 * t).min(t) as usize;
        let t = t as usize;

        // Single writer (`&mut self`), so load-then-store cannot race.
        let state = self.raw.state(idx);
        let seq = state.load(Ordering::Relaxed) & SEQ;
        // Always odd while writing, always even after, even if a crash
        // mid-write left the persisted sequence odd.
        let writing = (seq.wrapping_add(1) | 1) & SEQ;
        state.store(writing, Ordering::Release);
        // Keep the sample stores below from rising above the odd store.
        fence(Ordering::Release);
        let samples = self.raw.samples(idx);
        for (i, (s, &v)) in samples.iter().zip(data).enumerate() {
            let x = i % t;
            let y = (i / t) % t;
            let v = if x < vw && y < vh { v } else { 0.0 };
            s.store(v.to_bits().to_le(), Ordering::Relaxed);
        }
        state.store((writing.wrapping_add(1) & SEQ) | VALID, Ordering::Release);
        Ok(())
    }

    /// Copy tile `(tx, ty)` at `level` into `out`; false (contents of `out`
    /// unspecified) if the tile is invalid or was rewritten during the copy.
    ///
    /// # Panics
    /// If the level or tile is out of range, or `out` is not one tile long.
    pub fn read_tile(&self, level: u32, tx: u32, ty: u32, out: &mut [f32]) -> bool {
        self.raw
            .read_tile(self.raw.layout.index(level, tx, ty), out)
    }

    /// Invalidate, at every level, the tiles covering `r` (level-0 coords).
    pub fn invalidate_rect(&mut self, r: [u64; 4]) {
        let g = self.raw.layout.g;
        for level in 0..self.raw.layout.levels {
            for (tx, ty) in g.tiles_in_rect(level, r) {
                self.raw.invalidate(self.raw.layout.index(level, tx, ty));
            }
        }
    }

    /// Invalidate every tile at every level.
    pub fn invalidate_all(&mut self) {
        for idx in 0..self.raw.layout.total_tiles {
            self.raw.invalidate(idx);
        }
    }

    /// Number of valid tiles at `level`.
    ///
    /// # Panics
    /// If the level is out of range.
    pub fn valid_count(&self, level: u32) -> usize {
        let (nx, ny) = self.raw.layout.g.tiles(level);
        let start = self.raw.layout.index(level, 0, 0);
        (start..start + nx as usize * ny as usize)
            .filter(|&i| self.raw.is_valid(i))
            .count()
    }

    /// Flush the mapping to disk.
    pub fn flush(&self) -> Result<()> {
        self.map.flush().map_err(map_err(&self.path))
    }
}

/// Read-only view of a tile store (the viewer side), mapped separately from
/// the writer.
pub struct TileReader {
    raw: Raw,
    // Keeps the mapping that `raw.base` points into alive.
    _map: Mmap,
}

// SAFETY: `raw.base` points into `_map`, which lives as long as the struct
// and whose memory never moves; the reader only performs Relaxed atomic
// loads (read-only memory permits nothing else), so
// sharing or sending it across threads cannot race in the Rust model.
unsafe impl Send for TileReader {}
// SAFETY: as above.
unsafe impl Sync for TileReader {}

impl TileReader {
    /// Open the store at `path` read-only, validating its header and size.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(map_err(path))?;
        // SAFETY: a writer may modify the file concurrently through its own
        // mapping; this side reads the shared bytes only through Relaxed
        // atomic loads (each followed by an Acquire fence where ordering is
        // needed — the only atomic access std supports on read-only memory),
        // and the file is never resized while mapped.
        let map = unsafe { Mmap::map(&file) }.map_err(map_err(path))?;
        let layout = Layout::parse(path, &map)?;
        let raw = Raw {
            base: map.as_ptr() as *mut u8,
            layout,
        };
        Ok(TileReader { raw, _map: map })
    }

    /// The store's geometry.
    pub fn geometry(&self) -> TileGeometry {
        self.raw.layout.g
    }

    /// Copy tile `(tx, ty)` at `level` into `out`; false (contents of `out`
    /// unspecified) if the tile is invalid or was rewritten during the copy.
    ///
    /// # Panics
    /// If the level or tile is out of range, or `out` is not one tile long.
    pub fn read_tile(&self, level: u32, tx: u32, ty: u32, out: &mut [f32]) -> bool {
        self.raw
            .read_tile(self.raw.layout.index(level, tx, ty), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-tiles-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("tiles.mmmtile")
    }

    fn geom() -> TileGeometry {
        TileGeometry {
            width: 1000,
            height: 600,
            channels: 3,
            tile: TILE,
        }
    }

    fn filled(g: TileGeometry, v: f32) -> Vec<f32> {
        vec![v; g.tile_len()]
    }

    #[test]
    fn geometry_levels() {
        let g = geom();
        assert_eq!(g.levels(), 3);
        assert_eq!(g.level_dims(0), (1000, 600));
        assert_eq!(g.level_dims(1), (500, 300));
        assert_eq!(g.level_dims(2), (250, 150));
        assert_eq!(g.tiles(0), (4, 3));
    }

    #[test]
    fn align_rect_expands_and_clamps() {
        let g = geom();
        assert_eq!(g.align_rect([300, 10, 700, 20]), [256, 0, 768, 256]);
        // y0 = 500 lies in tile row 1 (256..512), so it expands to 256.
        assert_eq!(g.align_rect([900, 500, 1000, 600]), [768, 256, 1000, 600]);
        assert_eq!(g.align_rect([900, 520, 1000, 600]), [768, 512, 1000, 600]);
    }

    #[test]
    fn create_open_roundtrip() {
        let p = tmp("roundtrip");
        let g = geom();
        // Tile (1,2) is in the bottom tile row: only rows 0..88 are inside
        // the image, so the rest is stored as zero.
        let t = TILE as usize;
        let data: Vec<f32> = (0..g.tile_len())
            .map(|i| {
                if (i / t) % t < 88 {
                    i as f32 * 0.5
                } else {
                    0.0
                }
            })
            .collect();
        {
            let mut s = TileStore::create(&p, g).unwrap();
            assert_eq!(s.valid_count(0), 0);
            s.write_tile(0, 1, 2, &data).unwrap();
            s.flush().unwrap();
        }
        let s = TileStore::open(&p).unwrap();
        assert_eq!(s.geometry(), g);
        let mut out = filled(g, -1.0);
        assert!(s.read_tile(0, 1, 2, &mut out));
        assert!(out == data, "tile contents differ");
        assert_eq!(s.valid_count(0), 1);
        for l in 0..g.levels() {
            let (nx, ny) = g.tiles(l);
            for ty in 0..ny {
                for tx in 0..nx {
                    if (l, tx, ty) != (0, 1, 2) {
                        assert!(!s.is_valid(l, tx, ty), "({l},{tx},{ty}) should be invalid");
                    }
                }
            }
        }
        let r = TileReader::open(&p).unwrap();
        assert_eq!(r.geometry(), g);
        assert!(r.read_tile(0, 1, 2, &mut out));
        assert!(out == data, "tile contents differ");
        assert!(!r.read_tile(0, 0, 0, &mut out));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn edge_tiles_are_zero_padded() {
        let p = tmp("edge");
        let g = geom();
        let mut s = TileStore::create(&p, g).unwrap();
        // Tile (3,2) covers x 768..1024, y 512..768 of a 1000×600 image:
        // 232 columns and 88 rows are inside it.
        s.write_tile(0, 3, 2, &filled(g, 7.0)).unwrap();
        let mut out = filled(g, -1.0);
        assert!(s.read_tile(0, 3, 2, &mut out));
        let t = TILE as usize;
        for c in 0..3 {
            for y in 0..t {
                for x in 0..t {
                    let v = out[(c * t + y) * t + x];
                    let want = if x < 232 && y < 88 { 7.0 } else { 0.0 };
                    assert_eq!(v, want, "c{c} x{x} y{y}");
                }
            }
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn invalidate_rect_clears_all_levels() {
        let p = tmp("invalidate");
        let g = geom();
        let mut s = TileStore::create(&p, g).unwrap();
        let data = filled(g, 1.0);
        for l in 0..g.levels() {
            let (nx, ny) = g.tiles(l);
            for ty in 0..ny {
                for tx in 0..nx {
                    s.write_tile(l, tx, ty, &data).unwrap();
                }
            }
        }
        s.invalidate_rect([0, 0, 10, 10]);
        for l in 0..g.levels() {
            assert!(!s.is_valid(l, 0, 0), "level {l}");
        }
        assert!(s.is_valid(0, 1, 0));
        assert_eq!(s.valid_count(0), 11);
        s.invalidate_all();
        for l in 0..g.levels() {
            assert_eq!(s.valid_count(l), 0);
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reader_sees_invalid_while_tile_rewritten() {
        let p = tmp("race");
        let g = geom();
        let mut s = TileStore::create(&p, g).unwrap();
        s.write_tile(0, 1, 1, &filled(g, 1.0)).unwrap();
        let r = TileReader::open(&p).unwrap();
        let mut out = filled(g, 0.0);
        assert!(r.read_tile(0, 1, 1, &mut out));
        assert!(out.iter().all(|&v| v == 1.0));

        // Invalidation clears the valid bit (write_tile also does, first).
        s.invalidate_rect([256, 256, 257, 257]);
        assert!(!r.read_tile(0, 1, 1, &mut out));
        // Reader on another thread still sees it invalid.
        std::thread::scope(|sc| {
            sc.spawn(|| {
                let mut o = filled(g, 0.0);
                assert!(!r.read_tile(0, 1, 1, &mut o));
            });
        });

        s.write_tile(0, 1, 1, &filled(g, 2.0)).unwrap();
        assert!(r.read_tile(0, 1, 1, &mut out));
        assert!(out.iter().all(|&v| v == 2.0));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reader_rejects_copy_straddling_complete_rewrite() {
        let p = tmp("straddle");
        let g = geom();
        let mut s = TileStore::create(&p, g).unwrap();
        s.write_tile(0, 2, 1, &filled(g, 1.0)).unwrap();
        let r = TileReader::open(&p).unwrap();
        let idx = r.raw.layout.index(0, 2, 1);
        let mut out = filled(g, 0.0);

        // A complete rewrite lands between the copy and the re-check: the
        // copy holds the old data, the tile is valid again at both loads,
        // yet the read must be rejected.
        let mut new = filled(g, 2.0);
        new[0] = 9.0;
        assert!(!r.raw.read_tile_with(idx, &mut out, || {
            s.write_tile(0, 2, 1, &new).unwrap();
        }));
        assert!(s.is_valid(0, 2, 1));
        assert!(r.read_tile(0, 2, 1, &mut out));
        assert!(out == new, "tile contents differ");

        // So is an invalidation that lands mid-copy.
        assert!(!r.raw.read_tile_with(idx, &mut out, || {
            s.invalidate_rect([600, 300, 601, 301]);
        }));
        assert!(!r.read_tile(0, 2, 1, &mut out));

        // The state entry: valid and even when stable, sequence advancing by
        // two per write, valid bit cleared (sequence kept) on invalidate.
        let e = r.raw.state(idx).load(Ordering::Relaxed);
        assert_eq!(e, 4, "two writes then an invalidate");
        s.write_tile(0, 2, 1, &new).unwrap();
        assert_eq!(r.raw.state(idx).load(Ordering::Relaxed), VALID | 6);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn crash_mid_write_does_not_strand_tile() {
        let p = tmp("crash");
        let g = geom();
        let mut s = TileStore::create(&p, g).unwrap();
        s.write_tile(0, 0, 1, &filled(g, 1.0)).unwrap();
        let idx = s.raw.layout.index(0, 0, 1);
        // Simulate a crash after the odd store: entry left odd, not valid.
        s.raw.state(idx).store(3, Ordering::Relaxed);
        drop(s);
        let mut s = TileStore::open(&p).unwrap();
        assert!(!s.is_valid(0, 0, 1));
        s.write_tile(0, 0, 1, &filled(g, 2.0)).unwrap();
        assert!(s.is_valid(0, 0, 1));
        let mut out = filled(g, 0.0);
        assert!(TileReader::open(&p).unwrap().read_tile(0, 0, 1, &mut out));
        // Sequence wrap: from the top of the 31-bit range, still valid.
        s.raw.state(idx).store(SEQ, Ordering::Relaxed);
        s.write_tile(0, 0, 1, &filled(g, 3.0)).unwrap();
        assert!(s.is_valid(0, 0, 1));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn open_rejects_absurd_geometry() {
        let p = tmp("absurd");
        let g = geom();
        // Rewrite a fresh, valid store's header fields, then reopen.
        let patched = |fields: &[(usize, &[u8])]| {
            drop(TileStore::create(&p, g).unwrap());
            let mut f = std::fs::read(&p).unwrap();
            for &(off, bytes) in fields {
                f[off..off + bytes.len()].copy_from_slice(bytes);
            }
            std::fs::write(&p, &f).unwrap();
            TileStore::open(&p).is_err() && TileReader::open(&p).is_err()
        };
        // Tile edge over the limit.
        assert!(patched(&[(12, &u32::MAX.to_le_bytes())]));
        // u32::MAX channels × a huge canvas overflow the file size.
        assert!(patched(&[
            (16, &(u64::MAX / 2).to_le_bytes()),
            (32, &u32::MAX.to_le_bytes()),
        ]));
        // A grid that fits u32 but whose byte size overflows usize.
        let side = (256u64 << 20).to_le_bytes();
        assert!(patched(&[
            (16, &side),
            (24, &side),
            (32, &u32::MAX.to_le_bytes())
        ]));
        let big = TileGeometry {
            width: 256 << 20,
            height: 256 << 20,
            channels: u32::MAX,
            tile: TILE,
        };
        assert!(Layout::new(big).unwrap_err().contains("too large"));
        // u32::MAX channels alone: no overflow on 64-bit, but the file size
        // no longer matches.
        assert!(patched(&[(32, &u32::MAX.to_le_bytes())]));
        // Tile grid wider than u32 with tile 1.
        let huge = TileGeometry {
            width: u64::MAX,
            height: 1,
            channels: 1,
            tile: 1,
        };
        assert!(Layout::new(huge).is_err());
        assert!(TileStore::create(&p, huge).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn open_rejects_bad_files() {
        let p = tmp("bad");
        std::fs::write(&p, b"not a tile store").unwrap();
        assert!(TileReader::open(&p).is_err());
        let g = geom();
        drop(TileStore::create(&p, g).unwrap());
        let f = OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(Layout::new(g).unwrap().file_len as u64 - 4)
            .unwrap();
        assert!(TileStore::open(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
