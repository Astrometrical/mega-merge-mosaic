//! Property grids: muted labels beside emphasis values.

use egui::{AsIdSalt, Grid, RichText, Ui, vec2};

use crate::theme::palette::{TEXT_EMPHASIS, TEXT_MUTED};
use crate::theme::text::{Type, text};
use crate::theme::tokens::space;

/// A two-column grid of property rows; add rows with [`property_row`],
/// [`property_row_numeric`] or [`property_row_ui`] inside `add_rows`.
pub fn property_grid<R>(
    ui: &mut Ui,
    id_salt: impl AsIdSalt,
    add_rows: impl FnOnce(&mut Ui) -> R,
) -> R {
    Grid::new(id_salt)
        .num_columns(2)
        .spacing(vec2(space::L, space::S))
        .show(ui, add_rows)
        .inner
}

fn label(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(s).color(TEXT_MUTED));
}

/// A row: `label`, then `value` in the emphasis colour.
pub fn property_row(ui: &mut Ui, label_text: &str, value: &str) {
    label(ui, label_text);
    ui.label(RichText::new(value).color(TEXT_EMPHASIS));
    ui.end_row();
}

/// A row whose value is a number, size or path (mono font).
pub fn property_row_numeric(ui: &mut Ui, label_text: &str, value: &str) {
    label(ui, label_text);
    ui.label(text(Type::Numeric, value));
    ui.end_row();
}

/// A row whose value is drawn by `add_value` (chips, status, links).
pub fn property_row_ui(ui: &mut Ui, label_text: &str, add_value: impl FnOnce(&mut Ui)) {
    label(ui, label_text);
    ui.horizontal(add_value);
    ui.end_row();
}
