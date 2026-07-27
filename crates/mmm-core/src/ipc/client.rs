//! Worker-side client that requests bands from the host and awaits replies.
//!
//! [`HostLink`] owns the worker end of the IPC transport: the shared-memory
//! segment, the stdin/stdout control channel, and the bookkeeping needed to
//! let many worker threads pull input bands and push output bands
//! concurrently over that single pipe. One background thread
//! ([`HostLink::start`]) demultiplexes host replies by `request_id` onto
//! whichever caller is waiting; everything else runs on caller threads.
//!
//! # Deadlock-freedom
//!
//! Every blocking wait in this module (a `ReplySlot` waiting for its reply,
//! a `SlotPool` waiting for a free slot) is a `while`-loop over a
//! [`Condvar`] guarding a predicate that includes `shutdown` *and*
//! `cancelled`. When the reader thread observes the host pipe close
//! (`Ok(None)`) or error out, it sets `shutdown` and then wakes *every*
//! registered waiter — pending reply slots and both slot pools — so nothing
//! can block forever after the host goes away; a `HostMsg::Cancel` frame
//! does the same with `cancelled` instead (`notify_cancel`), without tearing
//! down the link itself, so a mid-stage cancellation unwinds promptly rather
//! than waiting for a reply a cancelled host will never send. See
//! `HostLink`'s private `run_reader` method for the exact sequencing that
//! rules out the lost-wakeup race in both cases (store the flag before
//! acquiring each waiter's mutex, so any waiter either observes the flag
//! already true when it takes the lock, or is already asleep in
//! `Condvar::wait` and gets woken by the subsequent `notify_all`). The
//! reader thread's shutdown path additionally runs from a drop guard
//! (`ShutdownGuard`), so it fires even if the reader thread panics instead
//! of returning normally — see `run_reader`'s doc comment.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::ipc::IPC_PROTOCOL_VERSION;
use crate::ipc::protocol::{
    BandReply, BandRequest, HostMsg, InitJob, JobMode, OutputBand, PanelDesc, WorkerMsg,
    read_host_frame, write_frame,
};
use crate::ipc::shm::{ShmSegment, SlotLayout};
use crate::{Error, Result};

/// A reply the reader thread hands to whichever caller is waiting on the
/// matching `request_id`.
enum Reply {
    /// Answers a `request_band` call.
    Band(BandReply),
    /// Answers a `send_output_band` call.
    OutputAck,
}

/// A wakeable box a caller blocks on until the reader thread delivers its
/// reply (or the link shuts down).
///
/// The `while`-loop in [`ReplySlot::wait`] re-checks the predicate
/// (`Some(reply)` or `shutdown`) every time it wakes, so a spurious wakeup
/// or a notify meant for a different reply never causes an early return.
struct ReplySlot {
    slot: Mutex<Option<Reply>>,
    cond: Condvar,
}

impl ReplySlot {
    fn new() -> Arc<ReplySlot> {
        Arc::new(ReplySlot {
            slot: Mutex::new(None),
            cond: Condvar::new(),
        })
    }

    /// Blocks until a reply is delivered, `shutdown` is set, or `cancelled`
    /// is set. A reply already in hand always wins (no point discarding
    /// real data that arrived before/alongside a `Cancel`); `shutdown` is
    /// checked before `cancelled` since a closed pipe is the more specific
    /// diagnosis. Re-checked every wakeup, so the reader thread's
    /// `notify_cancel`/`shut_down` (which wake *every* waiter, not just this
    /// one) can never cause a lost wakeup — see the module docs.
    fn wait(&self, shutdown: &AtomicBool, cancelled: &AtomicBool) -> Result<Reply> {
        let mut guard = self.slot.lock().unwrap();
        loop {
            if let Some(reply) = guard.take() {
                return Ok(reply);
            }
            if shutdown.load(Ordering::SeqCst) {
                return Err(Error::compute(
                    "HostLink: host pipe closed while waiting for a reply",
                ));
            }
            if cancelled.load(Ordering::SeqCst) {
                return Err(Error::compute("cancelled"));
            }
            guard = self.cond.wait(guard).unwrap();
        }
    }

    /// Delivers `reply` to whichever thread is (or will be) waiting.
    fn deliver(&self, reply: Reply) {
        let mut guard = self.slot.lock().unwrap();
        *guard = Some(reply);
        self.cond.notify_all();
    }

    /// Wakes a waiter with no reply, so it re-checks `shutdown`.
    ///
    /// Locking and immediately releasing `slot` before notifying is load-
    /// bearing, not decorative: it forces a happens-before edge with any
    /// thread inside [`Self::wait`]'s critical section (see the module
    /// docs), which is what rules out the lost-wakeup race.
    fn wake_for_shutdown(&self) {
        drop(self.slot.lock().unwrap());
        self.cond.notify_all();
    }
}

/// A bounded pool of free slot ids (input or output), acquired for the
/// duration of one band transfer and released immediately after.
struct SlotPool {
    free: Mutex<Vec<u32>>,
    cond: Condvar,
}

impl SlotPool {
    fn new(ids: impl Iterator<Item = u32>) -> SlotPool {
        SlotPool {
            free: Mutex::new(ids.collect()),
            cond: Condvar::new(),
        }
    }

    /// Blocks until a slot id is free, `shutdown` is set, or `cancelled` is
    /// set (see [`ReplySlot::wait`] for the same check-ordering rationale).
    fn acquire(&self, shutdown: &AtomicBool, cancelled: &AtomicBool) -> Result<u32> {
        let mut guard = self.free.lock().unwrap();
        loop {
            if let Some(id) = guard.pop() {
                return Ok(id);
            }
            if shutdown.load(Ordering::SeqCst) {
                return Err(Error::compute(
                    "HostLink: host pipe closed while waiting for a free slot",
                ));
            }
            if cancelled.load(Ordering::SeqCst) {
                return Err(Error::compute("cancelled"));
            }
            guard = self.cond.wait(guard).unwrap();
        }
    }

    fn release(&self, id: u32) {
        let mut guard = self.free.lock().unwrap();
        guard.push(id);
        self.cond.notify_all();
    }

    /// Wakes an acquirer with no slot freed, so it re-checks `shutdown`.
    /// See [`ReplySlot::wake_for_shutdown`] for why the lock/drop matters.
    fn wake_for_shutdown(&self) {
        drop(self.free.lock().unwrap());
        self.cond.notify_all();
    }
}

/// Drop guard that calls [`HostLink::shut_down`] no matter how the reader
/// thread's closure exits — normal return, an `Err` arm, *or* a panic.
///
/// This is the mechanism behind the crash-detection guarantee: a
/// `request_band`/`send_output_band` caller blocked in [`ReplySlot::wait`]
/// or [`SlotPool::acquire`] must be woken with an error if the reader thread
/// ever stops servicing the pipe, for *any* reason. An explicit
/// `self.shut_down()` call only covers the arms the author remembered to put
/// it in; a value whose `Drop` impl runs during unwind covers every exit
/// path, including ones a future refactor adds without thinking about
/// shutdown at all.
struct ShutdownGuard(Arc<HostLink>);

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        self.0.shut_down();
    }
}

/// Worker-side handle to the host over shared memory + stdin/stdout.
///
/// Built by [`HostLink::start`]; cheap to share (behind the `Arc` `start`
/// returns) across every worker thread that needs to pull input bands or
/// push output bands. `Send + Sync` — every field is either immutable after
/// construction or internally synchronized.
pub struct HostLink {
    shm: ShmSegment,
    layout: SlotLayout,
    output: Mutex<Box<dyn Write + Send>>,
    next_request_id: AtomicU32,
    pending: Mutex<HashMap<u32, Arc<ReplySlot>>>,
    input_pool: SlotPool,
    output_pool: SlotPool,
    cancelled: AtomicBool,
    shutdown: AtomicBool,
    init: InitJob,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
}

impl HostLink {
    /// Attaches to the shared-memory segment named in `init`, spawns the
    /// reader thread, and returns a ready-to-use link.
    ///
    /// `input`/`output` are the worker's stdin/stdout (already past the
    /// `HostMsg::Init` frame, which the caller decoded into `init`).
    /// Fails if `init.protocol_version` doesn't match
    /// [`IPC_PROTOCOL_VERSION`] or the shared-memory segment can't be
    /// attached.
    pub fn start(
        init: InitJob,
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
    ) -> Result<Arc<HostLink>> {
        if init.protocol_version != IPC_PROTOCOL_VERSION {
            return Err(Error::compute(format!(
                "HostLink::start: protocol version mismatch: worker is {IPC_PROTOCOL_VERSION}, host sent {}",
                init.protocol_version
            )));
        }

        let layout = SlotLayout {
            slot_bytes: init.slot_bytes,
            input_slots: init.input_slots,
            output_slots: init.output_slots,
        };
        let shm = ShmSegment::attach(&init.shm_name, layout.total_bytes())?;

        let link = Arc::new(HostLink {
            shm,
            layout,
            output: Mutex::new(output),
            next_request_id: AtomicU32::new(0),
            pending: Mutex::new(HashMap::new()),
            input_pool: SlotPool::new(0..init.input_slots),
            output_pool: SlotPool::new(0..init.output_slots),
            cancelled: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            init,
            reader_thread: Mutex::new(None),
        });

        let reader_link = link.clone();
        let handle = std::thread::Builder::new()
            .name("hostlink-reader".into())
            .spawn(move || reader_link.run_reader(input))
            .map_err(|e| Error::compute(format!("HostLink::start: spawn reader thread: {e}")))?;
        *link.reader_thread.lock().unwrap() = Some(handle);

        Ok(link)
    }

    /// Reader thread body: demultiplexes `HostMsg`s from `input` onto
    /// pending [`ReplySlot`]s until the pipe closes or errors, then wakes
    /// every waiter so nothing blocks forever afterward.
    ///
    /// `_guard` ties `shut_down` to this closure's *exit*, not just its
    /// normal EOF/error arm: whether the loop below returns cleanly or the
    /// thread panics partway through (a malformed frame triggering a bug in
    /// this function, say), `ShutdownGuard::drop` still runs and still wakes
    /// every waiter. That is the crash-detection guarantee this module
    /// promises — a dead/wedged host must never leave a `request_band` or
    /// `send_output_band` call blocked forever — extended to cover a panic
    /// in the reader thread itself, not just an orderly pipe close.
    fn run_reader(self: Arc<Self>, mut input: Box<dyn Read + Send>) {
        let _guard = ShutdownGuard(self.clone());
        loop {
            match read_host_frame(&mut input) {
                Ok(Some(HostMsg::BandReply(reply))) => {
                    let request_id = reply.request_id;
                    self.deliver(request_id, Reply::Band(reply));
                }
                Ok(Some(HostMsg::OutputAck { request_id })) => {
                    self.deliver(request_id, Reply::OutputAck);
                }
                Ok(Some(HostMsg::Cancel)) => {
                    self.notify_cancel();
                }
                Ok(Some(HostMsg::Init(_))) => {
                    // The host sends exactly one `Init`, already consumed by
                    // the caller before `start`; a second one is a protocol
                    // violation we simply ignore rather than tear down a
                    // link that may still be making progress.
                }
                Ok(None) | Err(_) => {
                    // `_guard`'s drop (below, at function exit) calls
                    // `shut_down`; no need to call it here too.
                    return;
                }
            }
        }
    }

    /// Hands `reply` to the pending request registered under `request_id`,
    /// if any (a reply for an id nobody is waiting on — e.g. after that
    /// caller already gave up — is silently dropped).
    fn deliver(&self, request_id: u32, reply: Reply) {
        let slot = self.pending.lock().unwrap().remove(&request_id);
        if let Some(slot) = slot {
            slot.deliver(reply);
        }
    }

    /// Marks the link shut down and wakes every waiter — pending reply
    /// slots and both slot pools — so no `request_band`/`send_output_band`
    /// call blocks forever after the host pipe closes or errors. See the
    /// module docs for the lost-wakeup argument.
    fn shut_down(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let pending: Vec<Arc<ReplySlot>> = self
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for slot in pending {
            slot.wake_for_shutdown();
        }
        self.input_pool.wake_for_shutdown();
        self.output_pool.wake_for_shutdown();
    }

    /// Marks the link cancelled (the host sent [`HostMsg::Cancel`]) and
    /// wakes every waiter — pending reply slots and both slot pools — so a
    /// `request_band`/`send_output_band` call blocked mid-wait fails
    /// promptly instead of blocking for a reply that a cancelled host will
    /// never send. Unlike [`Self::shut_down`], this does not tear down the
    /// link or its pipe (the host may still be reachable, e.g. to observe a
    /// later clean close) — only the in-flight job is aborted. Same
    /// store-before-lock ordering as `shut_down`, for the same lost-wakeup
    /// reason (module docs).
    fn notify_cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let pending: Vec<Arc<ReplySlot>> = self
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for slot in pending {
            slot.wake_for_shutdown();
        }
        self.input_pool.wake_for_shutdown();
        self.output_pool.wake_for_shutdown();
    }

    /// Looks up `panel_id`'s geometry, erroring if it isn't in this job.
    fn panel_desc(&self, panel_id: u32) -> Result<&PanelDesc> {
        self.init
            .panels
            .iter()
            .find(|p| p.panel_id == panel_id)
            .ok_or_else(|| {
                Error::compute(format!(
                    "HostLink::request_band: unknown panel_id {panel_id}"
                ))
            })
    }

    /// Requests rows `[y0, y1)` of `panel_id` and blocks until the host
    /// fills them in, copying the result into `dst` and freeing the input
    /// slot before returning.
    ///
    /// `dst.len()` must equal `channels * (y1 - y0) * width` for this
    /// panel.
    pub fn request_band(&self, panel_id: u32, y0: u64, y1: u64, dst: &mut [f32]) -> Result<()> {
        let panel = self.panel_desc(panel_id)?;
        let expected = panel.channels * (y1 - y0) * panel.width;
        if dst.len() as u64 != expected {
            return Err(Error::compute(format!(
                "HostLink::request_band: dst has {} elements, expected {expected} (channels {} * rows {} * width {})",
                dst.len(),
                panel.channels,
                y1 - y0,
                panel.width
            )));
        }

        let slot_id = self.input_pool.acquire(&self.shutdown, &self.cancelled)?;
        let result = self.request_band_inner(panel_id, y0, y1, slot_id, dst);
        self.input_pool.release(slot_id);
        result
    }

    fn request_band_inner(
        &self,
        panel_id: u32,
        y0: u64,
        y1: u64,
        slot_id: u32,
        dst: &mut [f32],
    ) -> Result<()> {
        // Fail fast rather than round-tripping to a host that has already
        // asked us to stop: a mid-stage `Cancel` must unwind the current
        // analyze/blend stage promptly, not after one more (possibly never
        // answered) request.
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(Error::compute("cancelled"));
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let reply_slot = ReplySlot::new();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id, reply_slot.clone());

        let req = BandRequest {
            request_id,
            panel_id,
            y0,
            y1,
            slot_id,
        };
        if let Err(e) = self.send(&WorkerMsg::BandRequest(req)) {
            self.pending.lock().unwrap().remove(&request_id);
            return Err(e);
        }

        let reply_result = reply_slot.wait(&self.shutdown, &self.cancelled);
        self.pending.lock().unwrap().remove(&request_id);
        match reply_result? {
            Reply::Band(reply) => {
                if reply.status != 0 {
                    return Err(Error::compute(format!(
                        "HostLink::request_band: host reported an error filling request {request_id}"
                    )));
                }
                let src = self
                    .shm
                    .slice(self.layout.input_offset(slot_id), dst.len() as u64);
                dst.copy_from_slice(src);
                Ok(())
            }
            Reply::OutputAck => Err(Error::compute(
                "HostLink::request_band: protocol violation: got OutputAck for a BandRequest",
            )),
        }
    }

    /// Reports progress within a pipeline stage. Best-effort: a write
    /// failure is swallowed rather than returned, since progress is
    /// advisory and the caller's real error will surface via
    /// `request_band`/`send_output_band`/`finish_ok` instead.
    pub fn send_progress(&self, stage: &str, done: u64, total: u64) {
        let _ = self.send(&WorkerMsg::Progress {
            stage: stage.to_string(),
            done,
            total,
        });
    }

    /// Returns whether the host has sent `Cancel`.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Announces output canvas geometry before any `send_output_band`
    /// call.
    pub fn begin_output(&self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.send(&WorkerMsg::Begin { w, h, ch })
    }

    /// Copies `planar` (`channels * rows * width` elements) into a free
    /// output slot, sends it to the host, and blocks until the host
    /// acknowledges, then frees the slot.
    pub fn send_output_band(&self, y0: u64, rows: u64, planar: &[f32]) -> Result<()> {
        let slot_id = self.output_pool.acquire(&self.shutdown, &self.cancelled)?;
        let result = self.send_output_band_inner(y0, rows, planar, slot_id);
        self.output_pool.release(slot_id);
        result
    }

    fn send_output_band_inner(
        &self,
        y0: u64,
        rows: u64,
        planar: &[f32],
        slot_id: u32,
    ) -> Result<()> {
        // Same fail-fast rationale as `request_band_inner`.
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(Error::compute("cancelled"));
        }
        let dst = self
            .shm
            .slice_mut(self.layout.output_offset(slot_id), planar.len() as u64);
        dst.copy_from_slice(planar);

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let reply_slot = ReplySlot::new();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id, reply_slot.clone());

        let band = OutputBand {
            request_id,
            y0,
            rows,
            slot_id,
        };
        if let Err(e) = self.send(&WorkerMsg::OutputBand(band)) {
            self.pending.lock().unwrap().remove(&request_id);
            return Err(e);
        }

        let reply_result = reply_slot.wait(&self.shutdown, &self.cancelled);
        self.pending.lock().unwrap().remove(&request_id);
        match reply_result? {
            Reply::OutputAck => Ok(()),
            Reply::Band(_) => Err(Error::compute(
                "HostLink::send_output_band: protocol violation: got BandReply for an OutputBand",
            )),
        }
    }

    /// Sends `WorkerMsg::Done`: the job completed successfully.
    pub fn finish_ok(&self) -> Result<()> {
        self.send(&WorkerMsg::Done)
    }

    /// Sends `WorkerMsg::Error`: the job failed. Best-effort like
    /// [`Self::send_progress`] — there is nothing more useful to do with a
    /// write failure while already reporting a fatal error.
    pub fn finish_err(&self, msg: &str) {
        let _ = self.send(&WorkerMsg::Error {
            message: msg.to_string(),
        });
    }

    /// Canvas dimensions as `[width, height, channels]`.
    pub fn canvas(&self) -> [u64; 3] {
        self.init.canvas
    }

    /// Per-panel geometry and metadata, in panel-id order.
    pub fn panels(&self) -> &[PanelDesc] {
        &self.init.panels
    }

    /// How to read/align input panels for this job.
    pub fn mode(&self) -> &JobMode {
        &self.init.mode
    }

    /// The shared-memory slot layout in use for this job.
    pub fn slot_layout(&self) -> SlotLayout {
        self.layout
    }

    /// Writes one frame to the shared outbound pipe and flushes it so the
    /// host sees it promptly. All writers share this one `Mutex`-guarded
    /// pipe, so frames from different threads never interleave.
    fn send(&self, msg: &impl crate::ipc::protocol::FrameBody) -> Result<()> {
        let mut out = self.output.lock().unwrap();
        write_frame(&mut *out, msg)
            .and_then(|()| out.flush())
            .map_err(|e| Error::compute(format!("HostLink: write to host failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::testhost::MockHost;
    use rayon::prelude::*;

    #[test]
    fn concurrent_band_requests_return_correct_pixels() {
        // Two panels, 8×16×1, value = panel*1000 + y*10 + x.
        let (w, h, ch) = (8u64, 16u64, 1u64);
        let mk = |p: u64| {
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (p * 1000 + y * 10 + x) as f32))
                .collect::<Vec<_>>()
        };
        let pixels = vec![mk(0), mk(1)];
        let job = MockHost::aligned_job(
            w,
            h,
            ch,
            /*panels*/ 2,
            /*input_slots*/ 4,
            /*slot_bytes*/ w * 8 * 4,
        );
        let (host, r, wr) = MockHost::spawn(job.clone(), pixels.clone());
        let link = HostLink::start(job, r, wr).unwrap();

        // Hammer request_band from many rayon threads over both panels/bands.
        (0..2u32).into_par_iter().for_each(|panel| {
            for y0 in (0..h).step_by(8) {
                let y1 = (y0 + 8).min(h);
                let mut dst = vec![0f32; ((y1 - y0) * w) as usize];
                link.request_band(panel, y0, y1, &mut dst).unwrap();
                for (i, v) in dst.iter().enumerate() {
                    let yy = y0 + (i as u64) / w;
                    let xx = (i as u64) % w;
                    assert_eq!(*v, (panel as u64 * 1000 + yy * 10 + xx) as f32);
                }
            }
        });
        link.finish_ok().unwrap();
        host.join();
    }

    /// Deadlock-freedom check: a thread blocked in `request_band` waiting
    /// for a free input slot (a pool with zero slots never has one) must be
    /// woken with an error, not hang forever, once the host pipe closes.
    #[test]
    fn eof_from_host_wakes_a_thread_blocked_on_an_empty_slot_pool() {
        let (w, h, ch) = (4u64, 4u64, 1u64);
        let pixels = vec![vec![0f32; (w * h) as usize]];
        // Zero input slots: `request_band` can never acquire one, so it
        // blocks in `SlotPool::acquire` until the link shuts down.
        let job = MockHost::aligned_job(w, h, ch, /*panels*/ 1, /*input_slots*/ 0, w * 4);
        let (host, r, wr) = MockHost::spawn(job.clone(), pixels);
        let link = HostLink::start(job, r, wr).unwrap();

        let blocked = link.clone();
        let handle = std::thread::spawn(move || {
            let mut dst = vec![0f32; (w * h) as usize];
            blocked.request_band(0, 0, h, &mut dst)
        });

        // Give the spawned thread a chance to actually enter the blocking
        // wait (not required for correctness — `acquire` would also error
        // out immediately if `shutdown` were already set by the time it
        // runs — but it's what exercises the wake-a-sleeping-waiter path
        // rather than the already-shut-down path).
        std::thread::sleep(std::time::Duration::from_millis(50));

        // End the run normally; the mock host reacts to `Done` by exiting
        // and dropping its write end of the worker's input pipe, which is
        // exactly what a crashed/exited host process does from the
        // worker's point of view: EOF on `input`.
        link.finish_ok().unwrap();
        host.join();

        let result = handle.join().expect("request_band thread should not panic");
        assert!(
            result.is_err(),
            "request_band must fail once the host pipe closes, not hang forever"
        );
    }
}
