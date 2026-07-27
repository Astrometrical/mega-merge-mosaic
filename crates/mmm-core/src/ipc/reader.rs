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
// concurrent `row` calls always compute *different* `idx` values, so no
// access to `cells[idx]` made by one thread is ever concurrent with an
// access to `cells[idx']` (`idx != idx'`) made by another. That
// per-thread disjointness is what `Sync` needs to be sound here (no
// cross-thread data race, even though the type has no lock).
//
// Within a single thread's own cell, `row` takes BOTH exclusive and shared
// borrows of the `UnsafeCell`, at different points, and callers legitimately
// hold multiple shared borrows of one cell alive at once (`analyze`'s
// `scan_reader` and `blend`'s per-panel readers both collect one slice per
// channel of a row, all borrowed from the same thread's cell, and read them
// together). The true invariant `row` maintains to make that sound:
//
// - The `&mut` taken on a cache miss (to fill `buf` via `request_band`) is
//   scoped to a block that ends, and is dropped, BEFORE `row` derives any
//   `&self`-lifetime slice to return — so that exclusive borrow is never
//   concurrent with, or overlapping the lifetime of, any slice handed back
//   to a caller (this call's or an earlier one's).
// - The value `row` returns is always derived from a fresh SHARED reborrow
//   (`&*cells[idx].get()`) taken after that block closes. Any number of
//   such shared reborrows for one thread's cell can coexist — that's what
//   lets one thread hold every channel's slice of a row at once — because
//   none of them overlaps the transient `&mut` from a miss, which by
//   construction happens-before all of them are created.
// - So on a given thread, the sequence is always: (optional) exclusive
//   borrow to fetch → drop it → shared borrow(s) to read, repeated per call;
//   never exclusive-concurrent-with-shared or exclusive-concurrent-with-
//   exclusive within that cell.
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

        let band_rows = self.band_rows as u64;
        let by0 = (canvas_y / band_rows) * band_rows;

        // Cheap shared peek to decide whether this call needs to fetch. This
        // is itself a `&*` reborrow — it coexists freely with any other live
        // shared reborrows of this cell (e.g. slices this same thread
        // returned for earlier channels of this row), so taking it can never
        // invalidate them.
        // SAFETY: see the `unsafe impl Sync` justification above — `idx` is
        // unique to this thread, so this is one of possibly many concurrent
        // shared reborrows of this thread's own cell, never of another
        // thread's.
        let need_fetch = {
            let band = unsafe { &*self.cells[idx].get() };
            !band.valid || band.y0 != by0
        };

        // Cache-miss path: fetch the needed band into this thread's cell.
        // The `&mut` below is taken ONLY when a fetch is actually needed,
        // and it is scoped to this block and dropped before any returned
        // slice is derived (see the SAFETY note further down) — it must
        // never be live at the same time as a `&self`-lifetime slice handed
        // back to a previous call, because callers legitimately hold slices
        // for every channel of one row simultaneously (see the `unsafe impl
        // Sync` justification above). Callers only ever hit this path on the
        // first channel of a row whose band hasn't been fetched yet, by
        // which point any slices returned for a previous row have already
        // been dropped by the caller — see the `unsafe impl Sync` doc.
        if need_fetch {
            // SAFETY: `idx` is unique to this thread, and — because
            // `need_fetch` was true — no shared reborrow this call is about
            // to derive exists yet; any shared reborrows from *earlier*
            // calls on this thread reference a row the caller has already
            // finished with (see above), so this exclusive borrow doesn't
            // overlap a live one.
            let band = unsafe { &mut *self.cells[idx].get() };
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

        // SAFETY: same `idx`-disjointness argument as above, but this is a
        // SHARED reborrow, not exclusive. Any number of these may coexist
        // for this thread's cell at once — which is exactly what callers
        // need: `analyze`'s `scan_reader` and `blend`'s per-panel readers
        // both hold one slice per channel of a row concurrently, all
        // borrowed from the same cell. No `&mut` to this cell is created
        // again until the *next* call to `row` on this thread, and that
        // call's exclusive borrow (if any, on a cache miss) is confined to
        // the block above, ending before it derives its own shared slice —
        // so it never aliases a shared slice this call is about to return.
        let band = unsafe { &*self.cells[idx].get() };

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
