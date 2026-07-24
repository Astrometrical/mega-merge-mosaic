//! Analyze stage: one streaming pass per panel producing the L8 summary,
//! content bbox, and per-channel statistics, persisted into a session dir.
//!
//! Panels are scanned in parallel (rayon; the work is I/O-bound). Each scan is
//! a single sequential pass over the mmap'd planes: per image row the channel
//! rows are read in step, coverage (all channels nonzero) is accumulated into
//! one L8 cell-row accumulator, flushed every 8 rows. No dense canvas-sized
//! allocations — only L8-resolution buffers per panel.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::formats::xisf::XisfPanel;
use crate::session::{PanelMeta, Session};
use crate::summary::{BLOCK, L8Summary};
use crate::{Error, Result};

struct PanelScan {
    meta: PanelMeta,
    summary: L8Summary,
    canvas: (u64, u64, u64),
}

/// Analyze `paths` into a session at `session_dir`: writes `session.json` and
/// `panels/<id>/summary.bin`, returns the populated [`Session`].
pub fn analyze(paths: &[PathBuf], session_dir: &Path) -> Result<Session> {
    if paths.is_empty() {
        return Err(Error::format(session_dir, "no input panels given"));
    }
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
    session.canvas = canvas;

    scans.par_iter().try_for_each(|scan| -> Result<()> {
        let path = session.summary_path(scan.meta.id);
        let parent = path.parent().expect("summary path has a parent");
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        scan.summary.write(&path)
    })?;

    session.panels = scans.into_iter().map(|scan| scan.meta).collect();
    session.save()?;
    Ok(session)
}

/// Single streaming pass over one panel.
fn scan_panel(id: usize, path: &Path) -> Result<PanelScan> {
    let panel = XisfPanel::open(path)?;
    panel.advise_sequential();
    let (w, h, ch) = (panel.width(), panel.height(), panel.channels());
    let block = BLOCK as u64;
    let w8 = w.div_ceil(block) as usize;
    let h8 = h.div_ceil(block) as usize;
    let nch = ch as usize;

    let mut summary = L8Summary::zeroed(w8 as u32, h8 as u32, ch as u32);

    // One L8 cell-row accumulator, flushed every BLOCK image rows.
    let mut cell_cnt = vec![0u32; w8];
    let mut cell_sum = vec![0f64; nch * w8];

    // Global per-panel stats.
    let (mut x0, mut y0, mut x1, mut y1) = (u64::MAX, u64::MAX, 0u64, 0u64);
    let mut covered_total = 0u64;
    let mut ch_min = vec![f32::INFINITY; nch];
    let mut ch_max = vec![f32::NEG_INFINITY; nch];
    let mut ch_sum = vec![0f64; nch];

    let mut rows: Vec<&[f32]> = Vec::with_capacity(nch);
    for y in 0..h {
        rows.clear();
        for c in 0..ch {
            rows.push(panel.row(c, y));
        }
        let mut row_covered = false;
        for x in 0..w as usize {
            let covered = rows.iter().all(|r| r[x] != 0.0);
            if !covered {
                continue;
            }
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
                let v = r[x];
                if v < ch_min[c] {
                    ch_min[c] = v;
                }
                if v > ch_max[c] {
                    ch_max[c] = v;
                }
                let v64 = v as f64;
                ch_sum[c] += v64;
                cell_sum[c * w8 + x8] += v64;
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
                        summary.mean[(c * h8 + y8) * w8 + x8] =
                            (cell_sum[c * w8 + x8] / cnt as f64) as f32;
                    }
                }
            }
            cell_cnt.fill(0);
            cell_sum.fill(0.0);
        }
    }

    let bbox = if covered_total == 0 { [0, 0, 0, 0] } else { [x0, y0, x1, y1] };
    let meta = PanelMeta {
        id,
        path: path.to_path_buf(),
        bbox,
        nonzero_frac: covered_total as f64 / (w * h) as f64,
        ch_min: ch_min.into_iter().map(|v| if v.is_finite() { v } else { 0.0 }).collect(),
        ch_max: ch_max.into_iter().map(|v| if v.is_finite() { v } else { 0.0 }).collect(),
        ch_mean: ch_sum
            .into_iter()
            .map(|s| if covered_total > 0 { s / covered_total as f64 } else { 0.0 })
            .collect(),
    };
    Ok(PanelScan { meta, summary, canvas: (w, h, ch) })
}
