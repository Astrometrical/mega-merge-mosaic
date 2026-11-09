//! A label over a canvas: a name, then a row of controls (buttons, a
//! two-way toggle) and a line of text, either on the same row or on a
//! second row under the controls, so the controls never move as the text
//! changes.
//!
//! - **Where.** At the top-left corner of the rect it names (in screen
//!   points), its name exactly where an [`Overlay`](crate::canvas_layers::Overlay)'s
//!   painted label would be ([`LABEL_OFFSET`]), on a translucent panel so it
//!   reads over any image. Draw it after the canvas, so it is on top, and
//!   pass the canvas's viewport as its clip: nothing outside is drawn or
//!   sensed.
//! - **Presses.** The label takes the presses that land on it: the canvas
//!   beneath neither pans nor clicks there. Each frame it reports the
//!   control clicked and the control the primary button is held down on
//!   (pressed there and not released, wherever the pointer has gone since),
//!   so a caller can make a control a momentary switch.
//! - **Accessibility.** Each control has its own accessible name (the
//!   caller's, e.g. "Compare Region 1"); a toggle's options are toggle
//!   buttons whose pressed state is the option selected. The option shown
//!   carries the fill; while it is not the one selected (a momentary switch
//!   is held) it is also underlined, so the switch is seen by more than
//!   colour. The label itself has an accessible name and description (say
//!   what it shows there).

use std::hash::Hash;
use std::sync::Arc;

use egui::{
    Color32, CornerRadius, Galley, Pos2, Rect, Response, Sense, Stroke, StrokeKind, TextStyle, Ui,
    UiBuilder, WidgetInfo, WidgetType, pos2, vec2,
};

use super::button::{ButtonKind, focus_ring, looks};
use crate::canvas_layers::LABEL_OFFSET;
use crate::theme::palette::{BORDER, SURFACE_700, SURFACE_800, TEXT_MUTED, TEXT_STRONG};
use crate::theme::scrim;
use crate::theme::tokens::{radius, space};

/// Height of a label's controls: compact, as a chip.
const HEIGHT: f32 = 22.0;
/// Thickness of the underline of an option shown while not selected.
const UNDERLINE: f32 = 2.0;

/// A control on a [`CanvasLabel`]'s first row.
#[derive(Clone, Copy, Debug)]
pub enum LabelControl<'a> {
    /// A push button.
    Button {
        /// What it says.
        text: &'a str,
        /// Its accessible name (e.g. "Compare Region 1").
        accessible: &'a str,
        /// Shown on hover.
        tooltip: Option<&'a str>,
        /// The label's main action: the accent fill.
        primary: bool,
    },
    /// Two joined buttons choosing one of two options (e.g. A | B).
    Toggle {
        /// What the options say.
        options: [&'a str; 2],
        /// Their accessible names.
        accessible: [&'a str; 2],
        /// The option chosen (0 or 1): its button reads as pressed.
        selected: usize,
        /// The option showing now: it carries the fill, and an underline
        /// while it is not `selected` (a momentary switch is held).
        shown: usize,
        /// Shown on hover over either option.
        tooltip: Option<&'a str>,
    },
}

/// A label drawn over a canvas (see the module docs).
#[derive(Clone, Copy, Debug)]
pub struct CanvasLabel<'a> {
    /// The top-left corner of the rect it names, in screen points.
    pub corner: Pos2,
    /// The canvas's viewport: nothing outside is drawn or sensed.
    pub clip: Rect,
    /// The name, first on the first row.
    pub name: &'a str,
    /// The name's colour (that of the rect it names).
    pub color: Color32,
    /// The controls after the name, in order.
    pub controls: &'a [LabelControl<'a>],
    /// A line of text (none if empty): after the controls, or under them.
    pub text: &'a str,
    /// The text goes on a second row under the controls, so they keep
    /// their place whatever it says.
    pub text_below: bool,
    /// The label's accessible name (its controls are named on their own).
    pub accessible: &'a str,
    /// The label's accessible description: what it shows now (none if
    /// empty).
    pub description: &'a str,
}

/// What a [`CanvasLabel`]'s controls did this frame: controls are given by
/// their index in [`CanvasLabel::controls`] with the option (0 for a
/// button, 0 or 1 for a toggle's).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LabelResponse {
    /// The control clicked (or activated from the keyboard) this frame.
    pub clicked: Option<(usize, usize)>,
    /// The control the primary button is held down on: pressed on it and
    /// not released yet, wherever the pointer has gone since.
    pub held: Option<(usize, usize)>,
}

/// The control a press went down on, kept for the rest of that press:
/// egui lets go of a click-only widget once a press can no longer be a
/// click (held past a click's time, or moved too far), but a momentary
/// switch holds for as long as the button does.
#[derive(Clone, Copy)]
struct Press {
    which: (usize, usize),
    /// When the press began ([`egui::PointerState::press_start_time`]).
    start: f64,
}

/// A control laid out: its option rects and their text.
struct Placed {
    rects: Vec<Rect>,
    galleys: Vec<Arc<Galley>>,
}

/// Draw `label` over the canvas in `ui` (after the canvas, so it is on
/// top) and report what its controls did. `id_salt` tells labels apart
/// (e.g. the id of the rect named).
pub fn canvas_label(
    ui: &mut Ui,
    id_salt: impl Hash + std::fmt::Debug,
    label: &CanvasLabel<'_>,
) -> LabelResponse {
    let id = ui.make_persistent_id(id_salt);
    let clip = label.clip.intersect(ui.clip_rect());
    let mut ui = ui.new_child(UiBuilder::new().id(id).max_rect(clip));
    ui.set_clip_rect(clip);
    let painter = ui.painter().clone();

    // The first row: the name where an overlay's label would be, then the
    // controls centred on its line.
    let name = painter.layout_no_wrap(
        label.name.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        label.color,
    );
    let name_rect = Rect::from_min_size(label.corner + LABEL_OFFSET, name.size());
    let mid = name_rect.center().y;
    let font = TextStyle::Button.resolve(ui.style());
    let mut x = name_rect.right() + space::S;
    let mut bounds = name_rect;
    let mut placed = Vec::new();
    for control in label.controls {
        let texts: &[&str] = match control {
            LabelControl::Button { text, .. } => std::slice::from_ref(text),
            LabelControl::Toggle { options, .. } => options,
        };
        let galleys: Vec<Arc<Galley>> = texts
            .iter()
            .map(|t| painter.layout_no_wrap((*t).to_owned(), font.clone(), Color32::PLACEHOLDER))
            .collect();
        let width = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 2.0 * space::S;
        let rects: Vec<Rect> = (0..galleys.len())
            .map(|k| {
                Rect::from_min_size(
                    pos2(x + k as f32 * width, mid - HEIGHT / 2.0),
                    vec2(width, HEIGHT),
                )
            })
            .collect();
        for r in &rects {
            bounds = bounds.union(*r);
        }
        x = bounds.right() + space::XS;
        placed.push(Placed { rects, galleys });
    }
    // The text: after the controls, or on a row of its own under them.
    let text = (!label.text.is_empty()).then(|| {
        let g = painter.layout_no_wrap(
            label.text.to_owned(),
            TextStyle::Small.resolve(ui.style()),
            TEXT_MUTED,
        );
        let at = if label.text_below {
            pos2(name_rect.left(), bounds.bottom() + space::XS)
        } else {
            let after = if placed.is_empty() {
                name_rect.right()
            } else {
                bounds.right()
            };
            pos2(after + space::S, mid - g.size().y / 2.0)
        };
        (at, g)
    });
    if let Some((at, g)) = &text {
        bounds = bounds.union(Rect::from_min_size(*at, g.size()));
    }

    // The panel, sensing presses so the canvas beneath gets none; the
    // controls sense on top of it.
    let panel = bounds.expand2(LABEL_OFFSET);
    let back = ui.interact(panel, id.with("panel"), Sense::CLICK | Sense::DRAG);
    back.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, label.accessible));
    if !label.description.is_empty() {
        ui.ctx().accesskit_node_builder(back.id, |node| {
            node.set_description(label.description);
        });
    }
    painter.rect(
        panel,
        CornerRadius::same(radius::SMALL),
        scrim(SURFACE_800, TEXT_MUTED),
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    painter.galley(name_rect.min, name, label.color);

    // The control held since an earlier frame of this press, if any.
    let press_id = id.with("press");
    let (down, start) = ui.input(|i| (i.pointer.primary_down(), i.pointer.press_start_time()));
    let mut out = LabelResponse {
        clicked: None,
        held: ui
            .data(|d| d.get_temp::<Press>(press_id))
            .filter(|p| down && start == Some(p.start))
            .map(|p| p.which),
    };
    for (i, (control, p)) in label.controls.iter().zip(placed).enumerate() {
        match *control {
            LabelControl::Button {
                accessible,
                tooltip,
                primary,
                ..
            } => {
                let r = ui.interact(p.rects[0], id.with(("control", i)), Sense::click());
                let held = follow(&r, down, tooltip, (i, 0), &mut out);
                let kind = if primary {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Secondary
                };
                let [idle, hovered, pressed] = looks(kind);
                let look = if held {
                    pressed
                } else if r.hovered() {
                    hovered
                } else {
                    idle
                };
                painter.rect(
                    r.rect,
                    CornerRadius::same(radius::SMALL),
                    look.fill,
                    Stroke::new(1.0, look.stroke),
                    StrokeKind::Inside,
                );
                let g = &p.galleys[0];
                painter.galley(r.rect.center() - g.size() / 2.0, g.clone(), look.text);
                focus_ring(&ui, &r, radius::SMALL);
                r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, accessible));
            }
            LabelControl::Toggle {
                accessible,
                selected,
                shown,
                tooltip,
                ..
            } => {
                let whole = p.rects[0].union(p.rects[1]);
                painter.rect_stroke(
                    whole,
                    CornerRadius::same(radius::SMALL),
                    Stroke::new(1.0, BORDER),
                    StrokeKind::Inside,
                );
                for (k, rect) in p.rects.iter().enumerate() {
                    let r = ui.interact(*rect, id.with(("control", i, k)), Sense::click());
                    follow(&r, down, tooltip, (i, k), &mut out);
                    let fill = if k == shown {
                        BORDER
                    } else if r.hovered() {
                        SURFACE_700
                    } else {
                        Color32::TRANSPARENT
                    };
                    let (s, z) = (radius::SMALL, 0);
                    let corners = if k == 0 {
                        CornerRadius {
                            nw: s,
                            sw: s,
                            ne: z,
                            se: z,
                        }
                    } else {
                        CornerRadius {
                            nw: z,
                            sw: z,
                            ne: s,
                            se: s,
                        }
                    };
                    painter.rect_filled(rect.shrink(1.0), corners, fill);
                    let ink = if k == shown { TEXT_STRONG } else { TEXT_MUTED };
                    let g = &p.galleys[k];
                    painter.galley(rect.center() - g.size() / 2.0, g.clone(), ink);
                    if k == shown && shown != selected {
                        let line = Rect::from_min_max(
                            pos2(rect.left() + space::XS, rect.bottom() - 1.0 - UNDERLINE),
                            pos2(rect.right() - space::XS, rect.bottom() - 1.0),
                        );
                        painter.rect_filled(line, 0.0, TEXT_STRONG);
                    }
                    focus_ring(&ui, &r, radius::SMALL);
                    let name = accessible[k];
                    r.widget_info(|| {
                        WidgetInfo::selected(WidgetType::Button, true, k == selected, name)
                    });
                }
            }
        }
    }
    if let Some((at, g)) = text {
        painter.galley(at, g, TEXT_MUTED);
    }
    ui.data_mut(|d| match (out.held, start) {
        (Some(which), Some(start)) => {
            d.insert_temp(press_id, Press { which, start });
        }
        _ => d.remove::<Press>(press_id),
    });
    out
}

/// Note in `out` what control `which` (its response `r`) did this frame
/// (`down`: the primary button is down), and show its tooltip. Whether it
/// is held.
fn follow(
    r: &Response,
    down: bool,
    tooltip: Option<&str>,
    which: (usize, usize),
    out: &mut LabelResponse,
) -> bool {
    if r.clicked() {
        out.clicked = Some(which);
    }
    if down && r.is_pointer_button_down_on() {
        out.held = Some(which);
    }
    if let Some(t) = tooltip {
        r.clone().on_hover_text(t);
    }
    out.held == Some(which)
}

#[cfg(test)]
#[path = "canvas_label_tests.rs"]
mod tests;
