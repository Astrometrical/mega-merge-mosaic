//! Buttons: primary, secondary, quiet, danger and ghost kinds; normal and
//! large sizes; icon buttons.

use egui::{
    Color32, CornerRadius, FontId, Response, RichText, Stroke, Ui, Vec2, Widget, WidgetInfo,
    WidgetType, style::WidgetVisuals,
};

use crate::theme::Tone;
use crate::theme::fonts::semibold;
use crate::theme::palette::*;
use crate::theme::tokens::{CONTROL_HEIGHT, CONTROL_HEIGHT_LARGE, radius};

/// What a button is for; decides its colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one main action of a view (accent fill).
    Primary,
    /// Ordinary actions (raised surface with a border).
    Secondary,
    /// Low-emphasis actions (text only until hovered).
    Quiet,
    /// Destructive actions (Hα outline).
    Danger,
    /// Actions over imagery: translucent, frosted look.
    Ghost,
}

/// Fill, border and text colour of one interaction state.
#[derive(Clone, Copy)]
struct StateLook {
    fill: Color32,
    stroke: Color32,
    text: Color32,
}

/// The idle, hovered and pressed looks of a kind.
fn looks(kind: ButtonKind) -> [StateLook; 3] {
    let s = |fill, stroke, text| StateLook { fill, stroke, text };
    let none = Color32::TRANSPARENT;
    match kind {
        ButtonKind::Primary => [
            s(ACCENT_500, ACCENT_500, ON_ACCENT),
            s(ACCENT_400, ACCENT_400, ON_ACCENT),
            s(ACCENT_600, ACCENT_600, ON_ACCENT),
        ],
        ButtonKind::Secondary => [
            s(SURFACE_700, BORDER, TEXT_EMPHASIS),
            s(SURFACE_700, ACCENT_500, TEXT_STRONG),
            s(BORDER, ACCENT_400, TEXT_STRONG),
        ],
        ButtonKind::Quiet => [
            s(none, none, TEXT_MUTED),
            s(SURFACE_700, none, TEXT_STRONG),
            s(BORDER, none, TEXT_STRONG),
        ],
        ButtonKind::Danger => {
            let edge = Color32::from_rgba_unmultiplied(HA_500.r(), HA_500.g(), HA_500.b(), 120);
            [
                s(none, edge, HA_400),
                s(Tone::Problem.tint(), edge, HA_400),
                s(Tone::Problem.tint(), HA_500, HA_400),
            ]
        }
        ButtonKind::Ghost => [
            s(
                Color32::from_white_alpha(31),
                Color32::from_white_alpha(41),
                TEXT_STRONG,
            ),
            s(
                Color32::from_white_alpha(51),
                Color32::from_white_alpha(51),
                TEXT_STRONG,
            ),
            s(
                Color32::from_white_alpha(64),
                Color32::from_white_alpha(64),
                TEXT_STRONG,
            ),
        ],
    }
}

fn set(w: &mut WidgetVisuals, l: StateLook) {
    w.bg_fill = l.fill;
    w.weak_bg_fill = l.fill;
    w.bg_stroke = Stroke::new(1.0, l.stroke);
    w.fg_stroke = Stroke::new(1.0, l.text);
    w.corner_radius = CornerRadius::same(radius::CONTROL);
    w.expansion = 0.0;
}

/// A themed button. Add it with `ui.add(MegaButton::new(kind, "Text"))`.
#[derive(Clone, Debug)]
pub struct MegaButton {
    kind: ButtonKind,
    text: String,
    icon: Option<char>,
    large: bool,
    square: bool,
}

impl MegaButton {
    /// A normal-size button of `kind` showing `text`.
    pub fn new(kind: ButtonKind, text: impl Into<String>) -> Self {
        MegaButton {
            kind,
            text: text.into(),
            icon: None,
            large: false,
            square: false,
        }
    }

    /// Large size: 38 points high with 14 pt semibold text. At most one per
    /// screen.
    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }

    /// Show the icon `glyph` (see [`crate::icons`]) before the text. The
    /// accessible label stays the text alone.
    pub fn icon(mut self, glyph: char) -> Self {
        self.icon = Some(glyph);
        self
    }
}

/// A square secondary button showing only `glyph`; `tooltip` is shown on
/// hover and is its accessible label.
pub fn icon_button(glyph: char, tooltip: &str) -> MegaButton {
    MegaButton {
        square: true,
        ..MegaButton::new(ButtonKind::Secondary, tooltip).icon(glyph)
    }
}

impl Widget for MegaButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let height = if self.large {
            CONTROL_HEIGHT_LARGE
        } else {
            CONTROL_HEIGHT
        };
        let label = match (self.icon, self.square) {
            (Some(g), true) => g.to_string(),
            (Some(g), false) => format!("{g}  {}", self.text),
            (None, _) => self.text.clone(),
        };
        let mut rich = RichText::new(label);
        if self.large {
            rich = rich.font(FontId::new(14.0, semibold()));
        }
        let [idle, hovered, pressed] = looks(self.kind);
        let enabled = ui.is_enabled();
        let response = ui
            .scope(|ui| {
                let w = &mut ui.style_mut().visuals.widgets;
                set(&mut w.inactive, idle);
                set(&mut w.hovered, hovered);
                set(&mut w.active, pressed);
                let min = if self.square {
                    ui.spacing_mut().button_padding = Vec2::ZERO;
                    Vec2::splat(height)
                } else {
                    Vec2::new(0.0, height)
                };
                ui.add(egui::Button::new(rich).min_size(min))
            })
            .inner;
        let text = self.text;
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &text));
        if self.square {
            response.on_hover_text(text)
        } else {
            response
        }
    }
}

#[cfg(test)]
#[path = "button_tests.rs"]
mod tests;
