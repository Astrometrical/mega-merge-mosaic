use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::theme::Tone;

struct State {
    inspector: InspectorState,
    badge: bool,
}

fn harness(initial: InspectorState, badge: bool) -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(320.0, 400.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let sections = [
                    Section {
                        id: "a",
                        title: "Alpha",
                        badge: s.badge.then(|| Badge {
                            text: "2".to_owned(),
                            tone: Tone::Warning,
                        }),
                    },
                    Section {
                        id: "b",
                        title: "Beta",
                        badge: None,
                    },
                    Section {
                        id: "c",
                        title: "Gamma",
                        badge: None,
                    },
                ];
                inspector(ui, &mut s.inspector, &sections, |ui, id| {
                    ui.label(format!("body {id}"));
                });
            },
            State {
                inspector: initial,
                badge,
            },
        )
}

fn open(ids: &[&str]) -> InspectorState {
    let mut s = InspectorState::default();
    for id in ids {
        s.toggle(id);
    }
    s
}

#[test]
fn headings_show_in_order_and_only_open_bodies_draw() {
    let mut h = harness(open(&["b"]), false);
    h.run();
    let mut last = f32::MIN;
    for l in ["ALPHA", "BETA", "GAMMA"] {
        let y = h.get_by_label(l).rect().min.y;
        assert!(y > last, "{l} at {y} after {last}");
        last = y;
    }
    h.get_by_label("body b");
    assert!(h.query_by_label("body a").is_none());
    assert!(h.query_by_label("body c").is_none());
}

#[test]
fn clicking_a_heading_toggles_it() {
    let mut h = harness(InspectorState::default(), false);
    h.run();
    assert!(h.query_by_label("body a").is_none());
    h.get_by_label("ALPHA").click();
    h.run();
    h.get_by_label("body a");
    h.get_by_label("ALPHA").click();
    h.run();
    assert!(h.query_by_label("body a").is_none());
}

#[test]
fn folding_leaves_only_the_show_button() {
    let mut h = harness(
        InspectorState {
            folded: true,
            ..Default::default()
        },
        false,
    );
    h.run();
    assert!(h.query_by_label("ALPHA").is_none());
    h.get_by_label("Show inspector").click();
    h.run();
    assert!(!h.state().inspector.folded);
    h.get_by_label("ALPHA");
}

#[test]
fn reveal_unfolds_and_opens() {
    let mut h = harness(
        InspectorState {
            folded: true,
            ..Default::default()
        },
        false,
    );
    h.run();
    h.state_mut().inspector.reveal("c");
    h.run();
    assert!(!h.state().inspector.folded);
    h.get_by_label("body c");
}

#[test]
fn badge_sits_in_its_heading() {
    let mut h = harness(InspectorState::default(), true);
    h.run();
    let heading = h.get_by_label("Alpha").rect();
    let badge = h.get_by_label("2").rect();
    assert!(heading.contains_rect(badge), "{badge:?} not in {heading:?}");
}

#[test]
fn width_follows_fold() {
    let mut s = InspectorState::default();
    assert_eq!(inspector_width(&s), crate::theme::tokens::INSPECTOR_WIDTH);
    s.folded = true;
    assert!(inspector_width(&s) < 60.0);
}
