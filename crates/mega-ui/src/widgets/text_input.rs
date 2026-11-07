//! Single-line text input, a control high.

use egui::{Margin, Response, TextEdit, Ui, Vec2};

use crate::theme::tokens::{CONTROL_HEIGHT, space};

/// A single-line text input over the available width (at most 360
/// points), [`CONTROL_HEIGHT`] high, showing `hint` when empty. Its fill
/// and focus ring come from the theme.
pub fn text_input(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let width = ui.available_width().min(360.0);
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint)
            .margin(Margin::symmetric(space::S as i8 + 2, space::XS as i8 + 2))
            .min_size(Vec2::new(width, CONTROL_HEIGHT))
            .desired_width(width),
    )
}
