//! Export a group's blended result (its tiled store, [`crate::tiles`]) to
//! a FITS or TIFF file: the whole canvas, linear and at full `f32`
//! precision.
//!
//! The level-0 tiles are read one tile row at a time through a
//! [`TileReader`] (never the store's writer lock, never the whole canvas in
//! memory). A **stale** tile (invalidated since it was blended) is exported
//! as it is and a **missing** one (never blended) as zeros, the no-data
//! value; [`ExportReport`] counts both, and [`export_check`] counts them
//! without writing anything so a frontend can warn first.
//!
//! - **FITS** streams the tile rows through [`FitsSink`]. The header
//!   carries the reference panel's passed-through cards, WCS cards for the
//!   group's frame ([`session_keywords`], top-down, as `mmm blend` writes
//!   them) and `HISTORY` provenance: the mmm version and the group's blend
//!   settings from the latest `"blend"` entry of `mosaic.json`'s
//!   provenance.
//! - **TIFF** is 32-bit IEEE float, chunky (interleaved) RGB or grayscale,
//!   written by mpp-core's streaming writer
//!   ([`write_tiff_native_with_progress`]), which copies the samples
//!   verbatim: no clamping, no scaling, so values outside `[0, 1]` survive.
//!   That writer pulls rows by random access, so the tile rows are first
//!   staged as planar `f32` into a scratch file next to the output
//!   (`<name>.scratch`, the size of the image, deleted afterwards) and
//!   memory-mapped. The provenance goes into `ImageDescription`; TIFF has
//!   no standard place for the WCS.
//!
//! Output is written to `<name>.part` beside the destination and renamed
//! over it only when complete: a failed or cancelled export removes its
//! partial and scratch files and leaves an existing destination untouched.
//! Cancellation is checked once per tile row. While the TIFF writer runs it
//! cannot be stopped; a cancel then makes the rest of the image read as
//! zeros, so the writer finishes at disk speed and the file is discarded.
//!
//! The caller is expected to hold the session lock
//! ([`crate::mosaic::lock_session`]) so no blend recreates the store under
//! the reader's mapping (see the tile store's concurrency contract).

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use mpp_core::formats::PlanarRows;
use mpp_core::formats::tiff::BitDepth;
use mpp_core::formats::tiff_write::{Container, TiffWriteOptions, write_tiff_native_with_progress};

use crate::error::{Error, Result};
use crate::formats::FitsKeyword;
use crate::job::{CancelToken, JobCtl};
use crate::mosaic::Mosaic;
use crate::output::fits::{FitsSink, WcsPlacement, session_keywords};
use crate::session::Session;
use crate::tiles::{TileGeometry, TileReader, TileState};

/// Progress stage reported by [`export_group`].
pub const EXPORT_STAGE: &str = "export";
/// Longest text of one FITS `HISTORY` card (columns 9–80).
const HISTORY_WIDTH: usize = 72;
/// Copies of a tile caught mid-write tried before it is exported as zeros.
const TORN_RETRIES: usize = 8;

/// File format of an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// FITS, `BITPIX = -32`, with WCS and `HISTORY` provenance.
    Fits,
    /// TIFF (BigTIFF past 4 GB), 32-bit float samples.
    Tiff,
}

impl ExportFormat {
    /// The usual file extension (`fits`, `tif`).
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Fits => "fits",
            ExportFormat::Tiff => "tif",
        }
    }

    /// A short name for the user (`FITS`, `TIFF`).
    pub fn name(self) -> &'static str {
        match self {
            ExportFormat::Fits => "FITS",
            ExportFormat::Tiff => "TIFF",
        }
    }
}

/// What an export found (or, from [`export_check`], would find) in the
/// group's result: level-0 tiles not valid, and the image size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportReport {
    /// Tiles invalidated since they were blended (exported as they are).
    pub stale_tiles: usize,
    /// Tiles never blended (exported as zeros).
    pub missing_tiles: usize,
    /// Image width in pixels.
    pub width: u64,
    /// Image height in pixels.
    pub height: u64,
}

impl ExportReport {
    /// Every tile is valid: the export is the current blend.
    pub fn is_complete(&self) -> bool {
        self.stale_tiles == 0 && self.missing_tiles == 0
    }

    /// A warning for the user when the result is stale or partly missing.
    pub fn warning(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.stale_tiles > 0 {
            parts.push(format!(
                "{} stale tile(s) (blended with earlier settings or inputs; exported as they are)",
                self.stale_tiles
            ));
        }
        if self.missing_tiles > 0 {
            parts.push(format!(
                "{} missing tile(s) (never blended; exported as zeros)",
                self.missing_tiles
            ));
        }
        (!parts.is_empty()).then(|| {
            format!(
                "the result is not fully blended: {} — blend the whole mosaic for a complete export",
                parts.join(", ")
            )
        })
    }
}

/// Open the result store at `result` for reading; a missing store is "not
/// blended yet".
fn open_result(result: &Path) -> Result<TileReader> {
    TileReader::open(result).map_err(|e| match e {
        Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            Error::compute(format!(
                "{} has not been blended yet — blend it before exporting",
                result.display()
            ))
        }
        e => e,
    })
}

/// Count the level-0 tiles of the result store at `result` that are not
/// valid (stale or missing), reading only the tile state table. Writes
/// nothing.
pub fn export_check(result: &Path) -> Result<ExportReport> {
    let reader = open_result(result)?;
    let g = reader.geometry();
    let (nx, ny) = g.tiles(0);
    let mut report = ExportReport {
        stale_tiles: 0,
        missing_tiles: 0,
        width: g.width,
        height: g.height,
    };
    for ty in 0..ny {
        for tx in 0..nx {
            match reader.state(0, tx, ty) {
                TileState::Valid => {}
                TileState::Stale => report.stale_tiles += 1,
                TileState::Missing => report.missing_tiles += 1,
            }
        }
    }
    Ok(report)
}

/// Export `group`'s blended result in the mosaic `session` (a mosaic
/// directory) to `out` as `format`, covering the whole canvas (see the
/// module docs). Returns the counts of stale and missing tiles exported.
///
/// Fails, writing nothing, for a group never analysed or never blended,
/// or whose result no longer matches its analysed canvas (blend again).
/// [`Error::Cancelled`] when `ctl` is cancelled; progress is reported as
/// [`EXPORT_STAGE`].
pub fn export_group(
    session: &Path,
    group: &str,
    out: &Path,
    format: ExportFormat,
    ctl: &JobCtl,
) -> Result<ExportReport> {
    let m = Mosaic::open(session)?;
    if !m.config.group_dirs.contains_key(group) {
        return Err(Error::compute(format!(
            "group '{group}' has not been analysed or blended — nothing to export"
        )));
    }
    let result = m.result_path(group);
    if !result.exists() {
        return Err(Error::compute(format!(
            "group '{group}' has not been blended yet — blend it before exporting"
        )));
    }
    let reader = open_result(&result)?;
    let g = reader.geometry();
    let group_session = Session::open(&m.group_session_dir(group))?;
    let (cw, ch, cc) = group_session.canvas;
    if (g.width, g.height, g.channels as u64) != (cw, ch, cc) {
        return Err(Error::compute(format!(
            "the result of group '{group}' is {}×{}×{} but its analysed canvas is \
             {cw}×{ch}×{cc} — blend it again before exporting",
            g.width, g.height, g.channels
        )));
    }
    let history = history_lines(&m, group);
    let part = sibling(out, "part");
    let written = match format {
        ExportFormat::Fits => {
            let place = WcsPlacement {
                origin: (0, 0),
                height: g.height,
                flipped: false,
            };
            let (mut keywords, _) = session_keywords(&group_session, (0, 0), Some(place))?;
            keywords.extend(history.iter().map(|h| FitsKeyword {
                name: "HISTORY".into(),
                value: h.clone(),
                comment: String::new(),
            }));
            write_fits(&reader, &part, keywords, ctl)
        }
        ExportFormat::Tiff => {
            let scratch = sibling(out, "scratch");
            let written = write_tiff(&reader, &part, &scratch, out, &history, ctl);
            let _ = std::fs::remove_file(&scratch);
            written
        }
    };
    let report = match written {
        Ok(report) => report,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    std::fs::rename(&part, out).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        Error::io(out, e)
    })?;
    Ok(report)
}

/// `<out>.<suffix>`, beside `out`.
fn sibling(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    out.with_file_name(name)
}

/// The provenance lines of an export of `group`: the mmm version, then the
/// latest blend of the group (version, time, rect and settings) from the
/// mosaic's provenance. ASCII only, each at most [`HISTORY_WIDTH`] long.
fn history_lines(m: &Mosaic, group: &str) -> Vec<String> {
    let mut text = vec![format!(
        "Exported by Mega Merge Mosaic (mmm {}), group '{group}'",
        env!("CARGO_PKG_VERSION")
    )];
    let blend = m
        .config
        .provenance
        .iter()
        .rev()
        .find(|p| p.action == "blend" && p.group.as_deref() == Some(group));
    match blend {
        Some(p) => {
            text.push(format!(
                "Last blend: mmm {}, unix_ms {}, rect {}",
                p.mmm_version, p.unix_ms, p.detail["rect"]
            ));
            text.push(format!("Blend settings: {}", p.detail["settings"]));
        }
        None => text.push("No blend of this group is recorded in the provenance".into()),
    }
    let mut lines = Vec::new();
    for t in text {
        let ascii: String = t
            .chars()
            .map(|c| {
                if c.is_ascii() && !c.is_ascii_control() {
                    c
                } else {
                    '?'
                }
            })
            .collect();
        let bytes = ascii.as_bytes();
        for chunk in bytes.chunks(HISTORY_WIDTH) {
            lines.push(String::from_utf8_lossy(chunk).into_owned());
        }
    }
    lines
}

/// Reads the level-0 tiles of a store one tile row at a time, counting
/// stale and missing tiles.
struct TileRows<'a> {
    reader: &'a TileReader,
    g: TileGeometry,
    tile: Vec<f32>,
    report: ExportReport,
}

impl<'a> TileRows<'a> {
    fn new(reader: &'a TileReader) -> Self {
        let g = reader.geometry();
        TileRows {
            reader,
            g,
            tile: vec![0.0; g.channels as usize * g.tile as usize * g.tile as usize],
            report: ExportReport {
                stale_tiles: 0,
                missing_tiles: 0,
                width: g.width,
                height: g.height,
            },
        }
    }

    /// Tile rows in the image.
    fn rows(&self) -> u32 {
        self.g.tiles(0).1
    }

    /// First image row and row count of tile row `ty`.
    fn span(&self, ty: u32) -> (u64, u64) {
        let y0 = ty as u64 * self.g.tile as u64;
        (y0, (self.g.tile as u64).min(self.g.height - y0))
    }

    /// Copy tile `(tx, ty)` into `self.tile`: a valid or stale copy as it
    /// is, a missing tile (or one still being written after
    /// [`TORN_RETRIES`] tries) as zeros, counted.
    fn read_tile(&mut self, tx: u32, ty: u32) {
        for _ in 0..TORN_RETRIES {
            match self.reader.read_tile_any(0, tx, ty, &mut self.tile) {
                Some(TileState::Valid) => return,
                Some(_) => {
                    self.report.stale_tiles += 1;
                    return;
                }
                None if self.reader.state(0, tx, ty) == TileState::Missing => break,
                // Caught mid-write: try again.
                None => std::thread::yield_now(),
            }
        }
        self.tile.fill(0.0);
        self.report.missing_tiles += 1;
    }

    /// Tile row `ty` as a [`crate::blend::RowSink`] band: planar,
    /// channel-major (each channel's rows, full width); returns its first
    /// row.
    fn band(&mut self, ty: u32, band: &mut Vec<f32>) -> u64 {
        let (y0, rows) = self.span(ty);
        let (w, t, ch) = (
            self.g.width as usize,
            self.g.tile as usize,
            self.g.channels as usize,
        );
        let rows = rows as usize;
        band.clear();
        band.resize(ch * rows * w, 0.0);
        let (nx, _) = self.g.tiles(0);
        for tx in 0..nx {
            self.read_tile(tx, ty);
            let x0 = tx as usize * t;
            let cols = t.min(w - x0);
            for c in 0..ch {
                for r in 0..rows {
                    let src = &self.tile[(c * t + r) * t..][..cols];
                    band[(c * rows + r) * w + x0..][..cols].copy_from_slice(src);
                }
            }
        }
        y0
    }
}

/// Stream every tile row into a FITS file at `path`.
fn write_fits(
    reader: &TileReader,
    path: &Path,
    keywords: Vec<FitsKeyword>,
    ctl: &JobCtl,
) -> Result<ExportReport> {
    use crate::blend::RowSink;

    let mut tiles = TileRows::new(reader);
    let g = tiles.g;
    let mut sink = FitsSink::create(path, keywords)?;
    sink.begin(g.width, g.height, g.channels as u64)?;
    let mut band = Vec::new();
    for ty in 0..tiles.rows() {
        ctl.check()?;
        let y0 = tiles.band(ty, &mut band);
        sink.band(y0, &band)?;
        let (_, rows) = tiles.span(ty);
        ctl.report(EXPORT_STAGE, y0 + rows, g.height)?;
    }
    sink.finish()?;
    Ok(tiles.report)
}

/// Planar `f32` image staged in a memory-mapped scratch file, as the TIFF
/// writer's row source. Once `cancel` is set every row reads as zeros, so
/// a cancelled write finishes quickly.
struct Staged<'a> {
    map: Mmap,
    width: u64,
    height: u64,
    channels: u64,
    zeros: Vec<f32>,
    cancel: &'a CancelToken,
}

impl PlanarRows<f32> for Staged<'_> {
    fn width(&self) -> u64 {
        self.width
    }

    fn height(&self) -> u64 {
        self.height
    }

    fn channels(&self) -> u64 {
        self.channels
    }

    fn row(&self, c: u64, y: u64) -> &[f32] {
        if self.cancel.is_cancelled() {
            return &self.zeros;
        }
        let w = self.width as usize;
        let off = ((c * self.height + y) as usize) * w * 4;
        // The mapping is page aligned and every row starts at a multiple
        // of 4 bytes, so the cast cannot fail on alignment.
        bytemuck::cast_slice(&self.map[off..off + w * 4])
    }
}

/// Stage every tile row into `scratch` (planar `f32`, native endian), then
/// write it to `path` as a 32-bit float TIFF. `out` (the final name)
/// decides the container: `.btf`/`.tf8` is BigTIFF, anything else BigTIFF
/// only when the image needs it.
fn write_tiff(
    reader: &TileReader,
    path: &Path,
    scratch: &Path,
    out: &Path,
    history: &[String],
    ctl: &JobCtl,
) -> Result<ExportReport> {
    let mut tiles = TileRows::new(reader);
    let g = tiles.g;
    let (w, h, ch) = (g.width, g.height, g.channels as u64);
    // Reading the tiles is the first half of the progress, writing the
    // TIFF the second.
    let total = 2 * h;
    {
        let mut file = File::create(scratch).map_err(|e| Error::io(scratch, e))?;
        file.set_len(ch * h * w * 4)
            .map_err(|e| Error::io(scratch, e))?;
        let mut band = Vec::new();
        for ty in 0..tiles.rows() {
            ctl.check()?;
            let y0 = tiles.band(ty, &mut band);
            let (_, rows) = tiles.span(ty);
            let per_channel = (rows * w) as usize;
            for c in 0..ch {
                let at = (c * h + y0) * w * 4;
                file.seek(SeekFrom::Start(at))
                    .map_err(|e| Error::io(scratch, e))?;
                let samples = &band[c as usize * per_channel..][..per_channel];
                file.write_all(bytemuck::cast_slice(samples))
                    .map_err(|e| Error::io(scratch, e))?;
            }
            ctl.report(EXPORT_STAGE, y0 + rows, total)?;
        }
        file.flush().map_err(|e| Error::io(scratch, e))?;
    }
    let file = File::open(scratch).map_err(|e| Error::io(scratch, e))?;
    // SAFETY: the scratch file is private to this export (created above,
    // removed by the caller) and nothing writes or resizes it while mapped.
    let map = unsafe { Mmap::map(&file) }.map_err(|e| Error::io(scratch, e))?;
    let staged = Staged {
        map,
        width: w,
        height: h,
        channels: ch,
        zeros: vec![0.0; w as usize],
        cancel: ctl.cancel_token(),
    };
    let big = out
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("btf") || e.eq_ignore_ascii_case("tf8"));
    let opts = TiffWriteOptions {
        bit_depth: BitDepth::F32,
        container: if big { Container::Big } else { Container::Auto },
        description: Some(history.join("\n")),
        software: Some(format!("Mega Merge Mosaic {}", env!("CARGO_PKG_VERSION"))),
        ..TiffWriteOptions::default()
    };
    let mut progress = |done: u64, _total: u64| {
        // A cancel is acted on below, once the writer returns.
        let _ = ctl.report(EXPORT_STAGE, h + done, total);
    };
    write_tiff_native_with_progress(&staged, path, &opts, &mut progress)?;
    ctl.check()?;
    Ok(tiles.report)
}
