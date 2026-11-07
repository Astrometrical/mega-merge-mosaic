use egui::{Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::theme::Tone;

#[derive(Default)]
struct State {
    clicked: u32,
    double: u32,
    rect: Option<Rect>,
}

fn harness(pinned: bool) -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(400.0, 300.0))
        .with_step_dt(0.02)
        .build_ui_state(
            move |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let card = SessionCardData {
                    title: "Carina Nebula SHO",
                    status: Some((Tone::Warning, "Re-merge needed")),
                    pinned,
                    dimmed: false,
                    selected: false,
                    thumbnail: None,
                };
                let r = session_card(ui, &card);
                s.clicked += r.clicked() as u32;
                s.double += r.double_clicked() as u32;
                s.rect = Some(r.rect);
            },
            State::default(),
        )
}

fn painted_text(h: &Harness<'static, State>) -> String {
    fn walk(shape: &egui::Shape, out: &mut String) {
        match shape {
            egui::Shape::Text(t) => out.push_str(t.galley.text()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

#[test]
fn card_clicks_and_double_clicks() {
    let mut h = harness(false);
    h.run();
    h.get_by_label("Carina Nebula SHO").click();
    h.run();
    assert_eq!(h.state().clicked, 1);
    assert_eq!(h.state().double, 0);
    let mut h = harness(false);
    h.run();
    h.get_by_label("Carina Nebula SHO").click();
    h.step();
    h.get_by_label("Carina Nebula SHO").click();
    h.step();
    assert_eq!(h.state().double, 1);
}

#[test]
fn card_is_labelled_by_title() {
    let mut h = harness(false);
    h.run();
    h.get_by_label("Carina Nebula SHO");
}

#[test]
fn pinned_card_paints_a_pin() {
    let mut h = harness(true);
    h.run();
    assert!(painted_text(&h).contains(crate::icons::PIN));
    let icon = crate::theme::fonts::icon_family();
    let mut ok = false;
    for c in &h.output().shapes {
        if let egui::Shape::Text(t) = &c.shape {
            for s in &t.galley.job.sections {
                let r = &s.byte_range;
                if t.galley.job.text[r.start.0..r.end.0].contains(crate::icons::PIN) {
                    ok = s.format.font_id.family == icon;
                }
            }
        }
    }
    assert!(ok, "pin drawn in the icon family");
    let mut h = harness(false);
    h.run();
    assert!(!painted_text(&h).contains(crate::icons::PIN));
}

#[test]
fn card_has_fixed_width() {
    let mut h = harness(false);
    h.run();
    assert_eq!(h.state().rect.unwrap().width(), CARD_WIDTH);
}

/// Click point over the status row, near the card's bottom.
fn status_point(h: &Harness<'static, State>) -> egui::Pos2 {
    let r = h.state().rect.unwrap();
    egui::pos2(r.left() + 40.0, r.bottom() - 16.0)
}

fn click_at(h: &mut Harness<'static, State>, pos: egui::Pos2) {
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    h.step();
}

#[test]
fn status_row_does_not_swallow_clicks() {
    let mut h = harness(false);
    h.run();
    let pos = status_point(&h);
    click_at(&mut h, pos);
    h.run();
    assert_eq!(h.state().clicked, 1);
    let mut h = harness(false);
    h.run();
    let pos = status_point(&h);
    click_at(&mut h, pos);
    click_at(&mut h, pos);
    assert_eq!(h.state().double, 1);
}
