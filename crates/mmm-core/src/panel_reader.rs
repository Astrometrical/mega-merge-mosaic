//! Uniform row access to a panel's pixel data, whatever its storage.
//!
//! Aligned full-canvas XISF frames (MosaicByCoordinates output) are read
//! straight from the source file's mmap — the file *is* the random-access
//! store. Reprojected panels (phase 5) live as cropped caches in the session
//! directory: raw planar f32 little-endian with the bbox's dimensions
//! (`channels` planes × bbox height × bbox width, row-major top-down), also
//! mmap'd. [`PanelReader::row`] presents both in canvas coordinates so the
//! analyze scan and the blender never care which storage they are reading.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::formats::xisf::XisfPanel;
use crate::ipc::client::HostLink;
use crate::ipc::reader::IpcBacking;
use crate::session::PanelMeta;
use crate::{Error, Result};

/// How a panel's pixel data is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PanelStorage {
    /// A full-canvas XISF frame: the file's geometry equals the session
    /// canvas and rows map 1:1 to canvas rows. The historical (phase 1–4)
    /// storage — sessions written before `storage` existed deserialize to
    /// this via `#[serde(default)]`.
    #[default]
    FullCanvasXisf,
    /// A cropped reprojection cache (`panels/<id>/aligned.bin`, see
    /// [`crate::align::reproject_panel`]): raw planar f32 little-endian with
    /// the bbox's dimensions. `bbox` is the cache's placement on the canvas,
    /// `[x0, y0, x1, y1]` exclusive. The panel's [`PanelMeta::path`] points
    /// at the cache file itself.
    CroppedCache {
        /// Canvas placement of the cache, `[x0, y0, x1, y1]` exclusive.
        bbox: [u64; 4],
    },
    /// Pixels served on demand from the IPC host, never a file on disk —
    /// see [`crate::ipc::reader::IpcBacking`]. Not written into persisted
    /// session metadata for real runs: an IPC job's panels are read once
    /// per stage and re-derived from the live [`HostLink`] rather than
    /// reopened from a session directory (see Task 8).
    Ipc {
        /// The panel id the host knows this panel by.
        panel_id: u32,
    },
}

enum Backing {
    Xisf(XisfPanel),
    Cache(Mmap),
    Ipc(IpcBacking),
}

/// A memory-mapped panel reader presenting rows in canvas coordinates.
pub struct PanelReader {
    backing: Backing,
    /// Storage extent on the canvas, `[x0, y0, x1, y1]` exclusive (the full
    /// canvas for [`PanelStorage::FullCanvasXisf`]).
    bbox: [u64; 4],
    /// Canvas geometry `(width, height, channels)`.
    canvas: (u64, u64, u64),
}

impl PanelReader {
    /// Open a panel for reading against the session canvas geometry.
    ///
    /// Validates that the storage is consistent with `canvas`: a full-canvas
    /// XISF must have exactly the canvas geometry; a cropped cache's bbox
    /// must be non-empty, lie within the canvas, and match the file size.
    pub fn open(meta: &PanelMeta, canvas: (u64, u64, u64)) -> Result<PanelReader> {
        match meta.storage {
            PanelStorage::FullCanvasXisf => {
                let x = XisfPanel::open(&meta.path)?;
                if (x.width(), x.height(), x.channels()) != canvas {
                    return Err(Error::format(
                        &meta.path,
                        format!(
                            "panel geometry {}x{}x{} does not match session canvas {}x{}x{}",
                            x.width(),
                            x.height(),
                            x.channels(),
                            canvas.0,
                            canvas.1,
                            canvas.2
                        ),
                    ));
                }
                Ok(PanelReader {
                    backing: Backing::Xisf(x),
                    bbox: [0, 0, canvas.0, canvas.1],
                    canvas,
                })
            }
            PanelStorage::CroppedCache { bbox } => {
                let [x0, y0, x1, y1] = bbox;
                if x0 >= x1 || y0 >= y1 || x1 > canvas.0 || y1 > canvas.1 {
                    return Err(Error::format(
                        &meta.path,
                        format!(
                            "cache bbox {bbox:?} is empty or outside the {}x{} canvas",
                            canvas.0, canvas.1
                        ),
                    ));
                }
                let file = File::open(&meta.path).map_err(|e| Error::io(&meta.path, e))?;
                // SAFETY: read-only map; truncation by another process during
                // a run is undefined, as is conventional for mmap readers.
                let mmap = unsafe { Mmap::map(&file) }.map_err(|e| Error::io(&meta.path, e))?;
                let expected = (x1 - x0) * (y1 - y0) * canvas.2 * 4;
                if mmap.len() as u64 != expected {
                    return Err(Error::format(
                        &meta.path,
                        format!(
                            "cache file is {} bytes, expected {expected} for bbox {bbox:?} \
                             × {} channels",
                            mmap.len(),
                            canvas.2
                        ),
                    ));
                }
                Ok(PanelReader {
                    backing: Backing::Cache(mmap),
                    bbox,
                    canvas,
                })
            }
            PanelStorage::Ipc { .. } => Err(Error::compute(
                "IPC panels are opened via open_ipc, not open",
            )),
        }
    }

    /// Open a plain full-canvas XISF file directly (no session metadata);
    /// the canvas geometry is the file's own. Used by the analyze scan, which
    /// runs before any [`PanelMeta`] exists.
    pub fn open_xisf(path: &Path) -> Result<PanelReader> {
        let x = XisfPanel::open(path)?;
        let canvas = (x.width(), x.height(), x.channels());
        Ok(PanelReader {
            backing: Backing::Xisf(x),
            bbox: [0, 0, canvas.0, canvas.1],
            canvas,
        })
    }

    /// Open a panel served over IPC by `link`: rows are pulled from the host
    /// on demand, `band_rows` canvas rows at a time (see
    /// [`crate::ipc::reader::IpcBacking`]). No session metadata or file is
    /// involved — used by IPC-driven runs, which never write panel pixels
    /// to disk.
    pub fn open_ipc(
        link: Arc<HostLink>,
        panel_id: u32,
        canvas: (u64, u64, u64),
        band_rows: usize,
    ) -> PanelReader {
        let (w, h, _ch) = canvas;
        PanelReader {
            backing: Backing::Ipc(IpcBacking::new(link, panel_id, canvas, band_rows)),
            bbox: [0, 0, w, h],
            canvas,
        }
    }

    /// The first transport error latched while reading an IPC-backed panel,
    /// if any; always `None` for non-IPC backings. Callers check this after
    /// a scan/blend stage completes, since [`Self::row`] can't surface a
    /// [`Result`] through its `Option` signature.
    pub fn ipc_error(&self) -> Option<Error> {
        match &self.backing {
            Backing::Ipc(ipc) => ipc.ipc_error(),
            Backing::Xisf(_) | Backing::Cache(_) => None,
        }
    }

    /// Canvas geometry `(width, height, channels)` this reader addresses.
    pub fn canvas(&self) -> (u64, u64, u64) {
        self.canvas
    }

    /// Storage extent on the canvas, `[x0, y0, x1, y1]` exclusive.
    pub fn bbox(&self) -> [u64; 4] {
        self.bbox
    }

    /// One channel row clipped to the panel's x extent: `(x0, slice)` in
    /// canvas coordinates — `slice[i]` is canvas pixel `x0 + i`. Canvas rows
    /// outside the storage bbox return `None`.
    pub fn row(&self, c: u64, canvas_y: u64) -> Option<(u64, &[f32])> {
        let [x0, y0, x1, y1] = self.bbox;
        if canvas_y < y0 || canvas_y >= y1 {
            return None;
        }
        match &self.backing {
            Backing::Xisf(p) => Some((0, p.row(c, canvas_y))),
            Backing::Cache(mmap) => {
                assert!(c < self.canvas.2, "channel {c} out of range");
                let (bw, bh) = ((x1 - x0) as usize, (y1 - y0) as usize);
                let plane: &[f32] = bytemuck::cast_slice(&mmap[..]);
                let start = c as usize * bw * bh + (canvas_y - y0) as usize * bw;
                Some((x0, &plane[start..start + bw]))
            }
            Backing::Ipc(ipc) => ipc.row(c, canvas_y),
        }
    }

    /// Advise the OS that access will be sequential (large streaming scans).
    /// The `madvise(2)` call is a no-op where unavailable (e.g. Windows).
    pub fn advise_sequential(&self) {
        match &self.backing {
            Backing::Xisf(p) => p.advise_sequential(),
            Backing::Cache(_mmap) => {
                #[cfg(unix)]
                let _ = _mmap.advise(memmap2::Advice::Sequential);
            }
            Backing::Ipc(_) => {
                // No mmap to advise; bands are already fetched sequentially
                // through the per-thread cache in `IpcBacking`.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::write_xisf;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-preader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta(path: PathBuf, storage: PanelStorage) -> PanelMeta {
        PanelMeta {
            id: 0,
            path,
            source: None,
            bbox: [0, 0, 0, 0],
            nonzero_frac: 0.0,
            ch_min: vec![],
            ch_max: vec![],
            ch_mean: vec![],
            storage,
        }
    }

    #[test]
    fn full_canvas_xisf_rows_match_source() {
        let dir = tmpdir("xisf");
        let (w, h, ch) = (6u64, 4u64, 2u64);
        let planes: Vec<f32> = (0..w * h * ch).map(|i| i as f32 + 1.0).collect();
        let path = dir.join("p.xisf");
        write_xisf(&path, w, h, ch, &planes).unwrap();

        let m = meta(path.clone(), PanelStorage::FullCanvasXisf);
        let r = PanelReader::open(&m, (w, h, ch)).unwrap();
        assert_eq!(r.bbox(), [0, 0, w, h]);
        assert_eq!(r.canvas(), (w, h, ch));
        r.advise_sequential();
        let src = XisfPanel::open(&path).unwrap();
        for c in 0..ch {
            for y in 0..h {
                let (x0, row) = r.row(c, y).expect("full canvas rows always present");
                assert_eq!(x0, 0);
                assert_eq!(row, src.row(c, y));
            }
        }

        // Geometry mismatch against the session canvas is refused.
        assert!(PanelReader::open(&m, (w + 1, h, ch)).is_err());

        // open_xisf infers the canvas from the file.
        let r2 = PanelReader::open_xisf(&path).unwrap();
        assert_eq!(r2.canvas(), (w, h, ch));
        assert_eq!(r2.row(1, 2).unwrap().1, src.row(1, 2));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cropped_cache_rows_are_canvas_placed_and_clipped() {
        let dir = tmpdir("cache");
        let canvas = (100u64, 80u64, 2u64);
        let bbox = [10u64, 20, 14, 23]; // 4×3
        let (bw, bh) = (4usize, 3usize);
        // Planar: value = c*100 + local_y*10 + local_x.
        let mut bytes = Vec::new();
        for c in 0..2u32 {
            for y in 0..bh {
                for x in 0..bw {
                    bytes.extend_from_slice(
                        &((c as f32) * 100.0 + y as f32 * 10.0 + x as f32).to_le_bytes(),
                    );
                }
            }
        }
        let path = dir.join("aligned.bin");
        std::fs::write(&path, &bytes).unwrap();

        let m = meta(path.clone(), PanelStorage::CroppedCache { bbox });
        let r = PanelReader::open(&m, canvas).unwrap();
        assert_eq!(r.bbox(), bbox);
        r.advise_sequential();

        // Rows outside the bbox y range → None.
        assert!(r.row(0, 19).is_none());
        assert!(r.row(0, 23).is_none());
        assert!(r.row(0, 79).is_none());

        // Rows inside: (x0, slice) in canvas coords.
        for c in 0..2u64 {
            for (ly, cy) in (20..23u64).enumerate() {
                let (x0, row) = r.row(c, cy).unwrap();
                assert_eq!(x0, 10);
                assert_eq!(row.len(), bw);
                for (lx, &v) in row.iter().enumerate() {
                    assert_eq!(v, c as f32 * 100.0 + ly as f32 * 10.0 + lx as f32);
                }
            }
        }

        // Size mismatch and out-of-canvas bboxes are refused.
        let bad = meta(
            path.clone(),
            PanelStorage::CroppedCache {
                bbox: [10, 20, 15, 23],
            },
        );
        assert!(PanelReader::open(&bad, canvas).is_err(), "size mismatch");
        let out = meta(
            path.clone(),
            PanelStorage::CroppedCache {
                bbox: [98, 78, 102, 81],
            },
        );
        assert!(
            PanelReader::open(&out, canvas).is_err(),
            "bbox outside canvas"
        );
        let empty = meta(
            path,
            PanelStorage::CroppedCache {
                bbox: [10, 20, 10, 23],
            },
        );
        assert!(PanelReader::open(&empty, canvas).is_err(), "empty bbox");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
