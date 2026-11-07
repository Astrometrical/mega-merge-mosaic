//! Toasts: the only way the apps notify. Non-modal, bottom-right, at most
//! [`MAX_TOASTS`] at once, each gone after [`TOAST_LIFETIME_S`] (paused
//! while hovered) or when its action is clicked.

use std::time::Duration;

use egui::{
    Align2, Area, Color32, Context, CornerRadius, Frame, Id, Margin, Order, Pos2, Sense, Shadow,
    Stroke, TextStyle, Vec2,
    text::{LayoutJob, TextFormat, TextWrapping},
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
                                        crate::icons::font(ICON),
                                        t.toast.tone.color(),
                                    );
                                }
                                // Room for the text: the row minus the icon,
                                // the gaps and the action button.
                                let gap = ui.spacing().item_spacing.x;
                                let action_w = t.toast.action.as_ref().map_or(0.0, |a| {
                                    let font = TextStyle::Button.resolve(ui.style());
                                    let w = ui
                                        .painter()
                                        .layout_no_wrap(a.clone(), font, TEXT_MUTED)
                                        .size()
                                        .x;
                                    w + 2.0 * ui.spacing().button_padding.x + gap
                                });
                                let text_w = (ui.available_width() - action_w).max(80.0);
                                ui.vertical(|ui| {
                                    ui.set_width(text_w);
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    clamped(
                                        ui,
                                        &t.toast.title,
                                        TextStyle::Body,
                                        TEXT_EMPHASIS,
                                        text_w,
                                    );
                                    if let Some(d) = &t.toast.detail {
                                        clamped(ui, d, TextStyle::Small, TEXT_MUTED, text_w);
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

/// Most rows a toast's title or detail takes before it is cut with "…".
const MAX_ROWS: usize = 2;

/// `text` in `style` and `color`, wrapped to `width` and cut to
/// [`MAX_ROWS`] with "…"; the full text is on hover and is the accessible
/// label.
fn clamped(ui: &mut egui::Ui, text: &str, style: TextStyle, color: Color32, width: f32) {
    let font = style.resolve(ui.style());
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: MAX_ROWS,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let cut = galley.elided;
    let response = ui.add(egui::Label::new(galley));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    if cut {
        response.on_hover_text(text);
    }
}

#[cfg(test)]
#[path = "toast_tests.rs"]
mod tests;
