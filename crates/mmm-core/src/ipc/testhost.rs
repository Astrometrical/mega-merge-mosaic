//! Test-only in-process host that stands in for the PixInsight PCL module,
//! letting IPC-backed pipeline tests run without a real host process.
//!
//! [`MockHost::spawn`] plays the host role faithfully (it is the reference
//! implementation the real PCL host must match): it creates the shared
//! memory segment, replies to `BandRequest`/`OutputBand` over an in-process
//! byte-pipe pair, and accumulates whatever the worker streams back as
//! blended output so tests can assert on it.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::ipc::IPC_PROTOCOL_VERSION;
use crate::ipc::protocol::{
    BlendParamsWire, HostMsg, InitJob, JobMode, PanelDesc, WorkerMsg, read_worker_frame,
    write_frame,
};
use crate::ipc::shm::{ShmSegment, SlotLayout};

/// One end of an in-process, blocking byte pipe backed by a shared queue.
///
/// `Read` blocks until bytes are available or the write end has been
/// dropped (clean EOF, `Ok(0)`); `Write` always succeeds by pushing onto the
/// queue and waking any blocked reader.
struct PipeInner {
    queue: Mutex<VecDeque<u8>>,
    cond: Condvar,
    /// Number of live `PipeWriter`s; when it hits zero, readers see EOF.
    writers: Mutex<usize>,
}

/// The read half of an in-process pipe (see [`pipe`]).
struct PipeReader {
    inner: Arc<PipeInner>,
}

/// The write half of an in-process pipe (see [`pipe`]).
struct PipeWriter {
    inner: Arc<PipeInner>,
}

/// Creates an in-process, blocking byte pipe: bytes written to the returned
/// [`PipeWriter`] become readable from the returned [`PipeReader`].
fn pipe() -> (PipeReader, PipeWriter) {
    let inner = Arc::new(PipeInner {
        queue: Mutex::new(VecDeque::new()),
        cond: Condvar::new(),
        writers: Mutex::new(1),
    });
    (
        PipeReader {
            inner: inner.clone(),
        },
        PipeWriter { inner },
    )
}

impl Clone for PipeWriter {
    fn clone(&self) -> Self {
        *self.inner.writers.lock().unwrap() += 1;
        PipeWriter {
            inner: self.inner.clone(),
        }
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut q = self.inner.queue.lock().unwrap();
        loop {
            if !q.is_empty() {
                let n = q.len().min(buf.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = q.pop_front().unwrap();
                }
                return Ok(n);
            }
            if *self.inner.writers.lock().unwrap() == 0 {
                return Ok(0); // clean EOF: no bytes, no writers left.
            }
            q = self.inner.cond.wait(q).unwrap();
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut q = self.inner.queue.lock().unwrap();
        q.extend(buf.iter().copied());
        self.inner.cond.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let mut writers = self.inner.writers.lock().unwrap();
        *writers -= 1;
        if *writers == 0 {
            // Wake any reader blocked waiting for more bytes so it can
            // observe EOF instead of hanging forever.
            drop(writers);
            self.inner.cond.notify_all();
        }
    }
}

/// Canvas geometry plus accumulated planar pixel data collected from
/// [`WorkerMsg::Begin`]/[`WorkerMsg::OutputBand`] by [`HostSide`].
type ResultGeom = (u64, u64, u64);

/// Handle to the background thread playing the host role in a test; created
/// by [`MockHost::spawn`].
pub struct MockHost;

/// The running mock host thread and the state it accumulates, returned by
/// [`MockHost::spawn`] alongside the worker's pipe ends.
pub struct HostSide {
    thread: Option<JoinHandle<()>>,
    result: Arc<Mutex<(ResultGeom, Vec<f32>)>>,
}

impl HostSide {
    /// Blocks until the mock host thread has processed `Done`/`Error` (or
    /// the worker pipe closed) and exited.
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            t.join().expect("mock host thread panicked");
        }
    }

    /// Returns the canvas geometry announced by `Begin` and the planar
    /// output pixels accumulated from `OutputBand` messages so far.
    pub fn result(&self) -> (ResultGeom, Vec<f32>) {
        let guard = self.result.lock().unwrap();
        (guard.0, guard.1.clone())
    }
}

impl MockHost {
    /// Builds an [`InitJob`] for [`JobMode::Aligned`] with `panels`
    /// same-sized panels of `w`×`h`×`ch`, `input_slots` input slots (no
    /// output slots — input-only jobs, as used by Task 4's concurrency
    /// test), each `slot_bytes` bytes.
    pub fn aligned_job(
        w: u64,
        h: u64,
        ch: u64,
        panels: u32,
        input_slots: u32,
        slot_bytes: u64,
    ) -> InitJob {
        Self::job(w, h, ch, panels, input_slots, 0, slot_bytes)
    }

    /// Builds an [`InitJob`] with zero input panels and `output_slots`
    /// output slots, for tests that only exercise the output-streaming
    /// path (`begin_output`/`send_output_band`).
    pub fn output_job(w: u64, h: u64, ch: u64, output_slots: u32, slot_bytes: u64) -> InitJob {
        Self::job(w, h, ch, 0, 0, output_slots, slot_bytes)
    }

    fn job(
        w: u64,
        h: u64,
        ch: u64,
        panels: u32,
        input_slots: u32,
        output_slots: u32,
        slot_bytes: u64,
    ) -> InitJob {
        let panel_descs = (0..panels)
            .map(|panel_id| PanelDesc {
                panel_id,
                width: w,
                height: h,
                channels: ch,
                properties: vec![],
            })
            .collect();
        InitJob {
            protocol_version: IPC_PROTOCOL_VERSION,
            shm_name: format!("/mmm-test-{}-{}", std::process::id(), unique_id()),
            slot_bytes,
            input_slots,
            output_slots,
            canvas: [w, h, ch],
            panels: panel_descs,
            mode: JobMode::Aligned,
            session_dir: "/tmp/mmm-testhost.mmm-session".into(),
            params: BlendParamsWire::default(),
        }
    }

    /// Creates the shared-memory segment named by `job.shm_name` and spawns
    /// a background thread that plays the host side of the protocol:
    /// serving `BandRequest`s from `panel_pixels` (one planar `channels ×
    /// height × width` buffer per panel, in `job.panels` order) and
    /// acknowledging `OutputBand`s into an accumulated result buffer.
    ///
    /// Returns the [`HostSide`] handle plus the pipe ends the worker side
    /// (`HostLink::start`) should use as its `input`/`output`. In-process
    /// only: creates its own shm segment and its own in-process pipe shim,
    /// then delegates to [`Self::serve_over`] for the actual serving loop.
    pub fn spawn(
        job: InitJob,
        panel_pixels: Vec<Vec<f32>>,
    ) -> (HostSide, Box<dyn Read + Send>, Box<dyn Write + Send>) {
        let layout = SlotLayout {
            slot_bytes: job.slot_bytes,
            input_slots: job.input_slots,
            output_slots: job.output_slots,
        };
        let shm = ShmSegment::create(&job.shm_name, layout.total_bytes())
            .expect("MockHost::spawn: ShmSegment::create failed");

        // host_to_worker: host writes, worker reads (worker's `input`).
        // worker_to_host: worker writes, host reads.
        let (worker_in, host_out) = pipe();
        let (host_in, worker_out) = pipe();

        let host = Self::serve_over(job, panel_pixels, shm, host_in, host_out);
        (host, Box::new(worker_in), Box::new(worker_out))
    }

    /// The same host-serving loop as [`Self::spawn`], but over a caller-
    /// supplied transport: a real, already-created [`ShmSegment`] and
    /// arbitrary `reader`/`writer` — typically a spawned child process's
    /// real stdout/stdin — instead of the in-process pipe shim.
    ///
    /// This is what lets a cross-process integration test drive a real
    /// `mmm-ipc-worker` child against the exact same reference-host logic
    /// [`Self::spawn`] uses for in-process unit tests: there is only one
    /// serving-loop implementation (`run_host`, private) behind both.
    ///
    /// The caller must have already written the `HostMsg::Init` frame to
    /// `writer` (or be about to — `run_host` never sends `Init` itself; the
    /// in-process `spawn` path hands `job` straight to `HostLink::start`
    /// instead of putting it on the wire).
    pub fn serve_over(
        job: InitJob,
        panel_pixels: Vec<Vec<f32>>,
        shm: ShmSegment,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> HostSide {
        let layout = SlotLayout {
            slot_bytes: job.slot_bytes,
            input_slots: job.input_slots,
            output_slots: job.output_slots,
        };
        let result: Arc<Mutex<(ResultGeom, Vec<f32>)>> = Arc::new(Mutex::new(((0, 0, 0), vec![])));
        let result_for_thread = result.clone();

        let thread = std::thread::Builder::new()
            .name("mock-host".into())
            .spawn(move || {
                run_host(
                    job,
                    panel_pixels,
                    shm,
                    layout,
                    reader,
                    writer,
                    result_for_thread,
                );
            })
            .expect("spawn mock host thread");

        HostSide {
            thread: Some(thread),
            result,
        }
    }
}

/// The one reference host-serving loop, shared by [`MockHost::spawn`]
/// (in-process pipe shim) and [`MockHost::serve_over`] (arbitrary
/// `Read`/`Write`, e.g. a real child process): generic over the transport so
/// neither caller needs its own copy.
fn run_host<R: Read, W: Write>(
    job: InitJob,
    panel_pixels: Vec<Vec<f32>>,
    shm: ShmSegment,
    layout: SlotLayout,
    mut host_in: R,
    host_out: W,
    result: Arc<Mutex<(ResultGeom, Vec<f32>)>>,
) {
    let host_out = Mutex::new(host_out);
    loop {
        let msg = match read_worker_frame(&mut host_in) {
            Ok(Some(m)) => m,
            Ok(None) => break, // worker closed its output: clean shutdown.
            Err(_) => break,
        };
        match msg {
            WorkerMsg::BandRequest(req) => {
                let panel = &panel_pixels[req.panel_id as usize];
                let desc = &job.panels[req.panel_id as usize];
                let width = desc.width;
                let channels = desc.channels;
                let rows = req.y1 - req.y0;
                let dst = shm.slice_mut(layout.input_offset(req.slot_id), channels * rows * width);
                // panel is planar: channels × height × width.
                let plane_stride = desc.height * width;
                for c in 0..channels {
                    let src_start = (c * plane_stride + req.y0 * width) as usize;
                    let src_len = (rows * width) as usize;
                    let dst_start = (c * rows * width) as usize;
                    dst[dst_start..dst_start + src_len]
                        .copy_from_slice(&panel[src_start..src_start + src_len]);
                }
                let reply = HostMsg::BandReply(crate::ipc::protocol::BandReply {
                    request_id: req.request_id,
                    slot_id: req.slot_id,
                    status: 0,
                });
                write_frame(&mut *host_out.lock().unwrap(), &reply).unwrap();
                host_out.lock().unwrap().flush().unwrap();
            }
            WorkerMsg::Begin { w, h, ch } => {
                let mut guard = result.lock().unwrap();
                guard.0 = (w, h, ch);
                guard.1 = vec![0f32; (w * h * ch) as usize];
            }
            WorkerMsg::OutputBand(band) => {
                let (w, _h, ch) = result.lock().unwrap().0;
                let rows = band.rows;
                let len = ch * rows * w;
                let slot_data = shm.slice(layout.output_offset(band.slot_id), len);
                {
                    let mut guard = result.lock().unwrap();
                    let plane_stride = guard.0.1 * w; // height*width
                    for c in 0..ch {
                        let src_start = (c * rows * w) as usize;
                        let src_len = (rows * w) as usize;
                        let dst_start = (c * plane_stride + band.y0 * w) as usize;
                        guard.1[dst_start..dst_start + src_len]
                            .copy_from_slice(&slot_data[src_start..src_start + src_len]);
                    }
                }
                let ack = HostMsg::OutputAck {
                    request_id: band.request_id,
                };
                write_frame(&mut *host_out.lock().unwrap(), &ack).unwrap();
                host_out.lock().unwrap().flush().unwrap();
            }
            WorkerMsg::Progress { .. } => {
                // Not asserted on by current tests; ignored.
            }
            WorkerMsg::Done => break,
            WorkerMsg::Error { .. } => break,
        }
    }
    // Dropping `host_out` (end of scope) closes the host→worker pipe,
    // signalling clean EOF to the worker's reader thread.
}

/// Cheap process-unique suffix for shm segment names so parallel tests
/// don't collide on the same name.
fn unique_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
