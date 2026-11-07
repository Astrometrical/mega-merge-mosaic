//! Astrometrical dark theme: palette constants and egui `Visuals` setup.

use egui::{Color32, Stroke};

pub mod palette;

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

/// The semantic tone of a status, check or notification. Tones are
/// paired with an icon wherever they are shown, so meaning never rests on
/// colour alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Done, passed, merged (OIII green).
    Success,
    /// Informational, in progress, ready (OIII accent).
    Info,
    /// Worth attention: notes, stale results (SII gold).
    Warning,
    /// Something is wrong (Hα).
    Problem,
    /// No particular meaning.
    Neutral,
}

impl Tone {
    /// Text and icon colour; meets AA on every surface.
    pub fn color(self) -> Color32 {
        match self {
            Tone::Success => palette::GREEN_300,
            Tone::Info => palette::ACCENT_400,
            Tone::Warning => palette::SII_400,
            Tone::Problem => palette::HA_400,
            Tone::Neutral => palette::TEXT_MUTED,
        }
    }

    /// Colour for borders and bars.
    pub fn accent(self) -> Color32 {
        match self {
            Tone::Success => palette::GREEN_400,
            Tone::Info => palette::ACCENT_500,
            Tone::Warning => palette::SII_400,
            Tone::Problem => palette::HA_500,
            Tone::Neutral => palette::BORDER_STRONG,
        }
    }

    /// A faint background tint: [`Tone::accent`] at alpha 20.
    pub fn tint(self) -> Color32 {
        let c = self.accent();
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 20)
    }
}

impl From<Severity> for Tone {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Problem => Tone::Problem,
            Severity::Note => Tone::Warning,
            Severity::Passed => Tone::Success,
        }
    }
}

/// Text colour used to present a [`Severity`] (its [`Tone::color`]).
pub fn severity_color(s: Severity) -> Color32 {
    Tone::from(s).color()
}

/// WCAG 2.x relative luminance of `c` (alpha ignored).
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG 2.x contrast ratio between `a` and `b` (1 to 21; alpha ignored).
/// Text needs at least 4.5 for AA.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `tint`, lightened toward white just enough to read as text on `bg`
/// (contrast at least 4.5:1); `tint` itself when it already does. Used for
/// text in a group or emission-line colour.
pub fn legible_text(tint: Color32, bg: Color32) -> Color32 {
    let mix = |v: u8, t: f32| (v as f32 + (255.0 - v as f32) * t).round() as u8;
    (0..=20)
        .map(|step| {
            let t = step as f32 / 20.0;
            Color32::from_rgb(mix(tint.r(), t), mix(tint.g(), t), mix(tint.b(), t))
        })
        .find(|c| contrast_ratio(*c, bg) >= 4.5)
        .unwrap_or(Color32::WHITE)
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
    use super::*;

    #[test]
    fn text_roles_meet_aa_on_all_surfaces() {
        use palette::*;
        let texts = [
            TEXT_STRONG,
            TEXT_EMPHASIS,
            TEXT_BODY,
            TEXT_MUTED,
            TEXT_SUBTLE,
            HA_400,
            SII_300,
            SII_400,
            GREEN_300,
            ACCENT_400,
        ];
        for bg in [SURFACE_900, SURFACE_800, SURFACE_700] {
            for t in texts {
                assert!(
                    contrast_ratio(t, bg) >= 4.5,
                    "{t:?} on {bg:?}: {}",
                    contrast_ratio(t, bg)
                );
            }
        }
    }

    #[test]
    fn ha_500_fails_aa_as_text() {
        // Why HA_400 exists: ha-500 is ~3.7:1 on astro-800.
        assert!(contrast_ratio(palette::HA_500, palette::SURFACE_800) < 4.5);
    }

    #[test]
    fn contrast_known_values() {
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(Color32::BLACK, Color32::BLACK) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn legible_text_keeps_hue_and_reaches_aa() {
        let t = legible_text(palette::HA_500, palette::SURFACE_700);
        assert!(contrast_ratio(t, palette::SURFACE_700) >= 4.5);
        assert!(t.r() > t.g() && t.r() > t.b(), "still red: {t:?}");
        assert_eq!(
            legible_text(palette::TEXT_STRONG, palette::SURFACE_700),
            palette::TEXT_STRONG
        );
    }

    #[test]
    fn severities_map_to_tones() {
        assert_eq!(severity_color(Severity::Problem), palette::HA_400);
        assert_eq!(severity_color(Severity::Note), palette::SII_400);
        assert_eq!(severity_color(Severity::Passed), palette::GREEN_300);
    }

    #[test]
    fn tones_have_colour_accent_and_tint() {
        assert_eq!(Tone::Warning.accent(), palette::SII_400);
        assert_eq!(Tone::Problem.accent(), palette::HA_500);
        let tint = Tone::Problem.tint();
        assert_eq!(tint.a(), 20);
        assert_eq!(Tone::from(Severity::Note), Tone::Warning);
    }
}
