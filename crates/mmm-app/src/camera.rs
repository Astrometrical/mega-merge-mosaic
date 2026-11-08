//! Where a canvas looks: a centre and a zoom in the shared frame's canvas
//! pixels. The plan and the merged mosaic are drawn in the same frame, so
//! one camera moves between them and nothing jumps when the window switches
//! the canvas it shows.

use mega_ui::viewer::{MAX_ZOOM, View};
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

    /// The camera that fits canvas rect `rect` (`[x0, y0, x1, y1]`) to a
    /// viewport of `viewport` size, centred, no closer than the canvases'
    /// [`MAX_ZOOM`].
    pub fn framing(rect: [f64; 4], viewport: egui::Vec2) -> Self {
        let w = (rect[2] - rect[0]).max(1e-9);
        let h = (rect[3] - rect[1]).max(1e-9);
        let zoom = (f64::from(viewport.x) / w)
            .min(f64::from(viewport.y) / h)
            .min(MAX_ZOOM);
        Camera {
            centre: ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0),
            zoom,
        }
    }
}
