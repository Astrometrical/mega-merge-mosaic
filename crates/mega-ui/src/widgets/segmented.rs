//! Segmented control: one joined row of mutually exclusive options.

use egui::{CornerRadius, Frame, Response, Stroke, Ui, Vec2};

use crate::theme::palette::{BORDER, SURFACE_700, TEXT_MUTED, TEXT_STRONG};
use crate::theme::tokens::{CONTROL_HEIGHT, radius};

/// One option of [`segmented_with`].
#[derive(Clone, Copy, Debug)]
pub struct Segment<'a, T> {
    /// The value the option stands for.
    pub value: T,
    /// The option's text (also its accessible label).
    pub label: &'a str,
    /// Why the option cannot be chosen now, shown on hover; `None` when it
    /// can.
    pub disabled: Option<&'a str>,
    /// Shown on hover while the option can be chosen (e.g. naming a
    /// keyboard shortcut).
    pub tooltip: Option<&'a str>,
}

/// Choose one of `options` into `value`. Returns the control's response,
/// marked changed when `value` changes.
pub fn segmented<T: PartialEq + Clone>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> Response {
    let options: Vec<Segment<'_, T>> = options
        .iter()
        .map(|(value, label)| Segment {
            value: value.clone(),
            label,
            disabled: None,
            tooltip: None,
        })
        .collect();
    segmented_with(ui, value, &options)
}

/// [`segmented`] with options that may be disabled: a disabled option
/// cannot be chosen and says why on hover.
pub fn segmented_with<T: PartialEq + Clone>(
    ui: &mut Ui,
    value: &mut T,
    options: &[Segment<'_, T>],
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
                for option in options {
                    let selected = *value == option.value;
                    let b = egui::Button::selectable(selected, option.label)
                        .min_size(Vec2::new(0.0, CONTROL_HEIGHT - 2.0));
                    let mut r = ui.add_enabled(option.disabled.is_none(), b);
                    if let Some(why) = option.disabled {
                        r = r.on_disabled_hover_text(why);
                    } else if let Some(tip) = option.tooltip {
                        r = r.on_hover_text(tip);
                    }
                    if r.clicked() && !selected {
                        *value = option.value.clone();
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
