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

#[test]
fn long_toast_stays_compact() {
    let title = "Export of the Carina Nebula SHO composite and every group finished \
                 after re-merging the stale tiles left by the two panels added since";
    let detail = "/astro/carina/export/carina-Ha.xisf · /astro/carina/export/carina-OIII.xisf ·                   /astro/carina/export/carina-SII.xisf · /astro/carina/export/carina-SHO.tiff";
    let mut h = Harness::builder()
        .with_size(Vec2::new(720.0, 480.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if crate::testutil::themed(ui) {
                    let now = s.now;
                    s.events.extend(s.toasts.show(ui.ctx(), now));
                }
            },
            State::default(),
        );
    h.run();
    for _ in 0..3 {
        h.state_mut().toasts.push(
            Toast {
                tone: Tone::Success,
                title: title.to_owned(),
                detail: Some(detail.to_owned()),
                action: Some("Show".to_owned()),
            },
            0.0,
        );
    }
    h.run();
    let row = 17.0;
    for node in h.query_all_by_label(title) {
        let r = node.rect();
        assert!(
            r.height() <= 2.0 * row + 2.0,
            "title at most two rows: {r:?}"
        );
        assert!(
            r.left() >= 720.0 - 16.0 - 340.0 - 1.0 && r.right() <= 720.0 - 16.0 + 1.0,
            "{r:?}"
        );
    }
    for node in h.query_all_by_label(detail) {
        let r = node.rect();
        assert!(
            r.height() <= 2.0 * 16.0 + 2.0,
            "detail at most two rows: {r:?}"
        );
    }
    for node in h.query_all_by_label("Show") {
        let r = node.rect();
        assert!(
            r.right() <= 720.0 - 16.0 + 1.0,
            "action stays inside the toast: {r:?}"
        );
    }
    assert_eq!(h.query_all_by_label(title).count(), 3);
}
