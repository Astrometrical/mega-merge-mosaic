use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::theme::tokens::DIALOG_WIDTH;

#[derive(Default)]
struct State {
    closes: u32,
    behind: u32,
    one: u32,
}

fn harness() -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(900.0, 700.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                if ui.button("Behind").clicked() {
                    s.behind += 1;
                }
                let r = Dialog {
                    title: "Export",
                    width: DIALOG_WIDTH,
                }
                .show(ui.ctx(), Id::new("dlg"), |ui| {
                    let one = ui.button("One").clicked();
                    let two = ui.button("Two").clicked();
                    one || two
                });
                s.one += u32::from(r.inner);
                s.closes += u32::from(r.close);
            },
            State::default(),
        )
}

fn click_at(h: &Harness<'_, State>, pos: Pos2) {
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
}

#[test]
fn esc_and_backdrop_report_close() {
    let mut h = harness();
    h.run();
    assert_eq!(h.state().closes, 0);
    h.key_press(Key::Escape);
    h.run();
    assert_eq!(h.state().closes, 1, "Esc reports close");
    // A click on the backdrop, well outside the dialog.
    click_at(&h, Pos2::new(850.0, 650.0));
    h.run();
    assert_eq!(h.state().closes, 2, "backdrop click reports close");
    // A click inside the dialog does not.
    h.get_by_label("One").click();
    h.run();
    assert_eq!(h.state().closes, 2);
    assert_eq!(h.state().one, 1);
}

#[test]
fn input_behind_is_blocked() {
    let mut h = harness();
    h.run();
    h.get_by_label("Behind").click();
    h.run();
    assert_eq!(h.state().behind, 0, "the backdrop swallows the click");
}

#[test]
fn tab_stays_inside() {
    let mut h = harness();
    h.run();
    assert!(
        h.get_by_label("One").is_focused(),
        "first control starts focused"
    );
    for _ in 0..6 {
        h.key_press(Key::Tab);
        h.run();
        let inside = h.get_by_label("One").is_focused() || h.get_by_label("Two").is_focused();
        assert!(inside, "Tab left the dialog");
        assert!(!h.get_by_label("Behind").is_focused());
    }
    for _ in 0..6 {
        h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
        h.run();
        let inside = h.get_by_label("One").is_focused() || h.get_by_label("Two").is_focused();
        assert!(inside, "Shift+Tab left the dialog");
    }
}

#[test]
fn footer_reports_the_clicked_button() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(600.0, 300.0))
        .build_ui_state(
            |ui, s: &mut Option<usize>| {
                if crate::testutil::themed(ui) {
                    let c = footer(
                        ui,
                        &[
                            (ButtonKind::Quiet, "Cancel"),
                            (ButtonKind::Primary, "Export"),
                        ],
                    );
                    *s = s.or(c);
                }
            },
            None,
        );
    h.run();
    let cancel = h.get_by_label("Cancel").rect();
    let export = h.get_by_label("Export").rect();
    assert!(export.min.x > cancel.max.x, "primary is rightmost");
    assert!(export.max.x > 550.0, "right-aligned");
    h.get_by_label("Export").click();
    h.run();
    assert_eq!(*h.state(), Some(1));
}
