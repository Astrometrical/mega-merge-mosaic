//! Where a canvas looks: a centre and a zoom in the shared frame's canvas
//! pixels. The plan and the merged mosaic are drawn in the same frame, so
//! one camera moves between them and nothing jumps when the window switches
//! the canvas it shows.

use mega_ui::viewer::View;
use serde::{Deserialize, Serialize};

/// A canvas's centre and zoom, without the animation state of a [`View`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// The canvas point at the middle of the viewport (canvas pixels).
    pub centre: (f64, f64),
    /// Screen pixels per canvas pixel.
    pub zoom: f64,
}

impl Camera {
    /// What `view` shows at `now` (`ui.input().time`).
    pub fn of(view: &View, now: f64) -> Self {
        Camera {
            centre: view.centre(now),
            zoom: view.zoom(now),
        }
    }

    /// Jump `view` to this camera, with no animation (the zoom is clamped
    /// to the view's limits).
    #[allow(dead_code)] // used by the mosaic view from Task 6 on
    pub fn apply(self, view: &mut View, now: f64) {
        view.set_view(self.centre, self.zoom, now);
    }
}
