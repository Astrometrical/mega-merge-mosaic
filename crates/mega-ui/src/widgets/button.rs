//! Buttons: primary, secondary, quiet, danger and ghost kinds; normal and
//! large sizes; icon buttons.

use egui::{
    Color32, CornerRadius, FontId, Response, Stroke, StrokeKind, TextStyle, Ui, Vec2, Widget,
    WidgetInfo, WidgetType,
    style::WidgetVisuals,
    text::{LayoutJob, TextFormat},
};

use crate::icons;
use crate::theme::Tone;
use crate::theme::fonts::semibold;
use crate::theme::palette::*;
use crate::theme::tokens::{CONTROL_HEIGHT, CONTROL_HEIGHT_LARGE, radius, space};

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
pub(crate) struct StateLook {
    pub(crate) fill: Color32,
    pub(crate) stroke: Color32,
    pub(crate) text: Color32,
}

/// The idle, hovered and pressed looks of a kind.
pub(crate) fn looks(kind: ButtonKind) -> [StateLook; 3] {
    let s = |fill, stroke, text| StateLook { fill, stroke, text };
    let none = Color32::TRANSPARENT;
    match kind {
        // Darker than the web's 500/400 so white text stays AA.
        ButtonKind::Primary => [
            s(ACCENT_600, ACCENT_600, ON_ACCENT),
            s(ACCENT_550, ACCENT_550, ON_ACCENT),
            s(ACCENT_700, ACCENT_700, ON_ACCENT),
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
    trailing: Option<char>,
    large: bool,
    square: bool,
    hover: Option<String>,
}

impl MegaButton {
    /// A normal-size button of `kind` showing `text`.
    pub fn new(kind: ButtonKind, text: impl Into<String>) -> Self {
        MegaButton {
            kind,
            text: text.into(),
            icon: None,
            trailing: None,
            large: false,
            square: false,
            hover: None,
        }
    }

    /// Show `text` on hover; an icon button shows it in place of its
    /// tooltip (e.g. to name a keyboard shortcut too). The accessible label
    /// stays the button's text.
    pub fn hover_text(mut self, text: impl Into<String>) -> Self {
        self.hover = Some(text.into());
        self
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

    /// Show the icon `glyph` after the text (e.g. a disclosure chevron).
    pub fn trailing_icon(mut self, glyph: char) -> Self {
        self.trailing = Some(glyph);
        self
    }
}

/// Paint the keyboard focus ring (2 pt accent, just outside `response`'s
/// rect) when `response` has focus. Every focusable Mega widget shows it.
pub fn focus_ring(ui: &Ui, response: &Response, corner: u8) {
    focus_ring_with(ui.painter(), response, corner);
}

/// [`focus_ring`] through `painter` (a clipped one keeps the ring inside a
/// canvas for a widget at its edge).
pub fn focus_ring_with(painter: &egui::Painter, response: &Response, corner: u8) {
    if response.has_focus() {
        painter.rect_stroke(
            response.rect.expand(2.0),
            CornerRadius::same(corner + 2),
            Stroke::new(2.0, ACCENT_400),
            StrokeKind::Outside,
        );
    }
}

/// A square secondary button showing only `glyph`; `tooltip` is shown on
/// hover and is its accessible label.
pub fn icon_button(glyph: char, tooltip: &str) -> MegaButton {
    icon_button_of(ButtonKind::Secondary, glyph, tooltip)
}

/// Like [`icon_button`], in the given `kind` (e.g. ghost over imagery).
pub fn icon_button_of(kind: ButtonKind, glyph: char, tooltip: &str) -> MegaButton {
    MegaButton {
        square: true,
        ..MegaButton::new(kind, tooltip).icon(glyph)
    }
}

impl Widget for MegaButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let height = if self.large {
            CONTROL_HEIGHT_LARGE
        } else {
            CONTROL_HEIGHT
        };
        let text_font = if self.large {
            FontId::new(14.0, semibold())
        } else {
            TextStyle::Button.resolve(ui.style())
        };
        // Placeholder colours take the button's state colour.
        let mut job = LayoutJob::default();
        if let Some(g) = self.icon {
            let format =
                TextFormat::simple(icons::font(text_font.size + 1.0), Color32::PLACEHOLDER);
            job.append(&g.to_string(), 0.0, format);
        }
        if !self.square {
            let gap = if self.icon.is_some() { space::S } else { 0.0 };
            let size = text_font.size;
            job.append(
                &self.text,
                gap,
                TextFormat::simple(text_font, Color32::PLACEHOLDER),
            );
            if let Some(g) = self.trailing {
                let format = TextFormat::simple(icons::font(size + 1.0), Color32::PLACEHOLDER);
                job.append(&g.to_string(), space::XS, format);
            }
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
                ui.add(egui::Button::new(job).min_size(min))
            })
            .inner;
        focus_ring(ui, &response, radius::CONTROL);
        let text = self.text;
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &text));
        match self.hover {
            Some(hover) => response.on_hover_text(hover),
            None if self.square => response.on_hover_text(text),
            None => response,
        }
    }
}

#[cfg(test)]
#[path = "button_tests.rs"]
mod tests;
