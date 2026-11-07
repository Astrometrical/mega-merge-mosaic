use std::sync::Arc;

use egui::{Color32, ColorImage, Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::viewer::ImageTileSource;

const EPS: f64 = 0.01;

struct State {
    stage: HeroStage,
    now: f64,
}

fn view(label: &str, color: Color32) -> HeroView {
    HeroView {
        label: label.to_string(),
        source: Arc::new(ImageTileSource::new(
            ColorImage::new([640, 480], vec![color; 640 * 480]),
            256,
        )),
    }
}

fn two_views() -> Vec<HeroView> {
    vec![
        view("Ha", Color32::from_rgb(200, 40, 40)),
        view("OIII", Color32::from_rgb(40, 120, 200)),
    ]
}

fn harness(views: Vec<HeroView>, reduced: bool) -> Harness<'static, State> {
    let mut stage = HeroStage::new_synchronous();
    stage.set_reduced_motion(reduced);
    stage.set_views(views, 0, 0.0);
    Harness::builder()
        .with_size(Vec2::new(900.0, 500.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let r = ui.max_rect();
                let image = Rect::from_min_max(pos2(r.left() + r.width() / 3.0, r.top()), r.max);
                s.stage.show(ui, image, s.now);
            },
            State { stage, now: 0.0 },
        )
}

fn frames(h: &mut Harness<'_, State>, now: f64) {
    h.state_mut().now = now;
    h.run_steps(3);
}

fn label(h: &Harness<'_, State>) -> Option<String> {
    h.state().stage.current_label().map(str::to_string)
}

#[test]
fn cycles_after_the_dwell() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert!(!h.state().stage.is_crossfading());
    frames(&mut h, 8.0 + EPS);
    assert!(h.state().stage.is_crossfading());
    frames(&mut h, 10.1);
    assert!(!h.state().stage.is_crossfading());
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

#[test]
fn pause_holds_and_next_advances() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    h.get_by_label("Pause slideshow").click();
    frames(&mut h, 1.0);
    frames(&mut h, 20.0);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert!(h.query_by_label("Resume slideshow").is_some());
    h.get_by_label("Next image").click();
    frames(&mut h, 20.0);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

#[test]
fn reduced_motion_cuts() {
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    let zoom = |h: &Harness<'_, State>| {
        let s = &h.state().stage;
        let v = s.viewers[s.current].as_ref().expect("viewer");
        let (_, t) = v.last_frame().expect("shown");
        v.view().zoom(t)
    };
    let z0 = zoom(&h);
    frames(&mut h, 4.0);
    assert_eq!(zoom(&h), z0);
    frames(&mut h, 8.0 + EPS);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
    assert!(!h.state().stage.is_crossfading());
}

#[test]
fn one_view_never_cycles() {
    let mut h = harness(vec![view("Ha", Color32::RED)], false);
    frames(&mut h, 0.0);
    frames(&mut h, 30.0);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert!(!h.state().stage.is_crossfading());
}

#[test]
fn message_shows_without_views() {
    let mut h = harness(Vec::new(), false);
    h.state_mut()
        .stage
        .set_message(Some("No panels yet — resume to add them".into()));
    frames(&mut h, 0.0);
    assert!(
        h.query_by_label("No panels yet — resume to add them")
            .is_some()
    );
    assert_eq!(label(&h), None);
}

#[test]
fn first_image_reports_drawn() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    assert!(h.state().stage.first_image_drawn());
}

#[test]
fn drift_zooms_in_over_time() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    let zoom = |h: &Harness<'_, State>| {
        let s = &h.state().stage;
        let v = s.viewers[s.current].as_ref().expect("viewer");
        let (_, t) = v.last_frame().expect("shown");
        v.view().zoom(t)
    };
    let z0 = zoom(&h);
    frames(&mut h, 5.0);
    assert!(zoom(&h) > z0 * 1.01);
}
