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
use crate::jobs::tests::{analyzed, drain};
use crate::jobs::{JobRunner, JobTarget};
use crate::panels_tab::FilesPicker;
use crate::pipeline::{Pipeline, Step};
use crate::rois::{Roi, save_rois};
use crate::session::{OpenSession, any_merged, classify_results, create_blocking, notes};
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::{PATIENCE, two_group_mono_session, two_group_session};

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

/// Step until `done` holds, stopping at the frame that made it so (bounded
/// by [`PATIENCE`]: background work).
fn step_to(h: &mut Harness<'_, State>, done: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done(h.state()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the window"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The plan has drawn the frame's canvas with every panel of its view in
/// place (the layout, read on a worker, has arrived): what it draws next
/// depends on its camera only.
fn plan_placed(st: &State) -> bool {
    let plan = st.w.plan();
    plan.canvas_frame().is_some() && !plan.items().is_empty() && plan.unplaced() == 0
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
    h.get_by_label("Not merged yet.");
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
        h.query_by_label("Analyse").is_none(),
        "analysis is automatic"
    );
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
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
    // The plan's panels are in place, and the merged result is opened
    // behind the plan (both on workers).
    step_until(&mut h, |st| {
        plan_placed(st) && st.w.mosaic().source().is_some()
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
        plan_placed(st)
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
    let s = two_group_mono_session(&root);
    let mut state = State::new(s, runner());
    // The previews say how many channels the panels have.
    state.run_pipeline = true;
    let mut h = harness(state);
    step_until(&mut h, |st| st.pipeline.previews_complete());
    h.run_steps(2);
    assert!(disabled(&h, "Linked"), "one channel: nothing to link");
    assert!(!disabled(&h, "Strong"));
    // The analysis that follows the previews ends before the session goes.
    step_until(&mut h, |st| !st.jobs.busy());
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
    // Accept there goes to the shell too (clicked through accessibility:
    // wherever the inspector's scrolling has put it).
    h.get_all_by_label("Accept")
        .last()
        .expect("the panel's Accept")
        .click_accesskit();
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
    let s = two_group_session(&root);
    let mut state = State::new(s, runner());
    state.run_pipeline = true;
    let gate = state.jobs.hold();
    let mut h = harness(state);
    // The previews start by themselves; the user cancels them (the
    // analysis follows all the same).
    step_until(&mut h, |st| st.jobs.busy());
    let ids: Vec<u64> = h.state().jobs.views().iter().map(|v| v.id).collect();
    for id in ids {
        h.state_mut().jobs.cancel(id);
    }
    gate.open();
    step_until(&mut h, |st| {
        !st.jobs.busy() && st.session.mosaic.config.group_dirs.len() == 2
    });
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
        tooltip: None,
    }
}

/// Poll the runner, then the pipeline (as the shell does, nothing being
/// edited), until `done` holds (bounded).
fn pump(
    s: &mut OpenSession,
    jobs: &mut JobRunner,
    pipeline: &mut Pipeline,
    done: impl Fn(&JobRunner, &Pipeline) -> bool,
) {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(jobs, pipeline) {
        assert!(Instant::now() < deadline, "timed out waiting for the jobs");
        std::thread::sleep(Duration::from_millis(2));
        let events = jobs.poll(s);
        pipeline.poll(&ctx, Instant::now(), s, jobs, &events, false);
    }
}

/// The steps the strip shows for the harness's state.
fn shown_steps(st: &State) -> [StripStep; 4] {
    steps(&st.session, &st.jobs, &st.pipeline, &st.checks)
}

/// The rightmost node labelled `label` (the main button, right of a step
/// of the same name), clicked.
fn click_rightmost(h: &Harness<'static, State>, label: &str) {
    let right = h
        .query_all_by_label(label)
        .map(|n| n.rect().center().x)
        .fold(f32::MIN, f32::max);
    h.query_all_by_label(label)
        .find(|n| n.rect().center().x == right)
        .expect("a node")
        .click();
}

/// Whether the rightmost node labelled `label` is disabled.
fn rightmost_disabled(h: &Harness<'static, State>, label: &str) -> bool {
    let right = h
        .query_all_by_label(label)
        .map(|n| n.rect().center().x)
        .fold(f32::MIN, f32::max);
    h.query_all_by_label(label)
        .find(|n| n.rect().center().x == right)
        .expect("a node")
        .accesskit_node()
        .is_disabled()
}

/// An ROI of the synthetic canvas.
const ROI: [u64; 4] = [0, 0, 64, 64];

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

    // Previews running: Panels works on them, and can be cancelled. The
    // analysis follows by itself: nothing waits for the user.
    let root = temp_dir("steps-previews");
    let mut s = two_group_session(&root);
    let mut jobs = runner();
    let previews = jobs.hold();
    let mut pipeline = Pipeline::new();
    let ctx = egui::Context::default();
    pipeline.poll(&ctx, Instant::now(), &mut s, &mut jobs, &[], false);
    let checks = ChecksEngine::new();
    let reading = StripStep {
        label: "Reading previews 0 / 8".into(),
        state: Working {
            fraction: Some(0.0),
        },
        cancellable: true,
        tooltip: None,
    };
    assert_eq!(
        steps(&s, &jobs, &pipeline, &checks),
        [
            reading,
            strip("Checks", Todo),
            strip("Merge", Todo),
            strip("Export", Todo),
        ]
    );
    // Cancelled: they wait to be asked for again; the analysis starts
    // (held here) and can be cancelled.
    let analysis = jobs.hold();
    for id in jobs.views().iter().map(|v| v.id).collect::<Vec<_>>() {
        jobs.cancel(id);
    }
    previews.open();
    pump(&mut s, &mut jobs, &mut pipeline, |_, p| {
        p.step() == Some(Step::Analyze)
    });
    let analysing = StripStep {
        label: "Analysing…".into(),
        state: Working { fraction: None },
        cancellable: true,
        tooltip: None,
    };
    assert_eq!(
        steps(&s, &jobs, &pipeline, &checks),
        [
            strip("Read previews", Next),
            analysing,
            strip("Merge", Todo),
            strip("Export", Todo),
        ]
    );
    // The analysis cancelled: the pipeline leaves it to the user, but one
    // step only is next.
    for id in jobs.views().iter().map(|v| v.id).collect::<Vec<_>>() {
        jobs.cancel(id);
    }
    analysis.open();
    pump(&mut s, &mut jobs, &mut pipeline, |j, _| !j.busy());
    assert_eq!(
        steps(&s, &jobs, &pipeline, &checks),
        [
            strip("Read previews", Next),
            strip("Analyse", Todo),
            strip("Merge", Todo),
            strip("Export", Todo),
        ]
    );
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
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
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
    assert_eq!(labels(&h.state().jobs), ["Merge Ha"]);
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

#[test]
fn an_roi_merge_leaves_a_merged_session_merged() {
    let (root, s, jobs) = merged("window-roi-merged");
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    // An ROI merge runs (held): the strip and the main button ignore it.
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", Some(ROI));
    }
    h.run_steps(2);
    assert_eq!(shown_steps(h.state())[2], strip("Merged", StepState::Done));
    h.get_by_label("Merged");
    h.get_by_label("Export…");
    assert!(h.query_by_label_contains("Merging").is_none());
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_starts_while_an_roi_merge_runs() {
    let (root, s, jobs) = analyzed("window-roi-merge-all");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    // An ROI merge of a group not shown runs (held).
    let gate = h.state_mut().jobs.hold();
    let roi = {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "OIII", Some(ROI))
    };
    h.run_steps(2);
    assert_eq!(shown_steps(h.state())[2], strip("Merge", StepState::Next));
    assert!(!rightmost_disabled(&h, "Merge"), "the main Merge");
    click_rightmost(&h, "Merge");
    h.run_steps(2);
    // The whole-canvas merge of every group is under way.
    let jobs = &h.state().jobs;
    let run = jobs.run_of(JobTarget::Blend).expect("a merge run");
    assert_ne!(run.id, roi);
    assert!(jobs.whole_canvas(run.id));
    assert_eq!((run.group.as_str(), run.count), ("Ha", 2));
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_merge_steps_cancel_spares_an_roi_merge() {
    let (root, s, jobs) = analyzed("window-roi-cancel");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    // An ROI merge of Ha runs (held); a whole-canvas merge of OIII waits.
    let gate = h.state_mut().jobs.hold();
    let roi = {
        let st = h.state_mut();
        let roi = st.jobs.start_blend(&mut st.session, "Ha", Some(ROI));
        st.jobs.start_blend(&mut st.session, "OIII", None);
        roi
    };
    h.run_steps(2);
    let working = "Merging OIII · 0 %";
    h.get_by_label(working).hover();
    h.run_steps(2);
    h.get_by_label(&format!("Cancel {working}")).click();
    h.run_steps(2);
    // The ROI merge runs on.
    let left: Vec<u64> = h.state().jobs.views().iter().map(|v| v.id).collect();
    assert_eq!(left, [roi]);
    assert!(!h.state().jobs.views()[0].stage.contains("Cancelling"));
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn analyse_step_restarts_a_stalled_analysis() {
    let root = temp_dir("window-analyse");
    let mut state = State::new(two_group_session(&root), runner());
    state.run_pipeline = true;
    let previews = state.jobs.hold();
    let mut h = harness(state);
    step_until(&mut h, |st| st.pipeline.step() == Some(Step::Previews));
    let analysis = h.state_mut().jobs.hold();
    h.run_steps(2);
    // The analysis follows the previews by itself: nothing to press.
    assert_eq!(shown_steps(h.state())[1], strip("Checks", StepState::Todo));
    assert!(h.query_by_label("Analyse").is_none());
    previews.open();
    step_until(&mut h, |st| {
        st.jobs.analyzing() && st.pipeline.previews_complete()
    });
    h.run_steps(2);
    // Cancelled from its step, the analysis is left to the user.
    h.get_by_label("Analysing…").hover();
    h.run_steps(2);
    h.get_by_label("Cancel Analysing…").click();
    h.run_steps(2);
    analysis.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(2);
    assert_eq!(shown_steps(h.state())[1], strip("Analyse", StepState::Next));
    assert!(
        h.state().w.mosaic().can_open_export(),
        "no analysis tracked"
    );
    // Pressed: it starts, and the mosaic tracks it.
    let again = h.state_mut().jobs.hold();
    h.get_by_label("Analyse").click();
    h.run_steps(2);
    assert!(h.state().jobs.analyzing());
    assert!(
        !h.state().w.mosaic().can_open_export(),
        "the analysis is tracked"
    );
    again.open();
    step_until(&mut h, |st| {
        !st.jobs.busy() && st.session.mosaic.config.group_dirs.len() == 2
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_merged_step_shows_the_mosaic() {
    let (root, s, jobs) = merged("window-merged-step");
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    h.run_steps(3);
    // Done: a look at the mosaic, never an hours-long merge.
    h.get_by_label("Merged").click();
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert!(!h.state().jobs.busy(), "no merge started");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);

    // Next: it merges.
    let (root, s, jobs) = analyzed("window-merge-step");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    let gate = h.state_mut().jobs.hold();
    let left = h
        .query_all_by_label("Merge")
        .map(|n| n.rect().center().x)
        .fold(f32::MAX, f32::min);
    h.query_all_by_label("Merge")
        .find(|n| n.rect().center().x == left)
        .unwrap()
        .click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

/// Every text the last frame painted and every accessibility label and
/// value on it.
fn all_text(h: &Harness<'_, State>) -> Vec<String> {
    fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for cs in &h.output().shapes {
        walk(&cs.shape, &mut out);
    }
    for n in h.root().children_recursive() {
        let node = n.accesskit_node();
        out.extend(node.label());
        out.extend(node.value());
    }
    out
}

fn assert_no_blend(h: &Harness<'_, State>, what: &str) {
    let bad: Vec<_> = all_text(h)
        .into_iter()
        .filter(|t| t.to_lowercase().contains("blend"))
        .collect();
    assert!(bad.is_empty(), "{what}: {bad:?}");
}

#[test]
fn no_user_visible_blend_wording() {
    let (root, mut s, jobs) = merged("window-wording");
    add_roi(&mut s);
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    for id in [PROBLEMS, PANELS, ROIS, MERGE_SETTINGS, COMPOSITES, NOTES] {
        h.state_mut().w.inspector_mut().open.insert(id.to_string());
    }
    step_until(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    step_until(&mut h, |st| st.w.mosaic().health().is_some());
    h.run_steps(3);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert_no_blend(&h, "mosaic");
    // The settings caption is among what was checked.
    assert!(
        all_text(&h)
            .iter()
            .any(|t| t.contains("MERGE · re-merge only"))
    );

    // The export dialog.
    let State { w, session, .. } = h.state_mut();
    w.mosaic.open_export(session);
    h.run_steps(4);
    assert!(
        !h.state().w.mosaic().can_open_export(),
        "the dialog is open"
    );
    assert_no_blend(&h, "export dialog");

    // The plan.
    h.state_mut().w.set_mode(CanvasMode::Plan);
    h.run_steps(4);
    assert_no_blend(&h, "plan");
    let _ = std::fs::remove_dir_all(&root);
}

/// The view bar's toggle `label` (the inspector has a "ROIs" heading too).
fn toggle(label: &str) -> By<'_> {
    By::new().label(label).predicate(|n| n.toggled().is_some())
}

/// Whether the view bar's toggle `label` is on.
fn toggle_on(h: &Harness<'_, State>, label: &str) -> bool {
    h.get(toggle(label)).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
}

/// Save what the window holds back, as the shell does on closing.
fn flush(h: &mut Harness<'static, State>) {
    let st = h.state_mut();
    st.w.flush_save(&mut st.session);
}

/// The session in `dir` opened again (lock and all), in a new window.
fn reopen(dir: &std::path::Path) -> Harness<'static, State> {
    let s = crate::session::open_blocking(dir).expect("the session opens again");
    harness(State::new(s, runner()))
}

#[test]
fn view_state_survives_reopening() {
    let (root, mut s, jobs) = merged("window-view-state");
    add_roi(&mut s);
    let dir = s.mosaic.dir.clone();
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic, "merged: the mosaic");
    // The plan of OIII, zoomed in; Notes open in place of Problems, the
    // inspector folded; ROIs off; Strong on.
    h.get_by_label("Plan").click();
    h.run_steps(2);
    h.get_by_label("OIII · 4").click();
    let oiii = Some(ViewKind::Group("OIII".into()));
    step_to(&mut h, |st| st.w.view() == oiii.as_ref() && plan_placed(st));
    zoom_plan_about(&mut h, (150.0, 130.0));
    {
        let inspector = h.state_mut().w.inspector_mut();
        inspector.toggle(PROBLEMS);
        inspector.toggle(NOTES);
    }
    h.get(toggle("ROIs")).click();
    h.get(toggle("Strong")).click();
    h.get_by_label("Hide inspector").click();
    h.run_steps(2);
    assert!(h.state().w.inspector().folded);
    let cam = h.state().w.plan().camera().expect("the plan's camera");
    flush(&mut h);
    drop(h);

    let mut h = reopen(&dir);
    // The first frame with every panel in place is drawn through the
    // camera: no fit is painted first.
    step_to(&mut h, plan_placed);
    let st = h.state();
    assert_eq!(st.w.mode(), CanvasMode::Plan);
    assert_eq!(st.w.view(), oiii.as_ref());
    assert_eq!(st.w.plan().view(), oiii.as_ref());
    assert_eq!(st.w.mosaic().view(), oiii.as_ref());
    assert_camera(st.w.plan().camera(), cam);
    assert_plan_painted_with(&h, cam);
    let inspector = st.w.inspector();
    assert!(inspector.folded);
    assert!(inspector.is_open(NOTES) && !inspector.is_open(PROBLEMS));
    assert!(!st.w.panel_list_open);
    assert!(toggle_on(&h, "Outlines"));
    assert!(!toggle_on(&h, "ROIs"));
    assert!(toggle_on(&h, "Strong"), "the display stretch");

    // The mosaic, Outlines off, ROIs on and the panel list open: kept too.
    h.get_by_label("Mosaic").click();
    h.run_steps(2);
    h.get(toggle("Outlines")).click();
    h.get(toggle("ROIs")).click();
    h.run_steps(2);
    h.get_by_label("8 panels").click();
    h.run_steps(2);
    assert!(h.state().w.panel_list_open);
    flush(&mut h);
    drop(h);
    let mut h = reopen(&dir);
    step_until(&mut h, |st| st.w.view().is_some());
    assert!(h.state().w.panel_list_open);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    h.get_by_label("8 panels").click();
    // The mosaic's first painted frame is through the camera too.
    step_to(&mut h, |st| {
        !st.w.panel_list_open && st.w.mosaic().viewer_frame().is_some()
    });
    assert_mosaic_painted_with(&h, cam);
    assert_camera(h.state().w.mosaic().camera(), cam);
    assert!(!toggle_on(&h, "Outlines"));
    assert!(toggle_on(&h, "ROIs"));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restored_view_that_no_longer_exists_falls_back() {
    let saved_cam = Camera {
        centre: (200.0, 150.0),
        zoom: 3.0,
    };
    // A group gone, a composite gone, and a group still there.
    for (key, shown) in [("SII", "Ha"), ("comp:Gone", "Ha"), ("OIII", "OIII")] {
        let (root, mut s, jobs) = analyzed("window-view-gone");
        assert!(!s.merged);
        let saved = ViewState {
            mode: CanvasMode::Mosaic,
            view: Some(key.into()),
            camera: Some(saved_cam),
            inspector_folded: false,
            inspector_open: vec![NOTES.into()],
            panel_list_open: false,
            outlines: true,
            rois: true,
        };
        saved.save(&mut s.mosaic.config.ui);
        let mut h = harness(State::new(s, jobs));
        step_to(&mut h, plan_placed);
        let st = h.state();
        // Nothing merged: the plan.
        assert_eq!(st.w.mode(), CanvasMode::Plan, "{key}");
        let view = Some(ViewKind::Group(shown.into()));
        assert_eq!(st.w.view(), view.as_ref(), "{key}");
        assert_eq!(st.w.plan().view(), view.as_ref(), "{key}");
        assert_eq!(st.w.mosaic().view(), view.as_ref(), "{key}");
        // The camera comes back with its view only.
        let cam = st.w.plan().camera().expect("the plan's camera");
        let restored = (cam.zoom - saved_cam.zoom).abs() < 1e-6
            && (cam.centre.0 - saved_cam.centre.0).abs() < 1e-6
            && (cam.centre.1 - saved_cam.centre.1).abs() < 1e-6;
        assert_eq!(restored, key == shown, "{key}: {cam:?}");
        // The rest is restored all the same.
        assert!(st.w.inspector().is_open(NOTES), "{key}");
        assert!(!st.w.inspector().is_open(PROBLEMS), "{key}");
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn shortcuts_toggle_the_canvas() {
    use egui::{Key, Modifiers};
    let (root, s, jobs) = merged("window-shortcuts");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    let group = |g: &str| Some(ViewKind::Group(g.into()));
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    assert_eq!(h.state().w.view(), group("Ha").as_ref());
    assert!(toggle_on(&h, "Outlines"));
    let press = |h: &mut Harness<'static, State>, key: Key| {
        h.key_press(key);
        h.run_steps(2);
    };
    press(&mut h, Key::O);
    assert!(!toggle_on(&h, "Outlines"));
    press(&mut h, Key::M);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    press(&mut h, Key::M);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    // Next and previous, round the chips (no composites).
    press(&mut h, Key::CloseBracket);
    assert_eq!(h.state().w.view(), group("OIII").as_ref());
    assert_eq!(h.state().w.mosaic().view(), group("OIII").as_ref());
    press(&mut h, Key::CloseBracket);
    assert_eq!(h.state().w.view(), group("Ha").as_ref());
    press(&mut h, Key::OpenBracket);
    assert_eq!(h.state().w.view(), group("OIII").as_ref());
    press(&mut h, Key::R);
    assert!(!toggle_on(&h, "ROIs"));
    press(&mut h, Key::S);
    press(&mut h, Key::L);
    let want = DisplayStretch {
        linked: true,
        strong: true,
    };
    assert_eq!(
        DisplayStretch::load(&h.state().session.mosaic.config.ui),
        want
    );
    press(&mut h, Key::I);
    assert!(h.state().w.inspector().folded);
    press(&mut h, Key::I);
    assert!(!h.state().w.inspector().folded);
    // With a modifier held, a letter is no shortcut.
    h.key_press_modifiers(Modifiers::CTRL, Key::O);
    h.run_steps(2);
    assert!(!toggle_on(&h, "Outlines"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ctrl_enter_merges_when_merge_is_allowed() {
    use egui::{Key, Modifiers};
    let (root, s, jobs) = analyzed("window-ctrl-enter");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    // Nothing merged: M has no mosaic to show.
    h.key_press(Key::M);
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    let gate = h.state_mut().jobs.hold();
    h.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    // Merging already: nothing more.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    gate.open();
    // Merged: the main button reads Export…, and Ctrl+Enter merges
    // nothing.
    step_until(&mut h, |st| {
        !st.jobs.busy() && st.session.merged && !super::steps::needs_merge(&st.session)
    });
    h.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    h.run_steps(2);
    assert!(!h.state().jobs.busy(), "no merge started");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shortcuts_ignored_while_typing() {
    use egui::Key;
    let root = temp_dir("window-typing");
    let mut h = harness(State::new(two_group_session(&root), runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    h.state_mut().w.inspector_mut().reveal(NOTES);
    h.run_steps(2);
    h.get_by_label("Add notes").click();
    h.run_steps(2);
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    // A key typed as a keyboard types it: the key, then its text.
    for (key, text) in [(Key::O, "o"), (Key::I, "i"), (Key::CloseBracket, "]")] {
        h.key_press(key);
        h.get_by_role(Role::MultilineTextInput).type_text(text);
        h.run_steps(2);
    }
    assert_eq!(notes(&h.state().session.mosaic.config.ui), "oi]");
    assert!(toggle_on(&h, "Outlines"), "outlines unchanged");
    assert!(!h.state().w.inspector().folded);
    assert_eq!(
        h.state().w.view(),
        Some(&ViewKind::Group("Ha".into())),
        "no next view"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shortcuts_ignored_in_the_export_dialog() {
    use egui::Key;
    let (root, s, jobs) = merged("window-export-keys");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.mosaic().can_open_export());
    let State { w, session, .. } = h.state_mut();
    w.mosaic.open_export(session);
    h.run_steps(2);
    h.key_press(Key::O);
    h.key_press(Key::M);
    h.run_steps(2);
    assert!(toggle_on(&h, "Outlines"));
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    let _ = std::fs::remove_dir_all(&root);
}

/// Whether the tooltip `want` shows once the pointer has rested at `at`
/// past the tooltip delay.
fn tooltip_shows(h: &mut Harness<'static, State>, at: egui::Pos2, want: &str) -> bool {
    h.hover_at(at);
    h.run_steps(80);
    h.query_all_by_label(want).next().is_some()
}

#[test]
fn shortcuts_show_in_tooltips() {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let (root, s, jobs) = analyzed("window-shortcut-tips");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    h.run_steps(2);
    let outlines = h.get(toggle("Outlines")).rect().center();
    let want = "Show the panels' outlines and numbers (O)";
    assert!(tooltip_shows(&mut h, outlines, want), "{want}");
    let plan = h.get_by_label("Plan").rect().center();
    let want = "Switch between plan and mosaic (M)";
    assert!(tooltip_shows(&mut h, plan, want), "{want}");
    let chip = h.get_by_label("OIII · 4").rect().center();
    let want = "Previous / next group or composite ([ / ])";
    assert!(tooltip_shows(&mut h, chip, want), "{want}");
    let hide = h.get_by_label("Hide inspector").rect().center();
    assert!(tooltip_shows(&mut h, hide, "Hide inspector (I)"));
    // The main button: the rightmost Merge.
    let main = h
        .query_all_by_label("Merge")
        .map(|n| n.rect().center())
        .fold(
            egui::pos2(f32::MIN, 0.0),
            |a, b| if b.x > a.x { b } else { a },
        );
    let keys = h
        .ctx
        .format_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter));
    let want = format!("Merge every group over the whole canvas ({keys})");
    assert!(tooltip_shows(&mut h, main, &want), "{want}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn picking_an_roi_frames_it_on_the_shown_canvas() {
    use mega_ui::viewer::MAX_ZOOM;
    let (root, mut s, jobs) = merged("window-roi-go-to");
    add_roi(&mut s);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    state.w.inspector_mut().reveal(ROIS);
    let mut h = harness(state);
    step_until(&mut h, |st| {
        plan_placed(st) && st.w.mosaic().source().is_some()
    });
    h.run_steps(2);
    let fit = h.state().w.plan().camera().expect("the plan's camera");
    // Picked in the list while the plan shows: the plan frames it.
    h.get_by_label("ROI 1").click();
    h.run_steps(3);
    let (rect, _) = h.state().w.plan().canvas_frame().expect("the plan drawn");
    let zoom = (f64::from(rect.width()) / 100.0)
        .min(f64::from(rect.height()) / 60.0)
        .min(MAX_ZOOM);
    let want = Camera {
        centre: (150.0, 130.0),
        zoom,
    };
    assert!(want.zoom > fit.zoom * 1.1, "{fit:?} then {want:?}");
    assert_camera(h.state().w.plan().camera(), want);
    assert_plan_painted_with(&h, want);
    // On to the mosaic: it shows the same, and nothing moves afterwards.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Mosaic && st.w.mosaic().viewer_frame().is_some()
    });
    assert_mosaic_painted_with(&h, want);
    h.run_steps(150);
    assert_camera(h.state().w.mosaic().camera(), want);
    assert_mosaic_painted_with(&h, want);
    // Panned away on the mosaic, picked again: the mosaic frames it.
    let (rect, _) = h.state().w.mosaic().viewer_frame().unwrap();
    drag(&mut h, rect.center(), egui::vec2(-60.0, 20.0));
    assert!(h.state().w.mosaic().camera().unwrap().centre != want.centre);
    h.get_by_label("ROI 1").click();
    h.run_steps(3);
    assert_camera(h.state().w.mosaic().camera(), want);
    assert_mosaic_painted_with(&h, want);
    let _ = std::fs::remove_dir_all(&root);
}

/// [`analyzed`] in a session folder named `name` (the top bar shows it).
fn analyzed_named(tag: &str, name: &str) -> (PathBuf, OpenSession, JobRunner) {
    let root = temp_dir(tag);
    let mut s = crate::testutil::two_group_session_named(&root, name);
    let mut jobs = runner();
    jobs.start_analyze(&mut s);
    let _ = drain(&mut jobs, &mut s);
    assert_eq!(s.mosaic.config.group_dirs.len(), 2, "analysed");
    (root, s, jobs)
}

/// The top bar's buttons and labels, each with its rect. Asserts none
/// overlaps another and all are inside a window `width` wide.
fn top_bar_controls(h: &Harness<'_, State>, width: f32) -> Vec<(String, egui::Rect)> {
    let bar = h.state().w.top_bar_rect.expect("the top bar drawn");
    let controls: Vec<(String, egui::Rect)> = h
        .root()
        .children_recursive()
        .filter(|n| {
            matches!(n.accesskit_node().role(), Role::Button | Role::Label)
                && bar.contains(n.rect().center())
        })
        .map(|n| {
            // A label's text is its value.
            let node = n.accesskit_node();
            let text = node.label().or(node.value()).unwrap_or_default();
            (text, n.rect())
        })
        .collect();
    for (label, r) in &controls {
        assert!(
            r.min.x >= -0.5 && r.max.x <= width + 0.5 && bar.expand(0.5).contains_rect(*r),
            "{label} at {r:?} is not inside the window ({width} wide) and its top bar {bar:?}"
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
fn the_top_bar_fits_narrow_windows() {
    let name = "Orion SHO mosaic, winter 2026 reprocessing with new flats";
    for width in [720.0, 960.0] {
        for merging in [false, true] {
            let tag = format!("window-top-bar-{width}-{merging}");
            let (root, s, jobs) = analyzed_named(&tag, name);
            let mut state = State::new(s, jobs);
            state.checks.set_report(problems(&state.session, 2));
            let mut h = harness_sized(state, egui::vec2(width, 800.0));
            step_until(&mut h, |st| {
                st.w.mosaic()
                    .merge_block_reason(&st.session, &st.jobs)
                    .is_none()
            });
            let gate = merging.then(|| {
                let gate = h.state_mut().jobs.hold();
                let st = h.state_mut();
                st.w.merge(&mut st.session, &mut st.jobs);
                gate
            });
            h.run_steps(3);
            let controls = top_bar_controls(&h, width);
            let what = format!("{width} pt, merging {merging}: {controls:?}");
            // Back, the name, the four steps, Merge and its settings' summary.
            assert!(controls.len() >= 8, "{what}");
            let step = if merging {
                "Merging Ha \u{b7} 1 of 2 \u{b7} 0 %"
            } else {
                "2 problems"
            };
            for label in [
                "Back to the start screen",
                step,
                "Pyramid \u{b7} feather 256 px",
            ] {
                assert!(controls.iter().any(|(l, _)| l == label), "{label}: {what}");
            }
            assert!(
                controls.iter().any(|(l, _)| l.starts_with("Orion SHO")),
                "the name: {what}"
            );
            if let Some(gate) = gate {
                gate.open();
                step_until(&mut h, |st| !st.jobs.busy());
            }
            drop(h);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// What the plan says while the session has no panels.
const EMPTY_PLAN: &str =
    "Add panels to plan a mosaic: use Add panels\u{2026} or drop the files here.";

#[test]
fn the_empty_plan_says_how_to_add_panels() {
    let root = temp_dir("window-empty-plan");
    let mut h = harness(State::new(empty(&root), runner()));
    h.run_steps(3);
    h.state_mut().w.panel_list_open = false;
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    let text = all_text(&h);
    assert!(text.iter().any(|t| t == EMPTY_PLAN), "{text:?}");
    assert!(!text.iter().any(|t| t.contains(" tab")), "{text:?}");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn analyse_starts_a_stalled_analysis_while_another_step_is_next() {
    use StepState::{Next, Todo};
    let root = temp_dir("window-analyse-todo");
    let mut h = harness(stalled_analysis(&root));
    h.run_steps(3);
    let shown = shown_steps(h.state());
    assert_eq!(shown[0], strip("Read previews", Next));
    assert_eq!(shown[1], strip("Analyse", Todo));
    // Pressed though it is not the next step: it analyses.
    let again = h.state_mut().jobs.hold();
    h.get_by_label("Analyse").click();
    h.run_steps(2);
    assert!(h.state().jobs.analyzing(), "the analysis started");
    again.open();
    step_until(&mut h, |st| !st.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

/// Whether hovering `at` shows the tooltip `want` (and it was not showing
/// before, the pointer resting on the canvas).
fn hover_says(h: &mut Harness<'static, State>, at: egui::Pos2, want: &str) -> bool {
    let (rest, _) = h.state().w.mosaic().viewer_frame().expect("the mosaic");
    h.hover_at(rest.center());
    h.run_steps(80);
    assert!(
        h.query_all_by_label(want).next().is_none(),
        "{want} already"
    );
    tooltip_shows(h, at, want)
}

#[test]
fn steps_that_cannot_act_say_why() {
    let (root, s, jobs) = merged("window-why-not");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.w.mosaic().can_open_export() && st.w.mosaic().viewer_frame().is_some()
    });
    // An analysis running (held): nothing merges or exports until it ends.
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    h.run_steps(2);
    assert_eq!(shown_steps(h.state())[2], strip("Merge", StepState::Todo));
    assert!(rightmost_disabled(&h, "Export\u{2026}"), "the main button");
    let why = "Waiting for the analysis to finish.";
    let merge = h.get_by_label("Merge").rect().center();
    assert!(hover_says(&mut h, merge, why), "the Merge step");
    let export = h.get_by_label("Export").rect().center();
    assert!(hover_says(&mut h, export, why), "the Export step");
    let main = h.get_by_label("Export\u{2026}").rect().center();
    assert!(hover_says(&mut h, main, why), "Export\u{2026}");
    // Clicked anyway: nothing starts.
    h.get_by_label("Merge").click();
    h.get_by_label("Export").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs).len(), 1, "the analysis only");
    assert!(!h.state().w.mosaic().export_open());
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composite_with_an_unmerged_member_keeps_the_canvas_in_place() {
    let (root, mut s, mut jobs) = analyzed("window-composite-note");
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    s.merged = any_merged(&s);
    s.results = classify_results(&s);
    add_roi(&mut s);
    let mut comp = Composition::new("HO");
    comp.channels = [Some(("Ha".into(), 1.0)), Some(("OIII".into(), 1.0)), None];
    save_compositions(&mut s.mosaic.config.ui, &[comp]);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    state.w.select_view(ViewKind::Composition(0));
    let mut h = harness(state);
    step_until(&mut h, |st| {
        plan_placed(st) && st.w.mosaic().source().is_some()
    });
    h.run_steps(2);
    let cam = h.state().w.plan().camera().expect("the plan's camera");
    let (plan, _) = h.state().w.plan().canvas_frame().unwrap();
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Mosaic && st.w.mosaic().viewer_frame().is_some()
    });
    h.run_steps(3);
    // OIII, not merged, is named over the image...
    assert!(
        all_text(&h).iter().any(|t| t.starts_with("OIII: ")),
        "{:?}",
        all_text(&h)
    );
    // ...which is drawn in the plan's place, through its camera.
    let (mosaic, _) = h.state().w.mosaic().viewer_frame().unwrap();
    assert!(
        (mosaic.min - plan.min).length() < 0.5 && (mosaic.max - plan.max).length() < 0.5,
        "the mosaic in {mosaic:?}, the plan in {plan:?}"
    );
    assert_camera(h.state().w.mosaic().camera(), cam);
    let (got, _) = painted(&h, mosaic, "ROI 1").expect("ROI label painted");
    let want = screen(plan, cam, (100.0, 100.0)) + egui::vec2(4.0, 2.0);
    assert!((got - want).length() < 1.0, "{got:?} != {want:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_one_group_merge_names_no_group() {
    let root = temp_dir("steps-one-group");
    let mut s = two_group_session(&root);
    s.mosaic.config.grouping.enabled = false;
    s.groups = s.mosaic.groups().ok();
    let mut jobs = runner();
    jobs.start_analyze(&mut s);
    let _ = drain(&mut jobs, &mut s);
    let gate = jobs.hold();
    let id = jobs.start_blend(&mut s, SINGLE_GROUP, None);
    assert_eq!(jobs.label(id), Some("Merge"), "the job (and its toasts)");
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    let want = StripStep {
        label: "Merging \u{b7} 0 %".into(),
        state: StepState::Working {
            fraction: Some(0.0),
        },
        cancellable: true,
        tooltip: None,
    };
    assert_eq!(got[2], want);
    gate.open();
    let events = drain(&mut jobs, &mut s);
    assert!(
        events.contains(&crate::jobs::JobEvent::Finished {
            id,
            label: "Merge".into()
        }),
        "{events:?}"
    );
    // One export: "Exporting", no "1 of 1"; the job is "Export".
    let gate = jobs.hold();
    let out = root.join("one.fits");
    let id = jobs.start_export(&s, SINGLE_GROUP, out, crate::export::ExportFormat::Fits);
    assert_eq!(jobs.label(id), Some("Export"));
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    assert_eq!(got[3].label, "Exporting");
    gate.open();
    let events = drain(&mut jobs, &mut s);
    assert!(
        events.contains(&crate::jobs::JobEvent::Finished {
            id,
            label: "Export".into()
        }),
        "{events:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shortcuts_work_with_a_button_focused() {
    use egui::Key;
    let (root, s, jobs) = merged("window-shortcut-focus");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    assert!(toggle_on(&h, "Outlines"));
    // Tab along the controls to a chip.
    let mut tabs = 0;
    while !h.get_by_label("OIII \u{b7} 4").is_focused() {
        assert!(tabs < 80, "the chip never took the focus");
        h.key_press(Key::Tab);
        h.run_steps(1);
        tabs += 1;
    }
    h.key_press(Key::O);
    h.run_steps(2);
    assert!(!toggle_on(&h, "Outlines"), "O toggles with a chip focused");
    assert!(
        h.get_by_label("OIII \u{b7} 4").is_focused(),
        "the focus stays"
    );
    // Typing in the notes still types.
    h.state_mut().w.inspector_mut().reveal(NOTES);
    h.run_steps(2);
    h.get_by_label("Add notes").click();
    h.run_steps(2);
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.key_press(Key::O);
    h.get_by_role(Role::MultilineTextInput).type_text("o");
    h.run_steps(2);
    assert_eq!(notes(&h.state().session.mosaic.config.ui), "o");
    assert!(!toggle_on(&h, "Outlines"), "unchanged while typing");
    let _ = std::fs::remove_dir_all(&root);
}

/// A two-group session whose previews and then analysis were cancelled:
/// the analysis is left to the user (Read previews is next, Analyse to
/// do), nothing runs.
fn stalled_analysis(root: &std::path::Path) -> State {
    let mut s = two_group_session(root);
    let mut jobs = runner();
    let mut pipeline = Pipeline::new();
    let previews = jobs.hold();
    let ctx = egui::Context::default();
    pipeline.poll(&ctx, Instant::now(), &mut s, &mut jobs, &[], false);
    let analysis = jobs.hold();
    for id in jobs.views().iter().map(|v| v.id).collect::<Vec<_>>() {
        jobs.cancel(id);
    }
    previews.open();
    pump(&mut s, &mut jobs, &mut pipeline, |_, p| {
        p.step() == Some(Step::Analyze)
    });
    for id in jobs.views().iter().map(|v| v.id).collect::<Vec<_>>() {
        jobs.cancel(id);
    }
    analysis.open();
    pump(&mut s, &mut jobs, &mut pipeline, |j, _| !j.busy());
    let mut state = State::new(s, jobs);
    state.pipeline = pipeline;
    state
}

#[test]
fn a_working_analysing_step_never_restarts_the_analysis() {
    let root = temp_dir("window-analysing-click");
    let mut h = harness(stalled_analysis(&root));
    h.run_steps(3);
    assert_eq!(shown_steps(h.state())[1], strip("Analyse", StepState::Todo));
    // The user restarts it (held, so it keeps running).
    let gate = h.state_mut().jobs.hold();
    h.get_by_label("Analyse").click();
    h.run_steps(2);
    assert!(h.state().jobs.analyzing());
    let running: Vec<u64> = h.state().jobs.views().iter().map(|v| v.id).collect();
    // Problems closed, to see the click open it.
    h.state_mut().w.inspector_mut().toggle(PROBLEMS);
    h.run_steps(2);
    assert!(!h.state().w.inspector().is_open(PROBLEMS));
    // A click on the working pill (not its Cancel): Problems, never a
    // second analysis in place of the first.
    h.get_by_label("Analysing\u{2026}").click();
    h.run_steps(2);
    let after: Vec<u64> = h.state().jobs.views().iter().map(|v| v.id).collect();
    assert_eq!(after, running, "the analysis runs on");
    assert!(h.state().w.inspector().is_open(PROBLEMS));
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_strip_never_wraps_beside_the_name() {
    let name = "Orion SHO mosaic, winter 2026 reprocessing with new flats";
    let (root, s, jobs) = analyzed_named("window-strip-row", name);
    let mut state = State::new(s, jobs);
    state.checks.set_report(problems(&state.session, 2));
    let mut h = harness_sized(state, egui::vec2(720.0, 800.0));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    // Across every width from the minimum to one that fits it all, the
    // four pills share one row: beside the name, or below it.
    let mut one_row_at = None;
    for width in (720..=1600).step_by(2) {
        h.set_size(egui::vec2(width as f32, 800.0));
        h.run_steps(2);
        let ys: Vec<f32> = shown_steps(h.state())
            .iter()
            .map(|s| {
                h.query_all_by_label(&s.label)
                    .map(|n| n.rect())
                    .min_by(|a, b| a.min.x.total_cmp(&b.min.x))
                    .expect("the step drawn")
                    .center()
                    .y
            })
            .collect();
        assert!(
            ys.iter().all(|y| (y - ys[0]).abs() < 0.5),
            "{width} pt: pills at {ys:?}"
        );
        let back = h.get_by_label("Back to the start screen").rect();
        if one_row_at.is_none() && (ys[0] - back.center().y).abs() < 0.5 {
            one_row_at = Some(width);
        }
    }
    assert!(one_row_at.is_some(), "never one row");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}
