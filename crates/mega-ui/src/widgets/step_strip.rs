//! Step strip: a row of pill-shaped steps showing status and progress and
//! acting as navigation (Panels · Checks · Merge · Export).

use egui::{
    Align, Color32, CornerRadius, Id, Layout, Rect, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2,
    WidgetInfo, WidgetType, pos2,
};

use crate::icons;
use crate::theme::Tone;
use crate::theme::palette::{ACCENT_500, ACCENT_700, BORDER, TEXT_MUTED, TEXT_STRONG};
use crate::theme::tokens::{CONTROL_HEIGHT, space};

use super::button::focus_ring;

/// Where a step stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StepState {
    /// Not reached yet.
    Todo,
    /// The step to do next.
    Next,
    /// Finished.
    Done,
    /// Finished or stale with something worth attention.
    Warn,
    /// Running; `fraction` is progress in `0..=1`, or `None` when unknown
    /// (drawn as a static half-strength fill).
    Working {
        /// Progress in `0..=1`, if known.
        fraction: Option<f32>,
    },
}

/// One step of the strip.
#[derive(Clone, Debug, PartialEq)]
pub struct StripStep {
    /// Step name; also its accessible label.
    pub label: String,
    /// Where the step stands.
    pub state: StepState,
    /// Whether a working step can be cancelled (shows a cancel button while
    /// hovered or focused).
    pub cancellable: bool,
    /// Shown on hover: why a click does nothing now, say.
    pub tooltip: Option<String>,
}

/// What the user did in the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripEvent {
    /// The step at this index was clicked.
    Open(usize),
    /// The cancel button of the step at this index was clicked.
    Cancel(usize),
}

/// Pill height, a little under [`CONTROL_HEIGHT`].
const HEIGHT: f32 = CONTROL_HEIGHT - 4.0;
/// Horizontal padding inside a pill.
const PAD: f32 = space::M;
/// Fill of a working pill's progress part (opaque; text stays AA on it).
pub(crate) const WORKING_FILL: Color32 = ACCENT_700;
/// Size of the cancel icon button.
const CANCEL: f32 = HEIGHT - 6.0;

/// Draw the strip of `steps` left to right and report a click on a step
/// ([`StripEvent::Open`]) or on a working step's cancel button
/// ([`StripEvent::Cancel`]). In a row taller than a pill (beside buttons)
/// the pills are centred on it. When they do not fit the width available
/// ([`step_strip_width`]) they wrap onto as many rows as they need.
pub fn step_strip(ui: &mut Ui, steps: &[StripStep]) -> Option<StripEvent> {
    let mut event = None;
    // A row as high as a pill: `horizontal`'s is the (lower) interact
    // height, which a centring parent centres, so the pills hung below.
    let row = Vec2::new(ui.available_size_before_wrap().x, HEIGHT);
    let wrap = step_strip_width(ui, steps) > row.x;
    let layout = Layout::left_to_right(Align::Center).with_main_wrap(wrap);
    ui.allocate_ui_with_layout(row, layout, |ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::XS);
        for (i, step) in steps.iter().enumerate() {
            if let Some(e) = pill(ui, i, step) {
                event = Some(e);
            }
        }
    });
    event
}

/// The width [`step_strip`] takes to show `steps` on one row in `ui`.
pub fn step_strip_width(ui: &Ui, steps: &[StripStep]) -> f32 {
    let gaps = steps.len().saturating_sub(1) as f32 * space::XS;
    steps.iter().map(|s| Pill::of(ui, s).width).sum::<f32>() + gaps
}

/// A pill's look and size, before it is drawn.
struct Pill {
    text_color: Color32,
    outline: Color32,
    galley: std::sync::Arc<egui::Galley>,
    icon: Option<std::sync::Arc<egui::Galley>>,
    cancellable: bool,
    width: f32,
}

impl Pill {
    fn of(ui: &Ui, step: &StripStep) -> Self {
        let (text_color, outline, icon): (Color32, Color32, Option<(char, Color32)>) =
            match step.state {
                StepState::Todo => (TEXT_MUTED, BORDER, None),
                StepState::Next => (TEXT_STRONG, ACCENT_500, None),
                StepState::Done => (
                    Tone::Success.color(),
                    Tone::Success.accent(),
                    Some((icons::CHECK, Tone::Success.color())),
                ),
                StepState::Warn => (
                    Tone::Warning.color(),
                    Tone::Warning.accent(),
                    Some((icons::TRIANGLE_ALERT, Tone::Warning.color())),
                ),
                StepState::Working { .. } => (TEXT_STRONG, Tone::Info.accent(), None),
            };
        let cancellable = step.cancellable && matches!(step.state, StepState::Working { .. });
        let font = TextStyle::Button.resolve(ui.style());
        let galley = ui
            .painter()
            .layout_no_wrap(step.label.clone(), font.clone(), text_color);
        let icon = icon.map(|(g, c)| {
            ui.painter()
                .layout_no_wrap(g.to_string(), icons::font(font.size + 1.0), c)
        });
        let icon_w = icon.as_ref().map_or(0.0, |g| g.size().x + space::XS);
        // Reserved even while the button is hidden, so the pill never resizes.
        let cancel_w = if cancellable { CANCEL + space::XS } else { 0.0 };
        let width = galley.size().x + icon_w + cancel_w + 2.0 * PAD;
        Pill {
            text_color,
            outline,
            galley,
            icon,
            cancellable,
            width,
        }
    }
}

fn pill(ui: &mut Ui, index: usize, step: &StripStep) -> Option<StripEvent> {
    let Pill {
        text_color,
        outline,
        galley,
        icon: icon_galley,
        cancellable,
        width,
    } = Pill::of(ui, step);
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click());
    let corner = (HEIGHT / 2.0) as u8;
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if let StepState::Working { fraction } = step.state {
            let (w, fill) = match fraction {
                Some(f) => (rect.width() * f.clamp(0.0, 1.0), WORKING_FILL),
                None => (rect.width(), WORKING_FILL.gamma_multiply(0.5)),
            };
            if w > 0.0 {
                let fill_rect = Rect::from_min_size(rect.min, Vec2::new(w, HEIGHT));
                p.rect_filled(fill_rect, CornerRadius::same(corner), fill);
            }
        }
        let border = if response.hovered() {
            ACCENT_500
        } else {
            outline
        };
        p.rect_stroke(
            rect,
            CornerRadius::same(corner),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let mut x = rect.left() + PAD;
        if let Some(g) = icon_galley {
            let pos = pos2(x, rect.center().y - g.size().y / 2.0);
            x += g.size().x + space::XS;
            p.galley(pos, g, text_color);
        }
        p.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
    }
    focus_ring(ui, &response, corner);
    let label = step.label.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &label));
    if let Some(tip) = &step.tooltip {
        response = response.on_hover_text(tip);
    }

    let mut event = response.clicked().then_some(StripEvent::Open(index));
    if cancellable {
        let cancel_id = response.id.with("cancel");
        let focused = response.has_focus() || ui.memory(|m| m.has_focus(cancel_id));
        if (ui.rect_contains_pointer(rect) || focused)
            && let Some(e) = cancel_button(ui, cancel_id, rect, index, &step.label)
        {
            event = Some(e);
        }
    }
    event
}

/// The cancel icon button at the right end of a working pill.
fn cancel_button(ui: &mut Ui, id: Id, pill: Rect, index: usize, label: &str) -> Option<StripEvent> {
    let rect = Rect::from_center_size(
        pos2(pill.right() - PAD / 2.0 - CANCEL / 2.0, pill.center().y),
        Vec2::splat(CANCEL),
    );
    let response = ui.interact(rect, id, Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if response.hovered() {
            TEXT_STRONG
        } else {
            TEXT_MUTED
        };
        let galley = ui
            .painter()
            .layout_no_wrap(icons::X.to_string(), icons::font(12.0), color);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    focus_ring(ui, &response, (CANCEL / 2.0) as u8);
    let name = format!("Cancel {label}");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &name));
    response
        .on_hover_text(name)
        .clicked()
        .then_some(StripEvent::Cancel(index))
}

#[cfg(test)]
#[path = "step_strip_tests.rs"]
mod tests;
