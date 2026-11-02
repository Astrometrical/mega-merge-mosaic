//! Deep-zoom image viewer building blocks: viewport maths, animated springs
//! and pyramid tile selection (no rendering, no clock).

mod view;

pub use view::{
    ANIMATION_TIME, MAX_ZOOM, PyramidGeometry, SPRING_STIFFNESS, Spring, TileKey, View, pick_level,
    visible_tiles,
};
