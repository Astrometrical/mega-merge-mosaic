//! The notes field: free text saved as you type.

use egui::{Response, TextEdit, Ui};

/// A multi-line notes field over the full available width, four rows
/// high, showing `hint` when empty. Read-only when `!enabled`. Saving is
/// the caller's job: save when the response is `changed()`.
pub fn notes_field(ui: &mut Ui, text: &mut String, hint: &str, enabled: bool) -> Response {
    ui.add(
        TextEdit::multiline(text)
            .hint_text(hint)
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .interactive(enabled),
    )
}
