//! The tile source interface the viewer pulls pixels from.

use super::{PyramidGeometry, TileKey};

/// Result of fetching one tile.
#[derive(Clone, Debug)]
pub enum Fetched {
    /// Current, display-ready pixels.
    Valid(egui::ColorImage),
    /// Pixels from before a change that has not been re-rendered yet; drawn
    /// dimmed and hatched.
    Stale(egui::ColorImage),
    /// No pixels for this tile (nothing rendered, or no data); drawn as
    /// background.
    Missing,
}

/// A pyramid of display-ready tiles.
///
/// Implementations are shared with the viewer's loader threads, so
/// [`TileSource::fetch`] may do I/O and decoding; it is never called on the UI
/// thread (except in the viewer's test-only synchronous mode).
///
/// [`TileSource::geometry`] and [`TileSource::generation`] are called on the
/// UI thread every frame: they must be cheap and never block (e.g. a stored
/// value and an atomic read), never wait on I/O or on a lock held during a
/// fetch.
pub trait TileSource: Send + Sync {
    /// Pyramid geometry of the image. Called on the UI thread every frame:
    /// must be cheap and non-blocking.
    fn geometry(&self) -> PyramidGeometry;

    /// Changes whenever any tile's content or appearance may have changed,
    /// e.g. a blend progressed or the display stretch changed. Called on the
    /// UI thread every frame: must be cheap and non-blocking (an atomic
    /// read).
    fn generation(&self) -> u64;

    /// Fetch one tile. Called on loader threads only.
    fn fetch(&self, key: TileKey) -> Fetched;
}
