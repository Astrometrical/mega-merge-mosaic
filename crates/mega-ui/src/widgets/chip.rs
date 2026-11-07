//! Chips: small pill labels, neutral or tinted.

use egui::{
    Color32, CornerRadius, Response, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2, WidgetInfo,
    WidgetType,
    text::{LayoutJob, TextFormat, TextWrapping},
};

use crate::theme::legible_text;
use crate::theme::palette::{BORDER, SURFACE_700, TEXT_BODY};

/// Widest a chip may grow; longer labels are truncated with "…" and shown
/// in full on hover.
pub const CHIP_MAX_WIDTH: f32 = 160.0;
/// Chip height.
const HEIGHT: f32 = 22.0;
/// Horizontal padding inside a chip.
const PAD: f32 = 9.0;

/// A chip showing `label`. With `tint` (a group or emission-line colour)
/// its border takes the tint and its text a legible version of it;
/// otherwise it is neutral. The accessible label is always the full text.
pub fn chip(ui: &mut Ui, label: &str, tint: Option<Color32>) -> Response {
    let (text_color, border) = match tint {
        None => (TEXT_BODY, BORDER),
        Some(t) => (
            legible_text(t, SURFACE_700),
            Color32::from_rgba_unmultiplied(t.r(), t.g(), t.b(), 110),
        ),
    };
    let font = TextStyle::Small.resolve(ui.style());
    let max_text = CHIP_MAX_WIDTH - 2.0 * PAD;
    let mut job = LayoutJob::single_section(
        label.to_owned(),
        TextFormat::simple(font.clone(), text_color),
    );
    job.wrap = TextWrapping {
        max_width: max_text,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let full_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, text_color)
        .size()
        .x;
    let truncated = full_width > max_text + 0.5;

    let size = Vec2::new(galley.size().x + 2.0 * PAD, HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect(
            rect,
            CornerRadius::same((HEIGHT / 2.0) as u8),
            SURFACE_700,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let pos = rect.center() - galley.size() / 2.0;
        p.galley(pos, galley, text_color);
    }
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
    if truncated {
        response.on_hover_text(label)
    } else {
        response
    }
}
