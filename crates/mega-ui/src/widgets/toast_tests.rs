use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::theme::Tone;

#[derive(Default)]
struct State {
    toasts: Toasts,
    now: f64,
    events: Vec<ToastEvent>,
}

fn harness() -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(900.0, 700.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if crate::testutil::themed(ui) {
                    let now = s.now;
                    s.events.extend(s.toasts.show(ui.ctx(), now));
                }
            },
            State::default(),
        )
}

fn toast(title: &str, action: Option<&str>) -> Toast {
    Toast {
        tone: Tone::Success,
        title: title.to_owned(),
        detail: Some("4 files".to_owned()),
        action: action.map(str::to_owned),
    }
}

#[test]
fn toast_shows_and_expires() {
    let mut h = harness();
    h.run();
    let id = h
        .state_mut()
        .toasts
        .push(toast("Export finished", None), 0.0);
    h.run();
    h.get_by_label("Export finished");
    h.get_by_label("4 files");
    h.state_mut().now = 6.1;
    h.run();
    assert!(h.query_by_label("Export finished").is_none());
    assert!(h.state().events.contains(&ToastEvent::Dismissed(id)));
}

#[test]
fn action_emits_event_and_dismisses() {
    let mut h = harness();
    h.run();
    let id = h
        .state_mut()
        .toasts
        .push(toast("Export finished", Some("Show")), 0.0);
    h.run();
    h.get_by_label("Show").click();
    h.run();
    assert_eq!(h.state().events, vec![ToastEvent::Action(id)]);
    assert!(h.query_by_label("Export finished").is_none());
}

#[test]
fn burst_keeps_three() {
    let mut h = harness();
    h.run();
    for i in 0..10 {
        h.state_mut()
            .toasts
            .push(toast(&format!("Toast {i}"), None), 0.0);
    }
    h.run();
    let visible: Vec<usize> = (0..10)
        .filter(|i| h.query_by_label(&format!("Toast {i}")).is_some())
        .collect();
    assert_eq!(visible, vec![7, 8, 9]);
    let dismissed = h
        .state()
        .events
        .iter()
        .filter(|e| matches!(e, ToastEvent::Dismissed(_)))
        .count();
    assert_eq!(dismissed, 7);
}

#[test]
fn toast_pushed_after_idle_still_shows() {
    // No frames run while the app idles; a toast pushed afterwards must not
    // be aged by the idle gap.
    let mut h = harness();
    h.run();
    h.state_mut().now = 100.0;
    h.state_mut()
        .toasts
        .push(toast("Export finished", None), 100.0);
    h.state_mut().now = 100.1;
    h.run();
    h.get_by_label("Export finished");
}
