//! Status lines and overline section labels.

use egui::{Response, RichText, Sense, TextStyle, Ui, Vec2};

use crate::theme::Tone;
use crate::theme::text::{Type, text};
use crate::theme::tokens::space;

/// Diameter of a status dot.
const DOT: f32 = 7.0;

/// A status line: a dot and `label`, both in `tone`'s colour (e.g.
/// "Merged", "Re-merge needed").
pub fn status(ui: &mut Ui, tone: Tone, label: &str) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        let (r, _) = ui.allocate_exact_size(Vec2::splat(DOT), Sense::hover());
        ui.painter()
            .circle_filled(r.center(), DOT / 2.0, tone.color());
        ui.label(
            RichText::new(label)
                .text_style(TextStyle::Small)
                .color(tone.color()),
        );
    })
    .response
}

/// An overline section label: subtle, upper-case, tracked (e.g. "RECENT
/// SESSIONS").
pub fn overline(ui: &mut Ui, label: &str) -> Response {
    ui.label(text(Type::Overline, label))
}
