//! The session window, driven as the shell drives it: the job runner's
//! events, the window's every-frame hooks, the pipeline when a test runs
//! it, then the window.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use mega_ui::widgets::{StepState, StripStep};
use mmm_core::checks::{Check, CheckCode, Severity};
use mmm_core::mosaic::{MosaicConfig, SINGLE_GROUP};

use super::steps::steps;
use super::*;
use crate::camera::Camera;
use crate::checks::{ChecksEngine, ChecksReport};
use crate::compositions::{Composition, ViewKind, load_compositions, save_compositions};
use crate::display_stretch::DisplayStretch;
use crate::jobs::JobRunner;
use crate::jobs::tests::{analyzed, drain};
use crate::panels_tab::FilesPicker;
use crate::pipeline::{Pipeline, set_auto_analyze};
use crate::rois::{Roi, save_rois};
use crate::session::{OpenSession, any_merged, classify_results, create_blocking, notes};
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::{two_group_mono_session, two_group_session};

/// A file dialog that records being asked and picks nothing.
#[derive(Default)]
struct AskedFiles(AtomicBool);

impl AskedFiles {
    fn asked(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl FilesPicker for AskedFiles {
    fn pick_files(&self) -> Option<Vec<PathBuf>> {
        self.0.store(true, Ordering::SeqCst);
        None
    }
}

/// The window and what the shell hands it each frame.
struct State {
    w: SessionWindow,
    session: OpenSession,
    jobs: JobRunner,
    pipeline: Pipeline,
    checks: ChecksEngine,
    /// The automatic pipeline runs (previews, then analysis when needed).
    run_pipeline: bool,
    /// The checks the window asked the shell to accept, in order.
    accepted: Vec<Check>,
    /// The panel list's file dialog.
    files: Arc<AskedFiles>,
}

impl State {
    fn new(session: OpenSession, jobs: JobRunner) -> Self {
        let files = Arc::new(AskedFiles::default());
        State {
            w: SessionWindow::new(files.clone()),
            session,
            jobs,
            pipeline: Pipeline::new(),
            checks: ChecksEngine::new(),
            run_pipeline: false,
            accepted: Vec::new(),
            files,
        }
    }
}

fn harness(state: State) -> Harness<'static, State> {
    harness_sized(state, egui::vec2(1200.0, 800.0))
}

/// [`harness`] in a window of `size`.
fn harness_sized(state: State, size: egui::Vec2) -> Harness<'static, State> {
    Harness::builder()
        .with_size(size)
        .with_step_dt(0.01)
        .with_max_steps(64)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let ctx = ui.ctx().clone();
                let events = s.jobs.poll(&mut s.session);
                for e in &events {
                    s.w.on_event(&mut s.session, &s.jobs, e);
                }
                s.w.poll_panels(&ctx, &mut s.session, &mut s.jobs);
                s.w.poll_settings(&ctx, &mut s.session, &mut s.jobs);
                if s.run_pipeline {
                    let editing = s.w.settings_editing(&s.session);
                    s.pipeline.poll(
                        &ctx,
                        Instant::now(),
                        &mut s.session,
                        &mut s.jobs,
                        &events,
                        editing,
                    );
                    if let Some(id) = s.pipeline.take_started_analysis() {
                        s.w.track_analysis(id);
                    }
                }
                s.w.poll(&ctx, &mut s.session, &mut s.jobs);
                let out =
                    s.w.ui(ui, &mut s.session, &mut s.jobs, &mut s.pipeline, &s.checks);
                s.accepted.extend(out.accept);
            },
            state,
        )
}

fn runner() -> JobRunner {
    JobRunner::new(egui::Context::default())
}

/// An analysed two-group session with both groups merged (and their
/// results classified, as opening it would).
fn merged(tag: &str) -> (PathBuf, OpenSession, JobRunner) {
    let (root, mut s, mut jobs) = analyzed(tag);
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    let _ = drain(&mut jobs, &mut s);
    s.merged = any_merged(&s);
    assert!(s.merged);
    s.results = classify_results(&s);
    (root, s, jobs)
}

/// A new, empty session in `root`.
fn empty(root: &std::path::Path) -> OpenSession {
    create_blocking(&root.join("e.mmm-session")).unwrap()
}

/// One ROI, "ROI 1", whose label the mosaic paints at its top-left corner.
fn add_roi(s: &mut OpenSession) {
    let roi = Roi {
        name: "ROI 1".into(),
        note: String::new(),
        sky: None,
        canvas: [100, 100, 200, 160],
    };
    save_rois(&mut s.mosaic.config.ui, &[roi]);
}

/// Step until `done` holds, stopping at the frame that made it so.
fn step_to(h: &mut Harness<'_, State>, done: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(h.state()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the window"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The job labels running or queued, in order.
fn labels(jobs: &JobRunner) -> Vec<String> {
    jobs.views().into_iter().map(|v| v.label).collect()
}

/// The **+** chip (the canvas's zoom-in button is a "+" too, but no
/// toggle).
fn plus_chip<'h>(h: &'h Harness<'static, State>) -> Option<egui_kittest::Node<'h>> {
    h.query(By::new().label("+").predicate(|n| n.toggled().is_some()))
}

fn disabled(h: &Harness<'static, State>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

fn assert_camera(got: Option<Camera>, want: Camera) {
    let got = got.expect("a camera");
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    assert!(
        near(got.centre.0, want.centre.0)
            && near(got.centre.1, want.centre.1)
            && near(got.zoom, want.zoom),
        "{got:?} != {want:?}"
    );
}

/// Where `cam` puts canvas point `p` in a canvas drawn in `rect`.
fn screen(rect: egui::Rect, cam: Camera, p: (f64, f64)) -> egui::Pos2 {
    rect.center()
        + egui::vec2(
            ((p.0 - cam.centre.0) * cam.zoom) as f32,
            ((p.1 - cam.centre.1) * cam.zoom) as f32,
        )
}

/// The first text `text` painted inside `rect` in the last frame: its
/// position and size.
fn painted(
    h: &Harness<'_, State>,
    rect: egui::Rect,
    text: &str,
) -> Option<(egui::Pos2, egui::Vec2)> {
    h.output().shapes.iter().find_map(|cs| match &cs.shape {
        egui::Shape::Text(t) if t.galley.text() == text && rect.contains(t.pos) => {
            Some((t.pos, t.galley.size()))
        }
        _ => None,
    })
}

/// Asserts the last frame the mosaic painted put each ROI's label where
/// `cam` puts the ROI's top-left corner: the painted frame itself.
fn assert_mosaic_painted_with(h: &Harness<'_, State>, cam: Camera) {
    let (rect, _) = h.state().w.mosaic().viewer_frame().expect("a viewer");
    let mut checked = 0;
    for r in h.state().w.mosaic().rois() {
        // The viewer writes an overlay's label just inside its corner.
        let corner = (r.canvas[0] as f64, r.canvas[1] as f64);
        let want = screen(rect, cam, corner) + egui::vec2(4.0, 2.0);
        let (got, _) = painted(h, rect, &r.name).expect("ROI label painted");
        assert!(
            (got - want).length() < 1.0,
            "{} painted at {got:?}; the camera puts it at {want:?}",
            r.name
        );
        checked += 1;
    }
    assert!(checked > 0, "no ROI painted");
}

/// Asserts the last frame the plan painted put each visible panel's number
/// where `cam` puts the panel's centre.
fn assert_plan_painted_with(h: &Harness<'_, State>, cam: Camera) {
    let (rect, _) = h.state().w.plan().canvas_frame().expect("the plan drawn");
    let mut checked = 0;
    for it in h.state().w.plan().items() {
        let r = egui::Rect::from_two_pos(
            screen(rect, cam, (it.rect[0], it.rect[1])),
            screen(rect, cam, (it.rect[2], it.rect[3])),
        );
        if !rect.contains(r.center()) {
            continue;
        }
        let (pos, size) = painted(h, rect, &it.label).expect("panel number painted");
        let got = pos + size / 2.0;
        assert!(
            (got - r.center()).length() < 0.5,
            "panel {} painted at {got:?}; the camera puts it at {:?}",
            it.label,
            r.center()
        );
        checked += 1;
    }
    assert!(checked > 0, "no panel painted");
}

/// Zoom the plan one wheel notch in about canvas point `p`, and let the
/// animation end.
fn zoom_plan_about(h: &mut Harness<'static, State>, p: (f64, f64)) {
    let at = h.state().w.plan().to_screen(p).expect("the plan drawn");
    h.hover_at(at);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 1.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(150);
}

/// Drag the canvas by `delta` from screen point `from`, and let the
/// animation end.
fn drag(h: &mut Harness<'static, State>, from: egui::Pos2, delta: egui::Vec2) {
    h.hover_at(from);
    h.drag_at(from);
    h.step();
    for i in 1..=4 {
        h.hover_at(from + delta * (i as f32 / 4.0));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: from + delta,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(150);
}

#[test]
fn plan_shows_until_something_is_merged() {
    let (root, s, jobs) = analyzed("window-plan-first");
    assert!(!s.merged);
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.plan().canvas_frame().is_some());
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    assert!(disabled(&h, "Mosaic"), "nothing merged yet");
    assert!(!disabled(&h, "Plan"));
    h.get_by_label("Mosaic").click();
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mosaic_shows_by_default_once_merged() {
    // Ha merged, OIII not: something is merged.
    let (root, mut s, mut jobs) = analyzed("window-mosaic-first");
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    s.merged = any_merged(&s);
    assert!(s.merged);
    let mut h = harness(State::new(s, jobs));
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert!(!disabled(&h, "Mosaic"));
    // The merged result is drawn.
    step_until(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    assert!(
        h.state().w.plan().canvas_frame().is_none(),
        "plan not drawn"
    );
    // A group not merged yet says so in the canvas.
    h.get_by_label("OIII · 4").click();
    step_until(&mut h, |st| st.w.mosaic().absent().is_some());
    h.get_by_label("Not blended yet.");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_switches_to_the_mosaic() {
    let (root, s, jobs) = analyzed("window-merge");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    assert!(
        h.query_by_label("Analyze").is_none(),
        "analysis is automatic"
    );
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert_eq!(labels(&h.state().jobs), ["Blend Ha", "Blend OIII"]);
    gate.open();
    // Merged once the merges finish: Mosaic can be chosen from then on.
    step_until(&mut h, |st| !st.jobs.busy() && st.session.merged);
    h.run_steps(2);
    assert!(!disabled(&h, "Mosaic"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mosaic_can_be_shown_while_the_first_merge_runs() {
    let (root, s, jobs) = analyzed("window-first-merge");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    assert!(disabled(&h, "Mosaic"), "nothing merged yet");
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    h.run_steps(2);
    assert!(!h.state().session.merged, "the merge has not finished");
    // Back to the plan mid-merge, then to the mosaic to watch it fill in.
    h.get_by_label("Plan").click();
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    assert!(!disabled(&h, "Mosaic"), "a merge has started");
    h.get_by_label("Mosaic").click();
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert!(h.state().jobs.blending());
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

/// Three groups (Ha, OIII and SII by hand) and two composites, in the
/// analysed `s`.
fn three_groups_two_composites(s: &mut OpenSession) {
    for p in &mut s.mosaic.config.panels[6..] {
        p.manual_group = Some("SII".into());
    }
    s.groups = s.mosaic.groups().ok();
    let comps = [Composition::new("SHO"), Composition::new("HOO natural")];
    save_compositions(&mut s.mosaic.config.ui, &comps);
}

/// The view bar's controls in a canvas `canvas_w` wide: Plan / Mosaic, a
/// chip per group and composite, +, and the four toggles (the only toggle
/// buttons in the window), each with its rect. Asserts none overlaps
/// another, the canvas or the inspector.
fn bar_controls(h: &Harness<'_, State>, canvas_w: f32) -> Vec<(String, egui::Rect)> {
    let controls: Vec<(String, egui::Rect)> = h
        .query_all(By::new().predicate(|n| n.toggled().is_some()))
        .map(|n| (n.accesskit_node().label().unwrap_or_default(), n.rect()))
        .collect();
    let (canvas, _) = h.state().w.plan().canvas_frame().expect("the plan drawn");
    for (label, r) in &controls {
        assert!(
            r.min.x >= -0.5 && r.max.x <= canvas_w + 0.5 && r.min.y >= -0.5,
            "{label} at {r:?} is not inside the canvas area"
        );
        assert!(
            r.max.y <= canvas.min.y + 0.5,
            "{label} at {r:?} is over the canvas {canvas:?}"
        );
    }
    for (i, (a, ra)) in controls.iter().enumerate() {
        for (b, rb) in &controls[i + 1..] {
            assert!(
                !ra.shrink(0.5).intersects(rb.shrink(0.5)),
                "{a} {ra:?} overlaps {b} {rb:?}"
            );
        }
    }
    controls
}

#[test]
fn the_view_bar_wraps_without_overlap() {
    for canvas_w in [700.0, 1400.0] {
        let (root, mut s, jobs) = analyzed("window-narrow-bar");
        three_groups_two_composites(&mut s);
        let size = egui::vec2(canvas_w + mega_ui::theme::tokens::INSPECTOR_WIDTH, 800.0);
        let mut h = harness_sized(State::new(s, jobs), size);
        step_until(&mut h, |st| st.w.plan().canvas_frame().is_some());
        h.run_steps(3);
        h.get_by_label("SII · 2");
        let controls = bar_controls(&h, canvas_w);
        assert_eq!(controls.len(), 12, "{controls:?}");
        // Narrow: the toggles on a row of their own, below the choices;
        // wide: one row.
        let top = |label: &str| {
            let (_, r) = controls.iter().find(|(l, _)| l == label).unwrap();
            r.center().y
        };
        let one_row = (top("Strong") - top("Plan")).abs() < 2.0;
        assert_eq!(one_row, canvas_w > 1000.0, "canvas {canvas_w}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn plan_and_mosaic_share_the_camera() {
    let (root, mut s, jobs) = merged("window-camera");
    add_roi(&mut s);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    // The merged result is opened behind the plan.
    step_until(&mut h, |st| {
        st.w.plan().canvas_frame().is_some() && st.w.mosaic().source().is_some()
    });
    // The plan zoomed in and panned...
    let fit = h.state().w.plan().camera().expect("the plan's camera");
    zoom_plan_about(&mut h, (150.0, 130.0));
    let (rect, _) = h.state().w.plan().canvas_frame().unwrap();
    drag(&mut h, rect.center(), egui::vec2(40.0, -30.0));
    let cam = h.state().w.plan().camera().expect("the plan's camera");
    assert!(cam.zoom > fit.zoom * 1.1 && cam.centre != fit.centre);
    // ...the mosaic shows the same.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Mosaic && st.w.mosaic().viewer_frame().is_some()
    });
    assert_camera(h.state().w.mosaic().camera(), cam);
    assert_mosaic_painted_with(&h, cam);
    // The mosaic panned: back on the plan, the plan shows the same.
    let (rect, _) = h.state().w.mosaic().viewer_frame().unwrap();
    drag(&mut h, rect.center(), egui::vec2(-60.0, 20.0));
    let before = cam;
    let cam = h.state().w.mosaic().camera().expect("the mosaic's camera");
    assert!(cam.centre != before.centre, "the mosaic panned");
    h.get_by_label("Plan").click();
    step_to(&mut h, |st| st.w.mode() == CanvasMode::Plan);
    h.step();
    assert_camera(h.state().w.plan().camera(), cam);
    assert_plan_painted_with(&h, cam);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn camera_survives_the_mosaic_viewer_being_rebuilt() {
    let (root, mut s, jobs) = analyzed("window-camera-rebuild");
    add_roi(&mut s);
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.plan().canvas_frame().is_some()
            && st
                .w
                .mosaic()
                .merge_block_reason(&st.session, &st.jobs)
                .is_none()
    });
    let fit = h.state().w.plan().camera().expect("the plan's camera");
    zoom_plan_about(&mut h, (150.0, 130.0));
    let cam = h.state().w.plan().camera().expect("the plan's camera");
    assert!(cam.zoom > fit.zoom * 1.1);
    // Merging: nothing to show until the merge has made Ha's result,
    // then a viewer is built for it, its first painted frame through the
    // plan's camera.
    let stepper = h.state_mut().jobs.hold_progress();
    {
        let st = h.state_mut();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    assert_mosaic_painted_with(&h, cam);
    assert_camera(h.state().w.mosaic().camera(), cam);
    stepper.open();
    step_until(&mut h, |st| !st.jobs.busy() && st.w.mosaic().is_settled());
    assert_mosaic_painted_with(&h, cam);
    // Merging the shown group again leaves the camera alone.
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    {
        let st = h.state_mut();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    assert!(h.state().jobs.blend_pending("Ha"));
    step_until(&mut h, |st| !st.jobs.busy() && st.w.mosaic().is_settled());
    assert_mosaic_painted_with(&h, cam);
    assert_camera(h.state().w.mosaic().camera(), cam);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn drawing_an_roi_shows_the_mosaic() {
    let (root, s, jobs) = merged("window-draw-roi");
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    state.w.inspector_mut().reveal("rois");
    let mut h = harness(state);
    step_until(&mut h, |st| st.w.mosaic().source().is_some());
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    h.get_by_label("+ new ROI").click();
    h.run_steps(2);
    assert!(h.state().w.mosaic().drawing());
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn chips_pick_the_view_in_both_modes() {
    let (root, s, jobs) = merged("window-chips");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.mosaic().view().is_some());
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    let group = |g: &str| Some(ViewKind::Group(g.into()));
    h.get_by_label("OIII · 4").click();
    h.run_steps(2);
    let st = h.state();
    assert_eq!(st.w.view(), group("OIII").as_ref());
    assert_eq!(st.w.plan().view(), group("OIII").as_ref());
    assert_eq!(st.w.mosaic().view(), group("OIII").as_ref());
    h.get_by_label("Plan").click();
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    h.get_by_label("Ha · 4").click();
    h.run_steps(2);
    let st = h.state();
    assert_eq!(st.w.view(), group("Ha").as_ref());
    assert_eq!(st.w.plan().view(), group("Ha").as_ref());
    assert_eq!(st.w.mosaic().view(), group("Ha").as_ref());
    // A colour view: Linked can be chosen.
    assert!(!disabled(&h, "Linked"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_chips_for_a_single_group() {
    let root = temp_dir("window-single-group");
    let mut s = two_group_session(&root);
    s.mosaic.config.grouping.enabled = false;
    s.groups = s.mosaic.groups().ok();
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    assert!(plus_chip(&h).is_none());
    let chip = By::new()
        .label_contains(" · ")
        .predicate(|n| n.toggled().is_some());
    assert!(h.query(chip).is_none(), "no group chips");
    assert_eq!(
        h.state().w.view(),
        Some(&ViewKind::Group(SINGLE_GROUP.into()))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plus_adds_a_composite_and_opens_composites() {
    let root = temp_dir("window-plus");
    let s = two_group_session(&root);
    let dir = s.mosaic.dir.clone();
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    assert!(!h.state().w.inspector().is_open("composites"));
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(2);
    let st = h.state();
    let list = load_compositions(&st.session.mosaic.config.ui);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "Composite 1");
    let comp = Some(ViewKind::Composition(0));
    assert_eq!(st.w.view(), comp.as_ref());
    assert_eq!(st.w.plan().view(), comp.as_ref());
    assert_eq!(st.w.mosaic().view(), comp.as_ref());
    assert!(st.w.inspector().is_open("composites"));
    // Its chip, and its editor in the Composites section.
    h.get_by_label("Composite 1");
    h.get_by_label("Delete composite");
    // Saved.
    step_until(&mut h, |_| {
        MosaicConfig::load(&dir).is_ok_and(|c| load_compositions(&c.ui).len() == 1)
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deleting_a_composite_keeps_the_view() {
    let root = temp_dir("window-delete-composite");
    let mut s = two_group_session(&root);
    let mut a = Composition::new("A");
    a.channels[0] = Some(("Ha".into(), 1.0));
    let mut b = Composition::new("B");
    b.channels[0] = Some(("OIII".into(), 1.0));
    save_compositions(&mut s.mosaic.config.ui, &[a, b]);
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    h.state_mut().w.select_view(ViewKind::Composition(1));
    h.state_mut().w.inspector_mut().reveal("composites");
    h.run_steps(2);
    // A goes: B, shown, moves up the list and stays shown.
    h.get_all_by_label("Delete composite")
        .next()
        .expect("A's delete")
        .click();
    h.run_steps(2);
    let st = h.state();
    let names: Vec<String> = load_compositions(&st.session.mosaic.config.ui)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["B"]);
    let b = Some(ViewKind::Composition(0));
    assert_eq!(st.w.view(), b.as_ref());
    assert_eq!(st.w.plan().view(), b.as_ref());
    assert_eq!(st.w.mosaic().view(), b.as_ref());
    // B goes too: the first group shows.
    h.get_by_label("Delete composite").click();
    h.run_steps(2);
    let ha = Some(ViewKind::Group("Ha".into()));
    assert_eq!(h.state().w.view(), ha.as_ref());
    assert_eq!(h.state().w.plan().view(), ha.as_ref());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn linked_is_disabled_for_a_mono_view() {
    let root = temp_dir("window-mono");
    let mut s = two_group_mono_session(&root);
    set_auto_analyze(&mut s, false).unwrap();
    let mut state = State::new(s, runner());
    // The previews say how many channels the panels have.
    state.run_pipeline = true;
    let mut h = harness(state);
    step_until(&mut h, |st| st.pipeline.previews_complete());
    h.run_steps(2);
    assert!(disabled(&h, "Linked"), "one channel: nothing to link");
    assert!(!disabled(&h, "Strong"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn inspector_sections_are_in_order() {
    let root = temp_dir("window-sections");
    let mut h = harness(State::new(two_group_session(&root), runner()));
    h.run_steps(3);
    let mut last = f32::MIN;
    for l in [
        "PROBLEMS",
        "PANELS",
        "ROIS",
        "MERGE SETTINGS",
        "COMPOSITES",
        "NOTES",
    ] {
        let y = h.get_by_label(l).rect().min.y;
        assert!(y > last, "{l} at {y} after {last}");
        last = y;
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn check(code: CheckCode, severity: Severity, panels: Vec<PathBuf>, message: &str) -> Check {
    Check {
        code,
        severity,
        panels,
        groups: Vec::new(),
        message: message.into(),
        remedy: None,
    }
}

#[test]
fn problems_accept_a_check() {
    let (root, s, jobs) = analyzed("window-accept");
    let first = s.mosaic.config.panels[0].path.clone();
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![
            check(
                CheckCode::FileChanged,
                Severity::Problem,
                vec![first],
                "First panel changed",
            ),
            check(
                CheckCode::GroupCountMismatch,
                Severity::Note,
                Vec::new(),
                "Groups differ",
            ),
        ],
        errors: Vec::new(),
    });
    let mut h = harness(state);
    h.run_steps(3);
    // Problems is open: the problem and its count, not the note.
    h.get_by_label("First panel changed");
    h.get_by_label("1");
    assert!(h.query_by_label("Groups differ").is_none());
    h.get_by_label("Accept").click();
    h.run_steps(2);
    let accepted: Vec<&str> = h
        .state()
        .accepted
        .iter()
        .map(|c| c.message.as_str())
        .collect();
    assert_eq!(accepted, ["First panel changed"]);
    // Every check, notes included.
    h.get_by_label("See all checks ›").click();
    h.run_steps(2);
    h.get_by_label("Groups differ");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn notes_section_saves_notes() {
    let root = temp_dir("window-notes");
    let s = two_group_session(&root);
    let dir = s.mosaic.dir.clone();
    let mut h = harness(State::new(s, runner()));
    h.run_steps(2);
    h.get_by_label("Notes").click();
    h.run_steps(2);
    h.get_by_label("Add notes").click();
    h.run_steps(2);
    let field = h.get_by_role(Role::MultilineTextInput);
    field.focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput)
        .type_text("Shoot row 4");
    h.run_steps(2);
    assert_eq!(notes(&h.state().session.mosaic.config.ui), "Shoot row 4");
    // In mosaic.json (the start screen reads them there), debounced.
    step_until(&mut h, |_| {
        MosaicConfig::load(&dir).is_ok_and(|c| notes(&c.ui) == "Shoot row 4")
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_selected_panel_shows_its_details_and_checks() {
    let (root, s, jobs) = analyzed("window-select");
    let first = s.mosaic.config.panels[0].path.clone();
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![
            check(
                CheckCode::FileChanged,
                Severity::Problem,
                vec![first.clone()],
                "First panel changed",
            ),
            check(
                CheckCode::SolutionPresent,
                Severity::Note,
                vec![state.session.mosaic.config.panels[1].path.clone()],
                "Second panel unsolved",
            ),
        ],
        errors: Vec::new(),
    });
    let mut h = harness(state);
    step_until(&mut h, |st| {
        st.w.plan().items().iter().any(|i| i.path == first)
    });
    // The flagged panel is outlined in Hα, the others in the accent.
    for it in h.state().w.plan().items() {
        let want = if it.path == first {
            mega_ui::theme::palette::HA_500
        } else {
            mega_ui::theme::palette::ACCENT_500
        };
        assert_eq!(it.outline, want, "{:?}", it.path);
    }
    // Click the first panel: its details open in the Panels section.
    let it = h
        .state()
        .w
        .plan()
        .items()
        .iter()
        .find(|i| i.path == first)
        .unwrap()
        .rect;
    let centre = ((it[0] + it[2]) / 2.0, (it[1] + it[3]) / 2.0);
    let p = h.state().w.plan().to_screen(centre).unwrap();
    h.hover_at(p);
    h.drag_at(p);
    h.event(egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(2);
    assert_eq!(h.state().w.plan().selected(), Some(first.as_path()));
    assert!(h.state().w.inspector().is_open("panels"));
    h.get_by_label("Ha_0.xisf");
    h.get_by_label("Checks for this panel");
    // Its header facts (read on a worker) and the panels it overlaps.
    step_until(&mut h, |st| st.w.plan().has_info(&first));
    h.get_by_label("600 × 420 (0.3 MP)");
    h.get_by_label("None (registered)");
    assert!(
        h.query_all_by_label("Ha_1.xisf").next().is_some(),
        "overlaps listed"
    );
    // Its own checks only: the other panel's note is not listed there.
    assert_eq!(
        h.query_all_by_label("First panel changed").count(),
        2,
        "in Problems and in the panel's checks"
    );
    assert!(h.query_by_label("Second panel unsolved").is_none());
    // Accept there goes to the shell too. The inspector scrolls to it as
    // a user would; the click waits for the scroll to settle.
    h.get_all_by_label("Accept")
        .last()
        .expect("the panel's Accept")
        .scroll_to_me();
    let _ = h.run_ok();
    h.get_all_by_label("Accept").last().unwrap().click();
    h.run_steps(2);
    let accepted: Vec<&str> = h
        .state()
        .accepted
        .iter()
        .map(|c| c.message.as_str())
        .collect();
    assert_eq!(accepted, ["First panel changed"]);
    // Esc clears the selection.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().w.plan().selected(), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn stretch_toggles_save_the_display_stretch() {
    let (root, s, jobs) = analyzed("window-stretch");
    let dir = s.mosaic.dir.clone();
    let mut state = State::new(s, jobs);
    state.run_pipeline = true;
    let mut h = harness(state);
    step_until(&mut h, |st| st.w.plan().is_settled());
    let normal = h.state().w.plan().stretch_for_view().map(<[_]>::to_vec);
    assert!(normal.is_some());
    h.get_by_label("Strong").click();
    h.run_steps(2);
    let want = DisplayStretch {
        linked: false,
        strong: true,
    };
    assert_eq!(
        DisplayStretch::load(&h.state().session.mosaic.config.ui),
        want
    );
    // The canvas follows, and the toggles are saved (rate-limited).
    step_until(&mut h, |st| {
        st.w.plan().is_settled() && st.w.plan().stretch_for_view().map(<[_]>::to_vec) != normal
    });
    step_until(&mut h, |_| {
        MosaicConfig::load(&dir).is_ok_and(|c| DisplayStretch::load(&c.ui) == want)
    });
    h.get_by_label("Linked").click();
    h.run_steps(2);
    assert!(DisplayStretch::load(&h.state().session.mosaic.config.ui).linked);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn previews_paused_offer_resume() {
    let root = temp_dir("window-resume");
    let mut s = two_group_session(&root);
    set_auto_analyze(&mut s, false).unwrap();
    let mut state = State::new(s, runner());
    state.run_pipeline = true;
    let gate = state.jobs.hold();
    let mut h = harness(state);
    // The previews start by themselves; the user cancels them.
    step_until(&mut h, |st| st.jobs.busy());
    let ids: Vec<u64> = h.state().jobs.views().iter().map(|v| v.id).collect();
    for id in ids {
        h.state_mut().jobs.cancel(id);
    }
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(2);
    assert!(!h.state().pipeline.previews_complete());
    h.get_by_label("Panels").click();
    h.run_steps(2);
    h.get_by_label("Previews paused");
    h.get_by_label("Resume").click();
    step_until(&mut h, |st| st.pipeline.previews_complete());
    let _ = std::fs::remove_dir_all(&root);
}

/// A step the strip shows, not cancellable.
fn strip(label: &str, state: StepState) -> StripStep {
    StripStep {
        label: label.into(),
        state,
        cancellable: false,
    }
}

/// A report of `n` problems, one per panel of `s`.
fn problems(s: &OpenSession, n: usize) -> ChecksReport {
    ChecksReport {
        checks: (0..n)
            .map(|i| {
                check(
                    CheckCode::FileChanged,
                    Severity::Problem,
                    vec![s.mosaic.config.panels[i].path.clone()],
                    &format!("Panel {i} changed"),
                )
            })
            .collect(),
        errors: Vec::new(),
    }
}

#[test]
fn steps_follow_the_session() {
    use StepState::{Done, Next, Todo, Warn, Working};
    // Empty: panels to add, nothing else to do yet.
    let root = temp_dir("steps-empty");
    let s = empty(&root);
    let got = steps(&s, &runner(), &Pipeline::new(), &ChecksEngine::new());
    assert_eq!(
        got,
        [
            strip("Add panels", Todo),
            strip("Checks", Todo),
            strip("Merge", Todo),
            strip("Export", Todo),
        ]
    );
    drop(s);
    let _ = std::fs::remove_dir_all(&root);

    // Previews running: Panels works on them, and can be cancelled.
    let root = temp_dir("steps-previews");
    let mut s = two_group_session(&root);
    set_auto_analyze(&mut s, false).unwrap();
    let mut jobs = runner();
    let gate = jobs.hold();
    let mut pipeline = Pipeline::new();
    let ctx = egui::Context::default();
    pipeline.poll(&ctx, Instant::now(), &mut s, &mut jobs, &[], false);
    let checks = ChecksEngine::new();
    let got = steps(&s, &jobs, &pipeline, &checks);
    let reading = StripStep {
        label: "Reading previews 0 / 8".into(),
        state: Working {
            fraction: Some(0.0),
        },
        cancellable: true,
    };
    assert_eq!(
        got,
        [
            reading,
            strip("Analyse", Next),
            strip("Merge", Next),
            strip("Export", Todo),
        ]
    );
    // Cancelled: they wait to be asked for again.
    let ids: Vec<u64> = jobs.views().iter().map(|v| v.id).collect();
    for id in ids {
        jobs.cancel(id);
    }
    gate.open();
    let events = drain(&mut jobs, &mut s);
    pipeline.poll(&ctx, Instant::now(), &mut s, &mut jobs, &events, false);
    assert!(!jobs.busy());
    let got = steps(&s, &jobs, &pipeline, &checks);
    assert_eq!(got[0], strip("Read previews", Next));
    let _ = std::fs::remove_dir_all(&root);

    // Analysed, with two problems: merging is next.
    let (root, s, jobs) = analyzed("steps-analysed");
    let mut checks = ChecksEngine::new();
    checks.set_report(problems(&s, 2));
    assert_eq!(
        steps(&s, &jobs, &Pipeline::new(), &checks),
        [
            strip("8 panels", Done),
            strip("2 problems", Warn),
            strip("Merge", Next),
            strip("Export", Todo),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);

    // Merged, the checks passed.
    let (root, s, jobs) = merged("steps-merged");
    let mut checks = ChecksEngine::new();
    checks.set_report(ChecksReport::default());
    assert_eq!(
        steps(&s, &jobs, &Pipeline::new(), &checks),
        [
            strip("8 panels", Done),
            strip("Checks passed", Done),
            strip("Merged", Done),
            strip("Export", Todo),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn checks_step_reveals_the_problems() {
    let (root, s, jobs) = analyzed("window-checks-step");
    let mut state = State::new(s, jobs);
    state.checks.set_report(problems(&state.session, 2));
    // The inspector folded away, Problems closed.
    state.w.inspector_mut().toggle("problems");
    state.w.inspector_mut().folded = true;
    let mut h = harness(state);
    h.run_steps(3);
    assert!(!h.state().w.inspector().is_open("problems"));
    h.get_by_label("2 problems").click();
    h.run_steps(2);
    let inspector = h.state().w.inspector();
    assert!(inspector.is_open("problems") && !inspector.folded);
    h.get_by_label("Panel 1 changed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn main_button_follows_the_work() {
    // Empty: Add panels… opens the panel list and its file dialog.
    let root = temp_dir("window-main-empty");
    let state = State::new(empty(&root), runner());
    let files = state.files.clone();
    let mut h = harness(state);
    h.run_steps(3);
    h.state_mut().w.panel_list_open = false;
    h.run_steps(2);
    assert!(h.query_by_label("Merge").is_some(), "the Merge step");
    h.get_by_label("Add panels…").click();
    h.run_steps(2);
    assert!(h.state().w.panel_list_open);
    step_until(&mut h, |_| files.asked());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);

    // Analysed, nothing merged: Merge (beside the Merge step).
    let (root, s, jobs) = analyzed("window-main-merge");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    h.run_steps(2);
    assert!(h.query_by_label("Add panels…").is_none());
    assert!(h.query_by_label("Export…").is_none());
    let merge: Vec<egui::Rect> = h.query_all_by_label("Merge").map(|n| n.rect()).collect();
    assert_eq!(merge.len(), 2, "the Merge step and the main button");
    // The main button is the one at the right: it merges every group.
    let gate = h.state_mut().jobs.hold();
    let right = merge.iter().map(|r| r.center().x).fold(f32::MIN, f32::max);
    h.query_all_by_label("Merge")
        .find(|n| n.rect().center().x == right)
        .unwrap()
        .click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Blend Ha", "Blend OIII"]);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);

    // Everything merged: Export….
    let (root, s, jobs) = merged("window-main-export");
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    h.get_by_label("Export…");
    h.get_by_label("Merged");
    assert!(h.query_by_label("Merge").is_none(), "nothing to merge");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn settings_summary_reveals_merge_settings() {
    let (root, s, jobs) = analyzed("window-settings-summary");
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    assert!(!h.state().w.inspector().is_open("merge-settings"));
    // One row, in order: the steps, Merge, then its settings' summary.
    let summary = h.get_by_label("Pyramid · feather 256 px").rect();
    let export = h.get_by_label("Export").rect();
    let mut merges: Vec<egui::Rect> = h.query_all_by_label("Merge").map(|n| n.rect()).collect();
    merges.sort_by(|a, b| a.min.x.total_cmp(&b.min.x));
    let (step, main) = (merges[0], merges[1]);
    assert!(step.max.x <= export.min.x, "Merge step, then Export");
    assert!(export.max.x < main.min.x && main.max.x < summary.min.x);
    assert!(summary.max.x <= 1200.0, "inside the window");
    for r in [step, export, main] {
        assert!(
            (r.center().y - summary.center().y).abs() < 1.0,
            "one row: {r:?} {summary:?}"
        );
    }
    h.get_by_label("Pyramid · feather 256 px").click();
    h.run_steps(2);
    assert!(h.state().w.inspector().is_open("merge-settings"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cancelling_a_working_step_cancels_its_job() {
    let (root, s, jobs) = analyzed("window-cancel-step");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    // Merging, held before any progress.
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    h.run_steps(2);
    let working = "Merging Ha · 1 of 2 · 0 %";
    let cancel = format!("Cancel {working}");
    assert!(
        h.query_by_label(&cancel).is_none(),
        "Cancel shows while hovered"
    );
    h.get_by_label(working).hover();
    h.run_steps(2);
    h.get_by_label(&cancel).click();
    h.run_steps(2);
    // The queued merge is dropped at once; the running one winds down.
    assert_eq!(labels(&h.state().jobs), ["Blend Ha"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(2);
    assert!(!h.state().jobs.blending(), "no merge job left");
    assert!(h.query_by_label_contains("Merging").is_none());
    assert_eq!(h.query_all_by_label("Merge").count(), 2, "to merge again");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn empty_session_opens_on_the_panel_list() {
    let root = temp_dir("window-empty");
    let mut h = harness(State::new(empty(&root), runner()));
    h.run_steps(3);
    assert!(h.state().w.panel_list_open);
    h.get_by_label("Add files…");
    for l in ["Add panels", "Checks", "Merge", "Export", "Add panels…"] {
        h.get_by_label(l);
    }
    assert!(
        h.query_by_label("Plan").is_none(),
        "the list over the canvas"
    );
    // Closed: the empty canvas, where nothing can be merged yet.
    h.get_by_label("Add panels").click();
    h.run_steps(3);
    assert!(!h.state().w.panel_list_open);
    assert!(disabled(&h, "Mosaic"));
    for l in ["Plan", "Outlines", "ROIs", "Linked", "Strong"] {
        let toggle = By::new().label(l).predicate(|n| n.toggled().is_some());
        assert!(h.query(toggle).is_some(), "{l}");
    }
    // Every section draws.
    for id in [
        "problems",
        "panels",
        "rois",
        "merge-settings",
        "composites",
        "notes",
    ] {
        h.state_mut().w.inspector_mut().reveal(id);
    }
    h.run_steps(3);
    h.get_by_label("Not checked yet.");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn panels_step_toggles_the_panel_list() {
    let root = temp_dir("window-panels-step");
    let mut h = harness(State::new(two_group_session(&root), runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    assert!(!h.state().w.panel_list_open, "a session with panels");
    assert!(h.query_by_label("Add files…").is_none());
    h.get_by_label("8 panels").click();
    h.run_steps(2);
    assert!(h.state().w.panel_list_open);
    h.get_by_label("Add files…");
    assert!(
        h.query_by_label("Plan").is_none(),
        "the list over the canvas"
    );
    h.get_by_label("8 panels").click();
    h.run_steps(2);
    assert!(!h.state().w.panel_list_open);
    h.get_by_label("Plan");
    assert!(h.query_by_label("Add files…").is_none());
    let _ = std::fs::remove_dir_all(&root);
}
