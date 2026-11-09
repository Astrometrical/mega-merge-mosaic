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

use super::steps::{CHECKS_STEP, EXPORT_STEP, MERGE_STEP, draft_changed, out_of_date, steps};
use super::*;
use crate::camera::Camera;
use crate::checks::{ChecksEngine, ChecksReport};
use crate::compositions::{Composition, ViewKind, load_compositions, save_compositions};
use crate::display_stretch::DisplayStretch;
use crate::jobs::tests::{analyzed, drain};
use crate::jobs::{ExportTarget, JobRunner, JobTarget};
use crate::panel_edits::FilesPicker;
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

    fn pick_folders(&self) -> Option<Vec<PathBuf>> {
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
    /// The toasts the window raised, in order.
    toasts: Vec<WindowToast>,
    /// The job runner's events, in order.
    events: Vec<JobEvent>,
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
            toasts: Vec::new(),
            events: Vec::new(),
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
                s.toasts.extend(out.toasts);
                s.events.extend(events);
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
        id: 1,
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

/// The **+** chip's accessible name (it shows "+").
const NEW_COMPOSITE: &str = "New composite";

/// The **+** chip.
fn plus_chip<'h>(h: &'h Harness<'static, State>) -> Option<egui_kittest::Node<'h>> {
    h.query(By::new().role(Role::Button).label(NEW_COMPOSITE))
}

/// Whether the **+** chip says its popover is open.
fn plus_expanded(h: &Harness<'static, State>) -> Option<bool> {
    plus_chip(h)
        .expect("a + chip")
        .accesskit_node()
        .data()
        .is_expanded()
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

/// The first label of ROI `name` painted inside `rect` in the last frame
/// (its name, followed by " · merging…" while its preview merges): its
/// position and size.
fn painted_roi(
    h: &Harness<'_, State>,
    rect: egui::Rect,
    name: &str,
) -> Option<(egui::Pos2, egui::Vec2)> {
    let merging = format!("{name} · merging…");
    h.output().shapes.iter().find_map(|cs| match &cs.shape {
        egui::Shape::Text(t)
            if (t.galley.text() == name || t.galley.text() == merging) && rect.contains(t.pos) =>
        {
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
        let (got, _) = painted_roi(h, rect, &r.name).expect("ROI label painted");
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
        st.w.merge_group(&mut st.session, &mut st.jobs, "Ha");
    }
    assert!(h.state().jobs.blend_pending("Ha"));
    step_until(&mut h, |st| !st.jobs.busy() && st.w.mosaic().is_settled());
    assert_mosaic_painted_with(&h, cam);
    assert_camera(h.state().w.mosaic().camera(), cam);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plan_rois_wait_while_the_frames_differ() {
    use mmm_core::reference::ReferenceFrame;
    let (root, mut s, jobs) = merged("window-plan-frames");
    add_roi(&mut s);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    state.w.inspector_mut().reveal(ROIS);
    let mut h = harness(state);
    step_until(&mut h, plan_placed);
    h.run_steps(3);
    let (rect, _) = h.state().w.plan().canvas_frame().unwrap();
    assert!(painted_roi(&h, rect, "ROI 1").is_some(), "frames agree");
    assert!(!disabled(&h, "Draw ROI"));
    // A canvas-changing edit: the analysed frame is not the one the plan
    // draws in until the analysis ends.
    h.state_mut().session.frame = Some(ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    assert!(
        painted_roi(&h, rect, "ROI 1").is_none(),
        "no outlines offset"
    );
    assert!(disabled(&h, "Draw ROI"));
    h.key_press(egui::Key::D);
    h.run_steps(2);
    assert!(!h.state().w.plan().drawing());
    let at = h.get_by_label("Draw ROI").rect().center();
    let why = "Available once the analysis catches up with the panels";
    assert!(tooltip_shows(&mut h, at, why), "{why}");
    // The mosaic is unaffected.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Mosaic && st.w.mosaic().viewer_frame().is_some()
    });
    h.run_steps(2);
    assert!(!disabled(&h, "Draw ROI"));
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    assert!(painted_roi(&h, frame, "ROI 1").is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn going_to_an_roi_waits_while_the_plan_frame_differs() {
    use mmm_core::reference::ReferenceFrame;
    let (root, mut s, jobs) = merged("window-plan-goto");
    add_roi(&mut s);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    state.w.inspector_mut().reveal(ROIS);
    let mut h = harness(state);
    step_until(&mut h, plan_placed);
    h.run_steps(3);
    let fit = h.state().w.plan().camera().unwrap();
    // The plan draws in another frame than the analysed one, which the
    // ROI is anchored in: picking it goes nowhere, and a toast says why.
    h.state_mut().session.frame = Some(ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    h.get_by_label("ROI 1").click();
    h.run_steps(3);
    assert_eq!(h.state().w.mosaic().selected_roi(), Some(1), "selected");
    assert_camera(h.state().w.plan().camera(), fit);
    let titles: Vec<String> = h
        .state()
        .toasts
        .iter()
        .map(|t| t.toast.title.clone())
        .collect();
    assert_eq!(
        titles,
        ["Available once the analysis catches up with the panels"]
    );
    // The mosaic is unaffected: picking it there frames it.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Mosaic && st.w.mosaic().viewer_frame().is_some()
    });
    h.run_steps(2);
    let before = h.state().w.mosaic().camera().unwrap();
    h.get_by_label("ROI 1").click();
    h.run_steps(3);
    let after = h.state().w.mosaic().camera().unwrap();
    assert!(after.zoom > before.zoom * 1.5, "{before:?} → {after:?}");
    assert_eq!(h.state().toasts.len(), 1, "no second toast");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn drawing_on_the_plan_adds_a_region() {
    let (root, s, jobs) = merged("window-draw-roi");
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    state.w.inspector_mut().reveal(ROIS);
    let mut h = harness(state);
    step_until(&mut h, |st| {
        plan_placed(st) && st.w.mosaic().source().is_some()
    });
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    assert!(h.query_by_label("None yet.").is_some());
    // Nothing forces the mosaic any more: the plan's own tool is armed.
    h.get_by_label("Draw ROI").click();
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    assert!(h.state().w.plan().drawing());
    assert!(!h.state().w.mosaic().drawing());
    let (rect, _) = h.state().w.plan().canvas_frame().expect("the plan drawn");
    let cam = h.state().w.plan().camera().expect("a camera");
    let (from, delta) = (
        rect.center() - egui::vec2(60.0, 30.0),
        egui::vec2(120.0, 60.0),
    );
    // Where the camera puts the two corners of the drag on the canvas.
    let at = |p: egui::Pos2| {
        let d = p - rect.center();
        (
            cam.centre.0 + f64::from(d.x) / cam.zoom,
            cam.centre.1 + f64::from(d.y) / cam.zoom,
        )
    };
    let (a, b) = (at(from), at(from + delta));
    drag(&mut h, from, delta);
    assert!(!h.state().w.plan().drawing(), "one rect per arming");
    let rois = h.state().w.mosaic().rois();
    assert_eq!(rois.len(), 1, "{rois:?}");
    assert_eq!(rois[0].name, "Region 1");
    let rect = rois[0].canvas;
    assert!(rect[2] > rect[0] && rect[3] > rect[1], "{rect:?}");
    // The drag mapped through the plan camera (whole pixels, outward).
    let want = [a.0, a.1, b.0, b.1];
    for (got, want) in rect.iter().zip(want) {
        assert!((*got as f64 - want).abs() < 1.5, "{rect:?} vs {want:?}");
    }
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    // Listed and selected; saved in the session's ui.
    let row = h.get_by_label("Region 1");
    assert!(row.accesskit_node().is_selected().unwrap_or(false));
    let saved = crate::rois::load_rois(&h.state().session.mosaic.config.ui);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].canvas, rect);
    // The mosaic draws what was drawn on the plan.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Mosaic && st.w.mosaic().viewer_frame().is_some()
    });
    h.run_steps(2);
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    assert!(painted_roi(&h, frame, "Region 1").is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_plan_draws_roi_outlines() {
    let (root, mut s, jobs) = merged("window-plan-rois");
    add_roi(&mut s);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    step_until(&mut h, plan_placed);
    h.run_steps(3);
    let (rect, _) = h.state().w.plan().canvas_frame().unwrap();
    let cam = h.state().w.plan().camera().expect("a camera");
    // The label sits just inside the ROI's top-left corner, as on the mosaic.
    let want = screen(rect, cam, (100.0, 100.0)) + egui::vec2(4.0, 2.0);
    let (got, _) = painted_roi(&h, rect, "ROI 1").expect("ROI label painted on the plan");
    assert!((got - want).length() < 1.0, "{got:?} != {want:?}");
    // R hides them on the plan too.
    h.key_press(egui::Key::R);
    h.run_steps(3);
    assert!(painted_roi(&h, rect, "ROI 1").is_none());
    h.key_press(egui::Key::R);
    h.run_steps(3);
    assert!(painted_roi(&h, rect, "ROI 1").is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_d_key_arms_the_draw_tool() {
    use egui::Key;
    let (root, s, jobs) = merged("window-d-key");
    let mut state = State::new(s, jobs);
    state.w.inspector_mut().reveal(ROIS);
    let mut h = harness(state);
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    h.run_steps(2);
    // On the mosaic: its tool.
    assert_eq!(h.state().w.mode(), CanvasMode::Mosaic);
    h.key_press(Key::D);
    h.run_steps(2);
    assert!(h.state().w.mosaic().drawing());
    assert!(!h.state().w.plan().drawing());
    // Esc cancels (and the tool goes), then on the plan: the plan's.
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().w.mosaic().drawing());
    h.key_press(Key::M);
    step_until(&mut h, plan_placed);
    h.run_steps(2);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    h.key_press(Key::D);
    h.run_steps(2);
    assert!(h.state().w.plan().drawing());
    assert!(!h.state().w.mosaic().drawing());
    // The button says so: disabled while armed, with the key in its tooltip
    // when not.
    assert!(disabled(&h, "Draw ROI"));
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().w.plan().drawing());
    assert!(!disabled(&h, "Draw ROI"));
    let at = h.get_by_label("Draw ROI").rect().center();
    assert!(tooltip_shows(&mut h, at, "Draw ROI (D)"));
    // Switching canvas disarms the one left behind.
    h.get_by_label("Draw ROI").click();
    h.run_steps(2);
    assert!(h.state().w.plan().drawing());
    h.get_by_label("Mosaic").click();
    h.run_steps(3);
    assert!(!h.state().w.plan().drawing() && !h.state().w.mosaic().drawing());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_d_key_reveals_the_rois_section_and_the_armed_tool_shows() {
    use egui::Key;
    let (root, s, jobs) = merged("window-d-reveals");
    let mut h = harness(State::new(s, jobs));
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    h.run_steps(2);
    // ROIs folded (Problems open), the inspector folded too.
    assert!(!h.state().w.inspector().open.contains(ROIS));
    h.key_press(Key::I);
    h.run_steps(2);
    assert!(h.state().w.inspector().folded);
    let centre = h.state().w.mosaic().viewer_frame().unwrap().0.center();
    let cursor = |h: &Harness<'static, State>| h.output().platform_output.cursor_icon;
    h.hover_at(centre);
    h.run_steps(2);
    assert_eq!(cursor(&h), egui::CursorIcon::Default);
    // D arms the tool and opens the section, so its hint (and Esc) show;
    // the pointer over the canvas is a crosshair.
    h.key_press(Key::D);
    h.run_steps(3);
    assert!(h.state().w.mosaic().drawing());
    assert!(!h.state().w.inspector().folded);
    assert!(h.state().w.inspector().open.contains(ROIS));
    h.get_by_label("Drag on the image to draw; Esc cancels.");
    let centre = h.state().w.mosaic().viewer_frame().unwrap().0.center();
    h.hover_at(centre);
    h.run_steps(2);
    assert_eq!(cursor(&h), egui::CursorIcon::Crosshair);
    // Esc disarms it: the plain pointer again.
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().w.mosaic().drawing());
    assert_eq!(cursor(&h), egui::CursorIcon::Default);
    // On the plan too.
    h.key_press(Key::M);
    step_until(&mut h, plan_placed);
    h.run_steps(2);
    h.key_press(Key::D);
    h.run_steps(2);
    assert!(h.state().w.plan().drawing());
    let centre = h.state().w.plan().canvas_frame().unwrap().0.center();
    h.hover_at(centre);
    h.run_steps(2);
    assert_eq!(cursor(&h), egui::CursorIcon::Crosshair);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_d_key_is_typing_in_a_text_field() {
    use egui::Key;
    let (root, s, jobs) = merged("window-d-typing");
    let mut state = State::new(s, jobs);
    state.w.inspector_mut().reveal(NOTES);
    let mut h = harness(state);
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    h.run_steps(2);
    h.get_by_label("Add notes").click();
    h.run_steps(2);
    h.get_by_role(Role::MultilineTextInput).focus();
    h.run_steps(2);
    h.key_press(Key::D);
    h.run_steps(2);
    assert!(!h.state().w.mosaic().drawing(), "typed, not a shortcut");
    let _ = std::fs::remove_dir_all(&root);
}

/// Two ROIs, "Core" (id 7) then "Halo" (id 9), in a merged session.
fn two_rois(tag: &str) -> (PathBuf, Harness<'static, State>) {
    let (root, mut s, jobs) = merged(tag);
    let roi = |id, name: &str, canvas| Roi {
        id,
        name: name.into(),
        note: String::new(),
        sky: None,
        canvas,
    };
    save_rois(
        &mut s.mosaic.config.ui,
        &[
            roi(7, "Core", [100, 100, 200, 160]),
            roi(9, "Halo", [0, 0, 64, 64]),
        ],
    );
    let mut state = State::new(s, jobs);
    state.w.inspector_mut().reveal(ROIS);
    let mut h = harness(state);
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    h.run_steps(3);
    (root, h)
}

/// The ids and names of the ROIs saved in the session's `ui`.
fn saved_rois(h: &Harness<'static, State>) -> Vec<(u64, String)> {
    crate::rois::load_rois(&h.state().session.mosaic.config.ui)
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect()
}

/// The detail line ROI row `name` reads (size, then state).
fn row_detail(h: &Harness<'static, State>, name: &str) -> String {
    h.get_by_label(name)
        .accesskit_node()
        .description()
        .unwrap_or_default()
}

/// Step until ROI row `name` reads `state` (bounded by [`PATIENCE`]).
fn step_until_row(h: &mut Harness<'static, State>, name: &str, state: &str) {
    let deadline = Instant::now() + PATIENCE;
    while !row_detail(h, name).ends_with(state) {
        assert!(
            Instant::now() < deadline,
            "{name} reads {:?}, never {state:?}",
            row_detail(h, name)
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Hover the row of `name` so its actions show.
fn hover_row(h: &mut Harness<'static, State>, name: &str) {
    let at = h.get_by_label(name).rect().center();
    h.hover_at(at);
    h.run_steps(2);
}

#[test]
fn deleting_an_roi_offers_undo() {
    let (root, mut h) = two_rois("window-roi-delete");
    // The highest id goes, so a reused id would show.
    hover_row(&mut h, "Halo");
    h.get_by_label("Delete Halo").click();
    h.run_steps(3);
    assert_eq!(saved_rois(&h), [(7, "Core".to_string())]);
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].toast.title, "Halo deleted");
    assert_eq!(toasts[0].toast.action.as_deref(), Some("Undo"));
    let Some(undo @ Undo::RoiDelete { .. }) = toasts.into_iter().next().and_then(|t| t.undo) else {
        panic!("an ROI undo");
    };
    // A region drawn meanwhile does not take the deleted one's id...
    {
        let st = h.state_mut();
        st.w.mosaic
            .on_drawn(&mut st.session, [0.0, 0.0, 40.0, 40.0], (9255, 18310));
    }
    let drawn = saved_rois(&h)[1].clone();
    assert!(drawn.0 > 9, "{drawn:?}");
    assert_eq!(drawn.1, "Region 1");
    // ...and Undo puts the deleted one back at its place, selected.
    {
        let st = h.state_mut();
        st.w.undo(undo, &mut st.session, &mut st.jobs);
    }
    h.run_steps(3);
    let names = saved_rois(&h);
    assert_eq!(names[1], (9, "Halo".to_string()), "{names:?}");
    assert_eq!(names.len(), 3);
    let row = h.get_by_label("Halo");
    assert!(row.accesskit_node().is_selected().unwrap_or(false));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn selection_survives_reorder_rename_and_other_deletes() {
    let (root, mut h) = two_rois("window-roi-selection");
    let selected = |h: &Harness<'static, State>, name: &str| {
        h.get_by_label(name)
            .accesskit_node()
            .is_selected()
            .unwrap_or(false)
    };
    h.get_by_label("Halo").click();
    h.run_steps(3);
    assert!(selected(&h, "Halo"));
    // Reordered: still Halo.
    hover_row(&mut h, "Halo");
    h.get_by_label("Move Halo up").click();
    h.run_steps(3);
    assert!(selected(&h, "Halo") && !selected(&h, "Core"));
    // Renamed: the same ROI stays selected.
    hover_row(&mut h, "Halo");
    h.get_by_label("Rename Halo").click();
    h.run_steps(3);
    h.get(
        By::new()
            .role(Role::TextInput)
            .predicate(|n| n.label().as_deref() != Some("Note")),
    )
    .type_text("Outer");
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(selected(&h, "Outer") && !selected(&h, "Core"));
    // Another one deleted: the selection stays on Outer.
    hover_row(&mut h, "Core");
    h.get_by_label("Delete Core").click();
    h.run_steps(3);
    assert_eq!(saved_rois(&h), [(9, "Outer".to_string())]);
    assert!(selected(&h, "Outer"));
    assert_eq!(h.state().w.mosaic().selected_roi(), Some(9));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reordering_and_renaming_persist() {
    let (root, mut h) = two_rois("window-roi-order");
    // Move Halo up: the order is saved, ids stay with their ROIs.
    hover_row(&mut h, "Halo");
    h.get_by_label("Move Halo up").click();
    h.run_steps(3);
    assert_eq!(
        saved_rois(&h),
        [(9, "Halo".to_string()), (7, "Core".to_string())]
    );
    // Rename Core.
    hover_row(&mut h, "Core");
    h.get_by_label("Rename Core").click();
    h.run_steps(3);
    h.get_by_role(Role::TextInput).type_text("Nebula core");
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(
        saved_rois(&h),
        [(9, "Halo".to_string()), (7, "Nebula core".to_string())]
    );
    // Written to the session's file with the window's save, ids included.
    {
        let st = h.state_mut();
        st.w.flush_save(&mut st.session);
    }
    let dir = h.state().session.mosaic.dir.clone();
    let back = MosaicConfig::load(&dir).unwrap();
    let rois = crate::rois::load_rois(&back.ui);
    let got: Vec<_> = rois.iter().map(|r| (r.id, r.name.as_str())).collect();
    assert_eq!(got, [(9, "Halo"), (7, "Nebula core")]);
    assert_eq!(rois[0].canvas, [0, 0, 64, 64]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_roi_is_gone() {
    let (root, mut h) = two_rois("window-no-merge-roi");
    assert!(h.query_by_label("Merge ROI").is_none());
    assert!(h.query_by_label("+ new ROI").is_none());
    h.get_by_label("Draw ROI");
    // Selecting a region merges nothing.
    h.get_by_label("Core").click();
    h.run_steps(3);
    assert!(!h.state().jobs.busy());
    assert!(labels(&h.state().jobs).is_empty());
    // Every row reads its state in text: "up to date" once its preview has
    // merged by itself, through no job.
    step_until_row(&mut h, "Core", "up to date");
    assert!(labels(&h.state().jobs).is_empty());
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
    // Chosen so: the two filters don't group the panels by themselves.
    crate::panel_edits::set_grouping_chosen(&mut s.mosaic.config.ui);
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

/// The Composites list's row called `name` (the chips are called the same,
/// but are no list items).
fn comp_row<'h>(h: &'h Harness<'static, State>, name: &'h str) -> egui_kittest::Node<'h> {
    h.get(By::new().role(Role::ListItem).label(name))
}

/// Hover the Composites row `name` so its actions show.
fn hover_comp_row(h: &mut Harness<'static, State>, name: &str) {
    let at = comp_row(h, name).rect().center();
    h.hover_at(at);
    h.run_steps(2);
}

/// The saved composites as (id, name, channel groups).
fn saved_comps(h: &Harness<'static, State>) -> Vec<(u64, String, Vec<Option<String>>)> {
    load_compositions(&h.state().session.mosaic.config.ui)
        .into_iter()
        .map(|c| {
            let groups = c
                .channels
                .iter()
                .map(|s| s.as_ref().map(|(g, _)| g.clone()));
            (c.id, c.name, groups.collect())
        })
        .collect()
}

fn some(g: &str) -> Option<String> {
    Some(g.to_string())
}

/// A button called `label` that is no toggle (the chips are called the
/// same).
fn button<'h>(h: &'h Harness<'static, State>, label: &'h str) -> egui_kittest::Node<'h> {
    h.get(
        By::new()
            .role(Role::Button)
            .label(label)
            .predicate(|n| n.toggled().is_none()),
    )
}

#[test]
fn the_plus_popover_creates_a_preset() {
    let root = temp_dir("window-plus");
    let s = two_group_session(&root);
    let dir = s.mosaic.dir.clone();
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    assert!(!h.state().w.inspector().is_open("composites"));
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(3);
    // Only what Ha and OIII allow, then Custom.
    button(&h, "HOO");
    for absent in ["RGB", "SHO", "HSO"] {
        assert!(
            h.query(By::new().role(Role::Button).label(absent))
                .is_none(),
            "{absent}"
        );
    }
    h.get_by_label("Custom");
    assert!(saved_comps(&h).is_empty(), "opening the popover makes none");
    button(&h, "HOO").click();
    h.run_steps(3);
    let hoo = vec![some("Ha"), some("OIII"), some("OIII")];
    assert_eq!(saved_comps(&h), [(1, "HOO".to_string(), hoo)]);
    let st = h.state();
    let comp = Some(ViewKind::Composition(1));
    assert_eq!(st.w.view(), comp.as_ref());
    assert_eq!(st.w.plan().view(), comp.as_ref());
    assert_eq!(st.w.mosaic().view(), comp.as_ref());
    assert!(st.w.inspector().is_open("composites"));
    // The popover closed; the chip and the selected row are there.
    assert!(h.query_by_label("Custom").is_none());
    assert!(
        comp_row(&h, "HOO")
            .accesskit_node()
            .is_selected()
            .unwrap_or(false)
    );
    // Again: a unique name.
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(3);
    button(&h, "HOO").click();
    h.run_steps(3);
    let names: Vec<(u64, String)> = saved_comps(&h).into_iter().map(|c| (c.0, c.1)).collect();
    assert_eq!(names, [(1, "HOO".into()), (2, "HOO 2".into())]);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(2)));
    // Saved.
    step_until(&mut h, |_| {
        MosaicConfig::load(&dir).is_ok_and(|c| load_compositions(&c.ui).len() == 2)
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_custom_form_creates_a_composite() {
    let root = temp_dir("window-custom");
    let s = two_group_session(&root);
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(3);
    // Nothing named, nothing mapped: Create is off.
    assert!(button(&h, "Create").accesskit_node().is_disabled());
    h.get_by_role_and_label(Role::TextInput, "Name").focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Name")
        .type_text("Mine");
    h.run_steps(2);
    assert!(
        button(&h, "Create").accesskit_node().is_disabled(),
        "named, but nothing mapped"
    );
    // R from OIII.
    button(&h, "R: None").click();
    h.run_steps(3);
    h.get_by_label("OIII").click();
    h.run_steps(3);
    assert!(!button(&h, "Create").accesskit_node().is_disabled());
    button(&h, "Create").click();
    h.run_steps(3);
    assert_eq!(
        saved_comps(&h),
        [(1, "Mine".to_string(), vec![some("OIII"), None, None])]
    );
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(1)));
    assert!(h.state().w.inspector().is_open("composites"));
    assert!(h.query_by_label("Custom").is_none(), "the popover closed");
    // A taken name cannot be created again.
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Name").focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Name")
        .type_text("Mine");
    h.run_steps(2);
    button(&h, "R: None").click();
    h.run_steps(3);
    h.get_by_label("Ha").click();
    h.run_steps(3);
    assert!(
        button(&h, "Create").accesskit_node().is_disabled(),
        "the name is taken"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The label of the widget with the keyboard focus.
fn focused_label(h: &Harness<'static, State>) -> Option<String> {
    h.query(By::new().predicate(|n| n.is_focused()))
        .and_then(|n| n.accesskit_node().label())
}

/// Press Tab until the focus is on `label` (bounded).
fn tab_to(h: &mut Harness<'static, State>, label: &str) {
    for _ in 0..30 {
        if focused_label(h).as_deref() == Some(label) {
            return;
        }
        h.key_press(egui::Key::Tab);
        h.run_steps(2);
    }
    panic!(
        "Tab never reached {label:?}; focus is on {:?}",
        focused_label(h)
    );
}

/// Focus the + chip and open its popover with `key`.
fn open_plus_by_keyboard(h: &mut Harness<'static, State>, key: egui::Key) {
    plus_chip(h).expect("a + chip").focus();
    h.run_steps(2);
    h.key_press(key);
    h.run_steps(3);
    h.get_by_label("Custom");
}

#[test]
fn the_plus_popover_works_from_the_keyboard() {
    let root = temp_dir("window-plus-keys");
    let s = two_group_session(&root);
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    // The chip shows "+" and is named for what it does, its popover shut.
    assert_eq!(plus_expanded(&h), Some(false));
    // Enter opens it and the focus lands on the first preset; Enter makes it.
    open_plus_by_keyboard(&mut h, egui::Key::Enter);
    assert_eq!(plus_expanded(&h), Some(true), "it says it is open");
    assert_eq!(focused_label(&h).as_deref(), Some("HOO"));
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(saved_comps(&h).len(), 1);
    assert_eq!(saved_comps(&h)[0].1, "HOO");
    assert!(h.query_by_label("Custom").is_none(), "the popover closed");
    assert_eq!(plus_expanded(&h), Some(false));
    // The focus is back on the + chip; Space opens it again, Escape closes
    // it and the focus is back again.
    assert_eq!(focused_label(&h).as_deref(), Some(NEW_COMPOSITE));
    h.key_press(egui::Key::Space);
    h.run_steps(3);
    h.get_by_label("Custom");
    assert_eq!(plus_expanded(&h), Some(true));
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label("Custom").is_none());
    assert_eq!(plus_expanded(&h), Some(false));
    assert_eq!(focused_label(&h).as_deref(), Some(NEW_COMPOSITE));
    assert_eq!(saved_comps(&h).len(), 1, "Escape makes nothing");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_click_elsewhere_keeps_the_focus_it_gave() {
    let root = temp_dir("window-plus-click");
    let s = two_group_session(&root);
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    h.state_mut().w.inspector_mut().reveal(MERGE_SETTINGS);
    h.run_steps(3);
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(3);
    h.get_by_label("Custom");
    // Clicking into the feather field closes the popover (a click outside)
    // and leaves the focus where the click put it.
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .click();
    h.run_steps(3);
    assert!(h.query_by_label("Custom").is_none(), "the popover closed");
    let field = h.get_by_role_and_label(Role::TextInput, "Feather (px)");
    assert!(field.is_focused(), "focus on the feather field");
    assert_ne!(focused_label(&h).as_deref(), Some(NEW_COMPOSITE));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_custom_form_works_from_the_keyboard() {
    let root = temp_dir("window-custom-keys");
    let s = two_group_session(&root);
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    open_plus_by_keyboard(&mut h, egui::Key::Enter);
    // Why Create is off shows without hovering.
    h.get_by_label("Give it a name.");
    // The name.
    tab_to(&mut h, "Name");
    h.get_by_role_and_label(Role::TextInput, "Name")
        .type_text("Keys");
    h.run_steps(2);
    h.get_by_label("Map at least one channel.");
    // R from OIII, by keyboard; the toggle says it is open, and the pick
    // collapses the in-place list.
    tab_to(&mut h, "R: None");
    assert_eq!(
        button(&h, "R: None").accesskit_node().data().is_expanded(),
        Some(false)
    );
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(
        button(&h, "R: None").accesskit_node().data().is_expanded(),
        Some(true)
    );
    tab_to(&mut h, "OIII");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(
        h.query(By::new().role(Role::Button).label("OIII"))
            .is_none(),
        "the submenu closed on the pick"
    );
    assert_eq!(
        button(&h, "R: OIII").accesskit_node().data().is_expanded(),
        Some(false)
    );
    // Create, by keyboard.
    tab_to(&mut h, "Create");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(
        saved_comps(&h),
        [(1, "Keys".to_string(), vec![some("OIII"), None, None])]
    );
    assert!(h.query_by_label("Custom").is_none());
    assert_eq!(focused_label(&h).as_deref(), Some(NEW_COMPOSITE));
    // A draft left open is dropped: close with Escape, reopen, it is empty.
    open_plus_by_keyboard(&mut h, egui::Key::Enter);
    tab_to(&mut h, "Name");
    h.get_by_role_and_label(Role::TextInput, "Name")
        .type_text("Draft");
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    open_plus_by_keyboard(&mut h, egui::Key::Enter);
    h.get_by_label("Give it a name.");
    let _ = std::fs::remove_dir_all(&root);
}

/// A composite mapping `group` to R.
fn comp_of(id: u64, name: &str, group: &str) -> Composition {
    let mut c = Composition::new(name);
    c.id = id;
    c.channels[0] = Some((group.into(), 1.0));
    c
}

#[test]
fn composites_rename_reorder_and_delete_with_undo() {
    let root = temp_dir("window-comp-list");
    let mut s = two_group_session(&root);
    save_compositions(
        &mut s.mosaic.config.ui,
        &[
            comp_of(1, "A", "Ha"),
            comp_of(2, "B", "OIII"),
            comp_of(3, "C", "Ha"),
        ],
    );
    let mut state = State::new(s, runner());
    state.w.inspector_mut().reveal(COMPOSITES);
    let mut h = harness(state);
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(3);
    let selected = |h: &Harness<'static, State>, name: &str| {
        comp_row(h, name)
            .accesskit_node()
            .is_selected()
            .unwrap_or(false)
    };
    // Clicking a row shows that composite, and opens its channel editor.
    comp_row(&h, "B").click();
    h.run_steps(3);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(2)));
    assert!(selected(&h, "B"));
    assert_eq!(h.get_all_by_role(Role::ComboBox).count(), 3, "R, G and B");
    assert!(h.query_by_label("Weight").is_none());
    // Reorder: B up. The order is saved, the view stays on B (its id).
    hover_comp_row(&mut h, "B");
    h.get_by_label("Move B up").click();
    h.run_steps(3);
    let order = |h: &Harness<'static, State>| -> Vec<String> {
        saved_comps(h).into_iter().map(|c| c.1).collect()
    };
    assert_eq!(order(&h), ["B", "A", "C"]);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(2)));
    // Rename: the id, and so the view, stay.
    hover_comp_row(&mut h, "B");
    h.get_by_label("Rename B").click();
    h.run_steps(3);
    h.get_by_role(Role::TextInput).type_text("Blue");
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(order(&h), ["Blue", "A", "C"]);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(2)));
    let key = h.state().w.view_key(&h.state().session.mosaic.config.ui);
    assert_eq!(key.as_deref(), Some("comp:2"));
    // A name another composite has is refused, with a toast.
    hover_comp_row(&mut h, "A");
    h.get_by_label("Rename A").click();
    h.run_steps(3);
    h.get_by_role(Role::TextInput).type_text("C");
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(order(&h), ["Blue", "A", "C"]);
    assert!(
        !std::mem::take(&mut h.state_mut().toasts).is_empty(),
        "the refusal is a toast"
    );
    // Channel editor (Blue is selected): G from Ha.
    h.get_all_by_role(Role::ComboBox).nth(1).unwrap().click();
    h.run_steps(3);
    h.get_by_label("Ha").click();
    h.run_steps(3);
    assert_eq!(
        saved_comps(&h).remove(0).2,
        vec![some("OIII"), some("Ha"), None]
    );
    // Delete the highest id (C): a toast with Undo, and a new composite
    // does not take C's id.
    hover_comp_row(&mut h, "C");
    h.get_by_label("Delete C").click();
    h.run_steps(3);
    assert_eq!(order(&h), ["Blue", "A"]);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(2)));
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].toast.title, "C deleted");
    assert_eq!(toasts[0].toast.action.as_deref(), Some("Undo"));
    let Some(undo @ Undo::CompositeDelete { .. }) = toasts.into_iter().next().and_then(|t| t.undo)
    else {
        panic!("a composite undo");
    };
    plus_chip(&h).expect("a + chip").click();
    h.run_steps(3);
    button(&h, "HOO").click();
    h.run_steps(3);
    let fresh = saved_comps(&h).pop().unwrap();
    assert_eq!((fresh.0, fresh.1.as_str()), (4, "HOO"));
    // Undo puts C back at its place with its id.
    {
        let st = h.state_mut();
        st.w.undo(undo, &mut st.session, &mut st.jobs);
    }
    h.run_steps(3);
    let all: Vec<(u64, String)> = saved_comps(&h).into_iter().map(|c| (c.0, c.1)).collect();
    assert_eq!(
        all,
        [
            (2, "Blue".into()),
            (1, "A".into()),
            (3, "C".into()),
            (4, "HOO".into())
        ]
    );
    // Delete the shown one: the first group shows.
    h.state_mut().w.select_view(ViewKind::Composition(2));
    h.run_steps(3);
    hover_comp_row(&mut h, "Blue");
    h.get_by_label("Delete Blue").click();
    h.run_steps(3);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Group("Ha".into())));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composite_with_a_missing_group_says_so() {
    let root = temp_dir("window-comp-missing");
    let mut s = two_group_session(&root);
    save_compositions(
        &mut s.mosaic.config.ui,
        &[comp_of(1, "Gone", "SII"), comp_of(2, "Fine", "Ha")],
    );
    let mut state = State::new(s, runner());
    state.w.inspector_mut().reveal(COMPOSITES);
    let mut h = harness(state);
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(3);
    let detail = |h: &Harness<'static, State>, name: &str| {
        comp_row(h, name)
            .accesskit_node()
            .description()
            .unwrap_or_default()
    };
    assert_eq!(detail(&h, "Gone"), "missing group SII");
    assert_eq!(detail(&h, "Fine"), "R Ha · G None · B None");
    // The mapping is kept, and the editor shows the group as missing.
    comp_row(&h, "Gone").click();
    h.run_steps(3);
    let r = h
        .get_all_by_role(Role::ComboBox)
        .next()
        .expect("R's editor");
    assert_eq!(r.accesskit_node().value().as_deref(), Some("SII (missing)"));
    assert_eq!(saved_comps(&h)[0].2, vec![some("SII"), None, None]);
    // The group comes back (named SII by hand): nothing is missing.
    {
        let st = h.state_mut();
        for p in &mut st.session.mosaic.config.panels[6..] {
            p.manual_group = Some("SII".into());
        }
        st.session.groups = st.session.mosaic.groups().ok();
    }
    h.run_steps(3);
    assert_eq!(detail(&h, "Gone"), "R SII · G None · B None");
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
    h.state_mut().w.select_view(ViewKind::Composition(2));
    h.state_mut().w.inspector_mut().reveal("composites");
    h.run_steps(2);
    // A goes: B, shown, stays shown.
    hover_comp_row(&mut h, "A");
    h.get_by_label("Delete A").click();
    h.run_steps(2);
    let st = h.state();
    let names: Vec<String> = load_compositions(&st.session.mosaic.config.ui)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["B"]);
    let b = Some(ViewKind::Composition(2));
    assert_eq!(st.w.view(), b.as_ref());
    assert_eq!(st.w.plan().view(), b.as_ref());
    assert_eq!(st.w.mosaic().view(), b.as_ref());
    // B goes too: the first group shows.
    hover_comp_row(&mut h, "B");
    h.get_by_label("Delete B").click();
    h.run_steps(2);
    let ha = Some(ViewKind::Group("Ha".into()));
    assert_eq!(h.state().w.view(), ha.as_ref());
    assert_eq!(h.state().w.plan().view(), ha.as_ref());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn old_composites_get_ids_and_keep_their_view() {
    use crate::stretch_panel::{PLAN_STRETCH_KEY, save_stretch_at, saved_stretch_at};
    use mmm_core::stretch::StretchParams;

    let root = temp_dir("window-old-composites");
    let mut s = two_group_session(&root);
    // What an earlier mmm saved: composites without ids, the shown view and
    // a plan stretch keyed by the composite's name.
    s.mosaic.config.ui = serde_json::json!({ "compositions": [
        { "name": "A", "channels": [["Ha", 1.0], null, null] },
        { "name": "B", "channels": [null, ["OIII", 1.0], null] },
    ]});
    let stretch = [StretchParams {
        black: 0.1,
        white: 0.9,
        midtones: 0.3,
    }; 3];
    save_stretch_at(
        &mut s.mosaic.config.ui,
        PLAN_STRETCH_KEY,
        "comp:B",
        Some(&stretch),
    );
    let saved = view_state::ViewState {
        view: Some("comp:B".into()),
        ..Default::default()
    };
    saved.save(&mut s.mosaic.config.ui);
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    let st = h.state();
    let list = load_compositions(&st.session.mosaic.config.ui);
    let ids: Vec<u64> = list.iter().map(|c| c.id).collect();
    assert_eq!(ids, [1, 2], "fresh ids, in order");
    // It opens on B, with B's stretch, now under B's id.
    let b = Some(ViewKind::Composition(2));
    assert_eq!(st.w.view(), b.as_ref());
    assert_eq!(st.w.plan().view(), b.as_ref());
    let ui = &st.session.mosaic.config.ui;
    assert_eq!(st.w.view_key(ui).as_deref(), Some("comp:2"));
    assert_eq!(
        saved_stretch_at(ui, PLAN_STRETCH_KEY, "comp:2").as_deref(),
        Some(&stretch[..])
    );
    assert!(saved_stretch_at(ui, PLAN_STRETCH_KEY, "comp:B").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn views_follow_a_composite_by_id_across_reorder_and_delete() {
    let root = temp_dir("window-composite-ids");
    let mut s = two_group_session(&root);
    let mk = |name: &str, g: &str| {
        let mut c = Composition::new(name);
        c.channels[0] = Some((g.into(), 1.0));
        c
    };
    save_compositions(
        &mut s.mosaic.config.ui,
        &[mk("A", "Ha"), mk("B", "OIII"), mk("C", "Ha")],
    );
    let mut h = harness(State::new(s, runner()));
    step_until(&mut h, |st| st.w.view().is_some());
    let list = load_compositions(&h.state().session.mosaic.config.ui);
    let id = |name: &str| list.iter().find(|c| c.name == name).unwrap().id;
    let (a, b, c) = (id("A"), id("B"), id("C"));
    assert!(a != b && b != c && a != c);
    h.state_mut().w.select_view(ViewKind::Composition(b));
    h.state_mut().w.inspector_mut().reveal("composites");
    h.run_steps(2);
    // Reordered: B is still the one shown.
    let mut reordered = load_compositions(&h.state().session.mosaic.config.ui);
    reordered.reverse();
    save_compositions(&mut h.state_mut().session.mosaic.config.ui, &reordered);
    h.run_steps(2);
    let shown = Some(ViewKind::Composition(b));
    assert_eq!(h.state().w.view(), shown.as_ref());
    assert_eq!(h.state().w.plan().view(), shown.as_ref());
    assert_eq!(h.state().w.mosaic().view(), shown.as_ref());
    let key = h.state().w.view_key(&h.state().session.mosaic.config.ui);
    assert_eq!(key, Some(format!("comp:{b}")));
    // The editors now run C, B, A. C goes: B stays shown.
    hover_comp_row(&mut h, "C");
    h.get_by_label("Delete C").click();
    h.run_steps(2);
    let names: Vec<String> = load_compositions(&h.state().session.mosaic.config.ui)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["B", "A"]);
    assert_eq!(h.state().w.view(), shown.as_ref());
    // B goes: the first group shows; A keeps its id.
    h.run_steps(2);
    hover_comp_row(&mut h, "B");
    h.get_by_label("Delete B").click();
    h.run_steps(2);
    let ha = Some(ViewKind::Group("Ha".into()));
    assert_eq!(h.state().w.view(), ha.as_ref());
    let left = load_compositions(&h.state().session.mosaic.config.ui);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, a);
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
    let deadline = Instant::now() + crate::testutil::PATIENCE;
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
    h.get_by_label("Plan");
    let list = h.state().w.list_rect.expect("the list beside the canvas");
    assert!(list.left() < h.get_by_label("Plan").rect().left());
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
    h.get_by_label("Plan");
    // Plan / Mosaic leaves the list beside the canvas.
    h.state_mut().w.set_mode(CanvasMode::Plan);
    h.run_steps(2);
    assert!(h.state().w.panel_list_open);
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
    let export_block = |st: &State| st.w.export_block_reason(&st.session, &st.jobs);
    assert_eq!(
        export_block(h.state()),
        Some("Nothing is merged yet."),
        "no analysis running"
    );
    // Pressed: it starts, and the mosaic tracks it.
    let again = h.state_mut().jobs.hold();
    h.get_by_label("Analyse").click();
    h.run_steps(2);
    assert!(h.state().jobs.analyzing());
    assert_eq!(
        export_block(h.state()),
        Some("Waiting for the analysis to finish."),
        "the analysis runs"
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
    let State {
        w, session, jobs, ..
    } = h.state_mut();
    w.open_export(session, jobs);
    h.run_steps(4);
    assert!(h.state().w.export_open(), "the dialog is open");
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
        !st.jobs.busy() && st.session.merged && !super::steps::merge_needed(&st.session)
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
    step_until(&mut h, |st| st.w.view().is_some());
    let State {
        w, session, jobs, ..
    } = h.state_mut();
    w.open_export(session, jobs);
    h.run_steps(2);
    assert!(h.state().w.export_open());
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
        st.w.export_block_reason(&st.session, &st.jobs).is_none()
            && st.w.mosaic().viewer_frame().is_some()
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
    assert!(!h.state().w.export_open());
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
    state.w.select_view(ViewKind::Composition(1));
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
    let (got, _) = painted_roi(&h, mosaic, "ROI 1").expect("ROI label painted");
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
    let target = ExportTarget::Group(SINGLE_GROUP.into());
    let id = jobs.start_export(&s, target, out, mmm_core::export::ExportFormat::Fits, false);
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
fn a_merge_waits_behind_an_export() {
    use crate::jobs::JobEvent;
    let (root, mut s, mut jobs) = analyzed("steps-merge-after-export");
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    // An export of Ha runs (held); a merge of Ha is pressed meanwhile.
    let gate = jobs.hold_exports();
    let out = root.join("ha.fits");
    let ha = ExportTarget::Group("Ha".into());
    let export = jobs.start_export(&s, ha, out, mmm_core::export::ExportFormat::Fits, false);
    let merge = jobs.start_blend(&mut s, "Ha", None);
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    let want = StripStep {
        label: "Merge queued after export".into(),
        state: StepState::Working { fraction: None },
        cancellable: true,
        tooltip: None,
    };
    assert_eq!(got[MERGE_STEP], want);
    // Cancel from the step drops the queued merge and spares the export.
    super::top_bar::cancel_step(MERGE_STEP, &mut jobs);
    let events = jobs.poll(&mut s);
    assert!(events.contains(&JobEvent::Cancelled { id: merge }));
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    assert_ne!(got[MERGE_STEP].label, "Merge queued after export");
    // A merge of another group runs beside the export, and the step shows
    // it (with Ha's merge queued again behind the export).
    let held = jobs.hold();
    let oiii = jobs.start_blend(&mut s, "OIII", None);
    let merge = jobs.start_blend(&mut s, "Ha", None);
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    assert_eq!(got[MERGE_STEP].label, "Merging OIII · 1 of 2 · 0 %");
    // Ha's merge runs once the export has ended.
    held.open();
    gate.open();
    let events = drain(&mut jobs, &mut s);
    let finished = |id: u64| {
        events
            .iter()
            .position(|e| matches!(e, JobEvent::Finished { id: i, .. } if *i == id))
    };
    let (export, merge) = (finished(export), finished(merge));
    assert!(finished(oiii).is_some(), "{events:?}");
    assert!(export.is_some() && merge > export, "{events:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_analysis_waits_behind_an_export() {
    use crate::jobs::JobEvent;
    let (root, mut s, mut jobs) = analyzed("steps-analysis-after-export");
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    // An export of Ha runs (held); an analysis is asked for meanwhile.
    let gate = jobs.hold_exports();
    let out = root.join("ha.fits");
    let ha = ExportTarget::Group("Ha".into());
    let fits = mmm_core::export::ExportFormat::Fits;
    let export = jobs.start_export(&s, ha, out.clone(), fits, false);
    let analysis = jobs.start_analyze(&mut s);
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    let want = StripStep {
        label: "Analysis queued after export".into(),
        state: StepState::Working { fraction: None },
        cancellable: true,
        tooltip: None,
    };
    assert_eq!(got[CHECKS_STEP], want);
    // Cancel from the step drops the queued analysis and spares the export.
    super::top_bar::cancel_step(CHECKS_STEP, &mut jobs);
    let events = jobs.poll(&mut s);
    assert!(events.contains(&JobEvent::Cancelled { id: analysis }));
    assert!(!events.contains(&JobEvent::Cancelled { id: export }));
    let got = steps(&s, &jobs, &Pipeline::new(), &ChecksEngine::new());
    assert_ne!(got[CHECKS_STEP].label, "Analysis queued after export");
    gate.open();
    let events = drain(&mut jobs, &mut s);
    assert!(
        events.contains(&JobEvent::Finished {
            id: export,
            label: "Export Ha".into()
        }),
        "{events:?}"
    );
    assert!(out.is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_export_step_says_exported() {
    use crate::session::Stamps;
    let (root, mut s, mut jobs) = analyzed("steps-exported");
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    s.results = classify_results(&s);
    let export = |s: &OpenSession, jobs: &JobRunner| {
        steps(s, jobs, &Pipeline::new(), &ChecksEngine::new())[EXPORT_STEP].clone()
    };
    let stamp = |blended_at, exported_at| Stamps {
        blended_at,
        exported_at,
    };
    let todo = strip("Export", StepState::Todo);
    let exported = strip("Exported", StepState::Done);
    // Ha is merged and never exported (OIII, not merged, is not ready).
    s.stamps.insert("Ha".into(), stamp(Some(100), None));
    assert_eq!(export(&s, &jobs), todo);
    // Exported since its merge.
    s.stamps.insert("Ha".into(), stamp(Some(100), Some(100)));
    assert_eq!(export(&s, &jobs), exported);
    // Merged again since.
    s.stamps.insert("Ha".into(), stamp(Some(200), Some(100)));
    assert_eq!(export(&s, &jobs), todo);
    s.stamps.insert("Ha".into(), stamp(Some(200), Some(300)));
    assert_eq!(export(&s, &jobs), exported);
    // Every group ready must have been exported: OIII merged, not yet.
    jobs.start_blend(&mut s, "OIII", None);
    let _ = drain(&mut jobs, &mut s);
    s.results = classify_results(&s);
    s.stamps.insert("OIII".into(), stamp(Some(400), None));
    assert_eq!(export(&s, &jobs), todo);
    s.stamps.insert("OIII".into(), stamp(Some(400), Some(500)));
    assert_eq!(export(&s, &jobs), exported);
    // While exports run, the step says so, and its Cancel cancels them all
    // (a composite's too).
    let gate = jobs.hold();
    let fits = mmm_core::export::ExportFormat::Fits;
    let ha = ExportTarget::Group("Ha".into());
    let first = jobs.start_export(&s, ha, root.join("ha.fits"), fits, false);
    let hoo = ExportTarget::Composite {
        id: 1,
        name: "HOO".into(),
        channels: [Some("Ha".into()), Some("OIII".into()), Some("OIII".into())],
    };
    let second = jobs.start_export(&s, hoo, root.join("hoo.fits"), fits, false);
    let got = export(&s, &jobs);
    assert_eq!(
        (got.label.as_str(), got.cancellable),
        ("Exporting 1 of 2", true)
    );
    super::top_bar::cancel_step(EXPORT_STEP, &mut jobs);
    gate.open();
    let events = drain(&mut jobs, &mut s);
    for id in [first, second] {
        assert!(events.contains(&JobEvent::Cancelled { id }), "{events:?}");
    }
    assert_eq!(export(&s, &jobs), exported);
    // An out-of-date group is not ready, so it does not count; with none
    // ready, nothing reads as exported.
    mmm_core::mosaic::mark_needs_analyze(&mut s.mosaic, &["OIII".to_string()]).unwrap();
    assert_eq!(export(&s, &jobs), exported, "Ha still is");
    mmm_core::mosaic::mark_needs_analyze(&mut s.mosaic, &["Ha".to_string()]).unwrap();
    assert_eq!(export(&s, &jobs), todo);
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

#[test]
fn an_analysis_change_raises_an_undo_toast() {
    use mmm_core::photometry::GainMode;

    let (root, s, jobs) = analyzed("window-analysis-toast");
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    let edit = |h: &mut Harness<'static, State>, gain| {
        let st = h.state_mut();
        let mut a = st.session.mosaic.config.analyze.clone();
        a.gain = gain;
        st.w.mosaic.edit_analysis(a, &mut st.session, &mut st.jobs)
    };
    assert!(edit(&mut h, GainMode::Unity).is_some());
    h.run_steps(3);
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].toast.title, "Analysis settings changed");
    assert_eq!(toasts[0].toast.action.as_deref(), Some("Undo"));
    let Some(Undo::Analyze(prev)) = toasts.into_iter().next().and_then(|t| t.undo) else {
        panic!("an analysis undo");
    };
    assert_eq!(prev.gain, GainMode::Fit);
    // Undo restores the values.
    {
        let st = h.state_mut();
        st.w.undo(Undo::Analyze(prev), &mut st.session, &mut st.jobs);
        assert_eq!(st.session.mosaic.config.analyze.gain, GainMode::Fit);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_draft_edit_is_saved_with_the_window() {
    let (root, s, jobs) = analyzed("window-draft-save");
    let dir = s.mosaic.dir.clone();
    let mut state = State::new(s, jobs);
    state.w.inspector_mut().reveal(MERGE_SETTINGS);
    let mut h = harness(state);
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Defect veto")
        .click();
    h.run_steps(2);
    h.get_by_label("Off").click();
    h.run_steps(2);
    assert!(h.state().w.save.pending(), "the draft awaits its save");
    {
        let st = h.state_mut();
        st.w.flush_save(&mut st.session);
    }
    let saved = MosaicConfig::load(&dir).unwrap();
    assert!(!crate::blend_settings::load_draft(&saved).defect_veto);
    assert!(saved.blend.defect_veto, "the config's settings are kept");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_old_session_shows_merged() {
    // A merged session saved before drafts: no `merge_draft` key.
    let (root, s, jobs) = merged("window-old-merged");
    assert!(s.mosaic.config.ui.get("merge_draft").is_none());
    let mut state = State::new(s, jobs);
    state.w.inspector_mut().reveal(MERGE_SETTINGS);
    let mut h = harness(state);
    h.run_steps(3);
    assert_eq!(shown_steps(h.state())[2], strip("Merged", StepState::Done));
    assert_eq!(
        h.query_all_by_label(crate::blend_settings::CHANGED_MARK)
            .count(),
        0
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_analysis_edit_raises_a_problem_toast() {
    use mmm_core::photometry::GainMode;

    let (root, s, jobs) = analyzed("window-analysis-problem");
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    {
        let st = h.state_mut();
        let file = st.session.mosaic.dir.join(mmm_core::mosaic::CONFIG_FILE);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        let mut a = st.session.mosaic.config.analyze.clone();
        a.gain = GainMode::Unity;
        st.w.mosaic.edit_analysis(a, &mut st.session, &mut st.jobs);
    }
    h.run_steps(3);
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].toast.tone, mega_ui::theme::Tone::Problem);
    assert!(
        toasts[0]
            .toast
            .title
            .starts_with("Could not change the settings")
    );
    assert!(toasts[0].undo.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// The merge settings draft of `s` with its feather width set to `px`.
fn set_draft_feather(s: &mut OpenSession, px: f32) {
    let mut draft = crate::blend_settings::load_draft(&s.mosaic.config);
    draft.feather_px = px;
    crate::blend_settings::save_draft(&mut s.mosaic.config.ui, &draft);
}

/// The feather width of `group`'s last whole-canvas merge.
fn merged_feather(s: &OpenSession, group: &str) -> Option<f32> {
    mmm_core::mosaic::last_merge_settings(&s.mosaic.config, group).map(|b| b.feather_px)
}

/// Step until nothing in the window blocks a merge.
fn until_mergeable(h: &mut Harness<'static, State>) {
    step_until(h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
}

/// The accessible description of the chip labelled `label` (a toggle).
fn chip_state(h: &Harness<'static, State>, label: &str) -> Option<String> {
    h.get_by_label(label).accesskit_node().description()
}

#[test]
fn merge_commits_the_draft_and_merges_what_needs_it() {
    let (root, mut s, jobs) = merged("window-merge-commits");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut h = harness(State::new(s, jobs));
    until_mergeable(&mut h);
    h.run_steps(3);
    // Merge is offered, and the draft is not yet the config's.
    assert!(h.state().w.can_merge(&h.state().session, &h.state().jobs));
    assert_eq!(h.state().session.mosaic.config.blend.feather_px, old);
    let gate = h.state_mut().jobs.hold();
    click_rightmost(&h, "Merge");
    h.run_steps(2);
    assert_eq!(
        h.state().session.mosaic.config.blend.feather_px,
        old + 100.0
    );
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let st = h.state();
    for g in ["Ha", "OIII"] {
        assert_eq!(merged_feather(&st.session, g), Some(old + 100.0), "{g}");
        assert!(!draft_changed(&st.session, g));
    }
    assert_eq!(shown_steps(st)[2], strip("Merged", StepState::Done));
    // Only what needs it merges: Ha went out of date.
    h.state_mut()
        .session
        .results
        .insert("Ha".into(), ResultState::Stale);
    until_mergeable(&mut h);
    let gate = h.state_mut().jobs.hold();
    click_rightmost(&h, "Merge");
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn settings_changed_is_not_out_of_date() {
    let (root, mut s, jobs) = merged("window-settings-changed");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut h = harness(State::new(s, jobs));
    until_mergeable(&mut h);
    h.run_steps(3);
    let st = h.state();
    assert_eq!(
        shown_steps(st)[MERGE_STEP],
        strip("Settings changed", StepState::Next)
    );
    assert!(draft_changed(&st.session, "Ha") && !out_of_date(&st.session, "Ha"));
    // The main button merges; export stays allowed.
    assert!(!rightmost_disabled(&h, "Merge"));
    assert!(st.w.export_block_reason(&st.session, &st.jobs).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn out_of_date_wins() {
    let (root, mut s, jobs) = merged("window-out-of-date-wins");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    s.results.insert("Ha".into(), ResultState::Stale);
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    assert_eq!(
        shown_steps(h.state())[MERGE_STEP],
        strip("Out of date", StepState::Next)
    );
    // A group needing analysis is out of date too, though its result is
    // complete.
    let st = h.state_mut();
    st.session
        .results
        .insert("Ha".into(), ResultState::Complete);
    st.session.mosaic.config.needs_analyze.insert("OIII".into());
    assert!(out_of_date(&st.session, "OIII"));
    assert!(!draft_changed(&st.session, "OIII"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_this_group_merges_one() {
    let (root, mut s, jobs) = merged("window-merge-this-group");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut h = harness(State::new(s, jobs));
    until_mergeable(&mut h);
    step_until(&mut h, |st| st.w.view().is_some());
    let gate = h.state_mut().jobs.hold();
    h.get_by_label("OIII · 4").click_secondary();
    h.run_steps(3);
    h.get_by_label("Merge this group").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge OIII"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let st = h.state();
    assert_eq!(merged_feather(&st.session, "OIII"), Some(old + 100.0));
    assert_eq!(merged_feather(&st.session, "Ha"), Some(old));
    assert!(draft_changed(&st.session, "Ha"));
    // Merge then takes up what is left.
    until_mergeable(&mut h);
    let gate = h.state_mut().jobs.hold();
    click_rightmost(&h, "Merge");
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_running_merge_keeps_its_settings_across_a_commit() {
    let (root, s, jobs) = merged("window-merge-keeps-settings");
    let old = s.mosaic.config.blend.feather_px;
    let mut h = harness(State::new(s, jobs));
    until_mergeable(&mut h);
    step_until(&mut h, |st| st.w.view().is_some());
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge_group(&mut st.session, &mut st.jobs, "OIII");
    }
    assert_eq!(labels(&h.state().jobs), ["Merge OIII"]);
    {
        let st = h.state_mut();
        set_draft_feather(&mut st.session, old + 100.0);
        st.w.merge_group(&mut st.session, &mut st.jobs, "Ha");
    }
    assert_eq!(labels(&h.state().jobs), ["Merge OIII", "Merge Ha"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let st = h.state();
    assert_eq!(
        merged_feather(&st.session, "OIII"),
        Some(old),
        "B keeps its settings"
    );
    assert!(draft_changed(&st.session, "OIII"));
    assert_eq!(st.session.results.get("OIII"), Some(&ResultState::Complete));
    assert_eq!(merged_feather(&st.session, "Ha"), Some(old + 100.0));
    assert_eq!(st.session.mosaic.config.blend.feather_px, old + 100.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn chips_mark_their_state() {
    let (root, mut s, jobs) = merged("window-chip-marks");
    let old = s.mosaic.config.blend.feather_px;
    let mut comp = Composition::new("HO");
    comp.channels = [Some(("Ha".into(), 1.0)), Some(("OIII".into(), 1.0)), None];
    save_compositions(&mut s.mosaic.config.ui, &[comp]);
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    assert_eq!(chip_state(&h, "Ha · 4"), None);
    assert_eq!(chip_state(&h, "OIII · 4"), None);
    // Settings changed: both groups, and the composite.
    set_draft_feather(&mut h.state_mut().session, old + 100.0);
    h.run_steps(2);
    let changed = Some("settings changed".to_string());
    assert_eq!(chip_state(&h, "Ha · 4"), changed);
    assert_eq!(chip_state(&h, "OIII · 4"), changed);
    assert_eq!(chip_state(&h, "HO"), changed);
    // Out of date wins, and the composite takes the worst.
    h.state_mut()
        .session
        .results
        .insert("Ha".into(), ResultState::Stale);
    h.run_steps(2);
    let stale = Some("out of date".to_string());
    assert_eq!(chip_state(&h, "Ha · 4"), stale);
    assert_eq!(chip_state(&h, "OIII · 4"), changed);
    assert_eq!(chip_state(&h, "HO"), stale);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_canvas_tags_say_why() {
    let (root, s, jobs) = merged("window-canvas-tags");
    let old = s.mosaic.config.blend.feather_px;
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("Ha".into()));
    let mut h = harness(state);
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    h.run_steps(3);
    let shown = |h: &Harness<'static, State>, t: &str| h.query_by_label(t).is_some();
    // Up to date: no tag.
    assert!(!shown(&h, "Settings changed since this merge"));
    // Settings changed since the merge.
    set_draft_feather(&mut h.state_mut().session, old + 100.0);
    h.run_steps(3);
    assert!(shown(&h, "Settings changed since this merge"));
    // Clicking it reveals Merge settings.
    assert!(!h.state().w.inspector().is_open("merge-settings"));
    h.get_by_label("Settings changed since this merge").click();
    h.run_steps(3);
    assert!(h.state().w.inspector().is_open("merge-settings"));
    // A merge that did not finish.
    h.state_mut()
        .session
        .results
        .insert("Ha".into(), ResultState::Partial);
    h.run_steps(3);
    assert!(shown(&h, "Out of date — merge didn't finish"));
    // The panels changed after the analysis.
    h.state_mut()
        .session
        .mosaic
        .config
        .needs_analyze
        .insert("Ha".into());
    h.run_steps(3);
    assert!(shown(&h, "Out of date — panels changed"));
    // The analysis settings changed.
    {
        use mmm_core::photometry::GainMode;
        let cfg = &mut h.state_mut().session.mosaic.config;
        cfg.analyze.gain = match cfg.analyze.gain {
            GainMode::Unity => GainMode::Fit,
            _ => GainMode::Unity,
        };
    }
    h.run_steps(3);
    assert!(shown(&h, "Out of date — analysis settings changed"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_group_never_merged_says_so() {
    let (root, mut s, mut jobs) = analyzed("window-not-merged-tag");
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    s.merged = any_merged(&s);
    s.results = classify_results(&s);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("OIII".into()));
    let mut h = harness(state);
    h.run_steps(5);
    h.get_by_label("Not merged yet");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_settings_open_when_analysis_first_finishes() {
    let root = temp_dir("window-settings-open");
    let mut state = State::new(two_group_session(&root), runner());
    state.run_pipeline = true;
    let mut h = harness(state);
    assert!(!h.state().w.inspector().is_open("merge-settings"));
    step_until(&mut h, |st| {
        st.w.mosaic()
            .merge_block_reason(&st.session, &st.jobs)
            .is_none()
    });
    assert!(!h.state().session.merged);
    assert!(h.state().w.inspector().is_open("merge-settings"));
    // Closed by the user, it stays closed through a later analysis.
    h.state_mut().w.inspector_mut().toggle("merge-settings");
    assert!(!h.state().w.inspector().is_open("merge-settings"));
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    step_until(&mut h, |st| !st.jobs.busy());
    assert!(!h.state().w.inspector().is_open("merge-settings"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn analysis_in_a_merged_session_leaves_merge_settings_alone() {
    let (root, s, jobs) = merged("window-settings-merged-analysis");
    let mut h = harness(State::new(s, jobs));
    h.run_steps(3);
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    step_until(&mut h, |st| !st.jobs.busy());
    assert!(!h.state().w.inspector().is_open("merge-settings"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Edit the session's panels through the window's panel list, as the
/// Panels tab does (`f` changes the list), and step the window until the
/// edit has applied. The frames reap the jobs it cancels and tell the
/// window and the pipeline, as the shell does; an edit driven outside them
/// could wait forever for a job nobody reaps, or hide its end from the
/// pipeline.
fn edit_panels(h: &mut Harness<'static, State>, f: impl FnOnce(&mut Vec<PanelEntry>)) {
    {
        let st = h.state_mut();
        let mut edit = crate::panel_edits::PanelsEdit::of(&st.session);
        f(&mut edit.panels);
        st.w.panel_list_mut()
            .submit_edit(edit, &mut st.session, &mut st.jobs);
    }
    step_to(h, |st| !st.w.panels_editing());
}

/// A merged session shown as Ha's mosaic, the automatic pipeline running.
fn shown_merged(tag: &str) -> (PathBuf, Harness<'static, State>) {
    let (root, s, jobs) = merged(tag);
    let mut state = State::new(s, jobs);
    state.run_pipeline = true;
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("Ha".into()));
    let mut h = harness(state);
    until_mergeable(&mut h);
    (root, h)
}

/// Step until the pipeline has re-analysed (`applied`: the edit is in) and
/// is idle, then classify the results as the summary writer would.
fn reanalysed(h: &mut Harness<'static, State>, applied: impl Fn(&State) -> bool) {
    step_to(h, |st| {
        applied(st) && !crate::pipeline::wants_analysis(&st.session) && !st.jobs.busy()
    });
    let st = h.state_mut();
    st.session.results = classify_results(&st.session);
    h.run_steps(4);
}

#[test]
fn the_tag_after_a_panel_change_says_panels() {
    let (root, mut h) = shown_merged("window-tag-panels");
    edit_panels(&mut h, |p| {
        let last = p
            .iter()
            .rposition(|e| {
                e.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("Ha")
            })
            .unwrap();
        p.remove(last);
    });
    reanalysed(&mut h, |st| st.session.mosaic.config.panels.len() == 7);
    // The pipeline cleared needs_analyze; Ha's result is still out of date.
    assert!(!h.state().session.mosaic.config.needs_analyze.contains("Ha"));
    assert!(out_of_date(&h.state().session, "Ha"));
    h.get_by_label("Out of date — panels changed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_tag_after_an_analysis_change_says_analysis_settings() {
    use mmm_core::photometry::GainMode;
    let (root, mut h) = shown_merged("window-tag-analysis");
    {
        let st = h.state_mut();
        let mut a = st.session.mosaic.config.analyze.clone();
        a.gain = GainMode::Unity;
        st.w.mosaic.edit_analysis(a, &mut st.session, &mut st.jobs);
    }
    reanalysed(&mut h, |st| {
        st.session.mosaic.config.analyze.gain == GainMode::Unity
    });
    assert!(!h.state().session.mosaic.config.needs_analyze.contains("Ha"));
    assert!(out_of_date(&h.state().session, "Ha"));
    h.get_by_label("Out of date — analysis settings changed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_tag_after_a_cancelled_merge_says_it_did_not_finish() {
    let (root, mut s, mut jobs) = merged("window-tag-cancelled");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let draft = crate::blend_settings::load_draft(&s.mosaic.config);
    mmm_core::mosaic::commit_blend_settings(&mut s.mosaic, draft).unwrap();
    // A fresh merge (new settings) stopped part-way.
    let stepper = jobs.hold_progress();
    let id = jobs.start_blend(&mut s, "Ha", None);
    let deadline = Instant::now() + PATIENCE;
    while !jobs.views().iter().any(|v| v.id == id && v.done > 0) {
        assert!(Instant::now() < deadline, "the merge never progressed");
        let _ = jobs.poll(&mut s);
        std::thread::sleep(Duration::from_millis(5));
    }
    jobs.cancel(id);
    stepper.open();
    let _ = drain(&mut jobs, &mut s);
    s.results = classify_results(&s);
    assert_ne!(s.results.get("Ha"), Some(&ResultState::Complete));
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("Ha".into()));
    let mut h = harness(state);
    h.run_steps(5);
    h.get_by_label("Out of date — merge didn't finish");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_tag_hides_only_for_a_whole_merge_of_a_shown_group() {
    let (root, mut s, jobs) = merged("window-tag-hides");
    s.results.insert("Ha".into(), ResultState::Stale);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("Ha".into()));
    let mut h = harness(state);
    until_mergeable(&mut h);
    let tag = "Out of date — merge didn't finish";
    h.get_by_label(tag);
    // Another group merging leaves it.
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge_group(&mut st.session, &mut st.jobs, "OIII");
    }
    h.run_steps(3);
    assert!(h.state().jobs.blending());
    h.get_by_label(tag);
    // An ROI merge of the shown group leaves it too.
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", Some(ROI));
    }
    h.run_steps(3);
    assert!(!h.state().jobs.whole_blend_pending("Ha"));
    h.get_by_label(tag);
    // Its own whole merge says it all.
    {
        let st = h.state_mut();
        st.w.merge_group(&mut st.session, &mut st.jobs, "Ha");
    }
    h.run_steps(3);
    assert!(h.query_by_label(tag).is_none());
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_settings_tag_is_quieter_than_the_out_of_date_tags() {
    use mega_ui::theme::tokens::space;
    use mega_ui::theme::{contrast_ratio, palette};
    let (root, mut s, jobs) = merged("window-tag-quiet");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("Ha".into()));
    let mut h = harness(state);
    h.run_steps(5);
    // A tag's ink, and the fill of the panel right behind it (the smallest
    // rect around its text).
    let look = |h: &Harness<'static, State>, text: &str| {
        let shapes = &h.output().shapes;
        let (ink, at) = shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == text => Some((
                    t.galley.job.sections[0].format.color,
                    t.visual_bounding_rect(),
                )),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{text} not drawn"));
        let fill = shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) if r.rect.contains_rect(at) => Some(r),
                _ => None,
            })
            .min_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
            .filter(|r| r.rect.width() < at.width() + 4.0 * space::S)
            .map(|r| r.fill);
        (ink, fill)
    };
    // Premultiplied `c` over white: the brightest image under a tag.
    let over_white = |c: egui::Color32| {
        let ch = |v: u8| v.saturating_add(255 - c.a());
        egui::Color32::from_rgb(ch(c.r()), ch(c.g()), ch(c.b()))
    };
    let (quiet_ink, quiet_fill) = look(&h, "Settings changed since this merge");
    assert_eq!(quiet_ink, palette::TEXT_MUTED);
    let quiet_fill = quiet_fill.expect("the quiet tag sits on a panel");
    assert!(
        quiet_fill.a() > 0 && quiet_fill.a() < 255,
        "translucent: {quiet_fill:?}"
    );
    let c = contrast_ratio(quiet_ink, over_white(quiet_fill));
    assert!(c >= 4.5, "AA over a white image: {c}");
    h.state_mut()
        .session
        .results
        .insert("Ha".into(), ResultState::Partial);
    h.run_steps(3);
    let (ink, fill) = look(&h, "Out of date — merge didn't finish");
    assert_eq!(
        (ink, fill),
        (palette::TEXT_BODY, Some(palette::SURFACE_800))
    );
    // Quieter: muted ink on a panel the image shows through.
    assert_ne!(quiet_ink, ink);
    assert!(quiet_fill.a() < palette::SURFACE_800.a());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_commit_queues_nothing_and_keeps_the_mode() {
    let (root, mut s, jobs) = merged("window-commit-fails");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    until_mergeable(&mut h);
    {
        let st = h.state_mut();
        let file = st.session.mosaic.dir.join(mmm_core::mosaic::CONFIG_FILE);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    h.run_steps(3);
    assert!(!h.state().jobs.busy(), "nothing queued");
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].toast.tone, mega_ui::theme::Tone::Problem);
    assert!(
        toasts[0]
            .toast
            .title
            .starts_with("Merge settings not saved")
    );
    // Merge this group likewise.
    {
        let st = h.state_mut();
        st.w.merge_group(&mut st.session, &mut st.jobs, "Ha");
    }
    assert!(!h.state().jobs.busy());
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_empty_merge_leaves_the_mode() {
    let (root, s, jobs) = merged("window-empty-merge");
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    until_mergeable(&mut h);
    {
        let st = h.state_mut();
        assert!(!super::steps::merge_needed(&st.session));
        st.w.merge(&mut st.session, &mut st.jobs);
    }
    assert!(!h.state().jobs.busy());
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_this_group_is_disabled_while_that_group_merges_or_the_feather_is_invalid() {
    let (root, s, jobs) = merged("window-menu-disabled");
    let mut h = harness(State::new(s, jobs));
    until_mergeable(&mut h);
    let item_disabled = |h: &mut Harness<'static, State>, chip: &str| {
        h.get_by_label(chip).click_secondary();
        h.run_steps(3);
        let d = h
            .get_by_label("Merge this group")
            .accesskit_node()
            .is_disabled();
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        d
    };
    assert!(!item_disabled(&mut h, "Ha · 4"));
    // Ha merging: its item is off, with the reason; OIII's is not.
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.w.merge_group(&mut st.session, &mut st.jobs, "Ha");
    }
    h.run_steps(2);
    let st = h.state();
    assert_eq!(
        st.w.mosaic()
            .merge_group_block_reason(&st.session, &st.jobs, "Ha"),
        Some("Merging Ha…".to_string())
    );
    assert!(item_disabled(&mut h, "Ha · 4"));
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    until_mergeable(&mut h);
    assert!(!item_disabled(&mut h, "Ha · 4"));
    // An invalid feather blocks it.
    h.state_mut().w.mosaic.force_feather_error();
    assert!(item_disabled(&mut h, "OIII · 4"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_merge_step_merges_from_out_of_date_and_settings_changed() {
    let (root, mut s, jobs) = merged("window-step-merges");
    let old = s.mosaic.config.blend.feather_px;
    set_draft_feather(&mut s, old + 100.0);
    let mut h = harness(State::new(s, jobs));
    until_mergeable(&mut h);
    let gate = h.state_mut().jobs.hold();
    h.get_by_label("Settings changed").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    until_mergeable(&mut h);
    h.state_mut()
        .session
        .results
        .insert("Ha".into(), ResultState::Stale);
    h.run_steps(3);
    let gate = h.state_mut().jobs.hold();
    h.get_by_label("Out of date").click();
    h.run_steps(2);
    assert_eq!(labels(&h.state().jobs), ["Merge Ha"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

/// A merged two-group session showing Ha on the mosaic whose provenance
/// carries a region merge of Ha with a feather 100 px above its whole
/// merge's: what a session touched by an older build that merged ROIs
/// into the store holds. Returns the committed feather too.
fn with_a_region_entry(tag: &str) -> (PathBuf, Harness<'static, State>, f32) {
    let (root, mut s, jobs) = merged(tag);
    let old = s.mosaic.config.blend.feather_px;
    let mut entry = s
        .mosaic
        .config
        .provenance
        .iter()
        .rev()
        .find(|e| e.action == "blend" && e.group.as_deref() == Some("Ha"))
        .cloned()
        .expect("Ha's whole merge");
    entry.detail["whole"] = false.into();
    entry.detail["rect"] = serde_json::json!(ROI);
    entry.detail["settings"]["feather_px"] = f64::from(old + 100.0).into();
    s.mosaic.config.provenance.push(entry);
    let mut state = State::new(s, jobs);
    state.w.set_mode(CanvasMode::Mosaic);
    state.w.select_view(ViewKind::Group("Ha".into()));
    let mut h = harness(state);
    step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some());
    h.run_steps(3);
    (root, h, old)
}

#[test]
fn a_region_entry_with_other_settings_still_makes_the_group_out_of_date() {
    let (root, mut h, old) = with_a_region_entry("window-roi-differs");
    let st = h.state();
    // Its store is complete: the region entry's settings are what make it
    // out of date, not the store.
    assert_eq!(st.session.results.get("Ha"), Some(&ResultState::Complete));
    assert!(out_of_date(&st.session, "Ha"));
    assert_eq!(
        shown_steps(st)[MERGE_STEP],
        strip("Out of date", StepState::Next)
    );
    h.get_by_label("Out of date — region merged with other settings");
    assert_eq!(chip_state(&h, "Ha · 4"), Some("out of date".to_string()));
    // Merge re-merges the whole canvas, which clears it.
    until_mergeable(&mut h);
    let gate = h.state_mut().jobs.hold();
    click_rightmost(&h, "Merge");
    h.run_steps(2);
    assert!(labels(&h.state().jobs).contains(&"Merge Ha".to_string()));
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(3);
    let st = h.state();
    assert_eq!(merged_feather(&st.session, "Ha"), Some(old));
    assert!(!out_of_date(&st.session, "Ha"));
    assert!(
        h.query_by_label("Out of date — region merged with other settings")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&root);
}

mod compare;
mod export;
mod previews;
