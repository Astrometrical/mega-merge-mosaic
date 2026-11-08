//! Inspector: a foldable column of sections. Headings always show in a
//! fixed order; each section's body folds; the whole column folds to a thin
//! strip. The app places it as a right-hand column and persists
//! [`InspectorState`] itself.

use std::collections::BTreeSet;

use egui::{
    Align, CornerRadius, Label, Layout, Rect, Response, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, pos2,
};

use crate::icons;
use crate::theme::Tone;
use crate::theme::palette::{ACCENT_500, BORDER};
use crate::theme::text::{Type, text};
use crate::theme::tokens::{CONTROL_HEIGHT, INSPECTOR_WIDTH, radius, space};
use crate::widgets::{ButtonKind, focus_ring, icon_button_of};

/// Fold and open state of an inspector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InspectorState {
    /// Whether the whole column is folded to a thin strip.
    pub folded: bool,
    /// Ids of the sections whose bodies are open.
    pub open: BTreeSet<String>,
    /// Section to scroll into view on the next draw.
    pending: Option<String>,
}

impl InspectorState {
    /// Whether section `id`'s body is open.
    pub fn is_open(&self, id: &str) -> bool {
        self.open.contains(id)
    }

    /// Open section `id` if closed, close it if open.
    pub fn toggle(&mut self, id: &str) {
        if !self.open.remove(id) {
            self.open.insert(id.to_owned());
        }
    }

    /// Unfold the column, open section `id` and scroll its heading into view
    /// on the next draw.
    pub fn reveal(&mut self, id: &str) {
        self.folded = false;
        self.open.insert(id.to_owned());
        self.pending = Some(id.to_owned());
    }
}

/// A short tone-coloured note at the right of a section heading.
#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    /// The badge text.
    pub text: String,
    /// Its tone; the text takes the tone's colour.
    pub tone: Tone,
}

/// One section of the inspector.
#[derive(Clone, Debug, PartialEq)]
pub struct Section<'a> {
    /// Stable id, passed to the body callback and used by
    /// [`InspectorState`].
    pub id: &'a str,
    /// Heading text (drawn as an overline).
    pub title: &'a str,
    /// Optional badge at the right of the heading.
    pub badge: Option<Badge>,
}

/// Width of the folded strip: the show button plus its margins.
const STRIP_WIDTH: f32 = CONTROL_HEIGHT + 2.0 * space::S;
/// Heading height.
const HEADING_HEIGHT: f32 = CONTROL_HEIGHT;

/// Width the caller should give the inspector column: [`INSPECTOR_WIDTH`],
/// or the folded strip's width.
pub fn inspector_width(state: &InspectorState) -> f32 {
    if state.folded {
        STRIP_WIDTH
    } else {
        INSPECTOR_WIDTH
    }
}

/// Draw the inspector into `ui`, which the caller sized to
/// [`inspector_width`]. `body` draws the open section with the given id.
pub fn inspector(
    ui: &mut Ui,
    state: &mut InspectorState,
    sections: &[Section<'_>],
    mut body: impl FnMut(&mut Ui, &str),
) {
    if state.folded {
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.add_space(space::S);
            let show = icon_button_of(ButtonKind::Quiet, icons::CHEVRON_LEFT, "Show inspector");
            if ui.add(show).clicked() {
                state.folded = false;
            }
        });
        return;
    }
    ui.with_layout(Layout::top_down(Align::Max), |ui| {
        let hide = icon_button_of(ButtonKind::Quiet, icons::CHEVRON_RIGHT, "Hide inspector");
        if ui.add(hide).clicked() {
            state.folded = true;
        }
    });
    let pending = state.pending.take();
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for section in sections {
                let open = state.is_open(section.id);
                let response = heading(ui, section, open);
                if pending.as_deref() == Some(section.id) {
                    response.scroll_to_me(Some(Align::TOP));
                }
                if response.clicked() {
                    state.toggle(section.id);
                }
                if open {
                    ui.add_space(space::XS);
                    ui.scope(|ui| {
                        ui.set_width(ui.available_width());
                        body(ui, section.id);
                    });
                    ui.add_space(space::S);
                }
            }
        });
}

/// A full-width heading button: title at the left, badge and chevron at the
/// right.
fn heading(ui: &mut Ui, section: &Section<'_>, open: bool) -> Response {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, HEADING_HEIGHT), Sense::hover());
    let id = ui.id().with(("inspector-heading", section.id));
    let response = ui.interact(rect, id, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_stroke(
                rect,
                CornerRadius::same(radius::CONTROL),
                Stroke::new(1.0, ACCENT_500),
                StrokeKind::Inside,
            );
        } else {
            painter.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, BORDER));
        }
    }
    focus_ring(ui, &response, radius::CONTROL);
    let title = section.title.to_owned();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &title));

    let inner = rect.shrink2(Vec2::new(space::S, 0.0));
    // Right side first (chevron, then the badge to its left), so the title
    // gets what is left.
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let glyph = if open {
        icons::CHEVRON_DOWN
    } else {
        icons::CHEVRON_RIGHT
    };
    right.add(Label::new(icons::rich(glyph, 14.0).color(Type::Overline.color())).selectable(false));
    if let Some(badge) = &section.badge {
        right.add_space(space::XS);
        let badge_text = RichText::new(&badge.text)
            .text_style(Type::Small.text_style())
            .color(badge.tone.color());
        right.add(Label::new(badge_text).selectable(false));
    }
    let left_rect = Rect::from_min_max(inner.min, pos2(right.min_rect().left(), inner.max.y));
    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.add(Label::new(text(Type::Overline, section.title)).selectable(false));
    response
}

#[cfg(test)]
#[path = "inspector_tests.rs"]
mod tests;
