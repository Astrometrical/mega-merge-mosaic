//! `mmm-ipc-worker` — the standalone worker process spawned by a host (the
//! PixInsight PCL module) to drive the `mmm-core` pipeline over shared
//! memory instead of files. See `mmm_core::ipc` for the transport.

use std::io::Write;
use std::path::PathBuf;

use mmm_core::analyze::{analyze_input, analyze_ipc_aligned, analyze_ipc_solved, solved_frame};
use mmm_core::blend::blend_with_source;
use mmm_core::ipc::IPC_PROTOCOL_VERSION;
use mmm_core::ipc::client::HostLink;
use mmm_core::ipc::protocol::{HostMsg, JobMode, read_host_frame};
use mmm_core::ipc::sink::ShmRowSink;
use mmm_core::ipc::source::IpcSource;
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::surfaces::Surfaces;

fn main() {
    let probe = std::env::args().any(|a| a == "--probe-frame");
    let result = if probe { probe_frame() } else { run() };
    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "mmm-ipc-worker: {e}");
        std::process::exit(1);
    }
}

/// `--probe-frame`: read an `InitJob`-shaped JSON object on stdin, build the
/// WCS models from each panel's `properties` via
/// [`mmm_core::analyze::solved_frame`], and print `"{w} {h} {ch}"`. Lets a
/// host size the shm output slots for solved mode without duplicating the
/// frame math (see PROTOCOL.md §11 / spec §15) — `solved_frame` is the same
/// helper `analyze_ipc_solved` uses, so this preflight enforces the same
/// cross-panel channel-count check the real analyze stage does.
fn probe_frame() -> mmm_core::Result<()> {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .map_err(|e| mmm_core::Error::compute(format!("reading probe JSON from stdin: {e}")))?;
    let job: mmm_core::ipc::protocol::InitJob = serde_json::from_str(&buf)
        .map_err(|e| mmm_core::Error::compute(format!("parsing probe InitJob JSON: {e}")))?;
    if job.panels.is_empty() {
        return Err(mmm_core::Error::compute("probe-frame: no panels"));
    }
    let (_models, frame, ch) = solved_frame(&job.panels).map_err(mmm_core::Error::compute)?;
    writeln!(std::io::stdout(), "{} {} {}", frame.width, frame.height, ch)
        .map_err(|e| mmm_core::Error::compute(format!("probe-frame: writing stdout: {e}")))?;
    Ok(())
}

/// Reads the one `HostMsg::Init` frame off stdin, drives analyze then blend
/// for that job over the IPC transport, and streams the blended mosaic back
/// to the host — mirroring the file-based CLI path exactly, except every
/// panel read/write crosses the [`HostLink`] instead of the filesystem.
fn run() -> mmm_core::Result<()> {
    let stdin = std::io::stdin();
    let init_frame = read_host_frame(&mut stdin.lock())
        .map_err(|e| mmm_core::Error::compute(format!("reading Init frame from stdin: {e}")))?;
    let init = match init_frame {
        Some(HostMsg::Init(job)) => job,
        _ => return Err(mmm_core::Error::compute("expected Init as the first frame")),
    };
    if init.protocol_version != IPC_PROTOCOL_VERSION {
        return Err(mmm_core::Error::compute("protocol version mismatch"));
    }

    // Capture what's needed before `init` is moved into `HostLink::start`.
    let band_rows = init.params.band_rows as usize;
    let session_dir = PathBuf::from(&init.session_dir);
    let params = init.params.to_params();
    let surface_order = init.params.surface_order;
    let mode = init.mode.clone();

    // `std::io::stdin()` is a handle onto the same global buffered stdin the
    // `read_host_frame` call above just read the `Init` frame from, so a
    // fresh handle here picks up exactly where that read left off — safe to
    // hand to `HostLink`'s reader thread.
    let link = HostLink::start(
        init,
        Box::new(std::io::stdin()),
        Box::new(std::io::stdout()),
    )?;

    // Analyze + blend as one fallible stage: once the host sends `Cancel`,
    // `HostLink::request_band`/`send_output_band` start failing fast (see
    // their doc comments), so a mid-analyze or mid-blend cancellation
    // surfaces here as a normal `Err` that unwound the stage promptly rather
    // than a hang. The `match` below (comparing that outcome against
    // `link.is_cancelled()`) is what turns *that specific* error into a
    // clean exit, while any other error still reaches `main`'s non-zero exit
    // path unchanged.
    let result: mmm_core::Result<()> = (|| {
        let session = match mode {
            JobMode::Aligned => {
                analyze_ipc_aligned(link.clone(), &session_dir, band_rows, surface_order)?
            }
            JobMode::Solved => {
                analyze_ipc_solved(link.clone(), &session_dir, band_rows, surface_order)?
            }
            JobMode::Files {
                paths,
                input_select,
            } => {
                let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
                analyze_input(
                    &paths,
                    &session_dir,
                    surface_order,
                    input_select.to_input_select(),
                )?
            }
        };

        if link.is_cancelled() {
            return Err(mmm_core::Error::compute("cancelled"));
        }

        let phot = Photometry::load(&session.photometry_path())?;
        let graph = OverlapGraph::load(&session.overlap_graph_path())?;
        let surfaces = Surfaces::load(&session.surfaces_path()).ok();
        let source = IpcSource::new(link.clone(), band_rows);
        let mut sink = ShmRowSink::new(link.clone());
        blend_with_source(
            &session,
            &phot,
            surfaces.as_ref(),
            &graph,
            &params,
            &source,
            &mut sink,
        )?;

        // Sends the final `Done`; `ShmRowSink::finish` is deliberately a
        // no-op (see its doc comment), so this is the one and only
        // completion frame on the success path.
        link.finish_ok()
    })();

    match result {
        Ok(()) => Ok(()),
        // A cancelled run is a clean exit (code 0), not a failure: the host
        // asked for this, so `main`'s stderr+exit(1) path is reserved for
        // errors the host didn't cause.
        Err(_) if link.is_cancelled() => {
            link.finish_err("cancelled");
            Ok(())
        }
        Err(e) => Err(e),
    }
}
