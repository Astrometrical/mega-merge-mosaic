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
/// thread (except in the viewer's synchronous test mode).
pub trait TileSource: Send + Sync {
    /// Pyramid geometry of the image.
    fn geometry(&self) -> PyramidGeometry;

    /// Changes whenever any tile's content or appearance may have changed,
    /// e.g. a blend progressed or the display stretch changed.
    fn generation(&self) -> u64;

    /// Fetch one tile. Called on loader threads only.
    fn fetch(&self, key: TileKey) -> Fetched;
}
