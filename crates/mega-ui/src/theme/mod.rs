//! The Mega theme: Astrometrical colour tokens, fonts, the type scale,
//! layout tokens and the complete egui style.

use egui::Color32;

pub mod fonts;
pub mod palette;
pub mod style;
pub mod text;
pub mod tokens;

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

    /// The Lucide glyph shown with this tone, so meaning never rests on
    /// colour alone; `None` for [`Tone::Neutral`].
    pub fn icon(self) -> Option<char> {
        use crate::icons;
        match self {
            Tone::Success => Some(icons::CHECK),
            Tone::Info => Some(icons::INFO),
            Tone::Warning => Some(icons::TRIANGLE_ALERT),
            Tone::Problem => Some(icons::CIRCLE_X),
            Tone::Neutral => None,
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

/// Install the Mega theme on `ctx`: the embedded fonts, the dark theme
/// (forced, whatever the OS prefers) and the complete [`style::style`].
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts::definitions());
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style::style());
}

/// Whether [`apply`]'s fonts are in use on `ctx`. Fonts set by `apply`
/// take effect on the next frame, and egui panics if a widget asks for a
/// family that isn't bound yet (e.g. [`fonts::semibold`]). Apps call
/// `apply` before their first frame; test harnesses that build their own
/// context check this and skip drawing for one frame.
pub fn is_applied(ctx: &egui::Context) -> bool {
    ctx.fonts(|f| f.definitions().families.contains_key(&fonts::semibold()))
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

    #[test]
    fn is_applied_once_fonts_have_loaded() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        assert!(!is_applied(&ctx));
        apply(&ctx);
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        assert!(is_applied(&ctx));
    }

    #[test]
    fn tones_carry_icons() {
        use crate::icons;
        assert_eq!(Tone::Success.icon(), Some(icons::CHECK));
        assert_eq!(Tone::Info.icon(), Some(icons::INFO));
        assert_eq!(Tone::Warning.icon(), Some(icons::TRIANGLE_ALERT));
        assert_eq!(Tone::Problem.icon(), Some(icons::CIRCLE_X));
        assert_eq!(Tone::Neutral.icon(), None);
    }
}
