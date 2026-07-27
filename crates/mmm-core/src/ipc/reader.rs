//! Per-thread buffer that copies a fetched band out of the shared-memory
//! slot so the slot can be freed for reuse.
//!
//! [`IpcBacking`] is a [`crate::panel_reader::PanelReader`] backing that
//! pulls rows from a [`HostLink`] instead of an mmap'd file. Rows are
//! fetched a *band* (a run of `band_rows` canvas rows) at a time and cached
//! in a per-calling-thread buffer, because `request_band` is a blocking
//! round-trip over the IPC pipe and re-fetching one row at a time would
//! serialize every caller on that round trip. Each rayon worker thread (plus
//! one fallback slot for calls made outside a rayon pool) gets its own
//! buffer, so concurrent callers never contend on it.

use std::cell::UnsafeCell;
use std::sync::{Arc, Mutex};

use crate::Error;
use crate::ipc::client::HostLink;

/// One calling thread's cached band: rows `[y0, y0 + rows_in_band)` of the
/// panel, planar (`channels` planes, each `rows_in_band * width` f32s), or
/// `valid = false` if nothing has been fetched into `buf` yet.
struct ThreadBand {
    /// First canvas row covered by `buf`, meaningful only if `valid`.
    y0: u64,
    /// Whether `buf` currently holds a fetched band (vs. never-yet-filled).
    valid: bool,
    /// Planar pixel data for the cached band; reused in place across bands
    /// (only the length actually written is meaningful).
    buf: Vec<f32>,
}

/// A [`crate::panel_reader::PanelReader`] backing that serves rows of one
/// panel by pulling bands over a [`HostLink`], one band-sized fetch per
/// calling thread's working set.
pub struct IpcBacking {
    link: Arc<HostLink>,
    panel_id: u32,
    /// Panel/canvas geometry `(width, height, channels)` — an IPC-backed
    /// panel always covers the full canvas (see [`JobMode::Aligned`],
    /// the only mode Task 5 serves).
    ///
    /// [`JobMode::Aligned`]: crate::ipc::protocol::JobMode::Aligned
    canvas: (u64, u64, u64),
    /// Rows per fetched band (the last band of the panel may be shorter).
    band_rows: usize,
    /// One cache slot per rayon worker thread, plus one fallback slot
    /// (index `cells.len() - 1`) for calls made outside any rayon pool. See
    /// the `unsafe impl Sync` below for why indexing by thread makes this
    /// sound without a lock.
    cells: Vec<UnsafeCell<ThreadBand>>,
    /// The first transport error encountered by any thread's `row` call, if
    /// any; latched so callers can check once after a scan/blend stage
    /// instead of threading a `Result` through `row`'s `Option` signature.
    error: Mutex<Option<String>>,
}

// SAFETY: `IpcBacking` is shared across rayon worker threads behind a
// `PanelReader` that must itself be `Sync` (blend fans out row reads across
// its thread pool). The only interior mutability is `cells`, a
// `Vec<UnsafeCell<ThreadBand>>`; `row` indexes it with
// `rayon::current_thread_index().unwrap_or(num_threads)`, which is a
// distinct value for every live rayon worker thread within one pool (plus
// exactly one extra reserved index for callers outside any pool). So two
// threads calling `row` concurrently always dereference *different* cells —
// the `&mut ThreadBand` each thread takes via `unsafe { &mut *cells[idx].get() }`
// never aliases another thread's `&mut`, which is what `Sync` needs to be
// sound here (no data race, even though the type is not internally
// lock-protected).
//
// The `&[f32]` `row` returns borrows `&self` (not the `&mut ThreadBand`), so
// by the time it's handed back to the caller the exclusive borrow is gone;
// nothing else touches that thread's cell until that same thread calls
// `row` again, at which point the previous slice's lifetime has already
// ended from the caller's point of view (analyze/blend read all channels of
// one row, then advance to the next row, never holding a row slice past
// that point) — this matches the doc contract on
// [`crate::panel_reader::PanelReader::row`].
unsafe impl Sync for IpcBacking {}

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
        let (w, _h, ch) = canvas;
        let cap = ch as usize * band_rows * w as usize;
        let num_threads = rayon::current_num_threads();
        let cells = (0..=num_threads)
            .map(|_| {
                UnsafeCell::new(ThreadBand {
                    y0: 0,
                    valid: false,
                    buf: vec![0f32; cap],
                })
            })
            .collect();
        IpcBacking {
            link,
            panel_id,
            canvas,
            band_rows,
            cells,
            error: Mutex::new(None),
        }
    }

    /// One channel row in canvas coordinates: `(x0, slice)`, always `x0 ==
    /// 0` since an IPC panel covers the full canvas. Fetches (and caches,
    /// per calling thread) the `band_rows`-row band containing `canvas_y` on
    /// a cache miss. Returns `None` on a transport error — the error itself
    /// is latched, see [`Self::ipc_error`] — or if `canvas_y` is out of
    /// range.
    pub fn row(&self, c: u64, canvas_y: u64) -> Option<(u64, &[f32])> {
        let (w, h, ch) = self.canvas;
        if canvas_y >= h || c >= ch {
            return None;
        }
        let num_threads = self.cells.len() - 1;
        let idx = rayon::current_thread_index().unwrap_or(num_threads);
        // SAFETY: see the `unsafe impl Sync` justification above — `idx` is
        // unique to this thread (within the pool, plus the poolless
        // fallback), so this is the only live reference to `cells[idx]`.
        let band = unsafe { &mut *self.cells[idx].get() };

        let band_rows = self.band_rows as u64;
        let by0 = (canvas_y / band_rows) * band_rows;
        if !band.valid || band.y0 != by0 {
            let by1 = (by0 + band_rows).min(h);
            let bh = (by1 - by0) as usize;
            let w_usize = w as usize;
            let ch_usize = ch as usize;
            let want = ch_usize * bh * w_usize;
            if let Err(e) = self
                .link
                .request_band(self.panel_id, by0, by1, &mut band.buf[..want])
            {
                self.latch_error(e);
                band.valid = false;
                return None;
            }
            band.y0 = by0;
            band.valid = true;
        }

        // The ACTUAL height of the cached band (not `self.band_rows` — the
        // final band of the panel is shorter whenever `h` isn't a multiple
        // of `band_rows`), which is the stride between channel planes.
        let by1 = (band.y0 + band_rows).min(h);
        let bh = (by1 - band.y0) as usize;
        let w_usize = w as usize;
        let ly = (canvas_y - band.y0) as usize;
        let plane_start = c as usize * bh * w_usize;
        let row_start = plane_start + ly * w_usize;
        Some((0, &band.buf[row_start..row_start + w_usize]))
    }

    /// Latches the first transport error seen by any thread; later errors
    /// are dropped (the first one is the useful diagnostic — subsequent
    /// `row` calls on other threads will keep failing for the same reason).
    fn latch_error(&self, e: Error) {
        let mut guard = self.error.lock().unwrap();
        if guard.is_none() {
            *guard = Some(e.to_string());
        }
    }

    /// The first transport error latched by any thread's `row` call, if
    /// any. `Error` isn't `Clone`, so this rebuilds an [`Error::Compute`]
    /// carrying the original message rather than returning the original
    /// value.
    pub fn ipc_error(&self) -> Option<Error> {
        self.error.lock().unwrap().clone().map(Error::compute)
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
