//! [`IpcBacking`], a [`crate::panel_reader::PanelReader`] backing that pulls
//! rows from a [`HostLink`] instead of an mmap'd file.
//!
//! Rows are fetched a *band* (a run of `band_rows` canvas rows) at a time
//! and cached in a per-calling-thread buffer, because `request_band` is a
//! blocking round-trip over the IPC pipe and re-fetching one row at a time
//! would serialize every caller on that round trip. The cache itself —
//! including the concurrent-use invariant that makes it sound as a `Sync`
//! type without a lock — lives in `crate::band_cache::BandCache`; this
//! module only supplies the IPC fetch.

use std::sync::Arc;

use crate::Error;
use crate::band_cache::BandCache;
use crate::ipc::client::HostLink;

/// A [`crate::panel_reader::PanelReader`] backing that serves rows of one
/// panel by pulling bands over a [`HostLink`], one band-sized fetch per
/// calling thread's working set (see `crate::band_cache::BandCache` for
/// the concurrent-use invariant this relies on).
pub struct IpcBacking {
    link: Arc<HostLink>,
    panel_id: u32,
    cache: BandCache,
}

impl IpcBacking {
    /// Builds a backing that serves `panel_id` out of `link`, fetching
    /// `band_rows`-row bands on demand. `canvas` is `(width, height,
    /// channels)`; sized cell buffers hold `channels * band_rows * width`
    /// f32s each.
    pub fn new(
        link: Arc<HostLink>,
        panel_id: u32,
        canvas: (u64, u64, u64),
        band_rows: usize,
    ) -> IpcBacking {
        IpcBacking {
            link,
            panel_id,
            cache: BandCache::new(canvas, band_rows),
        }
    }

    /// One channel row in canvas coordinates: `(x0, slice)`, always `x0 ==
    /// 0` since an IPC panel covers the full canvas. Fetches (and caches,
    /// per calling thread) the `band_rows`-row band containing `canvas_y` on
    /// a cache miss. Returns `None` on a transport error — the error itself
    /// is latched, see [`Self::ipc_error`] — or if `canvas_y` is out of
    /// range.
    pub fn row(&self, c: u64, canvas_y: u64) -> Option<(u64, &[f32])> {
        self.cache.row(c, canvas_y, |y0, y1, buf| {
            self.link.request_band(self.panel_id, y0, y1, buf)
        })
    }

    /// The first transport error latched by any thread's `row` call, if
    /// any. `Error` isn't `Clone`, so this rebuilds an [`Error::Compute`]
    /// carrying the original message rather than returning the original
    /// value.
    pub fn ipc_error(&self) -> Option<Error> {
        self.cache.error()
    }
}

#[cfg(test)]
mod tests {
    use crate::formats::xisf::XisfPanel;
    use crate::ipc::client::HostLink;
    use crate::ipc::testhost::MockHost;
    use crate::panel_reader::PanelReader;
    use crate::synth::write_xisf;
    use rayon::prelude::*;

    #[test]
    fn ipc_rows_match_the_source_under_concurrent_access() {
        let (w, h, ch) = (37u64, 91u64, 3u64); // non-multiples of band_rows on purpose
        let planes: Vec<f32> = (0..w * h * ch).map(|i| (i as f32) * 0.5 + 1.0).collect();
        let dir = std::env::temp_dir().join(format!("mmm-ipc-reader-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("p.xisf");
        write_xisf(&path, w, h, ch, &planes).unwrap();
        let src = XisfPanel::open(&path).unwrap();

        let job = MockHost::aligned_job(w, h, ch, 1, 8, w * ch * 32 * 4);
        let (host, r, wr) = MockHost::spawn(job.clone(), vec![planes.clone()]);
        let link = HostLink::start(job, r, wr).unwrap();
        let reader = PanelReader::open_ipc(link.clone(), 0, (w, h, ch), 32);

        (0..h).into_par_iter().for_each(|y| {
            for c in 0..ch {
                let (x0, got) = reader.row(c, y).unwrap();
                assert_eq!(x0, 0);
                assert_eq!(got, src.row(c, y), "mismatch at c={c} y={y}");
            }
        });
        link.finish_ok().unwrap();
        host.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
