//! A modal dialog frame: a backdrop, a titled card, and a footer of buttons.
//! The one modal of the app is the Export dialog; modals are only for
//! choosing, never for reporting.
//!
//! Built on [`egui::Modal`], which blocks input behind the dialog and reports
//! Esc and backdrop clicks. On top of that the dialog starts with keyboard
//! focus on the first control of its body, and Tab / Shift+Tab stay inside it.
//! Closed from the keyboard ([`Dialog::close`]), it gives the focus back to
//! the widget that had it when it opened. To assistive technology its root
//! is a modal [`Role::Dialog`] labelled by its title ("Export, dialog").

use egui::accesskit::Role;
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
        self.titled(ui, body).0
    }

    /// [`Dialog::contents`], and the title's id (what names the dialog).
    fn titled<R>(&self, ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> (R, Id) {
        ui.set_width(self.width);
        let title = ui.label(text(Type::Title, self.title)).id;
        ui.add_space(space::L);
        (body(ui), title)
    }

    /// Show the dialog over everything, in the modal layer. Call it every
    /// frame while the dialog is open, and [`Dialog::close`] once when it
    /// closes. Focus starts on the first control of the body when the dialog
    /// opens; the widget that had it is remembered, for `close` to give it
    /// back. The modal's root is a modal [`Role::Dialog`] node labelled by
    /// the title.
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
        if fresh {
            // Whatever opened the dialog (if anything had the focus), for
            // `close` to give the focus back to.
            ctx.add_plugin(ReturnFocus);
            let opener = ctx.memory(|m| m.focused());
            ctx.data_mut(|d| {
                d.remove::<Id>(opener_key(id));
                if let Some(o) = opener {
                    d.insert_temp(opener_key(id), o);
                }
            });
        }
        let mut title = None;
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
                let (inner, t) = self.titled(ui, body);
                title = Some(t);
                if tries > 0 {
                    // Nothing in the body took the focus (the area's hidden
                    // sizing pass has no live controls): no widget drawn
                    // after the dialog in this pass may take it instead.
                    ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                }
                inner
            });
        // The modal's root (its area's `Ui`, whose id is the modal's): the
        // dialog, named by its title.
        if let Some(title) = title {
            ctx.accesskit_node_builder(id, |node| {
                node.set_role(Role::Dialog);
                node.set_modal();
                node.push_labelled_by(title.accesskit_id());
            });
        }
        let landed = ctx.memory(|m| m.focused()).is_some();
        let left = if landed { 0 } else { tries.saturating_sub(1) };
        ctx.data_mut(|d| d.insert_temp(key, (pass, left)));
        DialogResponse {
            close: modal.should_close(),
            inner: modal.inner,
        }
    }

    /// The dialog shown as `id` has closed: call it once, in the pass the
    /// caller stops showing it. Closed from the keyboard (Esc, or a button
    /// pressed with Enter or Space), the focus goes back to the widget that
    /// had it when the dialog opened, if that widget is still there. Closed
    /// by a pointer click (a button, the backdrop), the focus stays where
    /// the click put it.
    ///
    /// The focus is given (by a small egui plugin) at the end of the next
    /// pass, the first the dialog is not shown in: until then the opener is
    /// behind the modal of the pass before, and egui takes the focus from a
    /// widget drawn there. So the key that closed the dialog never reaches
    /// the opener (Enter would press it again), and the focus is back a
    /// pass later.
    pub fn close(ctx: &Context, id: Id) {
        let opener = ctx.data_mut(|d| {
            let o = d.get_temp::<Id>(opener_key(id));
            d.remove::<Id>(opener_key(id));
            o
        });
        let clicked = ctx.input(|i| i.pointer.any_click());
        if let Some(opener) = opener.filter(|_| !clicked) {
            ctx.add_plugin(ReturnFocus);
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| d.insert_temp(return_key(), (opener, pass)));
            ctx.request_repaint();
        }
    }
}

/// Where a dialog remembers the widget that had the focus when it opened.
fn opener_key(id: Id) -> Id {
    id.with("dialog_opener")
}

/// Where [`Dialog::close`] leaves the widget to give the focus back to, and
/// the pass the dialog closed in.
fn return_key() -> Id {
    Id::new("mega_ui::dialog::return_focus")
}

/// Gives a closed dialog's opener the focus back at the end of the pass
/// after the one it closed in ([`Dialog::close`]): that pass shows no
/// modal, so from the next one on the opener may keep the focus. Added by
/// every dialog that opens (egui keeps one), so it runs from the pass after.
struct ReturnFocus;

impl egui::Plugin for ReturnFocus {
    fn debug_name(&self) -> &'static str {
        "mega_ui::dialog::ReturnFocus"
    }

    fn on_end_pass(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx();
        let Some((opener, closed)) = ctx.data(|d| d.get_temp::<(Id, u64)>(return_key())) else {
            return;
        };
        if ctx.cumulative_pass_nr() <= closed {
            // The pass the dialog closed in: it was still shown.
            return;
        }
        ctx.data_mut(|d| d.remove::<(Id, u64)>(return_key()));
        // Still there: drawn in this pass or the one before.
        if ctx.read_response(opener).is_some() {
            ctx.memory_mut(|m| m.request_focus(opener));
        }
    }
}

/// One button of [`footer_with`].
#[derive(Clone, Copy, Debug)]
pub struct FooterButton<'a> {
    /// What the button is for (its colours).
    pub kind: ButtonKind,
    /// Its text, also its accessible label.
    pub label: &'a str,
    /// Why it cannot be pressed now, shown on hover; `None` when it can.
    /// A disabled button takes no keyboard focus, so say why in the body
    /// too.
    pub disabled: Option<&'a str>,
}

/// A right-aligned row of buttons, given left to right (the primary action
/// last). Returns the index of the one clicked this frame. The buttons are
/// laid out left to right, so Tab follows what the eye sees.
pub fn footer(ui: &mut Ui, buttons: &[(ButtonKind, &str)]) -> Option<usize> {
    let buttons: Vec<FooterButton<'_>> = buttons
        .iter()
        .map(|&(kind, label)| FooterButton {
            kind,
            label,
            disabled: None,
        })
        .collect();
    footer_with(ui, &buttons)
}

/// [`footer`] with buttons that may be disabled: a disabled one cannot be
/// pressed and says why on hover.
pub fn footer_with(ui: &mut Ui, buttons: &[FooterButton<'_>]) -> Option<usize> {
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
        for (i, b) in buttons.iter().enumerate() {
            let r = ui.add_enabled(b.disabled.is_none(), MegaButton::new(b.kind, b.label));
            let r = match b.disabled {
                Some(why) => r.on_disabled_hover_text(why),
                None => r,
            };
            if r.clicked() {
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
