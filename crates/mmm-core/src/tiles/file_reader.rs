//! A tile reader over plain file reads, for a store another process may
//! recreate.
//!
//! [`TileReader`](super::TileReader) memory-maps the store, so truncating the
//! file under it faults the reader. [`FileTileReader`] never maps the file: it
//! uses positioned reads, so a store that is truncated or rebuilt under it
//! just yields `Missing` / `None` for the affected tiles.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use super::{HEADER_LEN, Layout, TileGeometry, TileState, classify, map_err};
use crate::error::Result;

/// Read access to a tile store, with the semantics of
/// [`TileReader`](super::TileReader)'s `state` and `read_tile_any`.
pub trait TileRead: Send + Sync {
    /// The store's geometry.
    fn geometry(&self) -> TileGeometry;

    /// The state of tile `(tx, ty)` at `level`, from its state entry alone.
    fn state(&self, level: u32, tx: u32, ty: u32) -> TileState;

    /// Copy tile `(tx, ty)` at `level` into `out` whether valid or stale:
    /// `Some(Valid | Stale)` if a complete, untorn copy was made, `None`
    /// (contents of `out` unspecified) otherwise.
    fn read_tile_any(&self, level: u32, tx: u32, ty: u32, out: &mut [f32]) -> Option<TileState>;
}

/// Tile reader using positioned file reads instead of a memory map: the
/// reader for a store that another process may recreate. It follows the same
/// per-tile seqlock protocol as [`TileReader`](super::TileReader). A short
/// read, an I/O error or an out-of-range tile reads as `Missing` / `None`;
/// it never panics and never faults.
pub struct FileTileReader {
    layout: Layout,
    file: Mutex<File>,
}

impl FileTileReader {
    /// Open the store at `path`, validating its header and size. Takes no
    /// lock.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(map_err(path))?;
        let len = file.metadata().map_err(map_err(path))?.len() as usize;
        let mut header = vec![0u8; len.min(HEADER_LEN)];
        file.read_exact(&mut header).map_err(map_err(path))?;
        let layout = Layout::parse_header(path, &header, len)?;
        Ok(FileTileReader {
            layout,
            file: Mutex::new(file),
        })
    }

    /// Global tile index, or `None` if the level or tile is out of range.
    fn index(&self, level: u32, tx: u32, ty: u32) -> Option<usize> {
        if level >= self.layout.levels {
            return None;
        }
        let (nx, ny) = self.layout.g.tiles(level);
        (tx < nx && ty < ny).then(|| {
            self.layout.level_start[level as usize] + ty as usize * nx as usize + tx as usize
        })
    }

    /// Fill `buf` from byte offset `off`; false on a short read or error.
    fn read_at(file: &mut File, off: u64, buf: &mut [u8]) -> bool {
        file.seek(SeekFrom::Start(off)).is_ok() && file.read_exact(buf).is_ok()
    }

    fn load_state(file: &mut File, idx: usize) -> Option<u32> {
        let mut b = [0u8; 4];
        Self::read_at(file, (HEADER_LEN + idx * 4) as u64, &mut b).then(|| u32::from_le_bytes(b))
    }
}

impl TileRead for FileTileReader {
    fn geometry(&self) -> TileGeometry {
        self.layout.g
    }

    fn state(&self, level: u32, tx: u32, ty: u32) -> TileState {
        let Some(idx) = self.index(level, tx, ty) else {
            return TileState::Missing;
        };
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        Self::load_state(&mut file, idx).map_or(TileState::Missing, classify)
    }

    fn read_tile_any(&self, level: u32, tx: u32, ty: u32, out: &mut [f32]) -> Option<TileState> {
        let idx = self.index(level, tx, ty)?;
        let n = self.layout.g.tile_len();
        if out.len() != n {
            return None;
        }
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let first = Self::load_state(&mut file, idx)?;
        if first == 0 || first & 1 != 0 {
            return None;
        }
        let mut bytes = vec![0u8; n * 4];
        let off = (self.layout.data_off + idx * n * 4) as u64;
        if !Self::read_at(&mut file, off, &mut bytes) {
            return None;
        }
        if Self::load_state(&mut file, idx)? != first {
            return None;
        }
        let (words, _) = bytes.as_chunks::<4>();
        for (o, c) in out.iter_mut().zip(words) {
            *o = f32::from_le_bytes(*c);
        }
        Some(classify(first))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{TILE, TileReader, TileStore};
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-filereader-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("tiles.mmmtile")
    }

    fn geom(width: u64, height: u64) -> TileGeometry {
        TileGeometry {
            width,
            height,
            channels: 2,
            tile: TILE,
        }
    }

    #[test]
    fn file_reader_matches_mapped_reader() {
        let p = tmp("match");
        let g = geom(1000, 600);
        let len = g.tile_len();
        {
            let mut s = TileStore::create(&p, g).unwrap();
            // Level 0: valid, stale and never-written tiles; level 1 likewise.
            for (i, (tx, ty)) in [(0, 0), (1, 0), (2, 1), (3, 2)].into_iter().enumerate() {
                s.write_tile(0, tx, ty, &vec![i as f32 + 1.0; len]).unwrap();
            }
            s.write_tile_stale(0, 0, 1, &vec![9.0; len]).unwrap();
            s.write_tile(1, 0, 0, &vec![5.0; len]).unwrap();
            s.write_tile_stale(1, 1, 0, &vec![6.0; len]).unwrap();
            s.flush().unwrap();
        }
        let mapped = TileReader::open(&p).unwrap();
        let file = FileTileReader::open(&p).unwrap();
        assert_eq!(TileRead::geometry(&file), g);
        let (mut a, mut b) = (vec![0.0; len], vec![0.0; len]);
        for l in 0..2 {
            let (nx, ny) = g.tiles(l);
            for ty in 0..ny {
                for tx in 0..nx {
                    assert_eq!(
                        file.state(l, tx, ty),
                        mapped.state(l, tx, ty),
                        "({l},{tx},{ty})"
                    );
                    a.fill(-1.0);
                    b.fill(-1.0);
                    let want = mapped.read_tile_any(l, tx, ty, &mut a);
                    assert_eq!(file.read_tile_any(l, tx, ty, &mut b), want);
                    if want.is_some() {
                        assert!(a == b, "samples differ at ({l},{tx},{ty})");
                    }
                }
            }
        }
        // Out of range never panics.
        assert_eq!(file.state(9, 0, 0), TileState::Missing);
        assert_eq!(file.read_tile_any(0, 99, 0, &mut b), None);
        drop(mapped);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn file_reader_survives_recreation() {
        let p = tmp("recreate");
        let g = geom(1000, 600);
        let len = g.tile_len();
        {
            let mut s = TileStore::create(&p, g).unwrap();
            s.write_tile(0, 0, 0, &vec![1.0; len]).unwrap();
        }
        let r = FileTileReader::open(&p).unwrap();
        let g2 = geom(300, 300);
        let mut s2 = TileStore::create(&p, g2).unwrap();
        s2.write_tile(0, 0, 0, &vec![2.0; g2.tile_len()]).unwrap();
        let mut out = vec![0.0; len];
        for l in 0..g.levels() {
            let (nx, ny) = g.tiles(l);
            for ty in 0..ny {
                for tx in 0..nx {
                    let _ = r.state(l, tx, ty);
                    // Either nothing, or a complete tile; never a panic.
                    if r.read_tile_any(l, tx, ty, &mut out).is_some() {
                        assert!(out.iter().all(|&v| v == out[0]), "torn tile");
                    }
                }
            }
        }
        drop(s2);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn file_reader_on_truncated_file_reads_none() {
        let p = tmp("truncate");
        let g = geom(1000, 600);
        let len = g.tile_len();
        {
            let mut s = TileStore::create(&p, g).unwrap();
            s.write_tile(0, 0, 0, &vec![1.0; len]).unwrap();
        }
        let r = FileTileReader::open(&p).unwrap();
        let mut out = vec![0.0; len];
        assert_eq!(r.read_tile_any(0, 0, 0, &mut out), Some(TileState::Valid));
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(4096).unwrap();
        assert_eq!(r.state(0, 0, 0), TileState::Missing);
        assert_eq!(r.read_tile_any(0, 0, 0, &mut out), None);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
