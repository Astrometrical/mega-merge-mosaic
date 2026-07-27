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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mmm_core::analyze::analyze_opts;
use mmm_core::blend::{BlendMode, BlendParams, RowSink, blend};
use mmm_core::ipc::IPC_PROTOCOL_VERSION;
use mmm_core::ipc::protocol::{BlendParamsWire, HostMsg, InitJob, JobMode, PanelDesc, write_frame};
use mmm_core::ipc::shm::{ShmSegment, SlotLayout};
use mmm_core::ipc::testhost::MockHost;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::surfaces::Surfaces;
use mmm_core::synth::write_xisf;

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
    let shm_name = format!("/mmm-ipc-worker-e2e-{}", std::process::id());
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
