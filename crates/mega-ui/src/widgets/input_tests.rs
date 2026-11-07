use egui::Vec2;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::notes::notes_field;
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

fn notes_harness(enabled: bool) -> Harness<'static, String> {
    Harness::builder()
        .with_size(Vec2::new(500.0, 200.0))
        .build_ui_state(
            move |ui, text: &mut String| {
                if crate::testutil::themed(ui) {
                    notes_field(ui, text, "Notes about this session", enabled);
                }
            },
            "Shoot row 4".to_owned(),
        )
}

#[test]
fn notes_field_edits_text() {
    let mut h = notes_harness(true);
    h.run();
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput)
        .type_text(" next moon");
    h.run();
    assert!(h.state().ends_with(" next moon"), "{:?}", h.state());
}

#[test]
fn disabled_notes_field_is_read_only() {
    let mut h = notes_harness(false);
    h.run();
    if let Some(n) = h.query_by_role(Role::MultilineTextInput) {
        n.focus();
    }
    h.step();
    if let Some(n) = h.query_by_role(Role::MultilineTextInput) {
        n.type_text(" next moon");
    }
    h.run();
    assert_eq!(h.state(), "Shoot row 4");
}
