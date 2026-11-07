//! Toasts: the only way the apps notify. Non-modal, bottom-right, at most
//! [`MAX_TOASTS`] at once, each gone after [`TOAST_LIFETIME_S`] (paused
//! while hovered) or when its action is clicked.

use std::time::Duration;

use egui::{
    Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Margin, Order, Pos2, RichText,
    Sense, Shadow, Stroke, TextStyle, Vec2,
};

use crate::theme::Tone;
use crate::theme::palette::{BORDER, SURFACE_700, TEXT_EMPHASIS, TEXT_MUTED};
use crate::theme::tokens::{radius, space};
use crate::widgets::{ButtonKind, MegaButton};

/// Seconds a toast stays up (not counting time hovered).
pub const TOAST_LIFETIME_S: f64 = 6.0;
/// Most toasts shown at once; pushing more dismisses the oldest.
pub const MAX_TOASTS: usize = 3;
/// Toast width, in points.
const WIDTH: f32 = 340.0;
/// Icon size, in points.
const ICON: f32 = 16.0;

/// Identifies a pushed toast in [`ToastEvent`]s.
pub type ToastId = u64;

/// A notification to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    /// Its tone (decides the icon and colour).
    pub tone: Tone,
    /// One-line message, e.g. "Export finished".
    pub title: String,
    /// Optional second line, e.g. where the files went.
    pub detail: Option<String>,
    /// Optional single action, e.g. "Show".
    pub action: Option<String>,
}

/// What happened to a toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastEvent {
    /// Its action was clicked (it is also dismissed).
    Action(ToastId),
    /// It expired or was pushed out by newer toasts.
    Dismissed(ToastId),
}

#[derive(Debug)]
struct Live {
    id: ToastId,
    toast: Toast,
    /// Seconds shown so far, not counting hovered time.
    age: f64,
    /// When `age` was last advanced.
    last_tick: f64,
    hovered: bool,
}

/// The app's toasts. Push with [`Toasts::push`]; call [`Toasts::show`] once
/// per frame.
#[derive(Debug, Default)]
pub struct Toasts {
    live: Vec<Live>,
    next_id: ToastId,
    pending: Vec<ToastEvent>,
}

impl Toasts {
    /// Show `toast` from `now` (seconds, e.g. `ctx.input(|i| i.time)`).
    pub fn push(&mut self, toast: Toast, now: f64) -> ToastId {
        let id = self.next_id;
        self.next_id += 1;
        self.live.push(Live {
            id,
            toast,
            age: 0.0,
            last_tick: now,
            hovered: false,
        });
        while self.live.len() > MAX_TOASTS {
            let gone = self.live.remove(0);
            self.pending.push(ToastEvent::Dismissed(gone.id));
        }
        id
    }

    /// Draw the toasts at time `now` and return what happened since the
    /// last call.
    pub fn show(&mut self, ctx: &Context, now: f64) -> Vec<ToastEvent> {
        let mut events = std::mem::take(&mut self.pending);
        for t in &mut self.live {
            if !t.hovered {
                t.age += (now - t.last_tick).max(0.0);
            }
            t.last_tick = now;
        }
        self.live.retain(|t| {
            let keep = t.age < TOAST_LIFETIME_S;
            if !keep {
                events.push(ToastEvent::Dismissed(t.id));
            }
            keep
        });
        if self.live.is_empty() {
            return events;
        }

        let mut clicked = None;
        Area::new(Id::new("mega-ui-toasts"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-space::L, -space::L))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = space::S;
                for t in &mut self.live {
                    let r = Frame::new()
                        .fill(SURFACE_700)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(CornerRadius::same(radius::CARD))
                        .inner_margin(Margin::symmetric(space::M as i8, space::S as i8 + 2))
                        .shadow(Shadow {
                            offset: [0, 8],
                            blur: 24,
                            spread: 0,
                            color: Color32::from_black_alpha(128),
                        })
                        .show(ui, |ui| {
                            ui.set_width(WIDTH - 2.0 * space::M);
                            ui.horizontal(|ui| {
                                let (icon, _) =
                                    ui.allocate_exact_size(Vec2::splat(ICON), Sense::hover());
                                if let Some(g) = t.toast.tone.icon() {
                                    ui.painter().text(
                                        Pos2::new(icon.min.x, icon.min.y),
                                        Align2::LEFT_TOP,
                                        g,
                                        FontId::proportional(ICON),
                                        t.toast.tone.color(),
                                    );
                                }
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.label(RichText::new(&t.toast.title).color(TEXT_EMPHASIS));
                                    if let Some(d) = &t.toast.detail {
                                        ui.label(
                                            RichText::new(d)
                                                .text_style(TextStyle::Small)
                                                .color(TEXT_MUTED),
                                        );
                                    }
                                });
                                if let Some(a) = &t.toast.action {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add(MegaButton::new(ButtonKind::Quiet, a.clone()))
                                                .clicked()
                                            {
                                                clicked = Some(t.id);
                                            }
                                        },
                                    );
                                }
                            });
                        });
                    t.hovered = r.response.contains_pointer();
                }
            });
        if let Some(id) = clicked {
            self.live.retain(|t| t.id != id);
            events.push(ToastEvent::Action(id));
            ctx.request_repaint();
        }
        if let Some(next) = self
            .live
            .iter()
            .filter(|t| !t.hovered)
            .map(|t| TOAST_LIFETIME_S - t.age)
            .reduce(f64::min)
        {
            ctx.request_repaint_after(Duration::from_secs_f64(next.max(0.0)));
        }
        events
    }
}

#[cfg(test)]
#[path = "toast_tests.rs"]
mod tests;
