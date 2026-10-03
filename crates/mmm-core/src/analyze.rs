//! Analyze stage: one streaming pass per panel producing the L8 summary,
//! content bbox, and per-channel statistics, persisted into a session dir,
//! followed by the overlap-graph build over the collected summaries and the
//! photometric solve (per-edge fits + global per-panel corrections).
//!
//! Panels are scanned in parallel (rayon; the work is I/O-bound). Each scan is
//! a single sequential pass over the mmap'd planes: per image row the channel
//! rows are read in step, coverage (all channels nonzero) is accumulated into
//! one L8 cell-row accumulator, flushed every 8 rows. No dense canvas-sized
//! allocations — only L8-resolution buffers per panel.
//!
//! ## Input kinds (phase 5)
//!
//! Two kinds of input are accepted, selected by [`InputSelect`]:
//!
//! - **Aligned**: pre-registered full-canvas frames (MosaicByCoordinates
//!   output) sharing one geometry — the historical path, unchanged.
//! - **Solved**: unaligned panels each carrying a PixInsight astrometric
//!   solution. The align stage loads every panel's [`WcsModel`], chooses a
//!   fresh [`crate::align::MosaicFrame`] via
//!   [`choose_frame`], reprojects each panel into
//!   the session cache (`panels/<id>/aligned.bin`, rayon-parallel rows —
//!   see [`crate::align::reproject_panel`]), and the per-panel scan then
//!   proceeds over [`PanelReader`] exactly as for aligned input.
//!
//! **Auto-detection** (`InputSelect::Auto`, the CLI default): ≥ 2 panels
//! sharing one header geometry are scanned as aligned; if every panel's
//! covered fraction then stays below [`ALIGNED_MAX_COVERAGE`] they *are*
//! aligned, otherwise the set is re-dispatched as solved (registered mosaic
//! panels cover a small part of their union canvas; raw solved frames cover
//! ~100% of their own). Mixed geometries or a single input go straight to
//! solved. The geometry signal is checked first because it is header-cheap;
//! the coverage rule costs one wasted scan in the rare same-geometry-raw
//! case. One combination stays undetectable: same-geometry *raw* panels that
//! somehow cover < 50% each would scan as aligned — `--input solved`
//! overrides. Conversely a 2-panel aligned mosaic whose panels cover ≥ 50%
//! of the canvas re-dispatches to solved (and errors without solutions) —
//! `--input aligned` overrides.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use crate::align::{MosaicFrame, choose_frame, reproject_from_reader, reproject_panel};
use crate::astrometry::{LinearWcs, WcsModel, describe_unsolved};
use crate::formats::InputPanel;
use crate::ipc::client::HostLink;
use crate::ipc::protocol::{PanelDesc, PanelProbeGeom, PanelProbeReply};
use crate::overlap::OverlapGraph;
use crate::panel_reader::{PanelReader, PanelStorage};
use crate::photometry::GainMode;
use crate::reference::ReferenceFrame;
use crate::session::{InputKind, PanelMeta, Session};
use crate::summary::{BLOCK, L8Summary};
use crate::{Error, Result};

/// How the caller wants the input panels interpreted (`--input` on the CLI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputSelect {
    /// Detect the kind (module docs); the default.
    #[default]
    Auto,
    /// Force the aligned full-canvas path (all panels must share the canvas
    /// geometry).
    Aligned,
    /// Force the solved path (every panel must carry an astrometric
    /// solution).
    Solved,
}

/// Auto-detection bound: a same-geometry panel set in which any panel covers
/// at least this fraction of the canvas is not believed to be an aligned
/// mosaic and is re-dispatched as solved.
pub const ALIGNED_MAX_COVERAGE: f64 = 0.5;

struct PanelScan {
    meta: PanelMeta,
    summary: L8Summary,
    canvas: (u64, u64, u64),
}

/// How an imager reads a channel count: `1 (mono)`, `3 (RGB)`, else the bare
/// number.
fn describe_channels(ch: u64) -> String {
    match ch {
        1 => "1 (mono)".to_string(),
        3 => "3 (RGB)".to_string(),
        n => n.to_string(),
    }
}

/// Verify that every input has the same channel count, given `(label,
/// channels)` pairs in input order; returns the shared count.
///
/// Every stage downstream assumes one channel count for the whole set: the
/// canvas carries a single `channels`, the IPC host sizes its shared-memory
/// slots from the first panel's, and the photometric solve is per channel
/// across panels. A mono/colour mix therefore has to be refused up front —
/// otherwise it surfaces far away from its cause (auto-detection blaming a
/// missing plate solution, or a band-size mismatch mid-scan).
///
/// The message names the first input of each distinct channel count.
fn check_uniform_channels<I>(panels: I) -> std::result::Result<u64, String>
where
    I: IntoIterator<Item = (String, u64)>,
{
    // (channel count, first input with it) in first-seen order.
    let mut groups: Vec<(u64, String)> = Vec::new();
    for (label, ch) in panels {
        if !groups.iter().any(|&(c, _)| c == ch) {
            groups.push((ch, label));
        }
    }
    match groups.as_slice() {
        [] => Err("no input panels given".to_string()),
        [(ch, _)] => Ok(*ch),
        _ => {
            let list: Vec<String> = groups
                .iter()
                .map(|(c, label)| format!("{label} has {}", describe_channels(*c)))
                .collect();
            Err(format!(
                "input panels must all have the same number of channels, but {} — mono \
                 and colour panels cannot be mixed in one mosaic; blend each set separately",
                list.join(", ")
            ))
        }
    }
}

/// Analyze `paths` into a session at `session_dir` with the default residual
/// surface order (quadratic) on the aligned path. See [`analyze_opts`].
pub fn analyze(paths: &[PathBuf], session_dir: &Path) -> Result<Session> {
    analyze_opts(paths, session_dir, Some(2))
}

/// Analyze pre-aligned full-canvas panels (the historical entry point —
/// equivalent to [`analyze_input`] with [`InputSelect::Aligned`]): writes
/// `session.json`, `panels/<id>/summary.bin`, `analysis/overlap_graph.json`,
/// `analysis/photometry.json`, and (unless `surface_order` is `None`)
/// `analysis/surfaces.json`, returns the populated [`Session`].
///
/// `surface_order = None` disables the residual surface fit entirely and
/// removes any stale `surfaces.json` so a later blend won't apply it.
pub fn analyze_opts(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
) -> Result<Session> {
    analyze_input(paths, session_dir, surface_order, InputSelect::Aligned)
}

/// Analyze `paths` into a session at `session_dir`, interpreting the inputs
/// per `input` (module docs). All artifacts and downstream stages are
/// identical between the kinds; solved input additionally persists the chosen
/// mosaic frame and the reprojection caches in the session directory.
pub fn analyze_input(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    input: InputSelect,
) -> Result<Session> {
    analyze_input_progress(paths, session_dir, surface_order, input, None)
}

/// [`analyze_input`] with an explicit photometric [`GainMode`] and
/// auto-detected input kind. `GainMode::Unity` pins every panel gain at 1 and
/// solves offsets only — for photometrically homogeneous mosaics.
pub fn analyze_gain(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    gain: GainMode,
) -> Result<Session> {
    analyze_full(
        paths,
        session_dir,
        surface_order,
        gain,
        InputSelect::Auto,
        None,
        None,
    )
}

/// Coarse per-panel progress observer for the file-based analyze pipeline:
/// `(stage, done, total)` with stage `"reproject"` (solved input only) or
/// `"analyze"`. Must be `Sync` — the scan stage invokes it from parallel
/// workers. Matches the stage vocabulary of the IPC `Progress` frames so a
/// host can render both paths identically.
pub type AnalyzeProgress<'a> = &'a (dyn Fn(&str, u64, u64) + Sync);

/// [`analyze_input`] with an optional progress observer (module docs on
/// [`AnalyzeProgress`]). The IPC worker uses this in Files mode to forward
/// per-panel progress to the host; `analyze_input` itself passes `None`.
pub fn analyze_input_progress(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    input: InputSelect,
    progress: Option<AnalyzeProgress>,
) -> Result<Session> {
    analyze_full(
        paths,
        session_dir,
        surface_order,
        GainMode::Fit,
        input,
        progress,
        None,
    )
}

/// The full-parameter analyze entry point: [`analyze_input_progress`] plus an
/// explicit photometric [`GainMode`] and an optional shared reference frame.
///
/// With `reference = Some(frame)` the session adopts that shared
/// [`ReferenceFrame`] instead of deriving its own frame (solved input) or
/// taking the canvas as given (aligned input), after checking the group fits
/// it — see [`crate::reference`]. The session then records `frame_imposed`.
pub fn analyze_full(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    gain: GainMode,
    input: InputSelect,
    progress: Option<AnalyzeProgress>,
    reference: Option<&ReferenceFrame>,
) -> Result<Session> {
    if paths.is_empty() {
        return Err(Error::format(session_dir, "no input panels given"));
    }
    // One channel count across the whole set, before anything dispatches on
    // mode: a mono/colour mix otherwise surfaces far from its cause (Auto
    // sees differing geometries, re-dispatches as solved, and blames a
    // missing plate solution). Files that fail to open are skipped here so
    // the stage below still reports them with its own richer message.
    let opened: Vec<(String, u64)> = paths
        .par_iter()
        .filter_map(|p| {
            InputPanel::open(p)
                .ok()
                .map(|x| (p.display().to_string(), x.channels()))
        })
        .collect();
    if let Err(reason) = check_uniform_channels(opened) {
        return Err(Error::compute(reason));
    }
    // Header facts (geometry + canvas WCS) for the Auto rule and the
    // reference pre-checks; header-only opens, so cheap even for 2 GB panels.
    let facts: Vec<(u64, u64, Option<LinearWcs>)> =
        if input == InputSelect::Auto || reference.is_some() {
            let mut v = Vec::with_capacity(paths.len());
            for path in paths {
                let x = InputPanel::open(path)?;
                v.push((x.width(), x.height(), x.linear_wcs()));
            }
            v
        } else {
            Vec::new()
        };
    let same_geometry =
        !facts.is_empty() && facts.iter().all(|f| (f.0, f.1) == (facts[0].0, facts[0].1));
    // Auto reads a set as aligned with ≥ 2 panels of one geometry whose
    // canvas solutions agree (registered canvases; raw panels from one
    // camera share a geometry but not a solution), or as a single panel
    // against an aligned reference whose canvas it matches (a one-frame Ha
    // group joining an LRGB set). The coverage rule in `analyze_aligned`
    // remains a backstop for registered canvases without any WCS.
    let auto_reads_aligned = input == InputSelect::Auto
        && same_geometry
        && if paths.len() >= 2 {
            let wcs: Vec<Option<LinearWcs>> = facts.iter().map(|f| f.2.clone()).collect();
            crate::reference::same_geometry_reads_aligned(&wcs, facts[0].0, facts[0].1)
        } else {
            matches!(
                reference,
                Some(ReferenceFrame::Aligned { width, height, .. })
                    if (facts[0].0, facts[0].1) == (*width, *height)
            )
        };

    // Header-knowable reference mismatches fail before any pixel scan — a
    // registered set can be tens of GB: a canvas geometry that differs from
    // an aligned reference, or a solved reference forced onto aligned input.
    // (A solved reference under Auto with same-geometry panels is left to
    // the scan: the ≥ 50 % coverage rule may still re-dispatch to solved.)
    if let Some(r) = reference {
        let geoms: Vec<(u64, u64)> = facts.iter().map(|f| (f.0, f.1)).collect();
        let forced_aligned = input == InputSelect::Aligned;
        let reads_aligned = forced_aligned || auto_reads_aligned;
        match r {
            ReferenceFrame::Aligned { width, height, .. } if reads_aligned => {
                if let Some(g) = geoms.first() {
                    crate::reference::check_aligned(*g, None, *width, *height, None)
                        .map_err(|reason| Error::format(session_dir, reason))?;
                }
            }
            ReferenceFrame::Solved { .. } if forced_aligned => {
                return Err(Error::format(session_dir, SOLVED_REFERENCE_ON_ALIGNED));
            }
            _ => {}
        }
    }
    match input {
        InputSelect::Aligned => analyze_aligned(
            paths,
            session_dir,
            surface_order,
            gain,
            false,
            progress,
            reference,
        ),
        InputSelect::Solved => {
            analyze_solved(paths, session_dir, surface_order, gain, progress, reference)
        }
        InputSelect::Auto => {
            if auto_reads_aligned {
                analyze_aligned(
                    paths,
                    session_dir,
                    surface_order,
                    gain,
                    true,
                    progress,
                    reference,
                )
            } else {
                analyze_solved(paths, session_dir, surface_order, gain, progress, reference)
            }
        }
    }
}

/// Kind-mismatch message: an aligned reference frame met solved input.
const ALIGNED_REFERENCE_ON_SOLVED: &str = "the reference frame is an aligned canvas but these panels were read as solved raw \
     panels: pass `--input aligned` if they are registered full-canvas frames, or derive a \
     solved reference from the raw panels";

/// Kind-mismatch message: a solved reference frame met aligned input.
const SOLVED_REFERENCE_ON_ALIGNED: &str = "the reference frame is a solved mosaic frame but these panels were read as aligned \
     full-canvas frames: pass `--input solved` if they are raw plate-solved panels, or derive \
     an aligned reference from the registered canvases";

/// Reports `done`/`total` for a stage through an optional observer.
fn report(progress: Option<AnalyzeProgress>, stage: &str, done: u64, total: u64) {
    if let Some(p) = progress {
        p(stage, done, total);
    }
}

/// The aligned full-canvas path. With `auto` set, the coverage rule applies
/// after the scan: any panel covering ≥ [`ALIGNED_MAX_COVERAGE`] of the
/// canvas re-dispatches the whole set to [`analyze_solved`].
fn analyze_aligned(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    gain: GainMode,
    auto: bool,
    progress: Option<AnalyzeProgress>,
    reference: Option<&ReferenceFrame>,
) -> Result<Session> {
    let mut session = Session::create(session_dir)?;

    report(progress, "analyze", 0, paths.len() as u64);
    // Claim + report under one lock: with a bare atomic claim a later
    // completion could report before an earlier one, leaving a non-final
    // count (e.g. 1/2 after 2/2) as the observer's last event.
    let scanned = std::sync::Mutex::new(0u64);
    let scans: Vec<PanelScan> = paths
        .par_iter()
        .enumerate()
        .map(|(id, path)| {
            let scan = scan_panel(id, path)?;
            {
                let mut done = scanned.lock().expect("progress lock poisoned");
                *done += 1;
                report(progress, "analyze", *done, paths.len() as u64);
            }
            Ok(scan)
        })
        .collect::<Result<_>>()?;

    let canvas = scans[0].canvas;
    for scan in &scans {
        if scan.canvas != canvas {
            return Err(Error::format(
                &scan.meta.path,
                format!(
                    "canvas geometry {}x{}x{} differs from {}x{}x{} of {}",
                    scan.canvas.0,
                    scan.canvas.1,
                    scan.canvas.2,
                    canvas.0,
                    canvas.1,
                    canvas.2,
                    scans[0].meta.path.display()
                ),
            ));
        }
    }

    if auto
        && scans
            .iter()
            .any(|s| s.meta.nonzero_frac >= ALIGNED_MAX_COVERAGE)
    {
        tracing::info!(
            "panels share one geometry but cover >= {:.0}% of it — treating input as solved \
             raw panels (--input aligned overrides)",
            ALIGNED_MAX_COVERAGE * 100.0
        );
        return analyze_solved(paths, session_dir, surface_order, gain, progress, reference);
    }

    if let Some(r) = reference {
        match r {
            ReferenceFrame::Aligned { width, height, wcs } => {
                let panel_wcs = InputPanel::open(&paths[0])?.linear_wcs();
                crate::reference::check_aligned(
                    (canvas.0, canvas.1),
                    panel_wcs.as_ref(),
                    *width,
                    *height,
                    wcs.as_ref(),
                )
                .map_err(|reason| Error::format(session_dir, reason))?;
                session.frame_imposed = true;
            }
            ReferenceFrame::Solved { .. } => {
                return Err(Error::format(session_dir, SOLVED_REFERENCE_ON_ALIGNED));
            }
        }
    }

    session.canvas = canvas;
    finish_session(session, scans, surface_order, gain)
}

/// The solved path: astrometric models → mosaic frame → reprojection caches →
/// the identical scan/graph/photometry pipeline over [`PanelReader`].
fn analyze_solved(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    gain: GainMode,
    progress: Option<AnalyzeProgress>,
    reference: Option<&ReferenceFrame>,
) -> Result<Session> {
    let mut session = Session::create(session_dir)?;

    // Every input must yield a model; report all unusable files at once.
    let mut panels: Vec<(InputPanel, WcsModel)> = Vec::with_capacity(paths.len());
    let mut errors: Vec<String> = Vec::new();
    for path in paths {
        match InputPanel::open(path) {
            Ok(p) => match p.wcs_model() {
                Ok(m) => panels.push((p, m)),
                Err(reason) => errors.push(format!("{}: {reason}", path.display())),
            },
            Err(e) => errors.push(e.to_string()),
        }
    }
    if !errors.is_empty() {
        return Err(Error::format(
            session_dir,
            format!(
                "solved input requires an astrometric solution in every panel:\n  {}",
                errors.join("\n  ")
            ),
        ));
    }
    let ch = check_uniform_channels(
        panels
            .iter()
            .map(|(p, _)| (p.path().display().to_string(), p.channels())),
    )
    .map_err(Error::compute)?;

    let models: Vec<WcsModel> = panels.iter().map(|(_, m)| m.clone()).collect();
    let frame = match reference {
        None => choose_frame(&models),
        Some(ReferenceFrame::Solved { frame }) => {
            crate::reference::check_footprints(
                panels
                    .iter()
                    .map(|(p, m)| (p.path().display().to_string(), m)),
                frame,
            )
            .map_err(|reason| Error::format(session_dir, reason))?;
            frame.clone()
        }
        Some(ReferenceFrame::Aligned { .. }) => {
            return Err(Error::format(session_dir, ALIGNED_REFERENCE_ON_SOLVED));
        }
    };
    let canvas = (frame.width, frame.height, ch);
    tracing::info!(
        "mosaic frame: {}x{} px, {:.3}\"/px, center RA {:.4} Dec {:+.4}",
        frame.width,
        frame.height,
        frame.scale_deg * 3600.0,
        frame.crval[0],
        frame.crval[1]
    );

    // Align stage: reproject each panel into the session cache. Sequential
    // over panels — reproject_panel is already rayon-parallel over output
    // rows, and one panel's working set at a time keeps memory bounded.
    let t_align = Instant::now();
    report(progress, "reproject", 0, panels.len() as u64);
    let mut aligned = Vec::with_capacity(panels.len());
    for (id, (panel, model)) in panels.iter().enumerate() {
        let t = Instant::now();
        let out_dir = session.dir.join("panels").join(id.to_string());
        let ap = reproject_panel(panel, model, &frame, &out_dir)?;
        report(progress, "reproject", (id as u64) + 1, panels.len() as u64);
        tracing::info!(
            "aligned panel {}/{}: bbox [{},{})x[{},{}) in {:.2}s",
            id + 1,
            panels.len(),
            ap.bbox[0],
            ap.bbox[2],
            ap.bbox[1],
            ap.bbox[3],
            t.elapsed().as_secs_f64()
        );
        aligned.push(ap);
    }
    let align_secs = t_align.elapsed().as_secs_f64();
    tracing::info!(
        "align stage: {} panels in {:.2}s",
        aligned.len(),
        align_secs
    );
    drop(panels); // source mmaps no longer needed

    // Scan the caches exactly like aligned frames, through PanelReader.
    report(progress, "analyze", 0, aligned.len() as u64);
    // Claim + report under one lock (see analyze_aligned's scan loop).
    let scanned = std::sync::Mutex::new(0u64);
    let scans: Vec<PanelScan> = aligned
        .par_iter()
        .enumerate()
        .map(|(id, ap)| {
            let meta = PanelMeta {
                id,
                path: ap.path.clone(),
                source: Some(paths[id].clone()),
                bbox: [0, 0, 0, 0],
                nonzero_frac: 0.0,
                ch_min: vec![],
                ch_max: vec![],
                ch_mean: vec![],
                storage: PanelStorage::CroppedCache { bbox: ap.bbox },
            };
            let reader = PanelReader::open(&meta, canvas)?;
            let scan = scan_reader(meta, reader)?;
            {
                let mut done = scanned.lock().expect("progress lock poisoned");
                *done += 1;
                report(progress, "analyze", *done, aligned.len() as u64);
            }
            Ok(scan)
        })
        .collect::<Result<_>>()?;

    session.canvas = canvas;
    session.input = InputKind::Solved;
    session.frame = Some(frame);
    session.frame_imposed = reference.is_some();
    session.align_secs = Some(align_secs);
    finish_session(session, scans, surface_order, gain)
}

/// Builds every panel's [`WcsModel`] from its carried `properties`, checks
/// that all panels yield a usable solution and share one channel count, then
/// chooses the shared [`crate::align::MosaicFrame`] via [`choose_frame`].
///
/// Shared by [`analyze_ipc_solved`] and the `mmm-ipc-worker` `--probe-frame`
/// preflight so the two paths cannot drift apart — the probe used to run its
/// own copy of this loop without the channel-count check, which is now
/// enforced for both callers alike. `panels` must be non-empty; callers
/// check that themselves first since their "no panels" errors carry
/// different context (a session dir vs. none).
///
/// Also returns the built [`WcsModel`]s (in panel order) so a caller that
/// goes on to reproject (`analyze_ipc_solved`) does not need to rebuild them.
///
/// Returns a plain string reason rather than an [`crate::Error`]: the two
/// callers wrap failures in different variants (`Error::format` with a
/// session dir vs. `Error::compute`).
pub fn solved_frame(
    panels: &[PanelDesc],
) -> std::result::Result<(Vec<WcsModel>, MosaicFrame, u64), String> {
    frame_from_models(panels.iter().map(|p| {
        (
            format!("panel {}", p.panel_id),
            p.channels,
            WcsModel::from_properties(&p.properties, p.width, p.height)
                .ok_or_else(|| describe_unsolved(&p.properties)),
        )
    }))
}

/// The mosaic frame over per-panel `(label, channels, model)` triples: every
/// panel that yielded no model is aggregated into one message (one line each,
/// named by `label`), channel uniformity is checked, and the frame is chosen
/// from the models.
///
/// Shared by [`solved_frame`], whose models come from XISF properties carried
/// over the wire, and [`probe_panels`], whose models come from opened panel
/// files of either format.
fn frame_from_models<I>(panels: I) -> std::result::Result<(Vec<WcsModel>, MosaicFrame, u64), String>
where
    I: IntoIterator<Item = (String, u64, std::result::Result<WcsModel, String>)>,
{
    let mut models: Vec<WcsModel> = Vec::new();
    let mut channels: Vec<(String, u64)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (label, ch, model) in panels {
        match model {
            Ok(m) => models.push(m),
            Err(reason) => errors.push(format!("{label}: {reason}")),
        }
        channels.push((label, ch));
    }
    if !errors.is_empty() {
        return Err(format!(
            "solved input requires an astrometric solution in every panel:\n  {}",
            errors.join("\n  ")
        ));
    }
    let ch = check_uniform_channels(channels)?;
    let frame = choose_frame(&models);
    Ok((models, frame, ch))
}

/// Files-mode metadata probe (PROTOCOL.md §11, `--probe-panels`): read each
/// panel's header — geometry plus its astrometric solution (XISF properties
/// or FITS WCS cards), never pixel data — and report per-panel geometry along
/// with the solved mosaic frame when the job can resolve to solved mode. Lets a GUI host size shm slots for a
/// Files-mode run without opening any panel file on its own thread.
///
/// `frame` follows the same rule the PixInsight host previously implemented
/// itself: `input` = [`InputSelect::Aligned`] never probes a frame;
/// [`InputSelect::Solved`] requires every panel to solve (erroring
/// otherwise, with the same message the analyze stage would produce);
/// [`InputSelect::Auto`] degrades to `None` when any panel lacks a usable
/// solution. Header reads run in parallel.
pub fn probe_panels(paths: &[PathBuf], input: InputSelect) -> Result<PanelProbeReply> {
    if paths.is_empty() {
        return Err(Error::compute("probe-panels: no input panels given"));
    }
    // Header-only opens, in parallel: the geometry the reply carries plus
    // each panel's astrometric model — read from XISF properties or FITS WCS
    // cards, whichever the file carries — for the frame below. (The wire
    // `PanelDesc` path cannot serve here: a FITS panel's solution is not in
    // XISF properties, which `solved_frame` is limited to.)
    let probed: Vec<(PanelProbeGeom, std::result::Result<WcsModel, String>)> = paths
        .par_iter()
        .map(|path| {
            let x = InputPanel::open(path)?;
            Ok((
                PanelProbeGeom {
                    width: x.width(),
                    height: x.height(),
                    channels: x.channels(),
                    filter: x.filter_name(),
                },
                x.wcs_model(),
            ))
        })
        .collect::<Result<_>>()?;

    // Refuse a mono/colour mix at the probe — the first worker contact of a
    // Files-mode run — so a host never sizes its shm slots from panels[0]
    // for a set the run stage would reject anyway (PROTOCOL.md §11).
    check_uniform_channels(
        probed
            .iter()
            .zip(paths)
            .map(|((g, _), path)| (path.display().to_string(), g.channels)),
    )
    .map_err(Error::compute)?;

    let panels: Vec<PanelProbeGeom> = probed.iter().map(|(g, _)| g.clone()).collect();

    let solved = || {
        frame_from_models(
            probed
                .iter()
                .zip(paths)
                .map(|((g, m), path)| (path.display().to_string(), g.channels, m.clone())),
        )
    };
    let frame = match input {
        InputSelect::Aligned => None,
        InputSelect::Solved => {
            let (_, frame, ch) = solved().map_err(Error::compute)?;
            Some([frame.width, frame.height, ch])
        }
        InputSelect::Auto => solved()
            .ok()
            .map(|(_, frame, ch)| [frame.width, frame.height, ch]),
    };

    // The shared reference frame for multi-group hosts: a forced solved set
    // propagates its derive error (as `frame` does above); otherwise an
    // underivable set simply reports none.
    let reference = match input {
        InputSelect::Solved => Some(crate::reference::derive(paths, input)?),
        _ => crate::reference::derive(paths, input).ok(),
    };

    Ok(PanelProbeReply {
        panels,
        frame,
        reference,
    })
}

/// The aligned path over panels streamed from an IPC host: mirrors
/// `analyze_aligned`, but each panel is read through
/// [`PanelReader::open_ipc`] instead of a file — no session metadata or
/// pixel data is ever written to disk except the analyze artifacts
/// themselves. Unlike the file path, this is an explicit entry point (no
/// `--input auto` coverage re-dispatch): the host tells us the job mode.
/// `gain` selects the photometric solve's gain handling (see [`GainMode`]).
pub fn analyze_ipc_aligned(
    link: Arc<HostLink>,
    session_dir: &Path,
    band_rows: usize,
    surface_order: Option<u32>,
    gain: GainMode,
    reference: Option<&ReferenceFrame>,
) -> Result<Session> {
    let mut session = Session::create(session_dir)?;

    let [cw, ch_, cc] = link.canvas();
    let canvas = (cw, ch_, cc);
    let n_panels = link.panels().len();
    if n_panels == 0 {
        return Err(Error::format(session_dir, "IPC job has no input panels"));
    }
    // Every panel is addressed with the canvas geometry (PanelReader::open_ipc),
    // so a panel that disagrees with it — most often a mono view among colour
    // ones — must be refused before the first band request.
    for p in link.panels() {
        if (p.width, p.height, p.channels) != canvas {
            return Err(Error::compute(format!(
                "panel {} is {}x{}x{} but the job canvas is {}x{}x{}: an aligned job's panels \
                 must all match the canvas, including the number of channels — mono and colour \
                 panels cannot be mixed in one mosaic",
                p.panel_id, p.width, p.height, p.channels, canvas.0, canvas.1, canvas.2
            )));
        }
    }

    // Every reference check on this path is knowable from the Init alone
    // (canvas geometry, panel-0 canvas WCS, reference kind): refuse before
    // the first band request — a registered set can be tens of GB.
    if let Some(r) = reference {
        match r {
            ReferenceFrame::Aligned { width, height, wcs } => {
                let panel_wcs = link
                    .panels()
                    .first()
                    .and_then(|p| crate::astrometry::wcs_from_properties(&p.properties));
                crate::reference::check_aligned(
                    (canvas.0, canvas.1),
                    panel_wcs.as_ref(),
                    *width,
                    *height,
                    wcs.as_ref(),
                )
                .map_err(|reason| Error::format(session_dir, reason))?;
                session.frame_imposed = true;
            }
            ReferenceFrame::Solved { .. } => {
                return Err(Error::format(session_dir, SOLVED_REFERENCE_ON_ALIGNED));
            }
        }
    }

    // Claim + report under one lock (see analyze_aligned's scan loop).
    let done = std::sync::Mutex::new(0u64);
    let total = n_panels as u64;
    let scans: Vec<PanelScan> = (0..n_panels)
        .into_par_iter()
        .map(|id| {
            let reader = PanelReader::open_ipc(link.clone(), id as u32, canvas, band_rows);
            let meta = PanelMeta {
                id,
                path: PathBuf::new(),
                source: None,
                bbox: [0, 0, 0, 0],
                nonzero_frac: 0.0,
                ch_min: vec![],
                ch_max: vec![],
                ch_mean: vec![],
                storage: PanelStorage::Ipc {
                    panel_id: id as u32,
                },
            };
            let s = scan_reader(meta, reader)?;
            {
                let mut d = done.lock().expect("progress lock poisoned");
                *d += 1;
                link.send_progress("analyze", *d, total);
            }
            Ok(s)
        })
        .collect::<Result<_>>()?;

    session.canvas = canvas;
    finish_session(session, scans, surface_order, gain)
}

/// The solved path over raw panels streamed from an IPC host: mirrors
/// `analyze_solved` — astrometric models (from
/// [`crate::ipc::protocol::PanelDesc::properties`]) → mosaic frame →
/// [`reproject_from_reader`] into `panels/<id>/aligned.bin` → the same
/// scan/graph/photometry pipeline over the caches on disk.
/// `gain` selects the photometric solve's gain handling (see [`GainMode`]).
pub fn analyze_ipc_solved(
    link: Arc<HostLink>,
    session_dir: &Path,
    band_rows: usize,
    surface_order: Option<u32>,
    gain: GainMode,
    reference: Option<&ReferenceFrame>,
) -> Result<Session> {
    let mut session = Session::create(session_dir)?;

    let panels = link.panels();
    if panels.is_empty() {
        return Err(Error::format(session_dir, "IPC job has no input panels"));
    }

    let (models, own_frame, ch) =
        solved_frame(panels).map_err(|reason| Error::format(session_dir, reason))?;
    let frame = match reference {
        None => own_frame,
        Some(ReferenceFrame::Solved { frame }) => {
            crate::reference::check_footprints(
                panels
                    .iter()
                    .zip(models.iter())
                    .map(|(p, m)| (format!("panel {}", p.panel_id), m)),
                frame,
            )
            .map_err(|reason| Error::format(session_dir, reason))?;
            frame.clone()
        }
        Some(ReferenceFrame::Aligned { .. }) => {
            return Err(Error::format(session_dir, ALIGNED_REFERENCE_ON_SOLVED));
        }
    };
    let canvas = (frame.width, frame.height, ch);
    tracing::info!(
        "mosaic frame: {}x{} px, {:.3}\"/px, center RA {:.4} Dec {:+.4}",
        frame.width,
        frame.height,
        frame.scale_deg * 3600.0,
        frame.crval[0],
        frame.crval[1]
    );

    // Align stage: reproject each panel into the session cache. Sequential
    // over panels — reproject_from_reader materializes one panel's raw
    // planes at a time (bounded memory), reproject_core is rayon-parallel
    // over output rows — matching analyze_solved's sequential-per-panel
    // design.
    let t_align = Instant::now();
    let mut aligned = Vec::with_capacity(panels.len());
    for (id, (p, model)) in panels.iter().zip(models.iter()).enumerate() {
        let t = Instant::now();
        let out_dir = session.dir.join("panels").join(id.to_string());
        let reader = PanelReader::open_ipc(
            link.clone(),
            p.panel_id,
            (p.width, p.height, p.channels),
            band_rows,
        );
        let ap = reproject_from_reader(&reader, None, model, &frame, &out_dir)?;
        tracing::info!(
            "aligned panel {}/{}: bbox [{},{})x[{},{}) in {:.2}s",
            id + 1,
            panels.len(),
            ap.bbox[0],
            ap.bbox[2],
            ap.bbox[1],
            ap.bbox[3],
            t.elapsed().as_secs_f64()
        );
        aligned.push(ap);
        link.send_progress("reproject", (id as u64) + 1, panels.len() as u64);
    }
    let align_secs = t_align.elapsed().as_secs_f64();
    tracing::info!(
        "align stage: {} panels in {:.2}s",
        aligned.len(),
        align_secs
    );

    // Scan the caches exactly like aligned frames, through PanelReader.
    // Claim + report under one lock (see analyze_aligned's scan loop).
    let done = std::sync::Mutex::new(0u64);
    let total = aligned.len() as u64;
    let scans: Vec<PanelScan> = aligned
        .par_iter()
        .enumerate()
        .map(|(id, ap)| {
            let meta = PanelMeta {
                id,
                path: ap.path.clone(),
                source: None,
                bbox: [0, 0, 0, 0],
                nonzero_frac: 0.0,
                ch_min: vec![],
                ch_max: vec![],
                ch_mean: vec![],
                storage: PanelStorage::CroppedCache { bbox: ap.bbox },
            };
            let reader = PanelReader::open(&meta, canvas)?;
            let s = scan_reader(meta, reader)?;
            {
                let mut d = done.lock().expect("progress lock poisoned");
                *d += 1;
                link.send_progress("analyze", *d, total);
            }
            Ok(s)
        })
        .collect::<Result<_>>()?;

    session.canvas = canvas;
    session.input = InputKind::Solved;
    session.frame = Some(frame);
    session.frame_imposed = reference.is_some();
    session.align_secs = Some(align_secs);
    finish_session(session, scans, surface_order, gain)
}

/// Shared tail of both paths: persist summaries, build/save the overlap
/// graph, photometric solve, optional residual surfaces, and `session.json`.
fn finish_session(
    mut session: Session,
    scans: Vec<PanelScan>,
    surface_order: Option<u32>,
    gain: GainMode,
) -> Result<Session> {
    let canvas = session.canvas;
    scans.par_iter().try_for_each(|scan| -> Result<()> {
        let path = session.summary_path(scan.meta.id);
        let parent = path.parent().expect("summary path has a parent");
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        scan.summary.write(&path)
    })?;

    let (metas, summaries): (Vec<_>, Vec<_>) = scans
        .into_iter()
        .map(|scan| (scan.meta, scan.summary))
        .unzip();
    session.panels = metas;

    let graph = OverlapGraph::build(&summaries);
    let graph_path = session.overlap_graph_path();
    let parent = graph_path.parent().expect("graph path has a parent");
    std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    graph.save(&graph_path)?;

    session.gain_mode = gain;
    let phot = crate::photometry::solve(&summaries, &graph, gain)?;
    phot.save(&session.photometry_path())?;

    match surface_order {
        Some(order) => {
            let surfaces = crate::surfaces::fit_surfaces(&summaries, &graph, &phot, canvas, order)?;
            surfaces.save(&session.surfaces_path())?;
        }
        None => {
            // Stale surfaces from a previous run must not survive an
            // explicit `--surface off` re-analyze.
            let _ = std::fs::remove_file(session.surfaces_path());
        }
    }

    session.save()?;
    Ok(session)
}

/// Single streaming pass over one aligned full-canvas panel.
fn scan_panel(id: usize, path: &Path) -> Result<PanelScan> {
    // The storage kind is the input file's own format, so a session written
    // here reopens through the matching backing (`PanelReader::open`); both
    // opens below are header-only mmaps.
    let storage = InputPanel::open(path)?.storage();
    let panel = PanelReader::open_file(path)?;
    let meta = PanelMeta {
        id,
        path: path.to_path_buf(),
        source: None,
        bbox: [0, 0, 0, 0],
        nonzero_frac: 0.0,
        ch_min: vec![],
        ch_max: vec![],
        ch_mean: vec![],
        storage,
    };
    scan_reader(meta, panel)
}

/// Single streaming pass over one panel through a [`PanelReader`], filling in
/// the scan-derived fields of `meta` (bbox, coverage, per-channel stats).
/// Storage-agnostic: rows outside a cropped cache's bbox read as absent.
fn scan_reader(mut meta: PanelMeta, panel: PanelReader) -> Result<PanelScan> {
    panel.advise_sequential();
    let (w, h, ch) = panel.canvas();
    let block = BLOCK as u64;
    let w8 = w.div_ceil(block) as usize;
    let h8 = h.div_ceil(block) as usize;
    let nch = ch as usize;

    let mut summary = L8Summary::zeroed(w8 as u32, h8 as u32, ch as u32);

    // One L8 cell-row accumulator, flushed every BLOCK image rows. Σv and Σv²
    // together yield mean and detail RMS once the cell completes (the cell
    // mean is unknown until then): RMS² = Σv²/n − mean².
    let mut cell_cnt = vec![0u32; w8];
    let mut cell_sum = vec![0f64; nch * w8];
    let mut cell_sum2 = vec![0f64; nch * w8];

    // Global per-panel stats.
    let (mut x0, mut y0, mut x1, mut y1) = (u64::MAX, u64::MAX, 0u64, 0u64);
    let mut covered_total = 0u64;
    let mut ch_min = vec![f32::INFINITY; nch];
    let mut ch_max = vec![f32::NEG_INFINITY; nch];
    let mut ch_sum = vec![0f64; nch];

    let mut rows: Vec<&[f32]> = Vec::with_capacity(nch);
    for y in 0..h {
        // Rows come back clipped to the panel's x extent (canvas x = rx0 + i);
        // rows outside the storage bbox are absent — fully uncovered. All
        // channels share one storage bbox, so the first channel decides.
        rows.clear();
        let mut rx0 = 0usize;
        for c in 0..ch {
            match panel.row(c, y) {
                Some((x0c, r)) => {
                    rx0 = x0c as usize;
                    rows.push(r);
                }
                None => break,
            }
        }
        let mut row_covered = false;
        for i in 0..rows.first().map_or(0, |r| r.len()) {
            let covered = rows.iter().all(|r| r[i] != 0.0);
            if !covered {
                continue;
            }
            let x = rx0 + i;
            covered_total += 1;
            row_covered = true;
            let xu = x as u64;
            if xu < x0 {
                x0 = xu;
            }
            if xu >= x1 {
                x1 = xu + 1;
            }
            let x8 = x / BLOCK as usize;
            cell_cnt[x8] += 1;
            for (c, r) in rows.iter().enumerate() {
                let v = r[i];
                if v < ch_min[c] {
                    ch_min[c] = v;
                }
                if v > ch_max[c] {
                    ch_max[c] = v;
                }
                let v64 = v as f64;
                ch_sum[c] += v64;
                cell_sum[c * w8 + x8] += v64;
                cell_sum2[c * w8 + x8] += v64 * v64;
            }
        }
        if row_covered {
            if y < y0 {
                y0 = y;
            }
            y1 = y + 1;
        }

        // Flush the cell-row on the last image row of each L8 block.
        if y % block == block - 1 || y == h - 1 {
            let y8 = (y / block) as usize;
            let cell_h = y - (y8 as u64) * block + 1;
            for (x8, &cnt) in cell_cnt.iter().enumerate() {
                let cell_w = (w - x8 as u64 * block).min(block);
                let n_pix = (cell_w * cell_h) as f32;
                summary.coverage[y8 * w8 + x8] = cnt as f32 / n_pix;
                if cnt > 0 {
                    for c in 0..nch {
                        let mean = cell_sum[c * w8 + x8] / cnt as f64;
                        summary.mean[(c * h8 + y8) * w8 + x8] = mean as f32;
                        let var = cell_sum2[c * w8 + x8] / cnt as f64 - mean * mean;
                        summary.detail[(c * h8 + y8) * w8 + x8] = var.max(0.0).sqrt() as f32;
                    }
                }
            }
            cell_cnt.fill(0);
            cell_sum.fill(0.0);
            cell_sum2.fill(0.0);
        }
    }

    meta.bbox = if covered_total == 0 {
        [0, 0, 0, 0]
    } else {
        [x0, y0, x1, y1]
    };
    meta.nonzero_frac = covered_total as f64 / (w * h) as f64;
    meta.ch_min = ch_min
        .into_iter()
        .map(|v| if v.is_finite() { v } else { 0.0 })
        .collect();
    meta.ch_max = ch_max
        .into_iter()
        .map(|v| if v.is_finite() { v } else { 0.0 })
        .collect();
    meta.ch_mean = ch_sum
        .into_iter()
        .map(|s| {
            if covered_total > 0 {
                s / covered_total as f64
            } else {
                0.0
            }
        })
        .collect();
    // Surface a mid-scan producer failure (IPC transport or FITS decode) as
    // a proper `Err` instead of a silently-wrong summary; a no-op for
    // Xisf/Cache backings, which always report `None` here — so the file
    // path stays byte-identical.
    if let Some(e) = panel.backing_error() {
        return Err(e);
    }

    Ok(PanelScan {
        meta,
        summary,
        canvas: (w, h, ch),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::xisf::XisfPanel;
    use crate::ipc::client::HostLink;
    use crate::ipc::protocol::PanelDesc;
    use crate::ipc::testhost::MockHost;
    use crate::reference::ReferenceFrame;
    use crate::synth::{SynthWcs, write_xisf, write_xisf_solved};

    fn tmpdir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mmm-analyze-ipc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two overlapping full-canvas panels, written to disk and also returned
    /// as planar buffers for [`MockHost::spawn`] — the same pixels served two
    /// ways.
    struct TwoPanels {
        dir: PathBuf,
        paths: Vec<PathBuf>,
        planar: Vec<Vec<f32>>,
    }

    fn synth_two_full_canvas_panels(tag: &str) -> TwoPanels {
        let dir = tmpdir(tag);
        let (w, h, ch) = (32u64, 24u64, 3u64);
        let make = |lo: u64, hi: u64, base: f32| -> Vec<f32> {
            let mut planes = vec![0f32; (w * h * ch) as usize];
            for c in 0..ch {
                for y in 0..h {
                    for x in lo..hi {
                        planes[(c * w * h + y * w + x) as usize] =
                            base + c as f32 * 0.1 + x as f32 * 0.001 + y as f32 * 0.0005;
                    }
                }
            }
            planes
        };
        // Overlap in x ∈ [12, 20).
        let a = make(0, 20, 0.3);
        let b = make(12, 32, 0.15);
        let pa = dir.join("a.xisf");
        let pb = dir.join("b.xisf");
        write_xisf(&pa, w, h, ch, &a).unwrap();
        write_xisf(&pb, w, h, ch, &b).unwrap();
        TwoPanels {
            dir,
            paths: vec![pa, pb],
            planar: vec![a, b],
        }
    }

    /// The IPC aligned scan must produce byte-identical session artifacts to
    /// the file-based aligned scan for the same pixels: same summaries, same
    /// photometry solve.
    #[test]
    fn ipc_aligned_analyze_matches_file_analyze() {
        let f = synth_two_full_canvas_panels("two");
        let ref_dir = f.dir.join("ref.mmm-session");
        let ref_sess = analyze_opts(&f.paths, &ref_dir, Some(2)).unwrap();

        let (w, h, ch) = ref_sess.canvas;
        let job = MockHost::aligned_job(w, h, ch, f.paths.len() as u32, 8, w * ch * 32 * 4);
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let ipc_dir = f.dir.join("ipc.mmm-session");
        let ipc_sess =
            analyze_ipc_aligned(link.clone(), &ipc_dir, 32, Some(2), GainMode::Fit, None).unwrap();
        link.finish_ok().unwrap();
        host.join();

        assert_eq!(ref_sess.canvas, ipc_sess.canvas);
        for id in 0..f.paths.len() {
            assert_eq!(
                std::fs::read(ref_sess.summary_path(id)).unwrap(),
                std::fs::read(ipc_sess.summary_path(id)).unwrap(),
                "summary {id}"
            );
        }
        assert_eq!(
            std::fs::read(ref_sess.photometry_path()).unwrap(),
            std::fs::read(ipc_sess.photometry_path()).unwrap()
        );

        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    /// The file-based analyze path reports coarse per-panel progress through
    /// the optional observer (the IPC worker forwards it as Progress frames
    /// so Files-mode runs get the same console bars as Views-mode runs).
    #[test]
    fn file_analyze_reports_progress() {
        let f = synth_two_full_canvas_panels("progress");
        let dir = f.dir.join("prog.mmm-session");

        let events = std::sync::Mutex::new(Vec::<(String, u64, u64)>::new());
        let observer = |stage: &str, done: u64, total: u64| {
            events
                .lock()
                .unwrap()
                .push((stage.to_string(), done, total));
        };
        analyze_input_progress(
            &f.paths,
            &dir,
            Some(2),
            InputSelect::Aligned,
            Some(&observer),
        )
        .unwrap();

        let events = events.into_inner().unwrap();
        assert!(!events.is_empty(), "no progress reported");
        // Aligned inputs scan panels: only the "analyze" stage, ending
        // complete at done == total == panel count.
        assert!(events.iter().all(|(s, _, _)| s == "analyze"));
        let &(_, done, total) = events.last().unwrap();
        assert_eq!((done, total), (2, 2));

        // The plain entry point still works without an observer.
        let quiet = f.dir.join("quiet.mmm-session");
        analyze_input(&f.paths, &quiet, Some(2), InputSelect::Aligned).unwrap();

        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    /// Progress events must arrive in claim order even when one panel's
    /// observer call is slow: the count claim and the report happen under
    /// one lock, so a worker that claimed `done = n` publishes it before
    /// any later claim is reported. Regression test for an out-of-order
    /// final event (1/2 arriving after 2/2) seen on a 2-core Windows CI
    /// runner; the stall below widens that race window so an unserialized
    /// claim+report pair fails here deterministically on any multicore box.
    #[test]
    fn file_analyze_progress_events_are_ordered() {
        let f = synth_two_full_canvas_panels("progress_order");
        let dir = f.dir.join("prog.mmm-session");

        let events = std::sync::Mutex::new(Vec::<(u64, u64)>::new());
        let observer = |_stage: &str, done: u64, total: u64| {
            if done == 1 {
                // Stall the first completion's report so a racing second
                // completion would overtake it if claim+report were not
                // one critical section.
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            events.lock().unwrap().push((done, total));
        };
        analyze_input_progress(
            &f.paths,
            &dir,
            Some(2),
            InputSelect::Aligned,
            Some(&observer),
        )
        .unwrap();

        let events = events.into_inner().unwrap();
        let dones: Vec<u64> = events.iter().map(|&(d, _)| d).collect();
        assert_eq!(dones, vec![0, 1, 2], "events out of claim order");

        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    /// An aligned IPC job whose panels disagree with the job canvas is
    /// refused before any band is requested: the reader addresses every
    /// panel with the canvas geometry, so a mono panel in a 3-channel job
    /// otherwise fails deep in the scan with an internal band-size
    /// complaint that names neither the panel nor the real cause.
    #[test]
    fn ipc_aligned_rejects_panel_geometry_mismatch() {
        let (w, h, ch) = (16u64, 8u64, 3u64);
        let mut job = MockHost::aligned_job(w, h, ch, 2, 4, w * ch * 8 * 4);
        job.panels[1].channels = 1;
        let pixels = vec![
            vec![0.25f32; (w * h * ch) as usize],
            vec![0.25f32; (w * h) as usize],
        ];
        let (host, r, wr) = MockHost::spawn(job.clone(), pixels);
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = tmpdir("ipc-mismatch");

        let err = analyze_ipc_aligned(link.clone(), &dir, 8, None, GainMode::Fit, None)
            .unwrap_err()
            .to_string();
        link.finish_ok().unwrap();
        host.join();

        assert!(err.contains("channels"), "got: {err}");
        assert!(err.contains("panel 1"), "got: {err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two raw solved mono panels (64×48 at RA 10°, 60×52 offset ~55 % east
    /// and 8 px north, rotated 6°), as files plus planar pixels plus wire
    /// descriptors carrying their solutions.
    struct SolvedPair {
        dir: PathBuf,
        descs: Vec<PanelDesc>,
        planar: Vec<Vec<f32>>,
    }

    fn synth_solved_pair(tag: &str) -> SolvedPair {
        let dir = tmpdir(tag);
        let s = 1.0e-3_f64;
        let specs = [
            (64u64, 48u64, [10.0f64, 0.0f64], 0.0f64),
            (60, 52, [10.0 + 64.0 * s * 0.55, 8.0 * s], 6.0),
        ];
        let mut descs = Vec::new();
        let mut planar = Vec::new();
        for (k, (w, h, crval, rot)) in specs.iter().enumerate() {
            let (sr, cr) = rot.to_radians().sin_cos();
            let cd = [[-s * cr, -s * sr], [-s * sr, s * cr]];
            let mut planes = vec![0f32; (w * h) as usize];
            for j in 0..*h {
                for i in 0..*w {
                    planes[(j * w + i) as usize] =
                        (((i * 7 + j * 13 + k as u64 * 41) % 97) as f32) / 97.0 + 0.01;
                }
            }
            let path = dir.join(format!("solved_{k}.xisf"));
            write_xisf_solved(
                &path,
                *w,
                *h,
                1,
                &planes,
                &SynthWcs {
                    crval: *crval,
                    refimg: [*w as f64 / 2.0, *h as f64 / 2.0],
                    cd,
                },
            )
            .unwrap();
            let props = XisfPanel::open(&path).unwrap().header().properties.clone();
            descs.push(PanelDesc {
                panel_id: k as u32,
                width: *w,
                height: *h,
                channels: 1,
                properties: props,
            });
            planar.push(planes);
        }
        SolvedPair { dir, descs, planar }
    }

    fn solved_job(descs: &[PanelDesc], slot_width: u64) -> crate::ipc::protocol::InitJob {
        let mut job = MockHost::aligned_job(0, 0, 1, 0, 8, slot_width * 8 * 4);
        job.panels = descs.to_vec();
        job.mode = crate::ipc::protocol::JobMode::Solved;
        job
    }

    #[test]
    fn ipc_solved_adopts_an_imposed_reference() {
        let f = synth_solved_pair("ipc-ref-solved");
        // A reference wider than the pair's own frame, as a second filter's
        // panels shifted east would give.
        let own = crate::reference::derive_from_descs(&f.descs, InputSelect::Solved).unwrap();
        let ReferenceFrame::Solved { frame: own_frame } = &own else {
            unreachable!()
        };
        let imposed = ReferenceFrame::Solved {
            frame: MosaicFrame {
                width: own_frame.width + 30,
                ..own_frame.clone()
            },
        };
        let mut job = solved_job(&f.descs, own_frame.width + 30);
        job.reference = Some(imposed.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let sess = analyze_ipc_solved(
            link.clone(),
            &dir,
            8,
            Some(2),
            GainMode::Fit,
            Some(&imposed),
        )
        .unwrap();
        link.finish_ok().unwrap();
        host.join();
        assert!(sess.frame_imposed);
        assert_eq!(sess.canvas.0, own_frame.width + 30);
        let ReferenceFrame::Solved { frame } = imposed else {
            unreachable!()
        };
        assert_eq!(sess.frame, Some(frame));
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_solved_refuses_a_panel_outside_the_reference() {
        let f = synth_solved_pair("ipc-ref-footprint");
        let own = crate::reference::derive_from_descs(&f.descs, InputSelect::Solved).unwrap();
        let ReferenceFrame::Solved { frame: own_frame } = &own else {
            unreachable!()
        };
        // Narrower than the panels' union: a panel overhangs an edge.
        let narrow = ReferenceFrame::Solved {
            frame: MosaicFrame {
                width: own_frame.width - 40,
                ..own_frame.clone()
            },
        };
        let mut job = solved_job(&f.descs, own_frame.width);
        job.reference = Some(narrow.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let err = analyze_ipc_solved(link.clone(), &dir, 8, Some(2), GainMode::Fit, Some(&narrow))
            .unwrap_err()
            .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(err.contains("beyond the"), "{err}");
        assert!(err.contains("panel 1") || err.contains("panel 0"), "{err}");
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_solved_refuses_an_aligned_reference() {
        let f = synth_solved_pair("ipc-ref-kind");
        let aligned = ReferenceFrame::Aligned {
            width: 64,
            height: 48,
            wcs: None,
        };
        let mut job = solved_job(&f.descs, 128);
        job.reference = Some(aligned.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let err = analyze_ipc_solved(
            link.clone(),
            &dir,
            8,
            Some(2),
            GainMode::Fit,
            Some(&aligned),
        )
        .unwrap_err()
        .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(
            err.contains("aligned canvas") && err.contains("--input aligned"),
            "{err}"
        );
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_aligned_adopts_reference_without_wcs() {
        let f = synth_two_full_canvas_panels("ipc-ref-aligned");
        let (w, h, ch) = (32u64, 24u64, 3u64);
        let reference = ReferenceFrame::Aligned {
            width: w,
            height: h,
            wcs: None,
        };
        let mut job = MockHost::aligned_job(w, h, ch, 2, 8, w * ch * 32 * 4);
        job.reference = Some(reference.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let sess = analyze_ipc_aligned(
            link.clone(),
            &dir,
            32,
            Some(2),
            GainMode::Fit,
            Some(&reference),
        )
        .unwrap();
        link.finish_ok().unwrap();
        host.join();
        assert!(sess.frame_imposed);
        assert!(sess.frame.is_none());
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_aligned_refuses_a_mismatched_canvas_reference() {
        let f = synth_two_full_canvas_panels("ipc-ref-aligned-size");
        let (w, h, ch) = (32u64, 24u64, 3u64);
        let reference = ReferenceFrame::Aligned {
            width: w + 8,
            height: h,
            wcs: None,
        };
        let mut job = MockHost::aligned_job(w, h, ch, 2, 8, w * ch * 32 * 4);
        job.reference = Some(reference.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let err = analyze_ipc_aligned(
            link.clone(),
            &dir,
            32,
            Some(2),
            GainMode::Fit,
            Some(&reference),
        )
        .unwrap_err()
        .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(err.contains("does not match the reference frame"), "{err}");
        assert!(
            err.contains("align every group to one common reference"),
            "{err}"
        );
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn init_job_without_reference_still_parses() {
        let mut job = MockHost::aligned_job(4, 4, 1, 1, 1, 64);
        job.reference = None;
        let mut v = serde_json::to_value(&job).unwrap();
        v.as_object_mut().unwrap().remove("reference");
        let back: crate::ipc::protocol::InitJob = serde_json::from_value(v).unwrap();
        assert_eq!(back.reference, None);
    }

    /// Review fix: every header-knowable reference refusal on the IPC aligned
    /// path (a displaced canvas WCS, not just a wrong size) happens before
    /// the first band request. The mock host is given NO pixels, so any band
    /// request panics its thread and `host.join()` fails.
    #[test]
    fn ipc_aligned_refuses_displaced_wcs_before_any_band() {
        let dir = tmpdir("ipc-ref-wcs-early");
        let (w, h) = (32u64, 24u64);
        let s = 1.0e-3_f64;
        let cd = [[-s, 0.0], [0.0, s]];
        let wcs_of = |crval: [f64; 2], crpix: [f64; 2]| crate::astrometry::LinearWcs {
            crval,
            crpix,
            cd,
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        };
        // Two registered canvases carrying one canvas WCS.
        let mut descs = Vec::new();
        for k in 0..2u32 {
            let path = dir.join(format!("canvas_{k}.xisf"));
            write_xisf_solved(
                &path,
                w,
                h,
                1,
                &vec![0.2f32; (w * h) as usize],
                &SynthWcs {
                    crval: [10.0, 0.0],
                    refimg: [w as f64 / 2.0, h as f64 / 2.0],
                    cd,
                },
            )
            .unwrap();
            let props = XisfPanel::open(&path).unwrap().header().properties.clone();
            descs.push(PanelDesc {
                panel_id: k,
                width: w,
                height: h,
                channels: 1,
                properties: props,
            });
        }
        // The reference's WCS puts the same sky 2 px to the right.
        let reference = ReferenceFrame::Aligned {
            width: w,
            height: h,
            wcs: Some(wcs_of(
                [10.0, 0.0],
                [w as f64 / 2.0 + 0.5 + 2.0, h as f64 / 2.0 + 0.5],
            )),
        };
        let mut job = MockHost::aligned_job(w, h, 1, 2, 8, w * 8 * 4);
        job.panels = descs;
        job.reference = Some(reference.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), vec![vec![], vec![]]);
        let link = HostLink::start(job, r, wr).unwrap();
        let err = analyze_ipc_aligned(
            link.clone(),
            &dir,
            8,
            Some(2),
            GainMode::Fit,
            Some(&reference),
        )
        .unwrap_err()
        .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(err.contains("displaced up to 2.00 px"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
