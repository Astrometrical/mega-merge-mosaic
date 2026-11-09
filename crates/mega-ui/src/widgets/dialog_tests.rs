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
    open: bool,
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
                if !s.open {
                    return;
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
            State {
                open: true,
                ..State::default()
            },
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

fn focused(h: &Harness<'_, State>) -> Vec<&'static str> {
    ["Behind", "One", "Two"]
        .into_iter()
        .filter(|l| h.query_by_label(l).is_some_and(|n| n.is_focused()))
        .collect()
}

#[test]
fn input_behind_is_blocked() {
    let mut h = harness();
    h.run();
    h.get_by_label("Behind").click();
    h.run();
    assert_eq!(h.state().behind, 0, "the backdrop swallows the click");
    // The keyboard too: Tab never lands behind, Enter / Space never press it.
    for _ in 0..4 {
        h.key_press(Key::Tab);
        h.run();
        assert!(!focused(&h).contains(&"Behind"));
    }
    h.key_press(Key::Enter);
    h.run();
    h.key_press(Key::Space);
    h.run();
    assert_eq!(h.state().behind, 0, "Enter / Space do not reach behind");
}

#[test]
fn tab_stays_inside() {
    let mut h = harness();
    h.run();
    assert_eq!(focused(&h), ["One"], "first control starts focused");
    let mut seen = Vec::new();
    for _ in 0..4 {
        h.key_press(Key::Tab);
        h.run();
        seen.push(focused(&h));
    }
    assert_eq!(seen, [["Two"], ["One"], ["Two"], ["One"]], "Tab wraps");
    let mut seen = Vec::new();
    for _ in 0..4 {
        h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
        h.run();
        seen.push(focused(&h));
    }
    assert_eq!(
        seen,
        [["Two"], ["One"], ["Two"], ["One"]],
        "Shift+Tab wraps"
    );
}

#[test]
fn focus_left_on_the_opener_moves_into_the_dialog() {
    let mut h = harness();
    h.state_mut().open = false;
    h.run();
    h.get_by_label("Behind").focus();
    h.run();
    assert_eq!(focused(&h), ["Behind"]);
    h.state_mut().open = true;
    h.run();
    assert_eq!(focused(&h), ["One"]);
}

#[test]
fn a_body_without_controls_does_not_repaint_forever() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 700.0))
        .build_ui_state(
            |ui, _: &mut ()| {
                if crate::testutil::themed(ui) {
                    Dialog {
                        title: "Export",
                        width: DIALOG_WIDTH,
                    }
                    .show(ui.ctx(), Id::new("dlg"), |ui| {
                        ui.label("Nothing to focus");
                    });
                }
            },
            (),
        );
    assert!(h.try_run().is_ok(), "the dialog keeps asking for repaints");
}

#[test]
fn footer_tab_order_matches_its_layout() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(600.0, 300.0))
        .build_ui_state(
            |ui, _: &mut ()| {
                if crate::testutil::themed(ui) {
                    footer(
                        ui,
                        &[
                            (ButtonKind::Quiet, "Cancel"),
                            (ButtonKind::Danger, "Overwrite"),
                            (ButtonKind::Primary, "Export"),
                        ],
                    );
                }
            },
            (),
        );
    h.run();
    h.get_by_label("Cancel").focus();
    h.run();
    let xs: Vec<f32> = ["Cancel", "Overwrite", "Export"]
        .iter()
        .map(|l| h.get_by_label(l).rect().min.x)
        .collect();
    assert!(xs[0] < xs[1] && xs[1] < xs[2], "left to right: {xs:?}");
    let mut order = vec!["Cancel"];
    for _ in 0..2 {
        h.key_press(Key::Tab);
        h.run();
        let now = ["Cancel", "Overwrite", "Export"]
            .into_iter()
            .find(|l| h.get_by_label(l).is_focused())
            .expect("a footer button has focus");
        order.push(now);
    }
    assert_eq!(order, ["Cancel", "Overwrite", "Export"]);
}

#[test]
fn text_is_legible_on_the_card() {
    use crate::theme::{contrast_ratio, palette::*};
    for t in [
        TEXT_STRONG,
        TEXT_EMPHASIS,
        TEXT_BODY,
        TEXT_MUTED,
        TEXT_SUBTLE,
    ] {
        assert!(contrast_ratio(t, SURFACE_700) >= 4.5, "{t:?}");
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

#[test]
fn a_disabled_footer_button_says_why_and_does_nothing() {
    use egui_kittest::kittest::NodeT;
    let mut h = Harness::builder()
        .with_size(Vec2::new(600.0, 300.0))
        .build_ui_state(
            |ui, s: &mut Option<usize>| {
                if crate::testutil::themed(ui) {
                    let c = footer_with(
                        ui,
                        &[
                            FooterButton {
                                kind: ButtonKind::Quiet,
                                label: "Cancel",
                                disabled: None,
                            },
                            FooterButton {
                                kind: ButtonKind::Primary,
                                label: "Export",
                                disabled: Some("Nothing is ticked."),
                            },
                        ],
                    );
                    *s = s.or(c);
                }
            },
            None,
        );
    h.run();
    assert!(h.get_by_label("Export").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Cancel").accesskit_node().is_disabled());
    h.get_by_label("Export").click();
    h.run();
    assert_eq!(*h.state(), None, "a disabled button does nothing");
    // Its reason shows on hover.
    let at = h.get_by_label("Export").rect().center();
    h.hover_at(at);
    for _ in 0..60 {
        h.run();
    }
    h.get_by_label("Nothing is ticked.");
    // The enabled one still reports its click.
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(*h.state(), Some(0));
}

#[test]
fn initial_focus_never_goes_to_what_is_drawn_after_it() {
    // The dialog shown first in the frame, the rest of the screen after it
    // (as a window that lets its dialog take Esc first draws them).
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 700.0))
        .build_ui_state(
            |ui, open: &mut bool| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                if *open {
                    Dialog {
                        title: "Export",
                        width: DIALOG_WIDTH,
                    }
                    .show(ui.ctx(), Id::new("dlg"), |ui| {
                        let _ = ui.button("One");
                    });
                }
                let _ = ui.button("After");
            },
            false,
        );
    h.run();
    *h.state_mut() = true;
    h.run();
    assert!(h.get_by_label("One").is_focused(), "focus starts inside");
    assert!(!h.get_by_label("After").is_focused());
}
