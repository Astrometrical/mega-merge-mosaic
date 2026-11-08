//! Chips: small pill labels, neutral or tinted.

use egui::{
    Color32, CornerRadius, Response, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2, WidgetInfo,
    WidgetType,
    text::{LayoutJob, TextFormat, TextWrapping},
};

use crate::theme::palette::{ACCENT_500, BORDER, SURFACE_700, TEXT_BODY, TEXT_STRONG};
use crate::theme::tokens::space;
use crate::theme::{Tone, legible_text};

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

/// A selectable chip: what [`choice_chip`] draws.
#[derive(Clone, Copy, Debug)]
pub struct ChoiceChip<'a> {
    /// The chip text (also its accessible label).
    pub label: &'a str,
    /// Colour dot shown before the label (a group or line colour).
    pub color: Option<Color32>,
    /// Whether the choice is currently selected.
    pub selected: bool,
    /// Draw the outline dashed (composites).
    pub dashed: bool,
    /// A state dot after the label (see [`ChipMark`]).
    pub mark: Option<ChipMark>,
}

/// A chip's state dot: what needs the user's attention, drawn in a tone's
/// colour and described to assistive technology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipMark {
    /// Out of date: the warning tone, "out of date".
    Attention,
    /// Changed since it was made: the info tone, "settings changed".
    Changed,
}

impl ChipMark {
    /// The dot's colour.
    pub fn color(self) -> Color32 {
        match self {
            ChipMark::Attention => Tone::Warning.accent(),
            ChipMark::Changed => Tone::Info.accent(),
        }
    }

    /// The state in words (the chip's accessible description).
    pub fn description(self) -> &'static str {
        match self {
            ChipMark::Attention => "out of date",
            ChipMark::Changed => "settings changed",
        }
    }
}

/// Dash and gap lengths of a dashed chip outline, in points.
const DASH: (f32, f32) = (3.0, 2.5);
/// Diameter of the colour dot.
const DOT: f32 = 8.0;
/// Diameter of the state dot.
const MARK_DOT: f32 = 6.0;

/// The outline of a rounded rectangle as a closed point path.
fn outline(rect: egui::Rect, r: f32) -> Vec<egui::Pos2> {
    let mut pts = Vec::new();
    let corners = [
        (rect.right_top() + Vec2::new(-r, r), -90.0_f32),
        (rect.right_bottom() + Vec2::new(-r, -r), 0.0),
        (rect.left_bottom() + Vec2::new(r, -r), 90.0),
        (rect.left_top() + Vec2::new(r, r), 180.0),
    ];
    for (c, start) in corners {
        for i in 0..=6 {
            let a = (start + 90.0 * i as f32 / 6.0).to_radians();
            pts.push(c + Vec2::new(a.cos(), a.sin()) * r);
        }
    }
    pts.push(pts[0]);
    pts
}

/// A chip that toggles: selected shows a raised surface and strong text,
/// unselected looks like [`chip`]. Returns the click response; the caller
/// flips its own state. Accessible as a toggle button with its selected
/// state.
pub fn choice_chip(ui: &mut Ui, chip: &ChoiceChip<'_>) -> Response {
    let font = TextStyle::Small.resolve(ui.style());
    let text_color = if chip.selected {
        TEXT_STRONG
    } else {
        TEXT_BODY
    };
    let galley = ui
        .painter()
        .layout_no_wrap(chip.label.to_owned(), font, text_color);
    let dot = if chip.color.is_some() {
        DOT + space::XS
    } else {
        0.0
    };
    let mark = if chip.mark.is_some() {
        MARK_DOT + space::XS
    } else {
        0.0
    };
    let size = Vec2::new(galley.size().x + 2.0 * PAD + dot + mark, HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if chip.selected { BORDER } else { SURFACE_700 };
        let border = if chip.selected || response.hovered() {
            ACCENT_500
        } else {
            BORDER
        };
        let corner = HEIGHT / 2.0;
        let p = ui.painter();
        if chip.dashed {
            p.rect_filled(rect, CornerRadius::same(corner as u8), fill);
            p.extend(egui::Shape::dashed_line(
                &outline(rect.shrink(0.5), corner - 0.5),
                Stroke::new(1.0, border),
                DASH.0,
                DASH.1,
            ));
        } else {
            p.rect(
                rect,
                CornerRadius::same(corner as u8),
                fill,
                Stroke::new(1.0, border),
                StrokeKind::Inside,
            );
        }
        let mut x = rect.left() + PAD;
        if let Some(c) = chip.color {
            p.circle_filled(egui::pos2(x + DOT / 2.0, rect.center().y), DOT / 2.0, c);
            x += dot;
        }
        let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
        let text_width = galley.size().x;
        p.galley(pos, galley, text_color);
        if let Some(m) = chip.mark {
            let c = egui::pos2(x + text_width + space::XS + MARK_DOT / 2.0, rect.center().y);
            p.circle_filled(c, MARK_DOT / 2.0, m.color());
        }
    }
    super::button::focus_ring(ui, &response, (HEIGHT / 2.0) as u8);
    let (selected, label) = (chip.selected, chip.label);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));
    if let Some(m) = chip.mark {
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_description(m.description());
        });
    }
    response
}
