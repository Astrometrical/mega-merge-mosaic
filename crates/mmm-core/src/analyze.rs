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
//!   fresh [`MosaicFrame`](crate::align::MosaicFrame) via
//!   [`choose_frame`](crate::align::choose_frame), reprojects each panel into
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
use std::time::Instant;

use rayon::prelude::*;

use crate::align::{choose_frame, reproject_panel};
use crate::astrometry::WcsModel;
use crate::formats::XisfProperty;
use crate::formats::xisf::XisfPanel;
use crate::overlap::OverlapGraph;
use crate::panel_reader::{PanelReader, PanelStorage};
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
    if paths.is_empty() {
        return Err(Error::format(session_dir, "no input panels given"));
    }
    match input {
        InputSelect::Aligned => analyze_aligned(paths, session_dir, surface_order, false),
        InputSelect::Solved => analyze_solved(paths, session_dir, surface_order),
        InputSelect::Auto => {
            // Cheap signal first: header geometries only.
            let mut geoms = Vec::with_capacity(paths.len());
            for path in paths {
                let x = XisfPanel::open(path)?;
                geoms.push((x.width(), x.height(), x.channels()));
            }
            if paths.len() >= 2 && geoms.iter().all(|&g| g == geoms[0]) {
                analyze_aligned(paths, session_dir, surface_order, true)
            } else {
                analyze_solved(paths, session_dir, surface_order)
            }
        }
    }
}

/// The aligned full-canvas path. With `auto` set, the coverage rule applies
/// after the scan: any panel covering ≥ [`ALIGNED_MAX_COVERAGE`] of the
/// canvas re-dispatches the whole set to [`analyze_solved`].
fn analyze_aligned(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
    auto: bool,
) -> Result<Session> {
    let mut session = Session::create(session_dir)?;

    let scans: Vec<PanelScan> = paths
        .par_iter()
        .enumerate()
        .map(|(id, path)| scan_panel(id, path))
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

    if auto && scans.iter().any(|s| s.meta.nonzero_frac >= ALIGNED_MAX_COVERAGE) {
        tracing::info!(
            "panels share one geometry but cover >= {:.0}% of it — treating input as solved \
             raw panels (--input aligned overrides)",
            ALIGNED_MAX_COVERAGE * 100.0
        );
        return analyze_solved(paths, session_dir, surface_order);
    }

    session.canvas = canvas;
    finish_session(session, scans, surface_order)
}

/// The solved path: astrometric models → mosaic frame → reprojection caches →
/// the identical scan/graph/photometry pipeline over [`PanelReader`].
fn analyze_solved(
    paths: &[PathBuf],
    session_dir: &Path,
    surface_order: Option<u32>,
) -> Result<Session> {
    let mut session = Session::create(session_dir)?;

    // Every input must yield a model; report all unusable files at once.
    let mut panels: Vec<(XisfPanel, WcsModel)> = Vec::with_capacity(paths.len());
    let mut errors: Vec<String> = Vec::new();
    for path in paths {
        match XisfPanel::open(path) {
            Ok(p) => {
                let h = p.header();
                match WcsModel::from_properties(&h.properties, h.width, h.height) {
                    Some(m) => panels.push((p, m)),
                    None => errors
                        .push(format!("{}: {}", path.display(), describe_unsolved(&h.properties))),
                }
            }
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
    let ch = panels[0].0.channels();
    if let Some((p, _)) = panels.iter().find(|(p, _)| p.channels() != ch) {
        return Err(Error::format(
            p.path(),
            format!("panel has {} channels, expected {ch} like {}", p.channels(), paths[0].display()),
        ));
    }

    let models: Vec<WcsModel> = panels.iter().map(|(_, m)| m.clone()).collect();
    let frame = choose_frame(&models);
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
    let mut aligned = Vec::with_capacity(panels.len());
    for (id, (panel, model)) in panels.iter().enumerate() {
        let t = Instant::now();
        let out_dir = session.dir.join("panels").join(id.to_string());
        let ap = reproject_panel(panel, model, &frame, &out_dir)?;
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
    tracing::info!("align stage: {} panels in {:.2}s", aligned.len(), align_secs);
    drop(panels); // source mmaps no longer needed

    // Scan the caches exactly like aligned frames, through PanelReader.
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
            scan_reader(meta, reader)
        })
        .collect::<Result<_>>()?;

    session.canvas = canvas;
    session.input = InputKind::Solved;
    session.frame = Some(frame);
    session.align_secs = Some(align_secs);
    finish_session(session, scans, surface_order)
}

/// Explain why a panel's properties yield no [`WcsModel`], for the solved
/// path's per-file error listing.
fn describe_unsolved(props: &[XisfProperty]) -> String {
    const REQUIRED: [&str; 3] = [
        "PCL:AstrometricSolution:ReferenceCelestialCoordinates",
        "PCL:AstrometricSolution:ReferenceImageCoordinates",
        "PCL:AstrometricSolution:LinearTransformationMatrix",
    ];
    let missing: Vec<&str> = REQUIRED
        .into_iter()
        .filter(|id| !props.iter().any(|p| p.id == *id))
        .collect();
    if !missing.is_empty() {
        format!("missing astrometric properties: {}", missing.join(", "))
    } else if props
        .iter()
        .any(|p| p.id.starts_with("PCL:AstrometricSolution:SplineWorldTransformation:"))
    {
        "spline solution present but its interpolation grids are missing or failed validation"
            .to_string()
    } else {
        "astrometric solution properties are present but invalid (unsupported projection or \
         malformed values)"
            .to_string()
    }
}

/// Shared tail of both paths: persist summaries, build/save the overlap
/// graph, photometric solve, optional residual surfaces, and `session.json`.
fn finish_session(
    mut session: Session,
    scans: Vec<PanelScan>,
    surface_order: Option<u32>,
) -> Result<Session> {
    let canvas = session.canvas;
    scans.par_iter().try_for_each(|scan| -> Result<()> {
        let path = session.summary_path(scan.meta.id);
        let parent = path.parent().expect("summary path has a parent");
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        scan.summary.write(&path)
    })?;

    let (metas, summaries): (Vec<_>, Vec<_>) =
        scans.into_iter().map(|scan| (scan.meta, scan.summary)).unzip();
    session.panels = metas;

    let graph = OverlapGraph::build(&summaries);
    let graph_path = session.overlap_graph_path();
    let parent = graph_path.parent().expect("graph path has a parent");
    std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    graph.save(&graph_path)?;

    let phot = crate::photometry::solve(&summaries, &graph)?;
    phot.save(&session.photometry_path())?;

    match surface_order {
        Some(order) => {
            let surfaces =
                crate::surfaces::fit_surfaces(&summaries, &graph, &phot, canvas, order)?;
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
    let panel = PanelReader::open_xisf(path)?;
    let meta = PanelMeta {
        id,
        path: path.to_path_buf(),
        source: None,
        bbox: [0, 0, 0, 0],
        nonzero_frac: 0.0,
        ch_min: vec![],
        ch_max: vec![],
        ch_mean: vec![],
        storage: PanelStorage::FullCanvasXisf,
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

    meta.bbox = if covered_total == 0 { [0, 0, 0, 0] } else { [x0, y0, x1, y1] };
    meta.nonzero_frac = covered_total as f64 / (w * h) as f64;
    meta.ch_min = ch_min.into_iter().map(|v| if v.is_finite() { v } else { 0.0 }).collect();
    meta.ch_max = ch_max.into_iter().map(|v| if v.is_finite() { v } else { 0.0 }).collect();
    meta.ch_mean = ch_sum
        .into_iter()
        .map(|s| if covered_total > 0 { s / covered_total as f64 } else { 0.0 })
        .collect();
    Ok(PanelScan { meta, summary, canvas: (w, h, ch) })
}
