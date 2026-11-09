//! Ordered list: rows the user can pick, go to, rename, reorder and delete,
//! such as the regions of interest (and, later, the composites). The list
//! only reports what the user did as [`ListEvent`]s; the caller changes its
//! data and passes the new rows next frame.
//!
//! - A click selects **and** activates (the caller treats activate as
//!   "go to"); Enter on a focused row does the same.
//! - A double-click, F2 on a focused row, or the **Rename** action edits the
//!   name in place. Enter or leaving the field commits, Esc cancels, and a
//!   name that is empty once trimmed is refused on Enter (the field stays
//!   open) and cancels the rename when focus leaves the field.
//! - Drag a row to reorder it; a line shows where it would land. Every row
//!   also has **Move up** and **Move down** actions for keyboards (disabled
//!   at the ends), and **Delete**.
//! - The row actions show while the row is hovered or has focus (or one of
//!   them does), never otherwise, and are never drawn while dragging.
//! - The selected row can show caller-drawn *details* directly under it
//!   (see [`ordered_list_with`]).

use std::hash::Hash;

use egui::accesskit::Role;
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat, TextWrapping};
use egui::text_edit::TextEditState;
use egui::{
    Align, Color32, CornerRadius, DragAndDrop, Frame, Id, Key, LayerId, Layout, Margin, Order,
    Rect, Sense, Stroke, StrokeKind, TextStyle, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, pos2,
    vec2,
};

use super::button::{ButtonKind, focus_ring, icon_button_of};
use super::text_input::text_input;
use crate::icons;
use crate::theme::palette::*;
use crate::theme::text::Type;
use crate::theme::tokens::{CONTROL_HEIGHT, radius, space};

/// Height of one row.
const ROW_HEIGHT: f32 = CONTROL_HEIGHT;
/// Inner padding of a row.
const PAD: f32 = space::S;
/// Width of the selected row's accent bar.
const BAR_WIDTH: f32 = 2.0;
/// Thickness of the insertion line shown while dragging.
const LINE_WIDTH: f32 = 2.0;
/// Row actions: rename, move up, move down, delete.
const ACTIONS: usize = 4;
/// Width the actions take.
const ACTIONS_WIDTH: f32 = ACTIONS as f32 * CONTROL_HEIGHT + (ACTIONS - 1) as f32 * space::XS;

/// One row to show.
#[derive(Clone, Copy, Debug)]
pub struct ListRow<'a> {
    /// The caller's id for the row; reported in events.
    pub id: u64,
    /// The row's name.
    pub label: &'a str,
    /// A short status shown muted at the right (e.g. "out of date"); it is
    /// text, so it never relies on colour. Hidden while the actions show.
    pub detail: Option<&'a str>,
    /// Whether the row is selected.
    pub selected: bool,
}

/// What the user did to the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListEvent {
    /// The row was clicked (also reported as [`ListEvent::Activate`]).
    Select(u64),
    /// The row was clicked or Entered: go to it.
    Activate(u64),
    /// The row was renamed to this (trimmed, non-empty, changed) name.
    Rename(u64, String),
    /// The row was dragged, or moved by its actions, to index `to` of the
    /// list as it will be once the row has left its old place. Never sent
    /// for a move that changes nothing.
    Move {
        /// The row.
        id: u64,
        /// Its new index.
        to: usize,
    },
    /// The row's **Delete** was used.
    Delete(u64),
}

/// The list's own state: the rename in progress. Keep one per list.
#[derive(Debug, Default)]
pub struct ListState {
    editing: Option<Editing>,
}

/// A rename in progress.
#[derive(Debug)]
struct Editing {
    id: u64,
    text: String,
    /// The field has yet to be focused (with its text selected).
    fresh: bool,
}

impl ListState {
    /// The row being renamed, if any.
    pub fn editing(&self) -> Option<u64> {
        self.editing.as_ref().map(|e| e.id)
    }

    /// Start renaming row `id`, currently called `label`.
    pub fn start_rename(&mut self, id: u64, label: &str) {
        self.editing = Some(Editing {
            id,
            text: label.to_owned(),
            fresh: true,
        });
    }
}

/// What a dragged row carries: its list and its id.
#[derive(Clone, Copy, Debug)]
struct ListDrag {
    list: Id,
    row: u64,
}

/// [`ordered_list_with`] without details.
pub fn ordered_list(
    ui: &mut Ui,
    id_salt: impl Hash,
    rows: &[ListRow],
    state: &mut ListState,
) -> Vec<ListEvent> {
    ordered_list_with(ui, id_salt, rows, state, |_, _| {})
}

/// Show `rows` over the full available width, and return what the user did
/// this frame. `details` is called with the id of every row whose
/// `selected` is set, to draw the caller's own content in a block under
/// that row (inset to the row's text; it takes the row's width and any
/// height).
pub fn ordered_list_with(
    ui: &mut Ui,
    id_salt: impl Hash,
    rows: &[ListRow],
    state: &mut ListState,
    mut details: impl FnMut(&mut Ui, u64),
) -> Vec<ListEvent> {
    let mut events = Vec::new();
    let salt = {
        use std::hash::Hasher;
        let mut hasher = std::hash::DefaultHasher::new();
        id_salt.hash(&mut hasher);
        hasher.finish()
    };
    ui.push_id(salt, |ui| {
        let list = ListUi {
            id: ui.id(),
            width: ui.available_width(),
        };
        if state
            .editing
            .as_ref()
            .is_some_and(|e| !rows.iter().any(|r| r.id == e.id))
        {
            state.editing = None;
        }
        ui.spacing_mut().item_spacing.y = space::XS;
        let mut rects = Vec::with_capacity(rows.len());
        let mut dragging = None;
        let mut dropped = None;
        for (index, row) in rows.iter().enumerate() {
            let at = list.row(ui, row, index, rows.len(), state, &mut events);
            rects.push(at.rect);
            if at.dragging {
                dragging = Some(row.id);
            }
            if at.dropped {
                dropped = Some(row.id);
            }
            if row.selected {
                Frame::NONE
                    .inner_margin(Margin::symmetric(PAD as i8, space::XS as i8))
                    .show(ui, |ui| {
                        ui.set_width(list.width - 2.0 * PAD);
                        details(ui, row.id);
                    });
            }
        }
        if let Some(id) = dragging {
            list.insertion_line(ui, rows, &rects, id);
        }
        if let Some(id) = dropped
            && let Some(to) = list.drop_target(ui, rows, &rects, id)
        {
            events.push(ListEvent::Move { id, to });
        }
    });
    events
}

/// What drawing a row found.
struct RowOutcome {
    rect: Rect,
    dragging: bool,
    dropped: bool,
}

/// What every row of one frame's list shares.
struct ListUi {
    id: Id,
    width: f32,
}

impl ListUi {
    /// One row, or its name editor. Returns where it is and how it is
    /// being dragged.
    fn row(
        &self,
        ui: &mut Ui,
        row: &ListRow,
        index: usize,
        count: usize,
        state: &mut ListState,
        events: &mut Vec<ListEvent>,
    ) -> RowOutcome {
        let (rect, _) = ui.allocate_exact_size(vec2(self.width, ROW_HEIGHT), Sense::hover());
        let id = self.id.with(("row", row.id));
        let mut out = RowOutcome {
            rect,
            dragging: false,
            dropped: false,
        };
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let editing_here = state.editing.as_ref().is_some_and(|e| e.id == row.id);
        let drag_active = DragAndDrop::has_payload_of_type::<ListDrag>(ui.ctx());
        let actions_key = id.with("actions");
        let focus_in_actions = ui
            .data(|d| d.get_temp::<[Id; ACTIONS]>(actions_key))
            .is_some_and(|ids| ui.memory(|m| m.focused()).is_some_and(|f| ids.contains(&f)));
        let hovered = ui.rect_contains_pointer(rect) && !drag_active;
        let show_actions =
            !editing_here && !drag_active && (hovered || response.has_focus() || focus_in_actions);

        if ui.is_rect_visible(rect) {
            self.paint(ui, row, rect, hovered || show_actions, show_actions);
        }
        if editing_here {
            self.editor(ui, row, rect, state, events);
            return out;
        }

        let label = row.label;
        let selected = row.selected;
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, label)
        });
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(Role::ListItem);
            node.clear_toggled();
            node.set_selected(selected);
            if let Some(detail) = row.detail {
                node.set_description(detail);
            }
        });
        focus_ring(ui, &response, radius::CONTROL);

        if response.double_clicked() {
            state.start_rename(row.id, row.label);
        } else if response.clicked() {
            events.push(ListEvent::Select(row.id));
            events.push(ListEvent::Activate(row.id));
        }
        if response.has_focus() && ui.input(|i| i.key_pressed(Key::F2)) {
            state.start_rename(row.id, row.label);
        }
        if response.drag_started() {
            DragAndDrop::set_payload(
                ui.ctx(),
                ListDrag {
                    list: self.id,
                    row: row.id,
                },
            );
        }
        let mine = DragAndDrop::payload::<ListDrag>(ui.ctx())
            .is_some_and(|p| p.list == self.id && p.row == row.id);
        if response.dragged() && mine {
            out.dragging = true;
            drag_ghost(ui, id, row.label);
        }
        out.dropped = response.drag_stopped() && mine;
        if show_actions {
            self.actions(ui, rect, row, index, count, state, events, actions_key);
        } else {
            ui.data_mut(|d| d.remove::<[Id; ACTIONS]>(actions_key));
        }
        out
    }

    /// The row's fill, accent bar, name and detail.
    fn paint(&self, ui: &Ui, row: &ListRow, rect: Rect, lit: bool, actions: bool) {
        let painter = ui.painter();
        let corner = CornerRadius::same(radius::CONTROL);
        if row.selected {
            painter.rect_filled(rect, corner, ACCENT_800);
            let bar = Rect::from_min_size(rect.min, vec2(BAR_WIDTH, rect.height()));
            painter.rect_filled(bar, corner, ACCENT_400);
        } else if lit {
            painter.rect_filled(rect, corner, SURFACE_700);
        }
        let color = if row.selected {
            TEXT_STRONG
        } else {
            TEXT_EMPHASIS
        };
        let body = TextStyle::Body.resolve(ui.style());
        let small = Type::Small.text_style().resolve(ui.style());
        let right = rect.right() - PAD;
        let mut label_max = rect.width() - 2.0 * PAD;
        if actions {
            label_max -= ACTIONS_WIDTH;
        } else if let Some(detail) = row.detail {
            let galley = elided(ui, detail, small, TEXT_MUTED, label_max / 2.0);
            label_max -= galley.size().x + space::M;
            let pos = pos2(
                right - galley.size().x,
                rect.center().y - galley.size().y / 2.0,
            );
            painter.galley(pos, galley, TEXT_MUTED);
        }
        let galley = elided(ui, row.label, body, color, label_max);
        let pos = pos2(
            rect.left() + PAD + BAR_WIDTH,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(pos, galley, color);
    }

    /// The name editor in place of the row's text.
    fn editor(
        &self,
        ui: &mut Ui,
        row: &ListRow,
        rect: Rect,
        state: &mut ListState,
        events: &mut Vec<ListEvent>,
    ) {
        let Some(edit) = state.editing.as_mut() else {
            return;
        };
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("rename", row.id))
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let field = text_input(&mut child, &mut edit.text, "Name");
        if edit.fresh {
            edit.fresh = false;
            field.request_focus();
            // Everything selected: typing replaces the old name.
            let mut te = TextEditState::load(ui.ctx(), field.id).unwrap_or_default();
            let all = CCursorRange::two(CCursor::new(0), CCursor::new(edit.text.chars().count()));
            te.cursor.set_char_range(Some(all));
            te.store(ui.ctx(), field.id);
            return;
        }
        if !field.lost_focus() {
            return;
        }
        let escaped = ui.input(|i| i.key_pressed(Key::Escape));
        let name = edit.text.trim().to_owned();
        if escaped || name == row.label {
            state.editing = None;
        } else if name.is_empty() {
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                // Refused: the field stays, ready to type again.
                edit.fresh = true;
            } else {
                // Focus went elsewhere: nothing to commit, so cancel.
                state.editing = None;
            }
        } else {
            events.push(ListEvent::Rename(row.id, name));
            state.editing = None;
        }
    }

    /// The row's actions at its right: rename, move up, move down, delete.
    #[allow(clippy::too_many_arguments)]
    fn actions(
        &self,
        ui: &mut Ui,
        rect: Rect,
        row: &ListRow,
        index: usize,
        count: usize,
        state: &mut ListState,
        events: &mut Vec<ListEvent>,
        key: Id,
    ) {
        let at = Rect::from_min_size(
            pos2(rect.right() - ACTIONS_WIDTH - PAD / 2.0, rect.top()),
            vec2(ACTIONS_WIDTH, ROW_HEIGHT),
        );
        // A child with its own id keeps the buttons' ids stable.
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(key)
                .max_rect(at)
                .layout(Layout::left_to_right(Align::Center)),
        );
        child.spacing_mut().item_spacing.x = space::XS;
        // The accessible label names the row; the tooltip stays generic.
        let button = |ui: &mut Ui, enabled: bool, glyph, verb: &str, label: String, kind| {
            let button = icon_button_of(kind, glyph, &label).hover_text(verb);
            ui.add_enabled(enabled, button)
        };
        let name = row.label;
        let quiet = ButtonKind::Quiet;
        let rename_label = format!("Rename {name}");
        let rename = button(
            &mut child,
            true,
            icons::PENCIL,
            "Rename",
            rename_label,
            quiet,
        );
        let up_label = format!("Move {name} up");
        let up = button(
            &mut child,
            index > 0,
            icons::ARROW_UP,
            "Move up",
            up_label,
            quiet,
        );
        let down_label = format!("Move {name} down");
        let down = button(
            &mut child,
            index + 1 < count,
            icons::ARROW_DOWN,
            "Move down",
            down_label,
            quiet,
        );
        let delete_label = format!("Delete {name}");
        let delete = button(
            &mut child,
            true,
            icons::TRASH_2,
            "Delete",
            delete_label,
            ButtonKind::Danger,
        );
        ui.data_mut(|d| d.insert_temp(key, [rename.id, up.id, down.id, delete.id]));
        if rename.clicked() {
            state.start_rename(row.id, row.label);
        }
        if up.clicked() {
            events.push(ListEvent::Move {
                id: row.id,
                to: index - 1,
            });
        }
        if down.clicked() {
            events.push(ListEvent::Move {
                id: row.id,
                to: index + 1,
            });
        }
        if delete.clicked() {
            events.push(ListEvent::Delete(row.id));
        }
    }

    /// The insertion gap (0..=rows) the pointer is over, if it is over the
    /// list, as the index it would drop at once `id` has left its place,
    /// when that changes anything.
    fn drop_target(&self, ui: &Ui, rows: &[ListRow], rects: &[Rect], id: u64) -> Option<usize> {
        let pointer = ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))?;
        let first = rects.first()?;
        let last = rects.last()?;
        let bounds = Rect::from_min_max(first.min, last.max).expand2(vec2(0.0, ROW_HEIGHT));
        if !bounds.contains(pointer) {
            return None;
        }
        let from = rows.iter().position(|r| r.id == id)?;
        let centres: Vec<f32> = rects.iter().map(|r| r.center().y).collect();
        drop_index(&centres, from, pointer.y)
    }

    /// While dragging row `id`: a line where it would land.
    fn insertion_line(&self, ui: &Ui, rows: &[ListRow], rects: &[Rect], id: u64) {
        let Some(to) = self.drop_target(ui, rows, rects, id) else {
            return;
        };
        let Some(from) = rows.iter().position(|r| r.id == id) else {
            return;
        };
        let y = line_gap(rects, to, from);
        let x = rects[0].x_range();
        let line = Rect::from_x_y_ranges(x, (y - LINE_WIDTH / 2.0)..=(y + LINE_WIDTH / 2.0));
        ui.painter().rect_filled(line, 0.0, ACCENT_400);
    }
}

/// Where a row currently at index `from` lands when dropped with the
/// pointer at height `y`, given the heights of the rows' centres: before
/// the first row whose centre is below the pointer, as an index once the
/// row has left its old place. `None` if that is where it already is.
pub(super) fn drop_index(centres: &[f32], from: usize, y: f32) -> Option<usize> {
    let gap = centres.iter().filter(|&&c| c < y).count();
    let to = if gap > from { gap - 1 } else { gap };
    (to != from).then_some(to)
}

/// The height of the insertion line for a row moving from index `from` to
/// `to`: in the gap between the rows around the slot it lands in (counted
/// in the list before the row leaves it), half a spacing outside the ends.
pub(super) fn line_gap(rects: &[Rect], to: usize, from: usize) -> f32 {
    let gap = if to > from { to + 1 } else { to };
    if gap == 0 {
        rects[0].top() - space::XS / 2.0
    } else if gap == rects.len() {
        rects[gap - 1].bottom() + space::XS / 2.0
    } else {
        (rects[gap - 1].bottom() + rects[gap].top()) / 2.0
    }
}

/// `s` on one line no wider than `max`, cut short with "…" if longer.
fn elided(
    ui: &Ui,
    s: &str,
    font: egui::FontId,
    color: Color32,
    max: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(s.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping {
        max_width: max.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// A label following the pointer while a row is dragged.
fn drag_ghost(ui: &Ui, id: Id, label: &str) {
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Tooltip, id.with("drag")));
    let font = TextStyle::Body.resolve(ui.style());
    let galley = painter.layout_no_wrap(label.to_owned(), font, TEXT_STRONG);
    let rect = Rect::from_min_size(
        pointer + Vec2::splat(space::M),
        galley.size() + vec2(2.0 * space::S, space::S),
    );
    painter.rect(
        rect,
        CornerRadius::same(radius::CONTROL),
        SURFACE_700,
        Stroke::new(1.0, BORDER_STRONG),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, TEXT_STRONG);
}
