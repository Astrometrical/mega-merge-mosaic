//! Session card: a thumbnail clipped to the mosaic's outline, a title and a
//! status line. Used by the start screen's shelf and session grid.

use egui::{
    Align2, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, TextStyle, TextureHandle, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetType,
    text::{LayoutJob, TextFormat, TextWrapping},
};

use super::focus_ring;
use crate::icons;
use crate::theme::Tone;
use crate::theme::palette::{ACCENT_500, BORDER, SURFACE_800, SURFACE_900, TEXT_STRONG};
use crate::theme::tokens::{DISABLED_ALPHA, radius, space};

/// Card width, in points.
pub const CARD_WIDTH: f32 = 220.0;
/// Height of the thumbnail band, in points.
pub const CARD_THUMB_HEIGHT: f32 = 96.0;
/// Height of the title and status area under the thumbnail.
const BODY_HEIGHT: f32 = 58.0;
/// Border width of a selected card.
const SELECTED_BORDER: f32 = 2.0;
/// Size of the pin glyph.
const PIN_SIZE: f32 = 14.0;
/// Diameter of the status dot (as in `status`).
const STATUS_DOT: f32 = 7.0;

/// What a [`session_card`] shows.
pub struct SessionCardData<'a> {
    /// Session title; also the card's accessible label.
    pub title: &'a str,
    /// Status line: tone and text, if any.
    pub status: Option<(Tone, &'a str)>,
    /// Show a pin marker at the thumbnail's top right.
    pub pinned: bool,
    /// Draw everything faded (a missing session).
    pub dimmed: bool,
    /// Draw the selected border.
    pub selected: bool,
    /// Thumbnail, fitted inside the band; transparent pixels (no data)
    /// show the band's dark fill through.
    pub thumbnail: Option<&'a TextureHandle>,
}

/// A session card of [`CARD_WIDTH`]: thumbnail band, title, status line.
/// Senses click and double-click; the title is its accessible label.
pub fn session_card(ui: &mut Ui, card: &SessionCardData) -> Response {
    let size = Vec2::new(CARD_WIDTH, CARD_THUMB_HEIGHT + BODY_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, card.title));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let mut ui = ui.new_child(UiBuilder::new().max_rect(rect));
    if card.dimmed {
        ui.set_opacity(DISABLED_ALPHA);
    }
    let corner = CornerRadius::same(radius::CARD);
    let p = ui.painter().clone();
    p.rect_filled(rect, corner, SURFACE_800);

    let band = Rect::from_min_size(rect.min, Vec2::new(CARD_WIDTH, CARD_THUMB_HEIGHT));
    let band_corner = CornerRadius {
        nw: radius::CARD,
        ne: radius::CARD,
        sw: 0,
        se: 0,
    };
    p.rect_filled(band, band_corner, SURFACE_900);
    if let Some(tex) = card.thumbnail {
        let tex_size = tex.size_vec2();
        if tex_size.x > 0.0 && tex_size.y > 0.0 {
            let inner = band.shrink(space::S);
            let scale = (inner.width() / tex_size.x).min(inner.height() / tex_size.y);
            let img = Rect::from_center_size(inner.center(), tex_size * scale);
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            p.image(tex.id(), img, uv, egui::Color32::WHITE);
        }
    }
    if card.pinned {
        p.text(
            band.right_top() + Vec2::new(-space::S, space::S),
            Align2::RIGHT_TOP,
            icons::PIN,
            icons::font(PIN_SIZE),
            TEXT_STRONG,
        );
    }

    let max_text = CARD_WIDTH - 2.0 * space::M;
    let mut job = LayoutJob::single_section(
        card.title.to_owned(),
        TextFormat::simple(TextStyle::Body.resolve(ui.style()), TEXT_STRONG),
    );
    job.wrap = TextWrapping {
        max_width: max_text,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = p.layout_job(job);
    let text_top = band.bottom() + space::S;
    let title_pos = egui::pos2(rect.left() + space::M, text_top);
    let title_h = galley.size().y;
    p.galley(title_pos, galley, TEXT_STRONG);
    if let Some((tone, label)) = card.status {
        // Painted, not a label: labels sense clicks and would swallow the
        // card's click and double-click over the status row.
        let y = text_top + title_h + space::XS;
        let x = rect.left() + space::M;
        let mut job = LayoutJob::single_section(
            label.to_owned(),
            TextFormat::simple(TextStyle::Small.resolve(ui.style()), tone.color()),
        );
        job.wrap = TextWrapping {
            max_width: max_text - STATUS_DOT - space::S,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = p.layout_job(job);
        let mid = y + galley.size().y / 2.0;
        p.circle_filled(
            egui::pos2(x + STATUS_DOT / 2.0, mid),
            STATUS_DOT / 2.0,
            tone.color(),
        );
        p.galley(
            egui::pos2(x + STATUS_DOT + space::S, y),
            galley,
            tone.color(),
        );
    }

    let (width, color) = if card.selected {
        (SELECTED_BORDER, ACCENT_500)
    } else {
        (1.0, BORDER)
    };
    p.rect_stroke(rect, corner, Stroke::new(width, color), StrokeKind::Inside);
    focus_ring(&ui, &response, radius::CARD);
    response
}

#[cfg(test)]
#[path = "session_card_tests.rs"]
mod tests;
