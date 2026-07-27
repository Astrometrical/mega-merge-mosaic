//! `PanelSource` — the abstraction that lets `analyze`/`blend` pull panel
//! pixels without knowing whether they come from a file or the IPC host.

use std::sync::Arc;

use crate::Result;
use crate::ipc::client::HostLink;
use crate::panel_reader::{PanelReader, PanelStorage};
use crate::session::PanelMeta;

/// Opens a [`PanelReader`] for one panel, hiding whether its pixels live in
/// a file or are pulled on demand from the IPC host. `blend`'s single
/// reader-opening choke-point ([`crate::blend::blend_with_source`]) goes
/// through this trait so the blend algorithm itself never learns which.
pub trait PanelSource: Sync {
    /// Open `meta` for reading against `canvas` geometry.
    fn open_reader(&self, meta: &PanelMeta, canvas: (u64, u64, u64)) -> Result<PanelReader>;
}

/// The default source: every panel is opened from its file on disk via
/// [`PanelReader::open`] — unchanged behaviour from before `PanelSource`
/// existed.
pub struct FileSource;

impl PanelSource for FileSource {
    fn open_reader(&self, meta: &PanelMeta, canvas: (u64, u64, u64)) -> Result<PanelReader> {
        PanelReader::open(meta, canvas)
    }
}

/// An IPC-driven source: panels stored as [`PanelStorage::Ipc`] are pulled
/// on demand from the host over `link`, `band_rows` canvas rows at a time.
/// Panels still backed by a solved-mode reprojection cache on disk
/// ([`PanelStorage::FullCanvasXisf`] / [`PanelStorage::CroppedCache`]) fall
/// back to the ordinary file open — those legitimately live on disk even in
/// an IPC run.
pub struct IpcSource {
    link: Arc<HostLink>,
    band_rows: usize,
}

impl IpcSource {
    /// Serve panels over `link`, requesting `band_rows` canvas rows per IPC
    /// fetch.
    pub fn new(link: Arc<HostLink>, band_rows: usize) -> IpcSource {
        IpcSource { link, band_rows }
    }
}

impl PanelSource for IpcSource {
    fn open_reader(&self, meta: &PanelMeta, canvas: (u64, u64, u64)) -> Result<PanelReader> {
        match meta.storage {
            PanelStorage::Ipc { panel_id } => Ok(PanelReader::open_ipc(
                self.link.clone(),
                panel_id,
                canvas,
                self.band_rows,
            )),
            PanelStorage::FullCanvasXisf | PanelStorage::CroppedCache { .. } => {
                PanelReader::open(meta, canvas)
            }
        }
    }
}
