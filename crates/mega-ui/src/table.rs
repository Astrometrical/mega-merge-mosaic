//! Sectioned table: rows under folding section headings, "a table, not a
//! spreadsheet". Columns line up for scanning; there are no grid lines;
//! rows are roomy; column headings are quiet overlines; numbers are in the
//! mono face. A section can roll up to its first rows (rows with a problem
//! always show) behind a "… N more" row. Rows carry an actions menu (shown
//! on hover or focus), are clicked with modifiers (the caller owns the
//! selection) and drag onto headings that take drops.
//!
//! The caller places the table in a `ScrollArea` and persists
//! [`TableState`]. Rows outside the visible area are laid out but not
//! painted; a rolled-up section bounds the rows it lays out.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Color32, CornerRadius, DragAndDrop, FontId, FontSelection, Galley, Id, LayerId, Layout,
    Modifiers, Order, Popup, Rect, Sense, Stroke, StrokeKind, TextStyle, TextWrapMode, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetText, WidgetType, pos2,
};

use crate::icons;
use crate::theme::Tone;
use crate::theme::fonts::semibold;
use crate::theme::palette::{
    ACCENT_500, ACCENT_800, BORDER, BORDER_STRONG, SURFACE_700, TEXT_BODY, TEXT_EMPHASIS,
    TEXT_MUTED, TEXT_STRONG,
};
use crate::theme::text::{Type, text};
use crate::theme::tokens::{CONTROL_HEIGHT, radius, space};
use crate::widgets::{ButtonKind, focus_ring, icon_button_of};

/// Rows a rolled-up section shows (besides rows with a problem) before
/// its "… N more" row.
pub const ROLL_UP_ROWS: usize = 3;

/// A column of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column<'a> {
    /// Heading text, drawn as an overline (upper case).
    pub title: &'a str,
    /// Whether the cells are numbers: right-aligned in the mono face.
    pub numeric: bool,
}

/// A row of the table.
#[derive(Clone, Debug, PartialEq)]
pub struct Row<'a> {
    /// The caller's id for the row, reported in [`TableEvent`]s.
    pub id: u64,
    /// One text per column; missing cells are blank. The first cell is the
    /// row's accessible label.
    pub cells: Vec<String>,
    /// What is wrong with the row: a warning icon after the first cell,
    /// this text its tooltip. A rolled-up section always shows the row.
    pub problem: Option<&'a str>,
    /// A small muted pill after the first cell (e.g. "by hand").
    pub tag: Option<&'a str>,
    /// Whether the row is selected (the caller's selection).
    pub selected: bool,
    /// Whether the row is highlighted (e.g. its item is hovered elsewhere).
    pub highlighted: bool,
}

/// The heading row of a section.
#[derive(Clone, Debug, PartialEq)]
pub struct Heading<'a> {
    /// Stable id: the key in [`TableState`] and the `section` of
    /// [`TableEvent::Drop`].
    pub id: &'a str,
    /// Heading text; also its accessible label.
    pub title: String,
    /// Colour dot before the title (a group colour).
    pub color: Option<Color32>,
    /// A warning after the title, in the warning tone.
    pub warning: Option<String>,
    /// Whether the section rolls up to its first [`ROLL_UP_ROWS`] rows
    /// (plus rows with a problem) until its "… N more" row is clicked.
    pub rolls_up: bool,
    /// Whether rows can be dropped on the heading ([`TableEvent::Drop`]).
    pub drop_target: bool,
}

/// A section of the table: a heading and its rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Section<'a> {
    /// The heading; `None` for a flat list (no heading, no fold, no
    /// roll-up).
    pub heading: Option<Heading<'a>>,
    /// The rows, in display order.
    pub rows: Vec<Row<'a>>,
}

/// Fold and roll-up state of a table, keyed by [`Heading::id`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableState {
    /// Sections whose rows are folded away.
    pub folded: BTreeSet<String>,
    /// Rolling-up sections the user opened in full ("… N more").
    pub expanded: BTreeSet<String>,
}

impl TableState {
    /// Whether section `id` is folded.
    pub fn is_folded(&self, id: &str) -> bool {
        self.folded.contains(id)
    }

    /// Fold section `id` if open, unfold it if folded.
    pub fn toggle_fold(&mut self, id: &str) {
        if !self.folded.remove(id) {
            self.folded.insert(id.to_owned());
        }
    }

    /// Whether rolling-up section `id` shows all its rows.
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// Show all rows of rolling-up section `id`.
    pub fn expand(&mut self, id: &str) {
        self.expanded.insert(id.to_owned());
    }
}

/// What the user did in the table this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableEvent {
    /// A row was clicked (or activated from the keyboard), not on its
    /// actions button, with these modifiers held: the caller updates its
    /// selection (plain, Shift, Ctrl/Cmd).
    Click {
        /// The row's id.
        id: u64,
        /// Modifiers held at the click.
        modifiers: Modifiers,
    },
    /// The pointer is over this row (at most one per frame).
    Hover(u64),
    /// Rows were dragged onto a heading with [`Heading::drop_target`]: the
    /// selected rows if the dragged row was selected, else that row. Also
    /// reported for a drop on the rows' own section; the caller ignores
    /// what does not move.
    Drop {
        /// Ids of the dropped rows, in display order.
        ids: Vec<u64>,
        /// [`Heading::id`] of the heading they were dropped on.
        section: String,
    },
}

/// Accessible label and tooltip of a row's actions button.
const ACTIONS_LABEL: &str = "Panel actions";
/// Row and heading height.
const ROW_HEIGHT: f32 = CONTROL_HEIGHT;
/// Column heading row height.
const HEADER_HEIGHT: f32 = space::XL;
/// Size of the chevron, problem and warning icons.
const ICON_SIZE: f32 = 14.0;
/// Diameter of a heading's colour dot.
const DOT: f32 = 8.0;
/// Inner padding at a row's left and right ends.
const PAD: f32 = space::S;
/// Gap between columns.
const COLUMN_GAP: f32 = space::L;
/// Rows under headings start below the colour dot, past the chevron.
const INDENT: f32 = ICON_SIZE + space::XS;
/// Narrowest the first column gets; the table overflows below it.
const MIN_FIRST_WIDTH: f32 = 120.0;
/// Tag pill height.
const TAG_HEIGHT: f32 = 18.0;
/// Horizontal padding inside a tag pill.
const TAG_PAD: f32 = space::S;
/// Opacity of a highlighted row's fill, the selection fill
/// ([`ACCENT_800`]) at part strength.
const HIGHLIGHT_STRENGTH: f32 = 0.5;

/// What a dragged row carries: the ids of the rows it moves.
#[derive(Clone, Debug)]
struct RowDrag(Vec<u64>);

/// Where the columns sit, as offsets from a row's left edge.
struct Geometry {
    /// Left of the first column.
    first_x: f32,
    /// Width of the first column.
    first_w: f32,
    /// Left and width of each further column.
    cols: Vec<(f32, f32)>,
    /// Left of the actions button.
    actions_x: f32,
}

/// What every line of one frame's table shares.
struct Table<'t> {
    /// Base of the ids of headings, rows and buttons.
    id: Id,
    /// Width of every line.
    width: f32,
    /// Column positions.
    g: Geometry,
    columns: &'t [Column<'t>],
    sections: &'t [Section<'t>],
}

/// Draw `sections` of rows under a row of `columns` headings, and report
/// what the user did. `row_menu` draws the contents of the actions menu of
/// the row with the given id (e.g. a **Move to** submenu, **Remove**); the table
/// draws the button that opens it (a quiet "⋯", "Panel actions") while
/// the row is hovered or focused. Takes the available width: the first
/// column flexes, the others fit their contents. One table per `Ui` (give
/// a second one its own `ui.push_id`).
pub fn sectioned_table(
    ui: &mut Ui,
    state: &mut TableState,
    columns: &[Column<'_>],
    sections: &[Section<'_>],
    mut row_menu: impl FnMut(&mut Ui, u64),
) -> Vec<TableEvent> {
    let mut events = Vec::new();
    let id = ui.id().with("mega-sectioned-table");
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width();
        let table = Table {
            id,
            width,
            g: geometry(ui, width, columns, sections),
            columns,
            sections,
        };
        table.column_headings(ui);
        let mut hovered = false;
        for (i, section) in sections.iter().enumerate() {
            let mut rolled = false;
            if let Some(heading) = &section.heading {
                if i > 0 {
                    ui.add_space(space::XS);
                }
                table.heading_row(ui, heading, state, &mut events);
                if state.is_folded(heading.id) {
                    continue;
                }
                rolled = heading.rolls_up && !state.is_expanded(heading.id);
            }
            let (shown, hidden) = shown_rows(&section.rows, rolled);
            for &r in &shown {
                let row = &section.rows[r];
                table.row_line(ui, row, &mut row_menu, &mut hovered, &mut events);
            }
            if hidden > 0
                && let Some(heading) = &section.heading
                && table.more_row(ui, heading.id, hidden)
            {
                state.expand(heading.id);
            }
        }
    });
    events
}

/// Indices of `rows` a section shows and how many it hides behind its
/// "… N more" row: all of them, or when `rolled` the first
/// [`ROLL_UP_ROWS`] and every row with a problem.
fn shown_rows(rows: &[Row<'_>], rolled: bool) -> (Vec<usize>, usize) {
    if !rolled {
        return ((0..rows.len()).collect(), 0);
    }
    let shown: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(i, r)| *i < ROLL_UP_ROWS || r.problem.is_some())
        .map(|(i, _)| i)
        .collect();
    let hidden = rows.len() - shown.len();
    (shown, hidden)
}

/// The rows a drag of row `dragged` carries: every selected row (in
/// display order, folded or rolled-up ones too) if it is selected, else
/// just it.
fn drag_ids(sections: &[Section<'_>], dragged: u64) -> Vec<u64> {
    let rows = || sections.iter().flat_map(|s| &s.rows);
    if rows().any(|r| r.id == dragged && r.selected) {
        rows().filter(|r| r.selected).map(|r| r.id).collect()
    } else {
        vec![dragged]
    }
}

/// The body font.
fn body_font(ui: &Ui) -> FontId {
    TextStyle::Body.resolve(ui.style())
}

/// The small font (tags, warnings).
fn small_font(ui: &Ui) -> FontId {
    Type::Small.text_style().resolve(ui.style())
}

/// `s` laid out on one line in `font`.
fn line(ui: &Ui, s: &str, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(s.to_owned(), font, color)
}

/// `glyph` in the icon font at [`ICON_SIZE`].
fn icon(ui: &Ui, glyph: char, color: Color32) -> Arc<Galley> {
    line(ui, &glyph.to_string(), icons::font(ICON_SIZE), color)
}

/// `s` on one line no wider than `max`, cut short with "…" if longer.
fn elided(ui: &Ui, s: &str, font: FontId, color: Color32, max: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(s.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping {
        max_width: max.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// A column heading laid out as an overline.
fn overline_galley(ui: &Ui, title: &str) -> Arc<Galley> {
    WidgetText::from(text(Type::Overline, title)).into_galley(
        ui,
        Some(TextWrapMode::Extend),
        f32::INFINITY,
        FontSelection::Default,
    )
}

/// Whether column `i` holds numbers.
fn is_numeric(columns: &[Column<'_>], i: usize) -> bool {
    columns.get(i).is_some_and(|c| c.numeric)
}

/// The font of cells in column `i`: mono for numbers.
fn cell_font(ui: &Ui, columns: &[Column<'_>], i: usize) -> FontId {
    if is_numeric(columns, i) {
        Type::Numeric.text_style().resolve(ui.style())
    } else {
        body_font(ui)
    }
}

/// Column positions for a table `width` wide. The columns after the first
/// fit the widest of their heading and every cell (in every section, shown
/// or not, so they hold still as sections fold and expand); the first
/// takes the rest. Rows under headings are indented past the chevron.
fn geometry(ui: &Ui, width: f32, columns: &[Column<'_>], sections: &[Section<'_>]) -> Geometry {
    let mut widths: Vec<f32> = columns
        .iter()
        .skip(1)
        .map(|c| overline_galley(ui, c.title).size().x)
        .collect();
    for (k, w) in widths.iter_mut().enumerate() {
        let i = k + 1;
        let font = cell_font(ui, columns, i);
        for row in sections.iter().flat_map(|s| &s.rows) {
            if let Some(cell) = row.cells.get(i).filter(|c| !c.is_empty()) {
                *w = w.max(line(ui, cell, font.clone(), TEXT_BODY).size().x);
            }
        }
    }
    let headed = sections.iter().any(|s| s.heading.is_some());
    let first_x = PAD + if headed { INDENT } else { 0.0 };
    let tail: f32 =
        widths.iter().map(|w| COLUMN_GAP + w).sum::<f32>() + space::S + CONTROL_HEIGHT + space::XS;
    let first_w = (width - first_x - tail).max(MIN_FIRST_WIDTH);
    let mut x = first_x + first_w;
    let cols = widths
        .into_iter()
        .map(|w| {
            x += COLUMN_GAP;
            let col = (x, w);
            x += w;
            col
        })
        .collect();
    Geometry {
        first_x,
        first_w,
        cols,
        actions_x: x + space::S,
    }
}

/// Paint `galley` at `x` from `rect`'s left in a cell `w` wide, centred
/// vertically and right-aligned when `right`. Returns the painted rect.
fn paint_in_cell(ui: &Ui, rect: Rect, x: f32, w: f32, galley: Arc<Galley>, right: bool) -> Rect {
    let left = if right {
        rect.left() + x + w - galley.size().x
    } else {
        rect.left() + x
    };
    let pos = pos2(left, rect.center().y - galley.size().y / 2.0);
    let painted = Rect::from_min_size(pos, galley.size());
    ui.painter().galley(pos, galley, TEXT_BODY);
    painted
}

/// A full-width line `height` high, laid out under the last.
fn allocate_line(ui: &mut Ui, width: f32, height: f32) -> Rect {
    ui.allocate_exact_size(Vec2::new(width, height), Sense::hover())
        .0
}

impl Table<'_> {
    /// The row of overline column headings.
    fn column_headings(&self, ui: &mut Ui) {
        if self.columns.is_empty() {
            return;
        }
        let rect = allocate_line(ui, self.width, HEADER_HEIGHT);
        if !ui.is_rect_visible(rect) {
            return;
        }
        for (i, column) in self.columns.iter().enumerate() {
            let (x, w) = if i == 0 {
                (self.g.first_x, self.g.first_w)
            } else {
                self.g.cols[i - 1]
            };
            let galley = overline_galley(ui, column.title);
            paint_in_cell(ui, rect, x, w, galley, column.numeric);
        }
    }

    /// A section heading: chevron, colour dot, title and warning. A click
    /// folds or unfolds it; a heading with `drop_target` takes dropped
    /// rows.
    fn heading_row(
        &self,
        ui: &mut Ui,
        heading: &Heading<'_>,
        state: &mut TableState,
        events: &mut Vec<TableEvent>,
    ) {
        let rect = allocate_line(ui, self.width, ROW_HEIGHT);
        let id = self.id.with(("heading", heading.id));
        let response = ui.interact(rect, id, Sense::click());
        let carrying = heading.drop_target && response.dnd_hover_payload::<RowDrag>().is_some();
        if ui.is_rect_visible(rect) {
            let corner = CornerRadius::same(radius::CONTROL);
            if carrying {
                let stroke = Stroke::new(1.0, ACCENT_500);
                let fill = Tone::Info.tint();
                ui.painter()
                    .rect(rect, corner, fill, stroke, StrokeKind::Inside);
            } else if response.hovered() {
                ui.painter().rect_filled(rect, corner, SURFACE_700);
            }
            let glyph = if state.is_folded(heading.id) {
                icons::CHEVRON_RIGHT
            } else {
                icons::CHEVRON_DOWN
            };
            let chevron = if response.hovered() {
                TEXT_STRONG
            } else {
                Type::Overline.color()
            };
            paint_in_cell(ui, rect, PAD, ICON_SIZE, icon(ui, glyph, chevron), false);

            let mut x = PAD + INDENT;
            if let Some(c) = heading.color {
                let centre = pos2(rect.left() + x + DOT / 2.0, rect.center().y);
                ui.painter().circle_filled(centre, DOT / 2.0, c);
                x += DOT + space::S;
            }
            let end = self.width - PAD;
            let font = FontId::new(body_font(ui).size, semibold());
            let title = elided(ui, &heading.title, font, TEXT_STRONG, end - x);
            x += paint_in_cell(ui, rect, x, 0.0, title, false).width() + space::M;
            if let Some(warning) = &heading.warning
                && x + ICON_SIZE < end
            {
                let tone = Tone::Warning.color();
                let alert = icon(ui, icons::TRIANGLE_ALERT, tone);
                x += paint_in_cell(ui, rect, x, 0.0, alert, false).width() + space::XS;
                let galley = elided(ui, warning, small_font(ui), tone, end - x);
                paint_in_cell(ui, rect, x, 0.0, galley, false);
            }
        }
        focus_ring(ui, &response, radius::CONTROL);
        let title = heading.title.as_str();
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, title));
        if let Some(warning) = &heading.warning {
            ui.ctx()
                .accesskit_node_builder(id, |node| node.set_description(warning.as_str()));
        }
        if response.clicked() {
            state.toggle_fold(heading.id);
        }
        if heading.drop_target
            && let Some(payload) = response.dnd_release_payload::<RowDrag>()
        {
            events.push(TableEvent::Drop {
                ids: payload.0.clone(),
                section: heading.id.to_owned(),
            });
        }
    }

    /// One row: its fill, cells, problem icon and tag, and while hovered or
    /// focused (or its menu is open) its actions button.
    fn row_line(
        &self,
        ui: &mut Ui,
        row: &Row<'_>,
        row_menu: &mut dyn FnMut(&mut Ui, u64),
        hovered: &mut bool,
        events: &mut Vec<TableEvent>,
    ) {
        let rect = allocate_line(ui, self.width, ROW_HEIGHT);
        let id = self.id.with(("row", row.id));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let pointer_in = ui.rect_contains_pointer(rect);
        if pointer_in && !*hovered {
            *hovered = true;
            events.push(TableEvent::Hover(row.id));
        }
        let menu_id = id.with("menu");
        let menu_open = Popup::is_id_open(ui.ctx(), menu_id);
        // The actions button's id from last frame, to know if it has focus.
        let button_key = id.with("actions");
        let button_focused = ui
            .data(|d| d.get_temp::<Id>(button_key))
            .is_some_and(|b| ui.memory(|m| m.has_focus(b)));
        let show_actions = pointer_in || menu_open || response.has_focus() || button_focused;

        let first = row.cells.first().map_or("", String::as_str);
        let mut cell = FirstCell::default();
        if ui.is_rect_visible(rect) {
            let corner = CornerRadius::same(radius::CONTROL);
            let painter = ui.painter();
            if row.selected {
                painter.rect_filled(rect, corner, ACCENT_800);
                if row.highlighted {
                    let stroke = Stroke::new(1.0, ACCENT_500);
                    painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);
                }
            } else if row.highlighted {
                // The selection fill at half strength: lighter than a
                // selected row, clearly apart from the hover fill.
                let fill = ACCENT_800.gamma_multiply(HIGHLIGHT_STRENGTH);
                painter.rect_filled(rect, corner, fill);
            } else if pointer_in || menu_open {
                painter.rect_filled(rect, corner, SURFACE_700);
            }
            cell = self.first_cell(ui, rect, row, first);
            for (k, &(x, w)) in self.g.cols.iter().enumerate() {
                let i = k + 1;
                let Some(text) = row.cells.get(i).filter(|c| !c.is_empty()) else {
                    continue;
                };
                let numeric = is_numeric(self.columns, i);
                let color = if numeric {
                    Type::Numeric.color()
                } else {
                    TEXT_BODY
                };
                let galley = line(ui, text, cell_font(ui, self.columns, i), color);
                paint_in_cell(ui, rect, x, w, galley, numeric);
            }
        }
        if let (Some(problem), Some(at)) = (row.problem, cell.icon) {
            let tip = ui.interact(at, id.with("problem"), Sense::hover());
            tip.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, problem));
            tip.on_hover_text(problem);
        }
        focus_ring(ui, &response, radius::CONTROL);
        let selected = row.selected;
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, first)
        });
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(Role::ListItem);
            node.clear_toggled();
            node.set_selected(selected);
        });
        let response = if cell.cut {
            response.on_hover_text(first)
        } else {
            response
        };

        if response.clicked() {
            let modifiers = ui.input(|i| i.modifiers);
            events.push(TableEvent::Click {
                id: row.id,
                modifiers,
            });
        }
        if response.drag_started() {
            DragAndDrop::set_payload(ui.ctx(), RowDrag(drag_ids(self.sections, row.id)));
        }
        if response.dragged() {
            drag_ghost(ui, id, first);
        }
        if show_actions {
            self.actions_button(ui, rect, row.id, button_key, menu_id, row_menu);
        }
    }

    /// Paint the first cell: its text (cut short to leave room), then the
    /// problem icon and the tag.
    fn first_cell(&self, ui: &Ui, rect: Rect, row: &Row<'_>, first: &str) -> FirstCell {
        let tag = row.tag.map(|t| line(ui, t, small_font(ui), TEXT_MUTED));
        let icon_w = if row.problem.is_some() {
            space::XS + ICON_SIZE
        } else {
            0.0
        };
        let tag_w = tag
            .as_ref()
            .map_or(0.0, |t| space::S + t.size().x + 2.0 * TAG_PAD);
        let color = if row.selected {
            TEXT_STRONG
        } else {
            TEXT_EMPHASIS
        };
        let max = self.g.first_w - icon_w - tag_w;
        let galley = elided(ui, first, body_font(ui), color, max);
        let mut out = FirstCell {
            cut: galley.elided,
            icon: None,
        };
        let mut x = self.g.first_x;
        x += paint_in_cell(ui, rect, x, 0.0, galley, false).width();
        if row.problem.is_some() {
            x += space::XS;
            let alert = icon(ui, icons::TRIANGLE_ALERT, Tone::Warning.color());
            let at = paint_in_cell(ui, rect, x, 0.0, alert, false);
            out.icon = Some(at);
            x += at.width();
        }
        if let Some(tag) = tag {
            x += space::S;
            let pill = Rect::from_min_size(
                pos2(rect.left() + x, rect.center().y - TAG_HEIGHT / 2.0),
                Vec2::new(tag.size().x + 2.0 * TAG_PAD, TAG_HEIGHT),
            );
            ui.painter().rect_stroke(
                pill,
                CornerRadius::same((TAG_HEIGHT / 2.0) as u8),
                Stroke::new(1.0, BORDER),
                StrokeKind::Inside,
            );
            paint_in_cell(ui, rect, x + TAG_PAD, 0.0, tag, false);
        }
        out
    }

    /// The row's quiet "⋯" button, opening a menu drawn by `row_menu`.
    fn actions_button(
        &self,
        ui: &mut Ui,
        rect: Rect,
        row: u64,
        button_key: Id,
        menu_id: Id,
        row_menu: &mut dyn FnMut(&mut Ui, u64),
    ) {
        let at = Rect::from_min_size(
            pos2(rect.left() + self.g.actions_x, rect.top()),
            Vec2::splat(CONTROL_HEIGHT),
        );
        // A child with its own id keeps the button's id stable however many
        // rows show theirs.
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(button_key)
                .max_rect(at)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let button = child.add(icon_button_of(
            ButtonKind::Quiet,
            icons::ELLIPSIS,
            ACTIONS_LABEL,
        ));
        ui.data_mut(|d| d.insert_temp(button_key, button.id));
        Popup::menu(&button)
            .id(menu_id)
            .show(|ui| row_menu(ui, row));
    }

    /// The "… N more" row: a button that opens the whole section. Returns
    /// whether it was clicked.
    fn more_row(&self, ui: &mut Ui, section: &str, hidden: usize) -> bool {
        let rect = allocate_line(ui, self.width, ROW_HEIGHT);
        let id = self.id.with(("more", section));
        let response = ui.interact(rect, id, Sense::click());
        let label = format!("… {hidden} more");
        if ui.is_rect_visible(rect) {
            let color = if response.hovered() {
                let corner = CornerRadius::same(radius::CONTROL);
                ui.painter().rect_filled(rect, corner, SURFACE_700);
                Tone::Info.color()
            } else {
                TEXT_MUTED
            };
            let galley = line(ui, &label, body_font(ui), color);
            paint_in_cell(ui, rect, self.g.first_x, 0.0, galley, false);
        }
        focus_ring(ui, &response, radius::CONTROL);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &label));
        response.clicked()
    }
}

/// What [`Table::first_cell`] painted.
#[derive(Default)]
struct FirstCell {
    /// Whether the text was cut short.
    cut: bool,
    /// Where the problem icon is, if any.
    icon: Option<Rect>,
}

/// A label following the pointer while row `id` is dragged: its first
/// cell, and how many more rows it carries.
fn drag_ghost(ui: &Ui, id: Id, first: &str) {
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let n = DragAndDrop::payload::<RowDrag>(ui.ctx()).map_or(1, |p| p.0.len());
    let label = if n > 1 {
        format!("{first}  +{}", n - 1)
    } else {
        first.to_owned()
    };
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Tooltip, id.with("drag")));
    let galley = painter.layout_no_wrap(label, body_font(ui), TEXT_STRONG);
    let rect = Rect::from_min_size(
        pointer + Vec2::splat(space::M),
        galley.size() + Vec2::new(2.0 * space::S, space::S),
    );
    painter.rect(
        rect,
        CornerRadius::same(radius::CONTROL),
        SURFACE_700,
        Stroke::new(1.0, BORDER_STRONG),
        StrokeKind::Inside,
    );
    let pos = rect.center() - galley.size() / 2.0;
    painter.galley(pos, galley, TEXT_STRONG);
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
