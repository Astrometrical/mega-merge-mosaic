//! Astrometrical dark theme: palette constants and egui `Visuals` setup.

use egui::{Color32, Stroke};

/// Astrometrical brand palette.
pub mod palette {
    use egui::Color32;

    /// Darkest surface (`#050505`).
    pub const SURFACE_900: Color32 = Color32::from_rgb(0x05, 0x05, 0x05);
    /// Panel surface (`#0e0e0e`).
    pub const SURFACE_800: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x0e);
    /// Raised surface (`#1a1a1a`).
    pub const SURFACE_700: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
    /// Borders (`#2a2a2a`).
    pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
    /// OIII accent, light (`#4EBCD4`).
    pub const ACCENT_400: Color32 = Color32::from_rgb(0x4e, 0xbc, 0xd4);
    /// OIII accent, primary (`#2A95AB`).
    pub const ACCENT_500: Color32 = Color32::from_rgb(0x2a, 0x95, 0xab);
    /// OIII accent, dark (`#1D7588`).
    pub const ACCENT_600: Color32 = Color32::from_rgb(0x1d, 0x75, 0x88);
    /// H-alpha red, for problems (`#C83050`).
    pub const HA_500: Color32 = Color32::from_rgb(0xc8, 0x30, 0x50);
    /// OIII green, light, for success (`#4ED090`).
    pub const GREEN_300: Color32 = Color32::from_rgb(0x4e, 0xd0, 0x90);
    /// OIII green, primary (`#2AB875`).
    pub const GREEN_400: Color32 = Color32::from_rgb(0x2a, 0xb8, 0x75);
    /// Strong text (`#eeeeee`).
    pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xee, 0xee, 0xee);
    /// Body text (`#aaaaaa`).
    pub const TEXT_BODY: Color32 = Color32::from_rgb(0xaa, 0xaa, 0xaa);
    /// Muted text (`#999999`).
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x99, 0x99, 0x99);
    /// Subtle text (`#8a8a8a`).
    pub const TEXT_SUBTLE: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
}

/// Severity of a check result, independent of any engine's own type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Something is wrong.
    Problem,
    /// Informational.
    Note,
    /// The check passed.
    Passed,
}

/// Colour used to present a [`Severity`].
pub fn severity_color(s: Severity) -> Color32 {
    match s {
        Severity::Problem => palette::HA_500,
        Severity::Note => palette::TEXT_MUTED,
        Severity::Passed => palette::GREEN_300,
    }
}

/// Install the dark Astrometrical visuals on `ctx`.
pub fn apply(ctx: &egui::Context) {
    use palette::*;
    let mut v = egui::Visuals::dark();
    v.panel_fill = SURFACE_800;
    v.window_fill = SURFACE_800;
    v.extreme_bg_color = SURFACE_900;
    v.faint_bg_color = SURFACE_700;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.override_text_color = None;
    v.hyperlink_color = ACCENT_400;
    v.selection.bg_fill = ACCENT_600;
    v.selection.stroke = Stroke::new(1.0, ACCENT_400);

    v.widgets.noninteractive.bg_fill = SURFACE_800;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_BODY);
    v.widgets.inactive.bg_fill = SURFACE_700;
    v.widgets.inactive.weak_bg_fill = SURFACE_700;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_BODY);
    v.widgets.hovered.bg_fill = SURFACE_700;
    v.widgets.hovered.weak_bg_fill = SURFACE_700;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_500);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT_STRONG);
    v.widgets.active.bg_fill = ACCENT_600;
    v.widgets.active.weak_bg_fill = ACCENT_600;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_400);
    v.widgets.active.fg_stroke = Stroke::new(2.0, TEXT_STRONG);
    v.widgets.open.bg_fill = SURFACE_700;
    v.widgets.open.weak_bg_fill = SURFACE_700;
    v.widgets.open.bg_stroke = Stroke::new(1.0, BORDER);
    ctx.set_visuals(v);
}

#[cfg(test)]
mod tests {
    use super::palette::*;

    #[test]
    fn palette_matches_brand() {
        let rgb = |c: egui::Color32| (c.r(), c.g(), c.b());
        assert_eq!(rgb(SURFACE_900), (0x05, 0x05, 0x05));
        assert_eq!(rgb(SURFACE_800), (0x0e, 0x0e, 0x0e));
        assert_eq!(rgb(SURFACE_700), (0x1a, 0x1a, 0x1a));
        assert_eq!(rgb(BORDER), (0x2a, 0x2a, 0x2a));
        assert_eq!(rgb(ACCENT_400), (0x4e, 0xbc, 0xd4));
        assert_eq!(rgb(ACCENT_500), (0x2a, 0x95, 0xab));
        assert_eq!(rgb(ACCENT_600), (0x1d, 0x75, 0x88));
        assert_eq!(rgb(HA_500), (0xc8, 0x30, 0x50));
        assert_eq!(rgb(GREEN_300), (0x4e, 0xd0, 0x90));
        assert_eq!(rgb(GREEN_400), (0x2a, 0xb8, 0x75));
        assert_eq!(rgb(TEXT_STRONG), (0xee, 0xee, 0xee));
        assert_eq!(rgb(TEXT_BODY), (0xaa, 0xaa, 0xaa));
        assert_eq!(rgb(TEXT_MUTED), (0x99, 0x99, 0x99));
        assert_eq!(rgb(TEXT_SUBTLE), (0x8a, 0x8a, 0x8a));
    }
}
