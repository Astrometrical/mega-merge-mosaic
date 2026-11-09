use egui::accesskit::Toggled;
use egui::{Pos2, Rect, Shape, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::canvas_layers::LABEL_OFFSET;
use crate::theme::palette;

const SCREEN: Vec2 = Vec2::new(800.0, 500.0);
/// The top-left corner of the rect the label names.
const CORNER: Pos2 = Pos2::new(100.0, 120.0);

/// A label over a canvas: one row (name, Compare, state) or, comparing,
/// the controls with the text on a second row.
struct State {
    comparing: bool,
    selected: usize,
    shown: usize,
    text: String,
    clip: Option<Rect>,
    /// What the label reported, frame by frame.
    log: Vec<LabelResponse>,
    /// The canvas beneath had the press, frame by frame.
    canvas_pressed: Vec<bool>,
}

fn harness(comparing: bool) -> Harness<'static, State> {
    Harness::builder()
        .with_size(SCREEN)
        .with_step_dt(0.01)
        .with_max_steps(200)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                // The canvas: senses clicks and drags all over.
                let (rect, canvas) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
                let toggle = [LabelControl::Toggle {
                    options: ["A", "B"],
                    accessible: ["Show A of Core", "Show B of Core"],
                    selected: s.selected,
                    shown: s.shown,
                    tooltip: None,
                }];
                let compare_controls = [
                    toggle[0],
                    LabelControl::Button {
                        text: "Use A",
                        accessible: "Use A for Core",
                        tooltip: None,
                        primary: false,
                    },
                    LabelControl::Button {
                        text: "Done",
                        accessible: "Done comparing Core",
                        tooltip: None,
                        primary: true,
                    },
                ];
                let compare = [LabelControl::Button {
                    text: "Compare",
                    accessible: "Compare Core",
                    tooltip: Some("Freeze this preview as A"),
                    primary: false,
                }];
                let label = CanvasLabel {
                    corner: CORNER,
                    clip: s.clip.unwrap_or(rect),
                    name: "Core",
                    color: palette::ACCENT_400,
                    controls: if s.comparing {
                        &compare_controls
                    } else {
                        &compare
                    },
                    text: &s.text,
                    text_below: s.comparing,
                    accessible: "Core on the canvas",
                    description: "Showing B",
                };
                let r = canvas_label(ui, "core-label", &label);
                s.log.push(r);
                s.canvas_pressed.push(canvas.is_pointer_button_down_on());
            },
            State {
                comparing,
                selected: 1,
                shown: 1,
                text: if comparing {
                    "B · feather 40 px · flatten 2".into()
                } else {
                    "up to date".into()
                },
                clip: None,
                log: Vec::new(),
                canvas_pressed: Vec::new(),
            },
        )
}

/// Where text `text` was painted last frame: its rect.
fn painted(h: &Harness<'_, State>, text: &str) -> Option<Rect> {
    h.output().shapes.iter().find_map(|cs| match &cs.shape {
        Shape::Text(t) if t.galley.text() == text => {
            Some(Rect::from_min_size(t.pos, t.galley.size()))
        }
        _ => None,
    })
}

fn rect_of(h: &Harness<'_, State>, label: &str) -> Rect {
    h.get_by_label(label).rect()
}

fn last(h: &Harness<'_, State>) -> LabelResponse {
    *h.state().log.last().expect("a frame")
}

fn press(h: &mut Harness<'_, State>, at: Pos2) {
    h.hover_at(at);
    h.drag_at(at);
    h.step();
}

fn release(h: &mut Harness<'_, State>, at: Pos2) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

#[test]
fn one_row_names_then_controls_then_state() {
    let mut h = harness(false);
    h.run_steps(3);
    // The name sits where an overlay's painted label does.
    let name = painted(&h, "Core").expect("the name painted");
    assert_eq!(name.min, CORNER + LABEL_OFFSET);
    // Compare after it on its row, then the state on the same row.
    let compare = rect_of(&h, "Compare Core");
    assert!(compare.left() > name.right(), "{compare:?} {name:?}");
    assert!((compare.center().y - name.center().y).abs() < 1.0);
    let state = painted(&h, "up to date").expect("the state painted");
    assert!(state.left() > compare.right());
    assert!((state.center().y - name.center().y).abs() < 1.0);
    // The label is described as the caller says.
    let node = h.get_by_label("Core on the canvas");
    assert_eq!(
        node.accesskit_node().description().as_deref(),
        Some("Showing B")
    );
}

#[test]
fn a_second_row_holds_the_text_and_the_controls_never_move() {
    let mut h = harness(true);
    h.run_steps(3);
    let name = painted(&h, "Core").expect("the name painted");
    assert_eq!(name.min, CORNER + LABEL_OFFSET);
    let controls = [
        "Show A of Core",
        "Show B of Core",
        "Use A for Core",
        "Done comparing Core",
    ];
    let before: Vec<Rect> = controls.iter().map(|c| rect_of(&h, c)).collect();
    // A | B, Use A, Done in order on the name's row.
    let mut x = name.right();
    for r in &before {
        assert!(r.left() >= x - 0.5, "{before:?}");
        assert!((r.center().y - name.center().y).abs() < 1.0);
        x = r.right();
    }
    // The text on a row of its own, under them, from the name's left.
    let text = painted(&h, "B · feather 40 px · flatten 2").expect("the text");
    let bottom = before
        .iter()
        .map(|r| r.bottom())
        .fold(name.bottom(), f32::max);
    assert!(text.top() >= bottom, "{text:?} under {bottom}");
    assert!((text.left() - name.left()).abs() < 0.5);
    // Other text, longer or shorter: nothing on the first row moves.
    for t in [
        "B · merging…",
        "A · feather 1000.5 px · flatten off and more",
    ] {
        h.state_mut().text = t.into();
        h.run_steps(2);
        assert!(painted(&h, t).is_some());
        let after: Vec<Rect> = controls.iter().map(|c| rect_of(&h, c)).collect();
        assert_eq!(after, before, "{t}");
    }
}

#[test]
fn clicks_and_holds_are_reported() {
    let mut h = harness(true);
    h.run_steps(3);
    // A click on Use A.
    h.get_by_label("Use A for Core").click();
    h.step();
    assert_eq!(last(&h).clicked, Some((1, 0)));
    h.step();
    assert_eq!(last(&h).clicked, None);
    // Pressing and holding B (the selected option) is held while it is; a
    // short one is a click on it as well when let go.
    let b = rect_of(&h, "Show B of Core").center();
    press(&mut h, b);
    assert_eq!(last(&h).held, Some((0, 1)));
    h.run_steps(5);
    assert_eq!(last(&h).held, Some((0, 1)));
    release(&mut h, b);
    assert_eq!(last(&h).held, None);
    assert_eq!(last(&h).clicked, Some((0, 1)));
    // Held however long, wherever the pointer goes meanwhile.
    press(&mut h, b);
    h.run_steps(100);
    assert_eq!(last(&h).held, Some((0, 1)));
    let away = b + Vec2::new(0.0, 200.0);
    h.hover_at(away);
    h.step();
    assert_eq!(last(&h).held, Some((0, 1)));
    release(&mut h, away);
    assert_eq!(last(&h).held, None);
    assert_eq!(last(&h).clicked, None);
    // A click on A: the other option.
    h.get_by_label("Show A of Core").click();
    h.step();
    assert_eq!(last(&h).clicked, Some((0, 0)));
    // No press anywhere: nothing held.
    h.run_steps(2);
    assert_eq!(last(&h).held, None);
}

#[test]
fn the_toggle_says_what_is_selected_and_underlines_a_peek() {
    let mut h = harness(true);
    h.run_steps(3);
    let toggled =
        |h: &Harness<'_, State>, label: &str| h.get_by_label(label).accesskit_node().toggled();
    assert_eq!(toggled(&h, "Show B of Core"), Some(Toggled::True));
    assert_eq!(toggled(&h, "Show A of Core"), Some(Toggled::False));
    // An underline: a strong-text bar along the bottom of an option.
    let underlined = |h: &Harness<'_, State>, label: &str| {
        let r = h.get_by_label(label).rect();
        h.output().shapes.iter().any(|cs| match &cs.shape {
            Shape::Rect(s) => {
                s.fill == palette::TEXT_STRONG
                    && r.contains_rect(s.rect)
                    && s.rect.height() <= 3.0
                    && (s.rect.bottom() - r.bottom()).abs() < 1.5
            }
            _ => false,
        })
    };
    // Showing what is selected: no underline.
    assert!(!underlined(&h, "Show A of Core") && !underlined(&h, "Show B of Core"));
    // Showing A while B stays selected (a peek): A is underlined, B still
    // reads as the one selected.
    h.state_mut().shown = 0;
    h.run_steps(2);
    assert!(underlined(&h, "Show A of Core"));
    assert!(!underlined(&h, "Show B of Core"));
    assert_eq!(toggled(&h, "Show B of Core"), Some(Toggled::True));
    assert_eq!(toggled(&h, "Show A of Core"), Some(Toggled::False));
}

#[test]
fn the_label_takes_presses_from_the_canvas_beneath() {
    let mut h = harness(true);
    h.run_steps(3);
    let canvas_pressed = |h: &Harness<'_, State>| *h.state().canvas_pressed.last().unwrap();
    // On a control, or on the label between controls: not the canvas's.
    let done = rect_of(&h, "Done comparing Core");
    let name = painted(&h, "Core").unwrap();
    for at in [done.center(), name.center()] {
        press(&mut h, at);
        assert!(!canvas_pressed(&h), "{at:?}");
        release(&mut h, at);
    }
    // Off the label: the canvas's.
    let away = Pos2::new(600.0, 400.0);
    press(&mut h, away);
    assert!(canvas_pressed(&h));
    assert_eq!(last(&h).held, None);
    release(&mut h, away);
}

#[test]
fn nothing_outside_the_clip_is_drawn_or_sensed() {
    let mut h = harness(true);
    h.run_steps(3);
    let done = rect_of(&h, "Done comparing Core");
    // Clip just past Use A: Done is outside.
    let use_a = rect_of(&h, "Use A for Core");
    let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(use_a.right() + 1.0, SCREEN.y));
    h.state_mut().clip = Some(clip);
    h.run_steps(2);
    // Whatever is painted where Done was (its fill, border and text) is
    // clipped away.
    for cs in &h.output().shapes {
        let bounds = cs.shape.visual_bounding_rect();
        if done.expand(1.0).contains_rect(bounds) {
            let shown = cs.clip_rect.intersect(bounds);
            assert!(
                shown.width() <= 0.0 || shown.height() <= 0.0,
                "{bounds:?} shows through {:?}",
                cs.clip_rect
            );
        }
    }
    h.hover_at(done.center());
    h.drag_at(done.center());
    h.step();
    release(&mut h, done.center());
    assert!(
        h.state()
            .log
            .iter()
            .rev()
            .take(3)
            .all(|r| r.clicked.is_none()),
        "a control out of the clip was clicked"
    );
}
