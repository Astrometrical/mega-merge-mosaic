use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::icons;
use crate::theme::Tone;

struct State {
    steps: Vec<StripStep>,
    event: Option<StripEvent>,
}

fn step(label: &str, state: StepState, cancellable: bool) -> StripStep {
    StripStep {
        label: label.to_owned(),
        state,
        cancellable,
    }
}

fn four() -> Vec<StripStep> {
    ["Panels", "Checks", "Merge", "Export"]
        .iter()
        .map(|l| step(l, StepState::Todo, false))
        .collect()
}

fn harness(steps: Vec<StripStep>) -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(600.0, 120.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                if let Some(e) = step_strip(ui, &s.steps) {
                    s.event = Some(e);
                }
            },
            State { steps, event: None },
        )
}

#[test]
fn steps_are_buttons_in_order() {
    let mut h = harness(four());
    h.run();
    let mut last = f32::MIN;
    for l in ["Panels", "Checks", "Merge", "Export"] {
        let x = h.get_by_label(l).rect().min.x;
        assert!(x > last, "{l} at {x} after {last}");
        last = x;
    }
}

#[test]
fn clicking_a_step_opens_it() {
    let mut h = harness(four());
    h.run();
    h.get_by_label("Merge").click();
    h.run();
    assert_eq!(h.state().event, Some(StripEvent::Open(2)));
}

#[test]
fn working_step_fills_to_its_fraction() {
    let mut steps = four();
    steps[2] = step(
        "Merge",
        StepState::Working {
            fraction: Some(0.43),
        },
        false,
    );
    let mut h = harness(steps);
    h.run();
    let pill = h.get_by_label("Merge").rect();
    let fill = Tone::Info.tint();
    let found = h.output().shapes.iter().any(|c| match &c.shape {
        egui::Shape::Rect(r) => {
            r.fill == fill
                && pill.expand(0.5).contains_rect(r.rect)
                && (r.rect.width() - 0.43 * pill.width()).abs() <= 1.0
        }
        _ => false,
    });
    assert!(found, "no fill of 0.43 x {} pt", pill.width());
}

#[test]
fn cancel_shows_while_hovered() {
    let mut steps = four();
    steps[2] = step(
        "Merging",
        StepState::Working {
            fraction: Some(0.2),
        },
        true,
    );
    let mut h = harness(steps);
    h.run();
    assert!(h.query_by_label("Cancel Merging").is_none());
    h.get_by_label("Merging").hover();
    h.run();
    h.get_by_label("Cancel Merging").click();
    h.run();
    assert_eq!(h.state().event, Some(StripEvent::Cancel(2)));
}

#[test]
fn done_and_warn_carry_icons() {
    let mut steps = four();
    steps[0] = step("Panels", StepState::Done, false);
    steps[1] = step("Checks", StepState::Warn, false);
    let mut h = harness(steps);
    h.run();
    let painted = |g: char| {
        h.output().shapes.iter().any(|c| match &c.shape {
            egui::Shape::Text(t) => {
                t.galley.text().contains(g)
                    && t.galley.job.sections[0].format.font_id.family
                        == crate::theme::fonts::icon_family()
            }
            _ => false,
        })
    };
    assert!(painted(icons::CHECK));
    assert!(painted(icons::TRIANGLE_ALERT));
}
