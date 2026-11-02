//! Deep-zoom image viewer: viewport maths, animated springs, pyramid tile
//! selection, and the tiled viewer widget with background loading.

mod cache;
mod source;
mod view;
mod widget;

pub use source::{Fetched, TileSource};
pub use view::{
    ANIMATION_TIME, MAX_ZOOM, PyramidGeometry, SPRING_STIFFNESS, Spring, TileKey, View, pick_level,
    visible_tiles,
};
pub use widget::{TiledViewer, ViewerResponse, tile_tint};
