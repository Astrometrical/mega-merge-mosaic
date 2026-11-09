//! A modal dialog frame: a backdrop, a titled card, and a footer of buttons.
//! The one modal of the app is the Export dialog; modals are only for
//! choosing, never for reporting.
//!
//! Built on [`egui::Modal`], which blocks input behind the dialog and reports
//! Esc and backdrop clicks. On top of that the dialog starts with keyboard
//! focus on the first control of its body, and Tab / Shift+Tab stay inside it.

use egui::{Color32, Context, CornerRadius, Frame, Id, Layout, Margin, Modal, Stroke, Ui, Vec2};

use crate::theme::palette::*;
use crate::theme::text::{Type, text};
use crate::theme::tokens::{CONTROL_HEIGHT, DIALOG_BACKDROP_ALPHA, radius, space};
use crate::widgets::button::{ButtonKind, MegaButton};

/// A modal dialog: `title` over a body, `width` points wide (use
/// [`crate::theme::tokens::DIALOG_WIDTH`]).
pub struct Dialog<'a> {
    /// The title, in the title type.
    pub title: &'a str,
    /// Width of the body in points.
    pub width: f32,
}

/// What a shown [`Dialog`] reports.
pub struct DialogResponse<R> {
    /// What the body closure returned.
    pub inner: R,
    /// Esc was pressed or the backdrop was clicked. The caller decides
    /// whether to close, and normally should. Esc also reports close when
    /// pressed in a text field (egui clears the field's focus first): a
    /// caller that keeps the dialog open must accept that focus is then
    /// nowhere until the user Tabs to the first control.
    pub close: bool,
}

/// Passes after opening in which the dialog asks for initial focus.
const FOCUS_PASSES: u8 = 4;

/// The card frame of a dialog: raised surface, strong border, card radius.
pub fn dialog_frame() -> Frame {
    Frame::new()
        .fill(SURFACE_700)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(CornerRadius::same(radius::CARD))
        .inner_margin(Margin::same(space::XL as i8))
}

impl Dialog<'_> {
    #[doc(hidden)]
    /// For inline previews (the gallery); real dialogs use [`Dialog::show`].
    /// Draw the title and `body` in the dialog's card, without a modal or
    /// backdrop (the gallery shows this inline).
    pub fn contents<R>(&self, ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> R {
        ui.set_width(self.width);
        ui.label(text(Type::Title, self.title));
        ui.add_space(space::L);
        body(ui)
    }

    /// Show the dialog over everything, in the modal layer. Call it every
    /// frame while the dialog is open. Focus starts on the first control of
    /// the body when the dialog opens.
    pub fn show<R>(
        self,
        ctx: &Context,
        id: Id,
        body: impl FnOnce(&mut Ui) -> R,
    ) -> DialogResponse<R> {
        // Initial focus. A newly opened dialog (not shown on the previous
        // pass) asks for the first control's focus for a few passes, until
        // it lands once: egui's first area pass is a hidden sizing pass.
        // Never after that, so a click on blank space is left alone and a
        // body with no control does not repaint forever.
        let pass = ctx.cumulative_pass_nr();
        let key = id.with("dialog_state");
        let (last, tries): (u64, u8) = ctx.data(|d| d.get_temp(key)).unwrap_or((0, 0));
        let fresh = last == 0 || last + 1 < pass;
        let tries = if fresh { FOCUS_PASSES } else { tries };
        let modal = Modal::new(id)
            .backdrop_color(Color32::from_black_alpha(DIALOG_BACKDROP_ALPHA))
            .frame(dialog_frame())
            .show(ctx, |ui| {
                if tries > 0 {
                    ui.memory_mut(|m| {
                        if fresh && let Some(f) = m.focused() {
                            // Focus left on whatever opened the dialog.
                            m.surrender_focus(f);
                        }
                        m.move_focus(egui::FocusDirection::Next);
                    });
                    ui.ctx().request_repaint();
                }
                self.contents(ui, body)
            });
        let landed = ctx.memory(|m| m.focused()).is_some();
        let left = if landed { 0 } else { tries.saturating_sub(1) };
        ctx.data_mut(|d| d.insert_temp(key, (pass, left)));
        DialogResponse {
            close: modal.should_close(),
            inner: modal.inner,
        }
    }
}

/// A right-aligned row of buttons, given left to right (the primary action
/// last). Returns the index of the one clicked this frame. The buttons are
/// laid out left to right, so Tab follows what the eye sees.
pub fn footer(ui: &mut Ui, buttons: &[(ButtonKind, &str)]) -> Option<usize> {
    let mut clicked = None;
    ui.add_space(space::L);
    // The row's width from the previous frame decides the leading space
    // (the first frame repaints once to settle).
    let key = ui.id().with("footer_width");
    let known: Option<f32> = ui.data(|d| d.get_temp(key));
    let avail = ui.available_width();
    let width = known.unwrap_or(avail);
    let size = Vec2::new(avail, CONTROL_HEIGHT);
    let out = ui.allocate_ui_with_layout(size, Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::S);
        ui.add_space((avail - width).max(0.0));
        let start = ui.cursor().min.x;
        for (i, (kind, label)) in buttons.iter().enumerate() {
            if ui.add(MegaButton::new(*kind, *label)).clicked() {
                clicked = Some(i);
            }
        }
        ui.cursor().min.x - start - ui.spacing().item_spacing.x
    });
    let measured = out.inner;
    if known.is_none_or(|k| (k - measured).abs() > 0.5) {
        ui.data_mut(|d| d.insert_temp(key, measured));
        ui.ctx().request_repaint();
    }
    clicked
}
