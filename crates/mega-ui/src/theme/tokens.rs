//! Layout and motion tokens: spacing steps, corner radii, control sizes,
//! durations. App code uses these instead of literals.

/// Spacing steps, in points. Use only these.
pub mod space {
    /// 4 points.
    pub const XS: f32 = 4.0;
    /// 8 points.
    pub const S: f32 = 8.0;
    /// 12 points.
    pub const M: f32 = 12.0;
    /// 16 points.
    pub const L: f32 = 16.0;
    /// 24 points.
    pub const XL: f32 = 24.0;
    /// 32 points.
    pub const XXL: f32 = 32.0;
}

/// Corner radii, in points. Pills (chips) use half their height.
pub mod radius {
    /// Small controls.
    pub const SMALL: u8 = 4;
    /// Buttons and inputs.
    pub const CONTROL: u8 = 6;
    /// Cards, panels, thumbnails, windows.
    pub const CARD: u8 = 8;
}

/// Width of the unfolded inspector column.
pub const INSPECTOR_WIDTH: f32 = 320.0;
/// Narrowest a list column beside the canvas (e.g. a panel list) gets.
pub const PANEL_LIST_MIN_WIDTH: f32 = 420.0;
/// Narrowest the canvas gets beside such a list (the list stops growing).
pub const CANVAS_MIN_WIDTH: f32 = 360.0;
/// Width of a short single-line input (a keyword, a header key).
pub const KEYWORD_INPUT_WIDTH: f32 = 120.0;
/// Height of buttons, inputs and segmented controls.
pub const CONTROL_HEIGHT: f32 = 30.0;
/// Height of a large button (at most one per screen).
pub const CONTROL_HEIGHT_LARGE: f32 = 38.0;
/// Hover and press colour fades, in seconds.
pub const HOVER_FADE_S: f32 = 0.1;
/// Opacity of disabled widgets.
pub const DISABLED_ALPHA: f32 = 0.4;

/// Hero slideshow: seconds each view is shown before the crossfade starts.
pub const HERO_DWELL_S: f64 = 8.0;
/// Hero slideshow: crossfade length, in seconds.
pub const HERO_CROSSFADE_S: f64 = 2.0;
/// Hero slideshow: zoom factor the drift reaches over a view's time on
/// screen.
pub const KEN_BURNS_ZOOM: f64 = 1.08;

/// Width of a modal dialog's body (the Export dialog).
pub const DIALOG_WIDTH: f32 = 520.0;
/// Alpha of the black backdrop behind a modal dialog.
pub const DIALOG_BACKDROP_ALPHA: u8 = 150;

/// Height of a canvas badge (icon and count), in points: constant on
/// screen however far the canvas is zoomed.
pub const BADGE_HEIGHT: f32 = 22.0;
/// Horizontal padding inside a canvas badge, in points.
pub const BADGE_PAD: f32 = 7.0;
/// Width of a polyline drawn over a canvas (a seam, say), in points.
pub const LINE_WIDTH: f32 = 2.0;
/// Width of a polyline that needs attention (a seam flagged as showing),
/// in points: thicker than [`LINE_WIDTH`], so its colour is not the only
/// signal.
pub const LINE_WIDTH_STRONG: f32 = 3.5;
/// Dash and gap lengths of a dashed polyline, in points.
pub const LINE_DASH: (f32, f32) = (8.0, 5.0);

/// Width of a hairline outline (a hovered row's border), in points.
pub const HAIRLINE_WIDTH: f32 = 1.0;
