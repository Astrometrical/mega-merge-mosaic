//! Progress: a thin accent bar with a label and a readout.

use egui::{
    Align, CornerRadius, Layout, Rect, Response, RichText, Sense, Ui, Vec2, WidgetInfo, WidgetType,
};

use crate::theme::palette::{ACCENT_500, SURFACE_700, TEXT_EMPHASIS, TEXT_MUTED};
use crate::theme::text::{Type, text};
use crate::theme::tokens::space;

/// Bar thickness, in points.
const BAR_HEIGHT: f32 = 2.0;
/// Seconds for the indeterminate segment to sweep across once.
const SWEEP_S: f64 = 1.5;

/// A thin progress bar over the available width: `Some(fraction)` (0 to 1)
/// fills that much; `None` is indeterminate, a sweeping segment that keeps
/// requesting frames while visible.
pub fn progress_bar(ui: &mut Ui, fraction: Option<f32>) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, BAR_HEIGHT), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let round = CornerRadius::same(1);
        p.rect_filled(rect, round, SURFACE_700);
        let (x0, x1) = match fraction {
            Some(f) => (0.0, width * f.clamp(0.0, 1.0)),
            None => {
                let seg = width / 4.0;
                let t = (ui.input(|i| i.time) / SWEEP_S).fract() as f32;
                let x = t * (width + seg) - seg;
                ui.ctx().request_repaint();
                (x.max(0.0), (x + seg).min(width))
            }
        };
        if x1 > x0 {
            let fill = Rect::from_min_max(
                rect.min + Vec2::new(x0, 0.0),
                rect.min + Vec2::new(x1, BAR_HEIGHT),
            );
            p.rect_filled(fill, round, ACCENT_500);
        }
    }
    response.widget_info(|| {
        let mut info = WidgetInfo::new(WidgetType::ProgressIndicator);
        info.value = fraction.map(f64::from);
        info
    });
    response
}

/// A progress row: `label` on the left, `readout` (e.g. "62% · 0:41
/// left") on the right in the numeric style, and the bar below.
pub fn progress(ui: &mut Ui, label: &str, fraction: Option<f32>, readout: &str) -> Response {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::XS;
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(TEXT_EMPHASIS));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(text(Type::Numeric, readout).color(TEXT_MUTED));
            });
        });
        progress_bar(ui, fraction);
    })
    .response
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn progress_shows_label_and_readout() {
        let mut h = Harness::new_ui(|ui| {
            if crate::testutil::themed(ui) {
                progress(ui, "Merging OIII", Some(0.62), "62% · 0:41 left");
            }
        });
        h.run();
        h.get_by_label("Merging OIII");
        h.get_by_label("62% · 0:41 left");
    }

    #[test]
    fn indeterminate_bar_keeps_animating() {
        let mut h = Harness::new_ui(|ui| {
            if crate::testutil::themed(ui) {
                progress_bar(ui, None);
            }
        });
        h.run_steps(2);
        assert!(
            h.ctx.has_requested_repaint(),
            "an indeterminate bar asks for the next frame"
        );
    }
}
