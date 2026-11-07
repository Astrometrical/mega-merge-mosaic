//! The complete egui [`Style`]: type scale, spacing, radii, motion and
//! the dark visuals.

use egui::{CornerRadius, FontFamily, FontId, Margin, Stroke, Style, TextStyle, Visuals, vec2};

use super::fonts::semibold;
use super::palette::*;
use super::text::{OVERLINE_SIZE, Type};
use super::tokens::{CONTROL_HEIGHT, DISABLED_ALPHA, HOVER_FADE_S, radius, space};

/// The Mega apps' egui style (dark).
pub fn style() -> Style {
    let mut s = Style {
        visuals: Visuals::dark(),
        ..Style::default()
    };

    let prop = FontFamily::Proportional;
    let mono = FontFamily::Monospace;
    s.text_styles = [
        (Type::Hero.text_style(), FontId::new(32.0, semibold())),
        (Type::Title.text_style(), FontId::new(20.0, semibold())),
        (TextStyle::Heading, FontId::new(15.0, semibold())),
        (TextStyle::Body, FontId::new(13.0, prop.clone())),
        (TextStyle::Small, FontId::new(12.0, prop.clone())),
        (TextStyle::Button, FontId::new(13.0, prop)),
        (
            Type::Overline.text_style(),
            FontId::new(OVERLINE_SIZE, semibold()),
        ),
        (Type::Numeric.text_style(), FontId::new(12.0, mono.clone())),
        (TextStyle::Monospace, FontId::new(12.0, mono)),
    ]
    .into();

    let sp = &mut s.spacing;
    sp.item_spacing = vec2(space::S, space::S);
    sp.button_padding = vec2(space::M, 6.0);
    sp.interact_size = vec2(40.0, CONTROL_HEIGHT);
    sp.window_margin = Margin::same(space::L as i8);
    sp.menu_margin = Margin::same(space::S as i8);
    sp.indent = space::L;

    s.animation_time = HOVER_FADE_S;

    let v = &mut s.visuals;
    v.panel_fill = SURFACE_800;
    v.window_fill = SURFACE_800;
    v.extreme_bg_color = SURFACE_900;
    v.faint_bg_color = SURFACE_700;
    v.text_edit_bg_color = Some(SURFACE_900);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(radius::CARD);
    v.menu_corner_radius = CornerRadius::same(radius::CONTROL);
    v.override_text_color = None;
    v.hyperlink_color = ACCENT_400;
    v.warn_fg_color = SII_400;
    v.error_fg_color = HA_400;
    v.disabled_alpha = DISABLED_ALPHA;
    v.selection.bg_fill = ACCENT_600;
    // egui outlines a focused text edit with the selection stroke.
    v.selection.stroke = Stroke::new(2.0, ACCENT_500);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE_800;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_BODY);
    w.inactive.bg_fill = SURFACE_700;
    w.inactive.weak_bg_fill = SURFACE_700;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_BODY);
    w.hovered.bg_fill = SURFACE_700;
    w.hovered.weak_bg_fill = SURFACE_700;
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT_500);
    w.hovered.fg_stroke = Stroke::new(1.5, TEXT_STRONG);
    w.active.bg_fill = ACCENT_600;
    w.active.weak_bg_fill = ACCENT_600;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT_400);
    w.active.fg_stroke = Stroke::new(2.0, TEXT_STRONG);
    w.open.bg_fill = SURFACE_700;
    w.open.weak_bg_fill = SURFACE_700;
    w.open.bg_stroke = Stroke::new(1.0, BORDER);
    for wv in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        wv.corner_radius = CornerRadius::same(radius::CONTROL);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::{apply, fonts, palette};
    use super::*;
    use crate::theme::text::{Type, text};
    use egui::{CornerRadius, FontFamily, Theme};

    #[test]
    fn type_scale_matches_spec() {
        let s = style();
        let size = |t: Type| s.text_styles[&t.text_style()].size;
        assert_eq!(
            [
                size(Type::Hero),
                size(Type::Title),
                size(Type::Heading),
                size(Type::Body),
                size(Type::Small),
                size(Type::Overline),
                size(Type::Numeric)
            ],
            [32.0, 20.0, 15.0, 13.0, 12.0, 10.5, 12.0]
        );
        assert_eq!(
            s.text_styles[&Type::Hero.text_style()].family,
            fonts::semibold()
        );
        assert_eq!(
            s.text_styles[&Type::Numeric.text_style()].family,
            FontFamily::Monospace
        );
    }

    #[test]
    fn density_and_radius_match_spec() {
        let s = style();
        assert_eq!(s.spacing.interact_size.y, 30.0);
        assert_eq!(
            s.visuals.widgets.inactive.corner_radius,
            CornerRadius::same(6)
        );
        assert_eq!(s.visuals.window_corner_radius, CornerRadius::same(8));
        assert_eq!(s.animation_time, 0.1);
    }

    #[test]
    fn apply_forces_dark_even_when_the_os_is_light() {
        let ctx = egui::Context::default();
        ctx.options_mut(|o| o.fallback_theme = Theme::Light);
        apply(&ctx);
        assert_eq!(ctx.theme(), Theme::Dark);
        assert_eq!(ctx.global_style().visuals.panel_fill, palette::SURFACE_800);

        // The OS switching to light while running changes nothing.
        let input = egui::RawInput {
            system_theme: Some(Theme::Light),
            ..Default::default()
        };
        ctx.run_ui(input, |_| {}).textures_delta.clear();
        assert_eq!(ctx.theme(), Theme::Dark);
        assert_eq!(ctx.global_style().visuals.panel_fill, palette::SURFACE_800);
    }

    #[test]
    fn overline_is_tracked_caps() {
        let job = text(Type::Overline, "Recent sessions");
        assert_eq!(job.text(), "RECENT SESSIONS");
    }
}
