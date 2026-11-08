use egui::accesskit::Role;
use egui::{Popup, Shape, TextEdit, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::icons;
use crate::theme::fonts::icon_family;

#[derive(Default)]
struct State {
    typed: String,
    chosen: Option<&'static str>,
}

/// An "Actions" button whose menu holds a "Move to" submenu (Ha, a text
/// field) and Remove.
fn harness() -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(500.0, 300.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let button = ui.button("Actions");
                Popup::menu(&button).show(|ui| {
                    submenu(ui, "Move to", |ui| {
                        if ui.button("Ha").clicked() {
                            s.chosen = Some("Ha");
                            ui.close();
                        }
                        ui.add(TextEdit::singleline(&mut s.typed).hint_text("Name"));
                    });
                    if ui.button("Remove").clicked() {
                        s.chosen = Some("Remove");
                    }
                });
            },
            State::default(),
        )
}

#[test]
fn a_submenu_shows_its_chevron_and_stays_open_for_typing() {
    let mut h = harness();
    h.run();
    h.get_by_label("Actions").click();
    h.run();
    // Read out as its label alone; the chevron is drawn in the icon font.
    let entry = h.get_by_role_and_label(Role::Button, "Move to").rect();
    let chevron = h.output().shapes.iter().find_map(|c| match &c.shape {
        Shape::Text(t) if t.galley.text() == icons::CHEVRON_DOWN.to_string() => Some(t.clone()),
        _ => None,
    });
    let chevron = chevron.expect("a chevron painted");
    assert_eq!(
        chevron.galley.job.sections[0].format.font_id.family,
        icon_family()
    );
    assert!(entry.contains(chevron.pos), "the chevron sits on its entry");
    // It opens beside the menu.
    h.get_by_label("Move to").click();
    h.run();
    h.get_by_label("Ha");
    // A click in its text field leaves it open for typing.
    h.get_by_role(Role::TextInput).click();
    h.run();
    h.get_by_role(Role::TextInput).type_text("Red");
    h.run();
    assert_eq!(h.state().typed, "Red");
    h.get_by_label("Ha");
    // An item that acts closes the whole menu.
    h.get_by_label("Ha").click();
    h.run();
    assert_eq!(h.state().chosen, Some("Ha"));
    assert!(h.query_by_label("Move to").is_none(), "menu closed");
    assert!(h.query_by_label("Remove").is_none(), "menu closed");
}
