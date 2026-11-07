//! Deep-zoom image viewer: viewport maths, animated springs, pyramid tile
//! selection, and the tiled viewer widget with background loading.

mod cache;
mod image_source;
mod source;
mod view;
mod widget;

pub use image_source::ImageTileSource;
pub use source::{Fetched, TileSource};
pub use view::{
    ANIMATION_TIME, MAX_ZOOM, PyramidGeometry, SPRING_STIFFNESS, Spring, TileKey, View, pick_level,
    visible_tiles,
};
pub use widget::{Overlay, TiledViewer, Tool, ViewerResponse, tile_tint};
pub(crate) use widget::{OverlayButton, drag_delta, overlay_buttons};
