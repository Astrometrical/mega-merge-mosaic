//! Cross-process proof that `mmm-ipc-worker` — the real binary, spawned as a
//! child process and driven purely over stdin/stdout plus a real POSIX
//! shared-memory segment — produces a blended mosaic byte-identical to the
//! ordinary file-based `analyze` + `blend` pipeline for the same input
//! panels.
//!
//! The reference host role (creating the shm segment, serving
//! `BandRequest`s from in-memory panel pixels, reassembling `OutputBand`s)
//! is played by `mmm_core::ipc::testhost::MockHost::serve_over` — the exact
//! same serving loop mmm-core's own unit tests use in-process, reached here
//! via the `testkit` feature (see this crate's `Cargo.toml`). Only the
//! transport differs: real OS pipes to a real child process instead of the
//! in-process pipe shim.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use mmm_core::align::choose_frame;
use mmm_core::analyze::{InputSelect, analyze_input, analyze_opts};
use mmm_core::astrometry::WcsModel;
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend};
use mmm_core::formats::xisf::XisfPanel;
use mmm_core::ipc::IPC_PROTOCOL_VERSION;
use mmm_core::ipc::protocol::{
    BlendParamsWire, HostMsg, InitJob, JobMode, PanelDesc, WorkerMsg, read_worker_frame,
    write_frame,
};
use mmm_core::ipc::shm::{ShmSegment, SlotLayout};
use mmm_core::ipc::testhost::MockHost;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

/// Captures blended rows into one planar `(c*h+y)*w+x` buffer — the same
/// layout `ipc::testhost::run_host` reassembles `OutputBand`s into (see its
/// `WorkerMsg::OutputBand` handling), so both sides of the byte-identical
/// assertion below use identical indexing.
struct CaptureSink {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<f32>,
}

impl CaptureSink {
    fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            ch: 0,
            data: Vec::new(),
        }
    }
}

impl RowSink for CaptureSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> mmm_core::Result<()> {
        self.w = w as usize;
        self.h = h as usize;
        self.ch = ch as usize;
        self.data = vec![0f32; self.w * self.h * self.ch];
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> mmm_core::Result<()> {
        assert_eq!(rows.len() % (self.ch * self.w), 0);
        let band_rows = rows.len() / (self.ch * self.w);
        for c in 0..self.ch {
            for r in 0..band_rows {
                let src = &rows[(c * band_rows + r) * self.w..][..self.w];
                let off = (c * self.h + y0 as usize + r) * self.w;
                self.data[off..off + self.w].copy_from_slice(src);
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> mmm_core::Result<()> {
        Ok(())
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmm-ipcworker-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two overlapping full-canvas panels on a 128x64 single-channel canvas: A =
/// 0.2 over x in [8,80), y in [8,56); B = 0.4 over x in [48,120), y in
/// [16,64). Same fixture shape as `mmm-core`'s own `blend_tests::make_panels`
/// (duplicated here — that helper is private to `mmm-core`, and this is a
/// separate crate's integration test). Written to disk (for the file-based
/// reference) and returned as in-memory planar buffers (for the mock host to
/// serve to the worker over shm).
fn synth_two_panels(dir: &Path) -> (u64, u64, u64, Vec<PathBuf>, Vec<Vec<f32>>) {
    let (w, h, ch) = (128u64, 64u64, 1u64);
    let fill = |frame: &mut [f32], v: f32, x0: u64, y0: u64, x1: u64, y1: u64| {
        for y in y0..y1 {
            for x in x0..x1 {
                frame[(y * w + x) as usize] = v;
            }
        }
    };

    let mut a = vec![0f32; (w * h) as usize];
    fill(&mut a, 0.2, 8, 8, 80, 56);
    let pa = dir.join("a.xisf");
    write_xisf(&pa, w, h, ch, &a).unwrap();

    let mut b = vec![0f32; (w * h) as usize];
    fill(&mut b, 0.4, 48, 16, 120, 64);
    let pb = dir.join("b.xisf");
    write_xisf(&pb, w, h, ch, &b).unwrap();

    (w, h, ch, vec![pa, pb], vec![a, b])
}

fn blend_params(band_rows: usize) -> BlendParams {
    BlendParams {
        feather_px: 16.0,
        downsample: 1,
        band_rows,
        mode: BlendMode::Pyramid,
        roi: None,
        defect_veto: true,
        flatten: None,
    }
}

/// Spawns the real `mmm-ipc-worker` binary, drives it over real OS pipes and
/// a real POSIX shm segment through a hand-built `Aligned` job, and asserts
/// the mosaic it streams back is byte-identical to the plain file-based
/// `analyze_opts` + `blend` pipeline over the same synthesized panels.
#[test]
fn worker_blend_is_byte_identical_to_file_blend() {
    let dir = tmpdir("main");
    let (w, h, ch, paths, planar) = synth_two_panels(&dir);
    let band_rows_u32 = 16u32;
    let params = blend_params(band_rows_u32 as usize);

    // --- Reference: plain file-based analyze + blend. ---
    let ref_session_dir = dir.join("ref.mmm-session");
    let ref_session = analyze_opts(&paths, &ref_session_dir, Some(2)).unwrap();
    let ref_phot = Photometry::load(&ref_session.photometry_path()).unwrap();
    let ref_graph = OverlapGraph::load(&ref_session.overlap_graph_path()).unwrap();
    let ref_surfaces = Surfaces::load(&ref_session.surfaces_path()).ok();
    let mut reference = CaptureSink::new();
    blend(
        &ref_session,
        &ref_phot,
        ref_surfaces.as_ref(),
        &ref_graph,
        &params,
        &mut reference,
    )
    .unwrap();

    // --- Worker: the real mmm-ipc-worker binary over real pipes + shm. ---
    let slot_bytes = w * ch * band_rows_u32 as u64 * 4;
    let layout = SlotLayout {
        slot_bytes,
        input_slots: 8,
        output_slots: 2,
    };
    let shm_name = format!("/mmm-e2e-{}", std::process::id());
    let shm = ShmSegment::create(&shm_name, layout.total_bytes()).unwrap();

    let panel_descs: Vec<PanelDesc> = (0..planar.len() as u32)
        .map(|panel_id| PanelDesc {
            panel_id,
            width: w,
            height: h,
            channels: ch,
            properties: vec![],
        })
        .collect();

    let worker_session_dir = dir.join("worker.mmm-session");
    let job = InitJob {
        protocol_version: IPC_PROTOCOL_VERSION,
        shm_name: shm_name.clone(),
        slot_bytes,
        input_slots: layout.input_slots,
        output_slots: layout.output_slots,
        canvas: [w, h, ch],
        panels: panel_descs,
        mode: JobMode::Aligned,
        session_dir: worker_session_dir.to_string_lossy().into_owned(),
        params: BlendParamsWire {
            feather_px: params.feather_px,
            downsample: params.downsample,
            band_rows: band_rows_u32,
            mode: "pyramid".to_string(),
            roi: params.roi,
            defect_veto: params.defect_veto,
            flatten: params.flatten,
            surface_order: Some(2),
            seam_map: false,
        },
    };

    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mmm-ipc-worker");

    let mut child_stdin = child.stdin.take().expect("child stdin");
    let child_stdout = child.stdout.take().expect("child stdout");
    // The worker reads exactly one `Init` frame off its stdin before
    // `HostLink` takes over; `serve_over`'s loop never sends one itself (see
    // its doc comment), so the caller — us, playing the host — must.
    write_frame(&mut child_stdin, &HostMsg::Init(job.clone()))
        .expect("write Init frame to child stdin");

    let host = MockHost::serve_over(job, planar, shm, child_stdout, child_stdin);

    let status = child.wait().expect("wait on mmm-ipc-worker");
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        panic!("mmm-ipc-worker exited with {status}: {stderr}");
    }

    let (worker_geom, worker_data) = host.result();
    host.join();

    // Guards against a vacuously-true byte comparison (e.g. both sides
    // silently empty or uniformly zero). Note the photometric solve
    // (`analyze_opts`, run for real here — not the `identity_phot` shortcut
    // `mmm-core`'s own in-process blend tests use) equalizes the two flat
    // panels toward a common level, so the output is not simply a 0.2/0.4
    // split; a non-zero, non-constant result is still the right bar.
    assert!(
        reference.data.iter().any(|&v| v != 0.0),
        "reference blend produced an all-zero mosaic; the fixture or params are degenerate"
    );
    let (min, max) = reference
        .data
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    assert!(
        max - min > 1e-6,
        "reference blend is perfectly constant ({min}); fixture or params are degenerate"
    );

    assert_eq!(
        worker_geom,
        (reference.w as u64, reference.h as u64, reference.ch as u64),
        "output geometry must match between the worker and the file-based reference"
    );
    assert_eq!(
        worker_data, reference.data,
        "worker blend output must be byte-identical to the file-based blend"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// Task 10: cancellation, worker-crash robustness, and the solved-mode e2e.
// ---------------------------------------------------------------------------

/// Runs `body` on a separate thread and requires it to finish within
/// `timeout`; a hang FAILS the test (via a `panic!`) instead of hanging the
/// suite. Unlike a bare join-with-timeout, a panic inside `body` is caught
/// and re-raised on the calling thread immediately — not only after waiting
/// out `timeout` — so an ordinary assertion failure still reports promptly
/// instead of being masked by a generic "timed out" message.
fn with_watchdog<F: FnOnce() + Send + 'static>(timeout: Duration, body: F) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(_) => {
            panic!("with_watchdog: body did not complete within {timeout:?} — hang regression")
        }
    }
}

/// Spawns a background thread that kills `child` after `timeout` if it is
/// still running. Paired with `with_watchdog` for tests that hold a real
/// child process: if a bug reintroduces a hang, killing the child unblocks
/// whatever blocking read/wait the test thread is stuck in, so the test
/// still fails on a concrete assertion instead of only the generic
/// `with_watchdog` timeout. Best-effort — a child that already exited is
/// simply a no-op `kill` error, ignored.
fn kill_after(child: Arc<Mutex<Child>>, timeout: Duration) {
    thread::spawn(move || {
        thread::sleep(timeout);
        if let Ok(mut c) = child.lock() {
            let _ = c.kill();
        }
    });
}

/// Spawns the real `mmm-ipc-worker` binary and writes `job` as its `Init`
/// frame, returning the child handle plus its stdin/stdout already
/// positioned right after that frame — the same handshake
/// `worker_blend_is_byte_identical_to_file_blend` does inline above,
/// factored out for the tests below.
fn spawn_worker_with_init(job: &InitJob) -> (Child, ChildStdin, ChildStdout) {
    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mmm-ipc-worker");
    let mut child_stdin = child.stdin.take().expect("child stdin");
    let child_stdout = child.stdout.take().expect("child stdout");
    write_frame(&mut child_stdin, &HostMsg::Init(job.clone()))
        .expect("write Init frame to child stdin");
    (child, child_stdin, child_stdout)
}

/// Two flat, non-zero, distinct-valued aligned panels — real content for the
/// analyze scan to chew on, with no need for the byte-exact values the
/// cancel/crash tests below don't check.
fn flat_two_panels(w: u64, h: u64, ch: u64) -> Vec<Vec<f32>> {
    let mk = |v: f32| vec![v; (w * h * ch) as usize];
    vec![mk(0.3), mk(0.5)]
}

/// Reads and answers up to `n` `BandRequest`s from `reader`, using the same
/// reply logic as `MockHost`'s reference host (fill the shm input slot from
/// `panel_pixels`, ack with a `BandReply`). Returns the number actually
/// served — fewer than `n` only if the worker exits or errors out first. Any
/// non-`BandRequest` frame seen while waiting (e.g. `Progress`) is skipped,
/// not counted.
fn serve_n_band_requests(
    reader: &mut ChildStdout,
    writer: &mut ChildStdin,
    job: &InitJob,
    panel_pixels: &[Vec<f32>],
    shm: &ShmSegment,
    layout: SlotLayout,
    n: usize,
) -> usize {
    let mut served = 0;
    while served < n {
        match read_worker_frame(reader) {
            Ok(Some(WorkerMsg::BandRequest(req))) => {
                let panel = &panel_pixels[req.panel_id as usize];
                let desc = &job.panels[req.panel_id as usize];
                let width = desc.width;
                let channels = desc.channels;
                let rows = req.y1 - req.y0;
                let dst = shm.slice_mut(layout.input_offset(req.slot_id), channels * rows * width);
                let plane_stride = desc.height * width;
                for c in 0..channels {
                    let src_start = (c * plane_stride + req.y0 * width) as usize;
                    let src_len = (rows * width) as usize;
                    let dst_start = (c * rows * width) as usize;
                    dst[dst_start..dst_start + src_len]
                        .copy_from_slice(&panel[src_start..src_start + src_len]);
                }
                let reply = HostMsg::BandReply(mmm_core::ipc::protocol::BandReply {
                    request_id: req.request_id,
                    slot_id: req.slot_id,
                    status: 0,
                });
                write_frame(writer, &reply).expect("write BandReply");
                writer.flush().expect("flush BandReply");
                served += 1;
            }
            Ok(Some(_)) => {} // Progress or similar: not a BandRequest, keep waiting.
            Ok(None) => break, // worker exited before we served `n` requests.
            Err(e) => panic!("serve_n_band_requests: read error: {e}"),
        }
    }
    served
}

/// Builds an `Aligned`-mode `InitJob`/shm segment for a `w`×`h`×`ch` two-panel
/// job, plus flat in-memory panel content, sharing the plumbing common to
/// `cancel_midrun_stops_promptly` and `worker_crash_is_observable`.
fn aligned_two_panel_job(
    dir: &Path,
    tag: &str,
    w: u64,
    h: u64,
    ch: u64,
    band_rows: u32,
) -> (InitJob, ShmSegment, Vec<Vec<f32>>) {
    let panel_pixels = flat_two_panels(w, h, ch);
    let slot_bytes = w * ch * band_rows as u64 * 4;
    let layout = SlotLayout {
        slot_bytes,
        input_slots: 4,
        output_slots: 2,
    };
    let shm_name = format!("/mmm-e2e-{tag}-{}", std::process::id());
    let shm = ShmSegment::create(&shm_name, layout.total_bytes()).unwrap();
    let panel_descs: Vec<PanelDesc> = (0..2u32)
        .map(|panel_id| PanelDesc {
            panel_id,
            width: w,
            height: h,
            channels: ch,
            properties: vec![],
        })
        .collect();
    let job = InitJob {
        protocol_version: IPC_PROTOCOL_VERSION,
        shm_name,
        slot_bytes,
        input_slots: layout.input_slots,
        output_slots: layout.output_slots,
        canvas: [w, h, ch],
        panels: panel_descs,
        mode: JobMode::Aligned,
        session_dir: dir
            .join("worker.mmm-session")
            .to_string_lossy()
            .into_owned(),
        params: BlendParamsWire {
            feather_px: 8.0,
            downsample: 1,
            band_rows,
            mode: "pyramid".to_string(),
            roi: None,
            defect_veto: true,
            flatten: None,
            surface_order: Some(2),
            seam_map: false,
        },
    };
    (job, shm, panel_pixels)
}

/// Cross-process proof that a mid-run `Cancel` aborts the worker promptly:
/// no full mosaic is ever produced and the worker exits cleanly (code 0),
/// per `main::run`'s cancel-to-clean-exit mapping in `mmm-ipc-worker`'s
/// `main.rs` — rather than hanging, or finishing the job anyway.
///
/// The property under test does not depend on exactly how many `BandRequest`s
/// land before the worker's reader thread processes `Cancel`: `HostLink`
/// fails a request fast if it is already cancelled before being sent, and
/// wakes (with an error) any request already in flight when `Cancel` is
/// processed (see `client.rs`'s `notify_cancel`/`ReplySlot::wait`) — so this
/// test is not a race between "serve N then cancel" and the worker's actual
/// progress.
#[test]
fn cancel_midrun_stops_promptly() {
    with_watchdog(Duration::from_secs(20), || {
        let dir = tmpdir("cancel");
        let (w, h, ch) = (96u64, 64u64, 1u64);
        let band_rows = 8u32;
        let (job, shm, panel_pixels) = aligned_two_panel_job(&dir, "cancel", w, h, ch, band_rows);
        let layout = SlotLayout {
            slot_bytes: job.slot_bytes,
            input_slots: job.input_slots,
            output_slots: job.output_slots,
        };

        let (child, mut child_stdin, mut child_stdout) = spawn_worker_with_init(&job);
        let child = Arc::new(Mutex::new(child));
        kill_after(child.clone(), Duration::from_secs(15));

        // Serve a handful of BandRequests normally — enough that the worker
        // is genuinely mid-analyze — then stop cooperating and cancel.
        let served = serve_n_band_requests(
            &mut child_stdout,
            &mut child_stdin,
            &job,
            &panel_pixels,
            &shm,
            layout,
            4,
        );
        assert!(
            served > 0,
            "worker must have issued at least one BandRequest before we cancel it"
        );

        write_frame(&mut child_stdin, &HostMsg::Cancel).expect("write Cancel");
        child_stdin.flush().expect("flush Cancel");

        // Drain whatever the worker sends after `Cancel`, replying to
        // nothing further: a hung worker would block here forever without
        // the fail-fast/wake-on-cancel semantics `HostLink` now implements;
        // `with_watchdog`/`kill_after` bound that risk rather than hanging
        // the suite if a regression reintroduces it.
        let mut saw_begin = false;
        let mut saw_done = false;
        let mut saw_cancelled_error = false;
        loop {
            match read_worker_frame(&mut child_stdout) {
                Ok(Some(WorkerMsg::Begin { .. })) => saw_begin = true,
                Ok(Some(WorkerMsg::Done)) => {
                    saw_done = true;
                    break;
                }
                Ok(Some(WorkerMsg::Error { message })) => {
                    saw_cancelled_error = message.contains("cancelled");
                    break;
                }
                Ok(Some(_)) => {} // BandRequest/Progress/OutputBand: ignore, don't reply.
                Ok(None) => break, // worker closed its stdout: it has exited.
                Err(e) => panic!("post-cancel drain: read error: {e}"),
            }
        }

        let status = child
            .lock()
            .unwrap()
            .wait()
            .expect("wait on mmm-ipc-worker");

        assert!(
            status.success(),
            "a cancelled run maps to a clean exit (code 0), got {status}"
        );
        assert!(
            !saw_begin,
            "a cancelled run must never start streaming output"
        );
        assert!(!saw_done, "a cancelled run must never send Done");
        assert!(
            saw_cancelled_error,
            "a cancelled run must report Error{{message: \"cancelled\"}}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    });
}

/// Host-side isolation: a worker that dies mid-run must not hang the host.
/// Kills the worker right after `Init` (simulating a crash before it can do
/// any real work) and asserts the reference host's serving loop
/// (`MockHost::serve_over`) still returns promptly: EOF on the dead worker's
/// closed stdout ends `read_worker_frame`'s loop in `run_host`, rather than
/// blocking forever waiting for a `BandRequest` that will never come. This
/// is the isolation guarantee a real PixInsight host depends on: a crashed
/// `mmm-ipc-worker` must not hang the PCL module that spawned it.
#[test]
fn worker_crash_is_observable() {
    with_watchdog(Duration::from_secs(20), || {
        let dir = tmpdir("crash");
        let (w, h, ch) = (32u64, 32u64, 1u64);
        let band_rows = 8u32;
        let (job, shm, panel_pixels) = aligned_two_panel_job(&dir, "crash", w, h, ch, band_rows);

        let (mut child, child_stdin, child_stdout) = spawn_worker_with_init(&job);

        // Simulate a crash: kill the worker immediately, before it can have
        // served or requested anything.
        child.kill().expect("kill child");

        let host = MockHost::serve_over(job, panel_pixels, shm, child_stdout, child_stdin);
        host.join(); // Must return promptly — see the doc comment above.

        let status = child.wait().expect("wait on killed mmm-ipc-worker");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                status.signal(),
                Some(9),
                "child should have exited via SIGKILL, got {status:?}"
            );
        }
        #[cfg(not(unix))]
        assert!(
            !status.success(),
            "a killed child must not report a successful exit"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    });
}

/// Cross-process proof that the worker actually emits `Progress` frames
/// during a run: unlike `serve_n_band_requests`/`MockHost`, which silently
/// skip non-`BandRequest` frames, this test hand-serves the whole run so it
/// can count every `WorkerMsg::Progress` it sees. A regression that stops
/// wiring `HostLink::send_progress` into the analyze scan or blend sweep
/// would leave `progress_count` at 0 while the run still completes
/// successfully — so `saw_done` alone would not catch it.
#[test]
fn progress_frames_are_emitted() {
    with_watchdog(Duration::from_secs(20), || {
        let dir = tmpdir("progress");
        let (w, h, ch) = (96u64, 64u64, 1u64);
        let band_rows = 8u32;
        let (job, shm, panel_pixels) = aligned_two_panel_job(&dir, "progress", w, h, ch, band_rows);
        let layout = SlotLayout {
            slot_bytes: job.slot_bytes,
            input_slots: job.input_slots,
            output_slots: job.output_slots,
        };
        let (mut child, mut child_stdin, mut child_stdout) = spawn_worker_with_init(&job);

        let mut progress_count = 0usize;
        let mut saw_done = false;
        loop {
            match read_worker_frame(&mut child_stdout) {
                Ok(Some(WorkerMsg::BandRequest(req))) => {
                    let panel = &panel_pixels[req.panel_id as usize];
                    let desc = &job.panels[req.panel_id as usize];
                    let rows = req.y1 - req.y0;
                    let dst = shm.slice_mut(
                        layout.input_offset(req.slot_id),
                        desc.channels * rows * desc.width,
                    );
                    let plane_stride = desc.height * desc.width;
                    for c in 0..desc.channels {
                        let ss = (c * plane_stride + req.y0 * desc.width) as usize;
                        let sl = (rows * desc.width) as usize;
                        let ds = (c * rows * desc.width) as usize;
                        dst[ds..ds + sl].copy_from_slice(&panel[ss..ss + sl]);
                    }
                    write_frame(
                        &mut child_stdin,
                        &HostMsg::BandReply(mmm_core::ipc::protocol::BandReply {
                            request_id: req.request_id,
                            slot_id: req.slot_id,
                            status: 0,
                        }),
                    )
                    .unwrap();
                    child_stdin.flush().unwrap();
                }
                Ok(Some(WorkerMsg::Progress { .. })) => progress_count += 1,
                Ok(Some(WorkerMsg::Begin { .. })) => {}
                Ok(Some(WorkerMsg::OutputBand(ob))) => {
                    write_frame(
                        &mut child_stdin,
                        &HostMsg::OutputAck {
                            request_id: ob.request_id,
                        },
                    )
                    .unwrap();
                    child_stdin.flush().unwrap();
                }
                Ok(Some(WorkerMsg::Done)) => {
                    saw_done = true;
                    break;
                }
                Ok(Some(WorkerMsg::Error { message })) => panic!("worker error: {message}"),
                Ok(None) => break,
                Err(e) => panic!("read error: {e}"),
            }
        }
        child.wait().unwrap();
        assert!(saw_done, "run should complete");
        assert!(
            progress_count > 0,
            "expected at least one Progress frame, got {progress_count}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    });
}

/// Two raw solved panels with distinct geometry, rotation, and sky offset —
/// enough overlap to exercise the photometric solve without depending on any
/// analytic "ground truth" scene: this test only checks that the IPC and
/// file-based paths agree with *each other*, bit for bit, not against a
/// known sky (contrast `mmm-core`'s own `solved_input_pipeline_recovers_ground_truth`).
/// Pixel content is an arbitrary deterministic pattern, always > 0 (the
/// no-data sentinel is exactly `0.0`).
fn write_two_solved_panels(dir: &Path) -> (Vec<PathBuf>, Vec<Vec<f32>>) {
    struct Spec {
        w: u64,
        h: u64,
        crval: [f64; 2],
        rot_deg: f64,
    }
    let scale_deg = 1.0e-3_f64;
    let ch = 1u64;
    let specs = [
        Spec {
            w: 64,
            h: 48,
            crval: [10.0, 0.0],
            rot_deg: 0.0,
        },
        Spec {
            w: 60,
            h: 52,
            // Offset a bit over half of panel 0's width east and a little
            // north, so the two footprints overlap by roughly 40%.
            crval: [10.0 + 64.0 * scale_deg * 0.55, 8.0 * scale_deg],
            rot_deg: 6.0,
        },
    ];

    let mut paths = Vec::new();
    let mut planars = Vec::new();
    for (k, spec) in specs.iter().enumerate() {
        let (sr, cr) = spec.rot_deg.to_radians().sin_cos();
        let cd = [
            [-scale_deg * cr, -scale_deg * sr],
            [-scale_deg * sr, scale_deg * cr],
        ];
        let refimg = [spec.w as f64 / 2.0, spec.h as f64 / 2.0];
        let (w, h) = (spec.w, spec.h);
        let mut planes = vec![0f32; (w * h) as usize];
        for j in 0..h {
            for i in 0..w {
                planes[(j * w + i) as usize] =
                    (((i * 7 + j * 13 + k as u64 * 41) % 97) as f32) / 97.0 + 0.01;
            }
        }
        let wcs = SynthWcs {
            crval: spec.crval,
            refimg,
            cd,
        };
        let path = dir.join(format!("solved_{k}.xisf"));
        write_xisf_solved(&path, w, h, ch, &planes, &wcs).unwrap();
        paths.push(path);
        planars.push(planes);
    }
    (paths, planars)
}

/// Solved-mode byte-identical e2e: two raw plate-solved panels, reprojected
/// and blended two ways — the file-based `analyze_input(Solved)` + `blend`
/// pipeline (`reproject_panel`, mmap'd source planes) and the real
/// `mmm-ipc-worker` binary's `JobMode::Solved` path (`analyze_ipc_solved` →
/// `reproject_from_reader`, planes materialized from `PanelReader::open_ipc`
/// bands served by the reference host) — asserted bit-for-bit identical.
/// Both paths share the same `reproject_core`/blend math once their source
/// planes are in hand, so any divergence here is a real bug in the
/// IPC-served path, not floating-point noise.
#[test]
fn solved_mode_reprojection_matches_file() {
    with_watchdog(Duration::from_secs(30), || {
        let dir = tmpdir("solved");
        let (paths, planars) = write_two_solved_panels(&dir);
        let band_rows_u32 = 8u32;
        let params = blend_params(band_rows_u32 as usize);

        // --- Reference: file-based analyze(Solved) + blend. ---
        let ref_session_dir = dir.join("ref.mmm-session");
        let ref_session =
            analyze_input(&paths, &ref_session_dir, Some(2), InputSelect::Solved).unwrap();
        let ref_phot = Photometry::load(&ref_session.photometry_path()).unwrap();
        let ref_graph = OverlapGraph::load(&ref_session.overlap_graph_path()).unwrap();
        let ref_surfaces = Surfaces::load(&ref_session.surfaces_path()).ok();
        let mut reference = CaptureSink::new();
        blend(
            &ref_session,
            &ref_phot,
            ref_surfaces.as_ref(),
            &ref_graph,
            &params,
            &mut reference,
        )
        .unwrap();
        assert!(
            reference.data.iter().any(|&v| v != 0.0),
            "reference solved blend produced an all-zero mosaic; fixture is degenerate"
        );

        // --- Worker: the real mmm-ipc-worker binary, JobMode::Solved. ---
        // Each panel's own raw width/height/channels and properties, read
        // back through the same XISF header parser the file path uses:
        // `analyze_ipc_solved` must see exactly what `analyze_solved` sees.
        let panel_headers: Vec<(u64, u64, u64, Vec<mmm_core::formats::XisfProperty>)> = paths
            .iter()
            .map(|p| {
                let xp = XisfPanel::open(p).unwrap();
                (
                    xp.width(),
                    xp.height(),
                    xp.channels(),
                    xp.header().properties.clone(),
                )
            })
            .collect();
        let max_w = panel_headers.iter().map(|(w, ..)| *w).max().unwrap();
        let ch = panel_headers[0].2;
        // A band slot must be big enough for the wider of (a) an input
        // band (one raw panel's own width) and (b) an output band (the
        // *reprojected mosaic frame's* width, which can exceed any single
        // panel's width) — compute the same frame `analyze_ipc_solved` will
        // choose so the shm segment is sized correctly up front.
        let models: Vec<WcsModel> = panel_headers
            .iter()
            .map(|(w, h, _, props)| WcsModel::from_properties(props, *w, *h).unwrap())
            .collect();
        let frame_w = choose_frame(&models).width;
        let slot_bytes = max_w.max(frame_w) * ch * band_rows_u32 as u64 * 4;
        let layout = SlotLayout {
            slot_bytes,
            input_slots: 8,
            output_slots: 2,
        };
        let shm_name = format!("/mmm-e2e-solved-{}", std::process::id());
        let shm = ShmSegment::create(&shm_name, layout.total_bytes()).unwrap();
        let panel_descs: Vec<PanelDesc> = panel_headers
            .iter()
            .enumerate()
            .map(|(id, (w, h, c, props))| PanelDesc {
                panel_id: id as u32,
                width: *w,
                height: *h,
                channels: *c,
                properties: props.clone(),
            })
            .collect();

        let worker_session_dir = dir.join("worker.mmm-session");
        let job = InitJob {
            protocol_version: IPC_PROTOCOL_VERSION,
            shm_name: shm_name.clone(),
            slot_bytes,
            input_slots: layout.input_slots,
            output_slots: layout.output_slots,
            // Unused by `JobMode::Solved` (the worker derives its own canvas
            // from the frame it chooses); left as a placeholder.
            canvas: [0, 0, ch],
            panels: panel_descs,
            mode: JobMode::Solved,
            session_dir: worker_session_dir.to_string_lossy().into_owned(),
            params: BlendParamsWire {
                feather_px: params.feather_px,
                downsample: params.downsample,
                band_rows: band_rows_u32,
                mode: "pyramid".to_string(),
                roi: params.roi,
                defect_veto: params.defect_veto,
                flatten: params.flatten,
                surface_order: Some(2),
                seam_map: false,
            },
        };

        let (mut child, child_stdin, child_stdout) = spawn_worker_with_init(&job);
        let host = MockHost::serve_over(job, planars, shm, child_stdout, child_stdin);

        let status = child.wait().expect("wait on mmm-ipc-worker");
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut stderr);
            }
            panic!("mmm-ipc-worker exited with {status}: {stderr}");
        }

        let (worker_geom, worker_data) = host.result();
        host.join();

        assert_eq!(
            worker_geom,
            (reference.w as u64, reference.h as u64, reference.ch as u64),
            "output geometry must match between the worker and the file-based reference"
        );
        assert_eq!(
            worker_data, reference.data,
            "worker solved-mode blend output must be byte-identical to the file-based blend"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    });
}

#[test]
fn probe_frame_prints_choose_frame_geometry() {
    use std::io::Write;
    let dir = tmpdir("probe");
    let (paths, _planars) = write_two_solved_panels(&dir);

    // Build the expected frame the same way analyze_ipc_solved will.
    let headers: Vec<(u64, u64, u64, Vec<mmm_core::formats::XisfProperty>)> = paths
        .iter()
        .map(|p| {
            let xp = XisfPanel::open(p).unwrap();
            (
                xp.width(),
                xp.height(),
                xp.channels(),
                xp.header().properties.clone(),
            )
        })
        .collect();
    let models: Vec<WcsModel> = headers
        .iter()
        .map(|(w, h, _, props)| WcsModel::from_properties(props, *w, *h).unwrap())
        .collect();
    let frame = choose_frame(&models);
    let ch = headers[0].2;

    // Build the probe Init JSON (panels with properties; mode Solved).
    let panels: Vec<PanelDesc> = headers
        .iter()
        .enumerate()
        .map(|(id, (w, h, c, props))| PanelDesc {
            panel_id: id as u32,
            width: *w,
            height: *h,
            channels: *c,
            properties: props.clone(),
        })
        .collect();
    let job = InitJob {
        protocol_version: IPC_PROTOCOL_VERSION,
        shm_name: String::new(),
        slot_bytes: 0,
        input_slots: 0,
        output_slots: 0,
        canvas: [0, 0, ch],
        panels,
        mode: JobMode::Solved,
        session_dir: String::new(),
        params: BlendParamsWire {
            feather_px: 0.0,
            downsample: 1,
            band_rows: 8,
            mode: "pyramid".into(),
            roi: None,
            defect_veto: true,
            flatten: None,
            surface_order: Some(2),
            seam_map: false,
        },
    };
    let json = serde_json::to_string(&job).unwrap();

    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = std::process::Command::new(exe)
        .arg("--probe-frame")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "probe exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.trim(),
        format!("{} {} {}", frame.width, frame.height, ch)
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// `--probe-panels`: paths in, per-panel geometry + solved frame out —
/// the metadata pass a Files-mode PixInsight host runs instead of opening
/// panels itself (PROTOCOL.md §11).
#[test]
fn probe_panels_reports_geometry_and_frame() {
    use std::io::Write;
    let dir = tmpdir("probe-panels");
    let (paths, _planars) = write_two_solved_panels(&dir);

    // Expected frame, computed exactly as the worker will.
    let headers: Vec<(u64, u64, u64, Vec<mmm_core::formats::XisfProperty>)> = paths
        .iter()
        .map(|p| {
            let xp = XisfPanel::open(p).unwrap();
            (
                xp.width(),
                xp.height(),
                xp.channels(),
                xp.header().properties.clone(),
            )
        })
        .collect();
    let models: Vec<WcsModel> = headers
        .iter()
        .map(|(w, h, _, props)| WcsModel::from_properties(props, *w, *h).unwrap())
        .collect();
    let frame = choose_frame(&models);

    let req = serde_json::json!({
        "paths": paths.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>(),
        "input_select": "Auto",
    });

    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = std::process::Command::new(exe)
        .arg("--probe-panels")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(req.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "probe exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reply: mmm_core::ipc::protocol::PanelProbeReply =
        serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply.panels.len(), 2);
    assert_eq!(
        (
            reply.panels[0].width,
            reply.panels[0].height,
            reply.panels[0].channels
        ),
        (headers[0].0, headers[0].1, headers[0].2)
    );
    assert_eq!(
        reply.frame,
        Some([frame.width, frame.height, headers[0].2])
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
