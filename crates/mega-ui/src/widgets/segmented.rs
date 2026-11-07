//! Segmented control: one joined row of mutually exclusive options.

use egui::{CornerRadius, Frame, Response, Stroke, Ui, Vec2};

use crate::theme::palette::{BORDER, SURFACE_700, TEXT_MUTED, TEXT_STRONG};
use crate::theme::tokens::{CONTROL_HEIGHT, radius};

/// Choose one of `options` into `value`. Returns the control's response,
/// marked changed when `value` changes.
pub fn segmented<T: PartialEq + Clone>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> Response {
    let mut changed = false;
    let mut inner = Frame::new()
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(radius::CONTROL))
        .show(ui, |ui| {
            let v = &mut ui.style_mut().visuals;
            v.selection.bg_fill = BORDER;
            v.selection.stroke = Stroke::new(1.0, TEXT_STRONG);
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.bg_stroke = Stroke::NONE;
                w.corner_radius = CornerRadius::same(radius::CONTROL);
                w.expansion = 0.0;
            }
            v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
            v.widgets.hovered.weak_bg_fill = SURFACE_700;
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| {
                for (option, label) in options {
                    let selected = value == option;
                    let b = egui::Button::selectable(selected, *label)
                        .min_size(Vec2::new(0.0, CONTROL_HEIGHT - 2.0));
                    if ui.add(b).clicked() && !selected {
                        *value = option.clone();
                        changed = true;
                    }
                }
            });
        })
        .response;
    if changed {
        inner.mark_changed();
    }
    inner
}
