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

/// Height of buttons, inputs and segmented controls.
pub const CONTROL_HEIGHT: f32 = 30.0;
/// Height of a large button (at most one per screen).
pub const CONTROL_HEIGHT_LARGE: f32 = 38.0;
/// Hover and press colour fades, in seconds.
pub const HOVER_FADE_S: f32 = 0.1;
/// Opacity of disabled widgets.
pub const DISABLED_ALPHA: f32 = 0.4;
