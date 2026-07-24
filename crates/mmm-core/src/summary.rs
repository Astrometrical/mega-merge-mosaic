//! L8 per-panel summaries: the canvas at 1/8 resolution.
//!
//! Each 8×8 canvas block becomes one cell holding the fraction of covered
//! pixels (all channels nonzero) and the per-channel mean over the covered
//! pixels. This single artifact drives overlap detection, photometric fitting,
//! distance/feather maps, and downsampled previews.
//!
//! On-disk format (`panels/<id>/summary.bin`, all little-endian):
//! magic `MMM8`, then `w8`, `h8`, `channels` as `u32`, then the coverage plane
//! (`w8*h8` × f32), then the mean planes (`channels` planes of `w8*h8` × f32).

use std::path::Path;

use crate::{Error, Result};

/// Canvas pixels per summary cell along each axis.
pub const BLOCK: u32 = 8;

const MAGIC: &[u8; 4] = b"MMM8";

/// One panel's 1/8-resolution summary.
#[derive(Debug, Clone, PartialEq)]
pub struct L8Summary {
    pub w8: u32,
    pub h8: u32,
    pub channels: u32,
    /// len `w8*h8`; fraction of covered pixels in each cell, 0..1.
    pub coverage: Vec<f32>,
    /// len `channels*w8*h8`, planar; mean of covered pixels (0 where none).
    pub mean: Vec<f32>,
}

impl L8Summary {
    /// Allocate an all-zero summary for the given grid.
    pub fn zeroed(w8: u32, h8: u32, channels: u32) -> Self {
        let cells = w8 as usize * h8 as usize;
        Self {
            w8,
            h8,
            channels,
            coverage: vec![0.0; cells],
            mean: vec![0.0; channels as usize * cells],
        }
    }

    /// Mean of covered pixels for channel `c` at cell `(x8, y8)`.
    #[inline]
    pub fn cell(&self, c: u32, x8: u32, y8: u32) -> f32 {
        debug_assert!(c < self.channels && x8 < self.w8 && y8 < self.h8);
        let cells = self.w8 as usize * self.h8 as usize;
        self.mean[c as usize * cells + y8 as usize * self.w8 as usize + x8 as usize]
    }

    /// Covered-pixel fraction at cell `(x8, y8)`.
    #[inline]
    pub fn cov(&self, x8: u32, y8: u32) -> f32 {
        debug_assert!(x8 < self.w8 && y8 < self.h8);
        self.coverage[y8 as usize * self.w8 as usize + x8 as usize]
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let cells = self.w8 as usize * self.h8 as usize;
        assert_eq!(self.coverage.len(), cells, "coverage plane size mismatch");
        assert_eq!(self.mean.len(), self.channels as usize * cells, "mean planes size mismatch");

        let mut bytes = Vec::with_capacity(16 + (self.coverage.len() + self.mean.len()) * 4);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.w8.to_le_bytes());
        bytes.extend_from_slice(&self.h8.to_le_bytes());
        bytes.extend_from_slice(&self.channels.to_le_bytes());
        for &v in &self.coverage {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for &v in &self.mean {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        if bytes.len() < 16 || &bytes[0..4] != MAGIC {
            return Err(Error::format(path, "not an MMM8 summary file"));
        }
        let w8 = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let h8 = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let channels = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let cells = w8 as usize * h8 as usize;
        let expected = 16 + (cells + channels as usize * cells) * 4;
        if bytes.len() != expected {
            return Err(Error::format(
                path,
                format!("summary size {} != expected {expected} for {w8}x{h8}x{channels}", bytes.len()),
            ));
        }
        let read_plane = |off: usize, n: usize| -> Vec<f32> {
            bytes[off..off + n * 4]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect()
        };
        let coverage = read_plane(16, cells);
        let mean = read_plane(16 + cells * 4, channels as usize * cells);
        Ok(Self { w8, h8, channels, coverage, mean })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let mut s = L8Summary::zeroed(3, 2, 2);
        s.coverage = vec![0.0, 0.5, 1.0, 0.25, 0.75, 1.0];
        s.mean = (0..12).map(|i| i as f32 * 0.1).collect();

        let dir = std::env::temp_dir().join(format!("mmm-summary-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("summary.bin");
        s.write(&path).unwrap();
        let r = L8Summary::read(&path).unwrap();
        assert_eq!(r, s);

        // Accessors address the planar layout correctly.
        assert_eq!(r.cov(1, 0), 0.5);
        assert_eq!(r.cov(2, 1), 1.0);
        assert!((r.cell(0, 1, 1) - 0.4).abs() < 1e-6);
        assert!((r.cell(1, 2, 0) - 0.8).abs() < 1e-6);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_magic_and_truncation() {
        let dir = std::env::temp_dir().join(format!("mmm-summary-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let bad = dir.join("bad.bin");
        std::fs::write(&bad, b"NOPE").unwrap();
        assert!(L8Summary::read(&bad).is_err());

        let trunc = dir.join("trunc.bin");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        std::fs::write(&trunc, bytes).unwrap();
        assert!(L8Summary::read(&trunc).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
