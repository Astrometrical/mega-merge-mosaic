//! Notes: free text read as a paragraph and edited in place. The first
//! [`NOTES_LINES`] lines show, with **More** for the rest; **Edit** swaps
//! in a text box (focused, the cursor at the end), and **Done** or Esc
//! swaps it back and returns the focus to **Edit**. Saving is the caller's
//! job.

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::text_edit::TextEditState;
use egui::{Align, Key, Label, Layout, Response, TextEdit, Ui, vec2};

use super::button::{ButtonKind, MegaButton};
use crate::icons;
use crate::theme::text::Type;
use crate::theme::tokens::CONTROL_HEIGHT;

/// Lines of notes shown before **More**.
pub const NOTES_LINES: usize = 8;

/// How [`notes_view`] shows the notes. The caller keeps it, one per notes
/// shown (e.g. a fresh one for another session's notes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotesMode {
    /// Read: the first [`NOTES_LINES`] lines, then **More** if there are
    /// more.
    #[default]
    Collapsed,
    /// Read: every line, then **Less**.
    Expanded,
    /// Edited in a text box, then **Done**.
    Editing,
}

/// What [`notes_view`] drew, and what happened in it this frame.
#[derive(Debug)]
pub struct NotesResponse {
    /// The text box, while editing.
    pub field: Option<Response>,
    /// **Edit** (**Add notes** when there are none), while reading.
    pub edit: Option<Response>,
    /// Editing ended this frame (**Done**, Esc, or the notes stopped being
    /// editable): a good moment to save.
    pub done: bool,
}

impl NotesResponse {
    /// Whether the text was edited this frame.
    pub fn changed(&self) -> bool {
        self.field.as_ref().is_some_and(Response::changed)
    }
}

/// `text` as notes over the full available width, in `mode`. `hint` shows
/// in the empty text box. **Edit** is disabled unless `editable`, and
/// editing stops when `editable` turns false. One per `ui` (its ids come
/// from `ui`'s): wrap more in scopes with their own ids.
pub fn notes_view(
    ui: &mut Ui,
    text: &mut String,
    mode: &mut NotesMode,
    hint: &str,
    editable: bool,
) -> NotesResponse {
    let id = ui.make_persistent_id("mega-notes");
    let field_id = id.with("field");
    // Focus moves next frame, once its target is drawn (focus on a widget
    // not in the frame's accessibility tree is an error there).
    let focus_field = id.with("focus-field");
    let refocus = id.with("refocus");
    let mut out = NotesResponse {
        field: None,
        edit: None,
        done: false,
    };
    if *mode == NotesMode::Editing && !editable {
        *mode = NotesMode::Collapsed;
        out.done = true;
    }
    if *mode == NotesMode::Editing {
        let field = ui.add(
            TextEdit::multiline(text)
                .id(field_id)
                .hint_text(hint)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        if take_flag(ui, focus_field) {
            field.request_focus();
        }
        let escaped = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape));
        let done = button_row(ui, |ui| {
            let done = MegaButton::new(ButtonKind::Secondary, "Done").icon(icons::CHECK);
            ui.add(done).clicked()
        });
        if escaped || done {
            *mode = NotesMode::Collapsed;
            out.done = true;
            set_flag(ui, refocus);
        }
        out.field = Some(field);
        return out;
    }
    let shown = text.trim_end();
    let mut hidden = false;
    if !shown.is_empty() {
        let font = Type::Body.text_style().resolve(ui.style());
        let width = ui.available_width();
        let mut job = LayoutJob::simple(shown.to_owned(), font, Type::Body.color(), width);
        if *mode == NotesMode::Collapsed {
            job.wrap.max_rows = NOTES_LINES;
            job.wrap.overflow_character = Some('…');
        }
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        hidden = galley.elided;
        ui.add(Label::new(galley));
    }
    button_row(ui, |ui| {
        if hidden || *mode == NotesMode::Expanded {
            let (label, glyph, next) = if *mode == NotesMode::Expanded {
                ("Less", icons::CHEVRON_UP, NotesMode::Collapsed)
            } else {
                ("More", icons::CHEVRON_DOWN, NotesMode::Expanded)
            };
            let toggle = MegaButton::new(ButtonKind::Quiet, label).trailing_icon(glyph);
            if ui.add(toggle).clicked() {
                *mode = next;
                ui.ctx().request_repaint();
            }
        }
        let label = if shown.is_empty() {
            "Add notes"
        } else {
            "Edit"
        };
        let edit = MegaButton::new(ButtonKind::Quiet, label).icon(icons::PENCIL);
        let edit = ui.add_enabled(editable, edit);
        if take_flag(ui, refocus) {
            edit.request_focus();
        }
        if edit.clicked() {
            *mode = NotesMode::Editing;
            // Focused, with the cursor at the end: ready to add to them.
            set_flag(ui, focus_field);
            let mut state = TextEditState::load(ui.ctx(), field_id).unwrap_or_default();
            let end = CCursor::new(text.chars().count());
            state.cursor.set_char_range(Some(CCursorRange::one(end)));
            state.store(ui.ctx(), field_id);
        }
        out.edit = Some(edit);
    });
    out
}

/// Raise the flag `id` (and draw again, to act on it).
fn set_flag(ui: &Ui, id: egui::Id) {
    ui.data_mut(|d| d.insert_temp(id, true));
    ui.ctx().request_repaint();
}

/// Whether the flag `id` was raised, lowering it.
fn take_flag(ui: &Ui, id: egui::Id) -> bool {
    ui.data_mut(|d| d.remove_temp::<bool>(id)).is_some()
}

/// A row a control high, so its items centre on each other.
fn button_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let row = vec2(ui.available_width(), CONTROL_HEIGHT);
    ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), add)
        .inner
}
