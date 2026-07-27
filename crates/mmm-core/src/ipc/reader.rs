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
///
/// # Concurrent-use invariant
///
/// `IpcBacking` is `Sync`, but only sound to call `row` on concurrently
/// under a specific precondition: every calling thread must have a distinct
/// [`rayon::current_thread_index`] value, with **at most one** concurrent
/// caller for which that value is `None` (poolless callers share a single
/// fallback cell). Concretely, that means:
///
/// - Any number of distinct worker threads of **one** rayon pool may call
///   `row` concurrently (this is what `blend` does on the global pool).
/// - At most one thread outside any rayon pool may call `row` at a time
///   relative to any other caller (this is what the sequential `analyze`
///   scan does).
/// - Sharing one `IpcBacking` across two plain `std::thread::spawn` threads,
///   or across workers from two *different* rayon pools/scopes, is **not**
///   permitted — both cases can alias `current_thread_index() == None` (or
///   an index reused across pools) onto the same cell, which is a data
///   race. See the `unsafe impl Sync` below for the full argument.
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

// SAFETY: this `unsafe impl` is sound ONLY under the precondition stated in
// the type-level "Concurrent-use invariant" doc above — it is not
// unconditionally sound for arbitrary concurrent callers, and callers that
// violate it are themselves responsible for the resulting unsoundness (this
// is the standard division of responsibility for an `unsafe impl Sync`: the
// impl guarantees soundness *given* its stated precondition, not for every
// possible use).
//
// The only interior mutability is `cells`, a `Vec<UnsafeCell<ThreadBand>>`;
// `row` indexes it with `rayon::current_thread_index().unwrap_or(num_threads)`.
// Given the precondition — every concurrent caller has a distinct
// `current_thread_index()`, with at most one caller reporting `None` — two
// concurrent `row` calls always compute *different* `idx` values, so the
// `unsafe { &mut *cells[idx].get() }` each takes never aliases another
// call's `&mut`. That disjointness is what `Sync` needs to be sound here (no
// data race, even though the type has no lock).
//
// The precondition is upheld by every caller in this codebase today: the
// `analyze` scan drives `row` sequentially (one caller, trivially
// non-concurrent with itself), and `blend` fans `row` out across the global
// rayon thread pool, where every worker has a distinct
// `current_thread_index()` for the lifetime of the pool. It would be
// violated by, e.g., sharing one `IpcBacking` across two plain
// `std::thread::spawn` threads (both see `current_thread_index() == None`
// and race on the fallback cell) or across two different rayon
// pools/scopes — such callers must not be introduced without revisiting
// this invariant (see the type doc; a future GUI or multi-pool consumer
// must either serialize its poolless callers or give each pool/thread its
// own `IpcBacking`).
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
        // `cells` is sized from `current_num_threads()` at construction
        // time; a call from a larger rayon pool than that (not currently
        // reachable — see the type-level "Concurrent-use invariant" doc)
        // would index out of range. The `Vec` index below is bounds-checked
        // either way (not UB), but assert loudly in debug builds rather
        // than let it surface as a bare index-out-of-bounds panic.
        debug_assert!(
            idx < self.cells.len(),
            "IpcBacking::row called from a larger rayon pool than the one it was constructed in"
        );
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
