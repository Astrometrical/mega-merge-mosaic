use egui::accesskit::Role;
use egui::{Shape, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::notes::{NOTES_LINES, NotesMode, notes_view};
use super::segmented::segmented;

#[derive(Default)]
struct Seg {
    value: u8,
    changed: bool,
}

#[test]
fn segmented_click_changes_value() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(500.0, 100.0))
        .build_ui_state(
            |ui, s: &mut Seg| {
                if crate::testutil::themed(ui) {
                    let opts = [(0, "Composite"), (1, "Hα"), (2, "OIII")];
                    s.changed |= segmented(ui, &mut s.value, &opts).changed();
                }
            },
            Seg::default(),
        );
    h.run();
    h.get_by_label("OIII").click();
    h.run();
    assert_eq!(h.state().value, 2);
    assert!(h.state().changed);
}

/// What the notes harness keeps between frames.
struct Notes {
    text: String,
    mode: NotesMode,
    editable: bool,
    /// Frames in which editing ended.
    done: usize,
    /// Whether any frame edited the text.
    changed: bool,
}

fn notes_harness(text: &str, editable: bool) -> Harness<'static, Notes> {
    let state = Notes {
        text: text.to_owned(),
        mode: NotesMode::default(),
        editable,
        done: 0,
        changed: false,
    };
    Harness::builder()
        .with_size(Vec2::new(500.0, 500.0))
        .build_ui_state(
            |ui, s: &mut Notes| {
                if crate::testutil::themed(ui) {
                    let r = notes_view(
                        ui,
                        &mut s.text,
                        &mut s.mode,
                        "Notes about this session",
                        s.editable,
                    );
                    s.changed |= r.changed();
                    s.done += usize::from(r.done);
                }
            },
            state,
        )
}

/// The rows of the painted notes text that starts with `start`.
fn painted_rows(h: &Harness<'static, Notes>, start: &str) -> usize {
    fn walk(shape: &Shape, start: &str, out: &mut Option<usize>) {
        match shape {
            Shape::Text(t) if t.galley.text().starts_with(start) => {
                *out = Some(t.galley.rows.len());
            }
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, start, out)),
            _ => {}
        }
    }
    let mut out = None;
    for c in &h.output().shapes {
        walk(&c.shape, start, &mut out);
    }
    out.unwrap_or_else(|| panic!("no painted text starting {start:?}"))
}

/// Twenty short lines.
fn twenty_lines() -> String {
    (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn notes_read_as_text_with_edit() {
    let mut h = notes_harness("Shoot row 4", true);
    h.run();
    h.get_by_role_and_label(Role::Label, "Shoot row 4");
    assert!(h.query_by_role(Role::MultilineTextInput).is_none());
    assert!(h.query_by_label("More").is_none(), "nothing is hidden");
    h.get_by_role_and_label(Role::Button, "Edit");
}

#[test]
fn long_notes_show_their_first_lines_then_more() {
    let mut h = notes_harness(&twenty_lines(), true);
    h.run();
    assert_eq!(painted_rows(&h, "line 1"), NOTES_LINES);
    h.get_by_role_and_label(Role::Button, "More").click();
    h.run();
    assert_eq!(painted_rows(&h, "line 1"), 20);
    h.get_by_role_and_label(Role::Button, "Less").click();
    h.run();
    assert_eq!(painted_rows(&h, "line 1"), NOTES_LINES);
    h.get_by_role_and_label(Role::Button, "More");
}

#[test]
fn edit_swaps_in_a_focused_text_box_and_done_swaps_it_back() {
    let mut h = notes_harness("Shoot row 4", true);
    h.run();
    h.get_by_role_and_label(Role::Button, "Edit").click();
    h.run();
    let field = h.get_by_role(Role::MultilineTextInput);
    assert!(field.accesskit_node().is_focused(), "ready to type");
    // The cursor is at the end: typing adds to the notes.
    field.type_text(" next moon");
    h.run();
    assert_eq!(h.state().text, "Shoot row 4 next moon");
    assert!(h.state().changed);
    h.get_by_role_and_label(Role::Button, "Done").click();
    h.run();
    assert!(h.query_by_role(Role::MultilineTextInput).is_none());
    assert_eq!(h.state().mode, NotesMode::Collapsed);
    assert_eq!(h.state().done, 1);
    h.get_by_role_and_label(Role::Label, "Shoot row 4 next moon");
    // Keyboard users carry on from where they started.
    assert!(
        h.get_by_role_and_label(Role::Button, "Edit")
            .accesskit_node()
            .is_focused()
    );
}

#[test]
fn escape_finishes_editing() {
    let mut h = notes_harness("Shoot row 4", true);
    h.run();
    h.get_by_role_and_label(Role::Button, "Edit").click();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_role(Role::MultilineTextInput).is_none());
    assert_eq!(h.state().mode, NotesMode::Collapsed);
    assert_eq!(h.state().done, 1);
}

#[test]
fn empty_notes_offer_add_notes() {
    let mut h = notes_harness("", true);
    h.run();
    assert!(h.query_by_label("Edit").is_none());
    h.get_by_role_and_label(Role::Button, "Add notes").click();
    h.run();
    h.get_by(|n| n.placeholder() == Some("Notes about this session"));
}

#[test]
fn read_only_notes_cannot_be_edited() {
    let mut h = notes_harness("Shoot row 4", false);
    h.run();
    let edit = h.get_by_role_and_label(Role::Button, "Edit");
    assert!(edit.accesskit_node().is_disabled());
    edit.click();
    h.run();
    assert!(h.query_by_role(Role::MultilineTextInput).is_none());
    assert_eq!(h.state().text, "Shoot row 4");
}

#[test]
fn notes_that_stop_being_editable_stop_editing() {
    let mut h = notes_harness("Shoot row 4", true);
    h.run();
    h.get_by_role_and_label(Role::Button, "Edit").click();
    h.run();
    h.state_mut().editable = false;
    h.run();
    assert!(h.query_by_role(Role::MultilineTextInput).is_none());
    assert_eq!(h.state().mode, NotesMode::Collapsed);
    assert_eq!(h.state().done, 1, "the caller saves what was typed");
}

#[test]
fn text_input_is_a_control_high_and_edits() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(500.0, 100.0))
        .build_ui_state(
            |ui, s: &mut (String, Option<egui::Rect>)| {
                if crate::testutil::themed(ui) {
                    s.1 = Some(super::text_input::text_input(ui, &mut s.0, "Session name").rect);
                }
            },
            (String::new(), None),
        );
    h.run();
    let r = h.state().1.unwrap();
    assert!((r.height() - 30.0).abs() < 0.5, "{r:?}");
    h.get_by_role(Role::TextInput).focus();
    h.step();
    h.get_by_role(Role::TextInput).type_text("Carina");
    h.run();
    assert_eq!(h.state().0, "Carina");
}
