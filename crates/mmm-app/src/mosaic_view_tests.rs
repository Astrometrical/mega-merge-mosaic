use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use mmm_core::stretch::StretchParams;

use super::*;
use crate::camera::Camera;
use crate::compositions::ViewKind;
use crate::display_stretch::DisplayStretch;
use crate::jobs::tests::{analyzed, drain};
use crate::plan_view::CanvasLook;
use crate::rois::{Roi, save_rois};
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::two_group_session;

/// The view and what the shell hands it each frame.
struct State {
    view: MosaicView,
    session: OpenSession,
    jobs: JobRunner,
    look: CanvasLook,
    plan_stretch: Option<Vec<StretchParams>>,
    events: Vec<JobEvent>,
    /// The canvas is drawn (else only the shell's every-frame work runs).
    canvas: bool,
    /// The ROI list and the health are drawn beside the canvas.
    inspector: bool,
}

impl State {
    fn new(session: OpenSession, jobs: JobRunner) -> Self {
        State {
            view: MosaicView::new(),
            session,
            jobs,
            look: CanvasLook::default(),
            plan_stretch: None,
            events: Vec::new(),
            canvas: true,
            inspector: false,
        }
    }
}

/// The view's canvas filling the window, driven as the shell drives it:
/// the job runner's events, the settings and the view's every-frame work
/// first, then (when shown) the inspector and the canvas, then the export
/// dialog.
fn harness(state: State) -> Harness<'static, State> {
    Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let ctx = ui.ctx().clone();
                let events = s.jobs.poll(&mut s.session);
                for e in &events {
                    s.view.on_event(e);
                }
                s.events.extend(events);
                s.view.poll_settings(&ctx, &mut s.session, &mut s.jobs);
                s.view.poll(&ctx, &mut s.session, &mut s.jobs);
                if s.inspector {
                    egui::Panel::right("inspector").show(ui, |ui| {
                        s.view.rois_ui(ui, &mut s.session);
                        s.view.health_ui(ui);
                    });
                }
                if s.canvas {
                    egui::CentralPanel::default().show(ui, |ui| {
                        s.view
                            .show(ui, &mut s.session, &s.look, s.plan_stretch.as_deref());
                    });
                }
                s.view.export_ui(&ctx, &mut s.session, &mut s.jobs);
            },
            state,
        )
}

/// Step until `done` holds, stopping at the frame that made it so.
fn step_to(h: &mut Harness<'_, State>, done: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for the view");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The job labels running or queued, in order.
fn labels(jobs: &JobRunner) -> Vec<String> {
    jobs.views().into_iter().map(|v| v.label).collect()
}

/// An ROI named `name` over canvas rect `canvas`.
fn roi(name: &str, canvas: [u64; 4]) -> Roi {
    Roi {
        name: name.into(),
        note: String::new(),
        sky: None,
        canvas,
    }
}

/// An analysed session with Ha merged (OIII not) and one ROI, "ROI 1",
/// whose label the viewer paints at the ROI's top-left corner.
fn merged_ha(tag: &str) -> (std::path::PathBuf, State) {
    let (root, mut s, mut jobs) = analyzed(tag);
    jobs.start_blend(&mut s, "Ha", None);
    let _ = drain(&mut jobs, &mut s);
    save_rois(
        &mut s.mosaic.config.ui,
        &[roi("ROI 1", [100, 100, 200, 160])],
    );
    (root, State::new(s, jobs))
}

/// The view's viewer drew its last frame.
fn viewer_drawn(s: &State) -> bool {
    s.view
        .viewer
        .as_ref()
        .is_some_and(|v| v.last_frame().is_some())
}

/// Asserts the last frame painted each ROI's label where `cam` puts the
/// ROI's top-left corner: the painted frame itself, not the view after it.
fn assert_painted_with(h: &Harness<'_, State>, cam: Camera) {
    let viewer = h.state().view.viewer.as_ref().expect("a viewer");
    let (rect, _) = viewer.last_frame().expect("drawn");
    let mut checked = 0;
    for r in h.state().view.rois() {
        // The viewer writes an overlay's label just inside its corner.
        let want: egui::Pos2 = rect.center()
            + egui::vec2(
                ((r.canvas[0] as f64 - cam.centre.0) * cam.zoom) as f32,
                ((r.canvas[1] as f64 - cam.centre.1) * cam.zoom) as f32,
            )
            + egui::vec2(4.0, 2.0);
        let got = h
            .output()
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == r.name => Some::<egui::Pos2>(t.pos),
                _ => None,
            })
            .expect("ROI label painted");
        assert!(
            (got - want).length() < 1.0,
            "{} painted at {got:?}; the camera puts it at {want:?}",
            r.name
        );
        checked += 1;
    }
    assert!(checked > 0, "no ROI painted");
}

fn assert_camera(got: Option<Camera>, want: Camera) {
    let got = got.expect("a camera");
    let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(
        near(got.centre.0, want.centre.0)
            && near(got.centre.1, want.centre.1)
            && near(got.zoom, want.zoom),
        "{got:?} != {want:?}"
    );
}

#[test]
fn merge_all_starts_one_merge_per_group() {
    let (root, s, jobs) = analyzed("mosaic-merge-all");
    let mut h = harness(State::new(s, jobs));
    // Allowed once the shown group's result has been looked for.
    step_until(&mut h, |st| {
        st.view.merge_block_reason(&st.session, &st.jobs).is_none()
    });
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.view.merge_all(&mut st.session, &mut st.jobs);
    }
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    let finished = h
        .state()
        .events
        .iter()
        .filter(|e| matches!(e, JobEvent::Finished { .. }))
        .count();
    assert_eq!(finished, 2);
    let merged: Vec<&str> = h
        .state()
        .session
        .mosaic
        .config
        .provenance
        .iter()
        .filter(|p| p.action == "blend")
        .filter_map(|p| p.group.as_deref())
        .collect();
    assert_eq!(merged, ["Ha", "OIII"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_block_reason_explains_missing_analysis() {
    let root = temp_dir("mosaic-unanalysed");
    let s = two_group_session(&root);
    let jobs = JobRunner::new(egui::Context::default());
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.view.view().is_some());
    let why = {
        let st = h.state();
        st.view.merge_block_reason(&st.session, &st.jobs)
    };
    let why = why.expect("blocked before any analysis");
    assert!(why.contains("analys"), "{why}");
    // Refused: nothing starts.
    {
        let st = h.state_mut();
        st.view.merge_all(&mut st.session, &mut st.jobs);
    }
    assert!(!h.state().jobs.busy());
    // While the analysis runs it says so...
    let gate = h.state_mut().jobs.hold();
    let id = {
        let st = h.state_mut();
        st.jobs.start_analyze(&mut st.session)
    };
    h.state_mut().view.track_analysis(id);
    h.step();
    let why = {
        let st = h.state();
        st.view.merge_block_reason(&st.session, &st.jobs)
    };
    assert!(
        why.as_deref().is_some_and(|w| w.contains("analys")),
        "{why:?}"
    );
    // ...and merging is allowed once it has run.
    gate.open();
    step_until(&mut h, |st| {
        !st.jobs.busy() && st.view.merge_block_reason(&st.session, &st.jobs).is_none()
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn stretch_holds_while_merging() {
    let (root, mut s, mut jobs) = analyzed("mosaic-stretch-hold");
    // Only the bottom row of tiles merged: the statistics change once the
    // top row is merged too.
    jobs.start_blend(&mut s, "Ha", Some([0, 256, 600, 420]));
    let _ = drain(&mut jobs, &mut s);
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| {
        st.view.source().is_some() && st.view.is_settled()
    });
    let src = h.state().view.source().unwrap().clone();
    let look = DisplayStretch::default();
    let before = src.stretch();
    assert_eq!(src.auto(look), Some(before.clone()), "automatic");
    // Merge the whole of Ha, the worker held after each progress update.
    let stepper = h.state_mut().jobs.hold_progress();
    {
        let st = h.state_mut();
        st.view
            .merge_groups(&mut st.session, &mut st.jobs, vec!["Ha".into()], None);
    }
    let (mut last, mut updates, mut moved) = (0, 0, false);
    loop {
        step_until(&mut h, |st| {
            st.jobs.blend_progress().is_none_or(|(_, _, n)| n != last)
        });
        let Some((_, _, n)) = h.state().jobs.blend_progress() else {
            break;
        };
        last = n;
        updates += 1;
        h.run_steps(3);
        assert_eq!(src.stretch(), before, "the stretch moved at update {n}");
        let shown = h.state().view.source().expect("still shown");
        assert!(Arc::ptr_eq(shown, &src));
        moved |= src.auto(look) != Some(before.clone());
        stepper.step();
    }
    stepper.open();
    assert!(updates > 0, "no progress seen");
    assert!(moved, "the merge never changed the statistics");
    // Worked out again once the merge has ended.
    step_until(&mut h, |st| !st.jobs.busy() && st.view.is_settled());
    let after = src.stretch();
    assert_ne!(after, before);
    assert_eq!(src.auto(look), Some(after));
    drop(src);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn first_merge_uses_the_plan_stretch() {
    let (root, s, jobs) = analyzed("mosaic-first-merge");
    let plan = vec![
        StretchParams {
            black: 0.001,
            white: 0.9,
            midtones: 0.3,
        };
        3
    ];
    let mut state = State::new(s, jobs);
    state.plan_stretch = Some(plan.clone());
    let mut h = harness(state);
    step_until(&mut h, |st| st.view.absent().is_some());
    assert!(h.state().view.source().is_none(), "nothing merged yet");
    let stepper = h.state_mut().jobs.hold_progress();
    {
        let st = h.state_mut();
        st.view.merge_all(&mut st.session, &mut st.jobs);
    }
    // The merge creates the result and reports (then waits): the result
    // opens through the plan's stretch...
    step_until(&mut h, |st| st.view.source().is_some());
    let src = h.state().view.source().unwrap().clone();
    assert_eq!(src.stretch(), plan);
    h.run_steps(5);
    assert!(h.state().jobs.blend_pending("Ha"));
    assert_eq!(src.stretch(), plan, "held while the merge runs");
    // ...until the merge ends: then it is worked out from the result.
    stepper.open();
    step_until(&mut h, |st| !st.jobs.busy() && st.view.is_settled());
    let auto = src
        .auto(DisplayStretch::default())
        .expect("the merged result's statistics");
    assert_ne!(auto, plan);
    assert_eq!(src.stretch(), auto);
    drop(src);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_hidden_view_still_follows_analyses_and_merges() {
    let (root, state) = merged_ha("mosaic-hidden");
    let mut h = harness(state);
    step_until(&mut h, |st| {
        st.view.source().is_some() && st.view.is_settled()
    });
    // The canvas is hidden (the window shows the plan).
    h.state_mut().canvas = false;
    h.run_steps(2);
    // An analysis ends (the automatic pipeline's, tracked by the shell):
    // the results are checked again without the canvas being drawn, and
    // merging is allowed again.
    let id = {
        let st = h.state_mut();
        st.jobs.start_analyze(&mut st.session)
    };
    h.state_mut().view.track_analysis(id);
    step_until(&mut h, |st| {
        !st.jobs.busy() && st.view.merge_block_reason(&st.session, &st.jobs).is_none()
    });
    // A merge from the plan: the merged result opens again, still hidden.
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.view.merge_all(&mut st.session, &mut st.jobs);
    }
    assert_eq!(labels(&h.state().jobs), ["Merge Ha", "Merge OIII"]);
    gate.open();
    step_until(&mut h, |st| {
        !st.jobs.busy()
            && st.view.source().is_some()
            && st.view.merge_block_reason(&st.session, &st.jobs).is_none()
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rois_and_health_show_without_the_canvas() {
    use egui_kittest::kittest::Queryable;

    let (root, mut state) = merged_ha("mosaic-inspector-only");
    state.canvas = false;
    state.inspector = true;
    let mut h = harness(state);
    // The session's ROIs are listed and the health computed, though the
    // canvas has never been drawn.
    step_until(&mut h, |st| st.view.health().is_some());
    h.get_by_label("ROI 1");
    h.get_by_label("Seam step median");
    assert!(h.query_by_label("None yet.").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rois_are_drawn_only_when_the_look_has_them() {
    let (root, state) = merged_ha("mosaic-rois-look");
    let mut h = harness(state);
    step_until(&mut h, |st| {
        st.view.source().is_some() && st.view.is_settled()
    });
    let painted = |h: &Harness<'_, State>| {
        h.output()
            .shapes
            .iter()
            .any(|cs| matches!(&cs.shape, egui::Shape::Text(t) if t.galley.text() == "ROI 1"))
    };
    assert!(painted(&h));
    h.state_mut().look.rois = false;
    h.run_steps(2);
    assert!(!painted(&h));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn camera_round_trips() {
    let (root, mut state) = merged_ha("mosaic-camera");
    // Set before the viewer exists.
    let early = Camera {
        centre: (250.0, 180.0),
        zoom: 1.25,
    };
    state.view.set_camera(early);
    assert_camera(state.view.camera(), early);
    let mut h = harness(state);
    // The viewer's first frame is painted through the camera, never its
    // home fit.
    step_to(&mut h, viewer_drawn);
    assert_painted_with(&h, early);
    assert_camera(h.state().view.camera(), early);
    step_until(&mut h, |st| st.view.is_settled());
    assert_painted_with(&h, early);
    assert_camera(h.state().view.camera(), early);
    // Set once drawn: painted from the next frame on.
    let later = Camera {
        centre: (180.0, 150.0),
        zoom: 2.5,
    };
    h.state_mut().view.set_camera(later);
    assert_camera(h.state().view.camera(), later);
    h.step();
    assert_painted_with(&h, later);
    assert_camera(h.state().view.camera(), later);
    // And it stays.
    h.run();
    assert_painted_with(&h, later);
    assert_camera(h.state().view.camera(), later);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn camera_survives_a_viewer_rebuild() {
    let (root, state) = merged_ha("mosaic-camera-rebuild");
    let mut h = harness(state);
    step_until(&mut h, |st| {
        st.view.source().is_some() && st.view.is_settled()
    });
    let cam = Camera {
        centre: (200.0, 150.0),
        zoom: 2.0,
    };
    h.state_mut().view.set_camera(cam);
    h.step();
    assert_painted_with(&h, cam);
    // OIII has no result: the viewer goes, and its camera is kept...
    h.state_mut()
        .view
        .select_view(ViewKind::Group("OIII".into()));
    step_until(&mut h, |st| {
        st.view.viewer.is_none() && st.view.absent().is_some()
    });
    assert_camera(h.state().view.camera(), cam);
    // ...for Ha's new viewer: its first painted frame is through it.
    h.state_mut().view.select_view(ViewKind::Group("Ha".into()));
    step_to(&mut h, viewer_drawn);
    assert_painted_with(&h, cam);
    step_until(&mut h, |st| st.view.is_settled());
    assert_painted_with(&h, cam);
    // A camera set while there is no viewer is the next viewer's first.
    h.state_mut()
        .view
        .select_view(ViewKind::Group("OIII".into()));
    step_until(&mut h, |st| st.view.viewer.is_none());
    let other = Camera {
        centre: (150.0, 130.0),
        zoom: 1.5,
    };
    h.state_mut().view.set_camera(other);
    assert_camera(h.state().view.camera(), other);
    h.state_mut().view.select_view(ViewKind::Group("Ha".into()));
    step_to(&mut h, viewer_drawn);
    assert_painted_with(&h, other);
    assert_camera(h.state().view.camera(), other);
    let _ = std::fs::remove_dir_all(&root);
}

/// The member state of `group`.
fn member<'a>(view: &'a mut MosaicView, group: &str) -> &'a mut Member {
    view.results.get_mut(group).expect("a member of the view")
}

/// The view's background work for one frame, without drawing it.
fn update(view: &mut MosaicView, ctx: &egui::Context, s: &mut OpenSession, jobs: &JobRunner) {
    view.update(ctx, s, jobs, DisplayStretch::default(), None);
}

/// Whether a merge of the view's groups may start (the guard without the
/// whole-mosaic readiness of [`MosaicView::merge_block_reason`]).
fn can_merge(view: &MosaicView, s: &OpenSession, jobs: &JobRunner) -> bool {
    view.merge_guard(s, jobs, &view.members).is_none()
}

#[test]
fn merge_blocked_until_result_checked_and_reader_released() {
    use mmm_core::tiles::{TileGeometry, TileStore};

    let (root, mut s, jobs) = analyzed("mosaic-guard");
    let mut view = MosaicView::new();
    view.select("Ha".into());
    // Not checked yet: the result is still to be opened.
    let why = view.merge_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("result"), "{why}");
    member(&mut view, "Ha").reopen = false;
    assert_eq!(view.merge_block_reason(&s, &jobs), None);
    // The analysis settings changed since the last analysis.
    s.mosaic.config.needs_analyze.insert("OIII".into());
    let why = view.merge_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("analys"), "{why}");
    // Only the shown groups matter to an ROI merge's guard.
    assert!(can_merge(&view, &s, &jobs));
    s.mosaic.config.needs_analyze.clear();

    // While an analysis runs (a merge would queue behind it and start
    // before the result is checked again).
    view.analyze_job = Some(7);
    let why = view.merge_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("analys"), "{why}");
    // Its end asks for a check of the result: still blocked.
    view.on_event(&JobEvent::Cancelled { id: 7 });
    assert!(view.analyze_job.is_none() && member(&mut view, "Ha").reopen);
    assert!(view.merge_block_reason(&s, &jobs).is_some());
    member(&mut view, "Ha").reopen = false;

    // A given-up source still mapped (by a loader thread, say).
    let g = TileGeometry {
        width: 300,
        height: 200,
        channels: 1,
        tile: 256,
    };
    let store = |name: &str| {
        let path = root.join(name);
        drop(TileStore::create(&path, g).unwrap());
        Arc::new(MosaicTileSource::open(&path, None).unwrap())
    };
    let held = store("ha.mmmt");
    member(&mut view, "Ha").source = Some(held.clone());
    view.retire_all();
    assert!(view.source().is_none() && view.viewer.is_none());
    view.retired.retain(|w| w.strong_count() > 0);
    assert!(view.merge_block_reason(&s, &jobs).is_some());
    drop(held);
    view.retired.retain(|w| w.strong_count() > 0);
    assert_eq!(view.merge_block_reason(&s, &jobs), None);

    // A composite holds every member's reader: none of the view's groups
    // may merge until it has been dropped, after all members are given up.
    let ha = store("ha2.mmmt");
    let oiii = store("oiii.mmmt");
    view.apply_view(
        ViewKind::Composition(0),
        vec!["Ha".to_string(), "OIII".to_string()],
    );
    for (name, src) in [("Ha", &ha), ("OIII", &oiii)] {
        let m = member(&mut view, name);
        m.reopen = false;
        m.source = Some(src.clone());
    }
    let composite = Arc::new(
        CompositeTileSource::new(&[Some((ha.clone(), 1.0)), Some((oiii.clone(), 1.0)), None])
            .unwrap(),
    );
    drop((ha, oiii));
    // Only OIII is given up (say its result went out of date), but the
    // composite still maps it.
    let m = view.results.get_mut("OIII").unwrap();
    m.retire(&mut view.retired);
    view.retired.retain(|w| w.strong_count() > 0);
    assert!(view.merge_block_reason(&s, &jobs).is_some());
    drop(composite);
    view.retired.retain(|w| w.strong_count() > 0);
    assert!(view.retired.is_empty());

    // A merge that ends with no result open asks for an open.
    view.select("Ha".into());
    member(&mut view, "Ha").reopen = false;
    view.merge_jobs.insert(9, "Ha".into());
    view.on_event(&JobEvent::Finished {
        id: 9,
        label: "Merge Ha".into(),
    });
    assert!(view.merge_jobs.is_empty() && member(&mut view, "Ha").reopen && view.want_health);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_open_while_a_shown_merge_is_unstarted() {
    let (root, mut s, mut jobs) = analyzed("mosaic-unstarted-open");
    let ctx = egui::Context::default();
    let gate = jobs.hold();
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);

    // Switching to a group whose merge is running without progress, or
    // queued, opens nothing: the merge may still (re)create the store.
    let mut view = MosaicView::new();
    for g in ["Ha", "OIII"] {
        view.select(g.into());
        update(&mut view, &ctx, &mut s, &jobs);
        assert!(
            member(&mut view, g).opening.is_none(),
            "{g}: opened during its merge"
        );
        assert!(member(&mut view, g).reopen, "{g}: open forgotten");
    }

    // Once the merges end, the deferred open happens.
    gate.open();
    drain(&mut jobs, &mut s);
    update(&mut view, &ctx, &mut s, &jobs);
    let m = member(&mut view, "OIII");
    assert!(m.opening.is_some() && !m.reopen);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pending_roi_save_is_flushed() {
    use mmm_core::mosaic::MosaicConfig;

    use crate::rois::load_rois;
    use crate::testutil::CANVAS;
    use crate::util::SAVE_INTERVAL;

    let root = temp_dir("mosaic-flush-save");
    let mut s = two_group_session(&root);
    s.mosaic.save().unwrap();
    let mut view = MosaicView::new();
    // A change saved at once, then a second one held back by the rate
    // limit (not due yet)...
    view.save.saved(Instant::now());
    view.on_drawn(&mut s, [10.0, 20.0, 110.0, 90.0], CANVAS);
    assert!(view.save.pending());
    assert!(!view.save.due(Instant::now()));
    // ...is written by the flush (window closing), and only once.
    view.flush_save(&s);
    let back = MosaicConfig::load(&s.mosaic.dir).unwrap();
    assert_eq!(load_rois(&back.ui), view.rois());
    assert_eq!(view.rois().len(), 1);
    assert!(!view.save.due(Instant::now() + SAVE_INTERVAL));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recompute_without_statistics_keeps_the_stretch() {
    use mmm_core::tiles::{TileGeometry, TileStore};

    use crate::tile_source::LINEAR;

    let root = temp_dir("mosaic-auto-none");
    let mut s = two_group_session(&root);
    let ctx = egui::Context::default();
    let jobs = JobRunner::new(ctx.clone());
    // An empty (never merged) result: no statistics to stretch from.
    let path = root.join("result.mmmt");
    let g = TileGeometry {
        width: 300,
        height: 200,
        channels: 3,
        tile: 256,
    };
    drop(TileStore::create(&path, g).unwrap());
    let mut view = MosaicView::new();
    view.select("Ha".into());
    let src = Arc::new(MosaicTileSource::open(&path, None).unwrap());
    let m = member(&mut view, "Ha");
    m.reopen = false;
    m.source = Some(src.clone());
    // A new look asks for the stretch again...
    let linked = DisplayStretch {
        linked: true,
        strong: false,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    view.update(&ctx, &mut s, &jobs, linked, None);
    assert!(member(&mut view, "Ha").auto.is_some(), "asked for");
    while !member(&mut view, "Ha").idle() {
        assert!(Instant::now() < deadline, "auto stretch never ended");
        std::thread::sleep(Duration::from_millis(2));
        view.update(&ctx, &mut s, &jobs, linked, None);
    }
    // ...there are none: the stretch stays, and nothing asks again (no
    // spinning on the same empty result).
    assert_eq!(src.stretch(), vec![LINEAR; 3]);
    assert_eq!(member(&mut view, "Ha").stretch, None);
    view.update(&ctx, &mut s, &jobs, linked, None);
    assert!(member(&mut view, "Ha").auto.is_none());
    drop(src);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn merge_roi_starts_one_merge_per_view_group() {
    use crate::compositions::{Composition, save_compositions};
    use crate::testutil::CANVAS;

    let (root, mut s, mut jobs) = analyzed("mosaic-roi-groups");
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 0.5)),
    ];
    save_compositions(&mut s.mosaic.config.ui, &[hoo]);
    let mut view = MosaicView::new();
    view.set_view(&s, ViewKind::Composition(0));
    assert_eq!(view.members, ["Ha", "OIII"]);
    assert_eq!(
        view.group(),
        Some("Ha"),
        "the first member's health is shown"
    );
    for g in ["Ha", "OIII"] {
        member(&mut view, g).reopen = false;
    }

    // A drawn rect becomes "ROI 1" in whole canvas pixels, selected and
    // saved in the session's ui.
    view.on_drawn(&mut s, [10.4, 20.0, 299.5, 200.0], CANVAS);
    let rois = view.rois.rois();
    assert_eq!(rois.len(), 1);
    assert_eq!(rois[0].name, "ROI 1");
    assert_eq!(rois[0].canvas, [10, 20, 300, 200]);
    assert_eq!(view.rois.selected, Some(0));
    assert!(view.save.pending() && view.rois.want_sync && view.rois.want_fresh);
    assert_eq!(
        crate::rois::load_rois(&s.mosaic.config.ui),
        view.rois.rois(),
        "kept in ui"
    );
    view.on_drawn(&mut s, [0.0, 0.0, 50.0, 50.0], CANVAS);
    assert_eq!(view.rois.rois()[1].name, "ROI 2");
    assert_eq!(view.rois.selected, Some(1));
    view.rois.selected = Some(0);

    // Not while the ROIs are being reprojected (the rect may be from an
    // older frame).
    let why = view.merge_roi_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("regions of interest"), "{why}");
    view.merge_roi(&mut s, &mut jobs);
    assert!(!jobs.busy() && view.merge_jobs.is_empty());
    view.rois.want_sync = false;

    // Merge ROI: one merge of the ROI for each group of the view.
    assert_eq!(view.merge_roi_block_reason(&s, &jobs), None);
    let gate = jobs.hold();
    view.merge_roi(&mut s, &mut jobs);
    assert_eq!(labels(&jobs), ["Merge Ha", "Merge OIII"]);
    let mut groups: Vec<&String> = view.merge_jobs.values().collect();
    groups.sort();
    assert_eq!(groups, ["Ha", "OIII"]);
    gate.open();
    for e in drain(&mut jobs, &mut s) {
        assert!(matches!(e, JobEvent::Finished { .. }), "{e:?}");
        view.on_event(&e);
    }
    assert!(view.merge_jobs.is_empty());
    let merges: Vec<_> = s
        .mosaic
        .config
        .provenance
        .iter()
        .filter(|p| p.action == "blend")
        .map(|p| (p.group.clone(), p.detail["rect"].clone()))
        .collect();
    assert_eq!(merges.len(), 2);
    // Each merge covered the ROI (recorded tile-aligned: 256-px tiles).
    for (_, rect) in &merges {
        assert_eq!(*rect, serde_json::json!([0, 0, 512, 256]));
    }

    // A group view merges just that group's ROI (once its result, asked
    // for again by the merge's end, has been looked at)...
    view.set_view(&s, ViewKind::Group("OIII".into()));
    assert_eq!(view.members, ["OIII"]);
    assert!(member(&mut view, "OIII").reopen);
    assert!(view.merge_roi_block_reason(&s, &jobs).is_some());
    member(&mut view, "OIII").reopen = false;
    let gate = jobs.hold();
    view.merge_roi(&mut s, &mut jobs);
    assert_eq!(labels(&jobs), ["Merge OIII"]);
    gate.open();
    drain(&mut jobs, &mut s);
    // ...but merging the whole mosaic merges every group (spec §5.4).
    let gate = jobs.hold();
    view.merge_all(&mut s, &mut jobs);
    assert_eq!(labels(&jobs), ["Merge Ha", "Merge OIII"]);
    gate.open();
    drain(&mut jobs, &mut s);
    // No selected ROI: nothing to merge, and the reason says so.
    view.rois.selected = None;
    let why = view.merge_roi_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("region of interest"), "{why}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn composition_shows_merged_members() {
    use crate::compositions::{Composition, load_compositions, save_compositions};

    let (root, mut s, mut jobs) = analyzed("mosaic-partial-comp");
    jobs.start_blend(&mut s, "Ha", None);
    drain(&mut jobs, &mut s);
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 0.5)),
    ];
    save_compositions(&mut s.mosaic.config.ui, &[hoo]);
    let compositions = load_compositions(&s.mosaic.config.ui);
    let ctx = egui::Context::default();
    let mut view = MosaicView::new();
    view.set_view(&s, ViewKind::Composition(0));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !view.results.values().all(Member::idle) {
        assert!(Instant::now() < deadline, "results never opened");
        std::thread::sleep(Duration::from_millis(2));
        update(&mut view, &ctx, &mut s, &jobs);
    }
    view.sync_viewer(&compositions);
    // OIII was never merged: Ha shows, and OIII is named.
    assert!(view.results["Ha"].source.is_some());
    assert!(view.results["OIII"].source.is_none());
    assert!(view.viewer.is_some(), "the merged member is shown");
    assert_eq!(view.member_notes(), ["OIII: Not merged yet."]);
    assert_eq!(view.shown_text(), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// Answers the save dialog with a fixed path.
struct FixedSave(std::path::PathBuf);

impl crate::export::SavePicker for FixedSave {
    fn pick_save(
        &self,
        _format: crate::export::ExportFormat,
        _name: &str,
    ) -> Option<std::path::PathBuf> {
        Some(self.0.clone())
    }
}

struct ExportState {
    view: MosaicView,
    session: OpenSession,
    jobs: JobRunner,
    events: Vec<JobEvent>,
}

#[test]
fn export_dialog_warns_then_runs() {
    use egui_kittest::kittest::Queryable;
    use mmm_core::formats::InputPanel;

    use crate::testutil::CANVAS;

    let (root, mut s, mut jobs) = analyzed("mosaic-export-dialog");
    // Only the top-left tile of Ha is merged: 5 of its 6 tiles missing.
    jobs.start_blend(&mut s, "Ha", Some([0, 0, 256, 256]));
    drain(&mut jobs, &mut s);
    let picked = root.join("picked.fits");
    let mut view = MosaicView::new();
    view.select("Ha".into());
    view.export.set_picker(Arc::new(FixedSave(picked.clone())));
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, st: &mut ExportState| {
                // The view's background work (opening the result), then
                // the dialog, as `MosaicView::show` and the shell do each
                // frame.
                update(&mut st.view, ui.ctx(), &mut st.session, &st.jobs);
                st.view.export_ui(ui.ctx(), &mut st.session, &mut st.jobs);
                let events = st.jobs.poll(&mut st.session);
                for e in &events {
                    st.view.on_event(e);
                }
                st.events.extend(events);
            },
            ExportState {
                view,
                session: s,
                jobs,
                events: Vec::new(),
            },
        );
    // Wait until the result is checked (merging becomes possible).
    step_until(&mut h, |st| can_merge(&st.view, &st.session, &st.jobs));
    {
        let st = h.state_mut();
        st.view.open_export(&st.session);
    }
    h.run_steps(2);
    assert!(h.state().view.export.is_open());

    // Browse… fills the destination from the save dialog.
    h.get_by_label("Browse…").click();
    let want = picked.display().to_string();
    step_until(&mut h, |st| st.view.export.path() == want);

    // Export checks first: the result is partly missing, so it warns.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
        .click();
    step_until(&mut h, |st| st.view.export.warning_shown());
    h.get_by_label_contains("Ha: 5 missing tile(s)");
    assert!(
        !h.state().jobs.busy(),
        "nothing runs before the user decides"
    );
    assert!(!picked.exists());

    // Merging first merges the chosen groups; no export.
    h.get_by_label("Merge whole mosaic first").click();
    h.run_steps(1);
    assert!(!h.state().view.export.is_open());
    assert_eq!(labels(&h.state().jobs), ["Merge Ha"]);
    step_until(&mut h, |st| {
        !st.jobs.busy() && can_merge(&st.view, &st.session, &st.jobs)
    });
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label, .. } if label == "Merge Ha"))
    );

    // Stale it all, then "Export anyway" runs the export as a job.
    {
        let st = h.state_mut();
        mmm_core::mosaic::invalidate_group(&st.session.mosaic, "Ha").unwrap();
        st.view.open_export(&st.session);
    }
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
        .click();
    step_until(&mut h, |st| st.view.export.warning_shown());
    h.get_by_label_contains("Ha: 6 stale tile(s)");
    h.get_by_label("Export anyway").click();
    h.run_steps(1);
    assert!(!h.state().view.export.is_open());
    assert_eq!(labels(&h.state().jobs), ["Export Ha"]);
    step_until(&mut h, |st| !st.jobs.busy());
    assert!(
        h.state()
            .events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { label, .. } if label == "Export Ha")),
        "{:?}",
        h.state().events
    );
    let p = InputPanel::open(&picked).unwrap();
    assert_eq!(
        (p.width(), p.height(), p.channels()),
        (CANVAS.0, CANVAS.1, 3)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_merge_while_the_export_check_runs() {
    let (root, mut s, mut jobs) = analyzed("mosaic-during-check");
    let mut view = MosaicView::new();
    view.select("Ha".into());
    let ctx = egui::Context::default();
    for _ in 0..500 {
        if can_merge(&view, &s, &jobs) {
            break;
        }
        update(&mut view, &ctx, &mut s, &jobs);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(can_merge(&view, &s, &jobs), "allowed when idle");
    view.rois.rois.push(roi("ROI 1", [0, 0, 64, 64]));
    view.rois.selected = Some(0);
    view.rois.want_sync = false;
    assert_eq!(view.merge_roi_block_reason(&s, &jobs), None);
    view.open_export(&s);
    let (release, rx) = std::sync::mpsc::channel();
    view.export.force_checking(&ctx, rx);
    assert!(!view.export.idle());
    let why = view.merge_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("export"), "{why}");
    assert!(view.merge_roi_block_reason(&s, &jobs).is_some());
    view.merge_all(&mut s, &mut jobs);
    view.merge_roi(&mut s, &mut jobs);
    view.merge_groups(&mut s, &mut jobs, vec!["Ha".into()], Some([0, 0, 64, 64]));
    assert!(!jobs.busy(), "no merge job started during the check");
    assert!(view.merge_jobs.is_empty());
    drop(release);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_refused_for_a_pending_merge_after_switching_view() {
    use egui_kittest::kittest::{NodeT, Queryable};

    let (root, mut s, mut jobs) = analyzed("mosaic-export-switch");
    let gate = jobs.hold();
    let mut view = MosaicView::new();
    view.select("Ha".into());
    view.open_export(&s);
    // Switch the view away from Ha; the dialog still has Ha.
    view.select("OIII".into());
    jobs.start_blend(&mut s, "Ha", None);
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, st: &mut ExportState| {
                st.view.export_ui(ui.ctx(), &mut st.session, &mut st.jobs);
            },
            ExportState {
                view,
                session: s,
                jobs,
                events: Vec::new(),
            },
        );
    h.run_steps(2);
    assert!(
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label_contains("Waiting for the merge of Ha");
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn absent_reason_hidden_while_reopening() {
    // A reason from an earlier open is not reported while the result is
    // being opened again (the shown text is "Opening the result…" then).
    let mut view = MosaicView::new();
    view.select("Ha".into());
    let m = member(&mut view, "Ha");
    m.absent = Some("Cannot read the group's analysis: gone".into());
    m.reopen = true;
    assert_eq!(view.absent(), None);
    member(&mut view, "Ha").reopen = false;
    assert_eq!(
        view.absent(),
        Some("Cannot read the group's analysis: gone")
    );
}

/// The export dialog alone, as the view drives it: the user's decisions
/// are collected, nothing is started.
struct DialogState {
    dialog: crate::export::ExportDialog,
    session: OpenSession,
    jobs: JobRunner,
    merging: std::collections::BTreeSet<String>,
    actions: Vec<crate::export::ExportAction>,
}

fn dialog_harness(
    session: OpenSession,
    jobs: JobRunner,
    groups: Vec<String>,
) -> Harness<'static, DialogState> {
    let mut dialog = crate::export::ExportDialog::new(Arc::new(FixedSave(Default::default())));
    dialog.open(&session, groups);
    Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, st: &mut DialogState| {
                st.jobs.poll(&mut st.session);
                st.merging = st
                    .session
                    .mosaic
                    .config
                    .group_dirs
                    .keys()
                    .filter(|g| st.jobs.blend_pending(g))
                    .cloned()
                    .collect();
                let can_merge = true;
                if let Some(a) = st.dialog.ui(ui.ctx(), &st.session, can_merge, &st.merging) {
                    st.actions.push(a);
                }
            },
            DialogState {
                dialog,
                session,
                jobs,
                merging: Default::default(),
                actions: Vec::new(),
            },
        )
}

fn export_button<'h>(h: &'h Harness<'static, DialogState>) -> egui_kittest::Node<'h> {
    use egui_kittest::kittest::Queryable;
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Export")
}

fn export_disabled(h: &Harness<'static, DialogState>) -> bool {
    use egui_kittest::kittest::NodeT;
    export_button(h).accesskit_node().is_disabled()
}

#[test]
fn export_disabled_while_a_chosen_group_merges() {
    let (root, mut s, mut jobs) = analyzed("mosaic-export-merging");
    let gate = jobs.hold();
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    let mut h = dialog_harness(s, jobs, vec!["Ha".into()]);
    {
        let st = h.state_mut();
        st.dialog
            .set_path(&root.join("ha.fits").display().to_string());
    }
    h.run_steps(2);
    // Ha's merge runs: no check (it maps the store the merge may recreate).
    assert!(export_disabled(&h));
    export_button(&h).click();
    h.run_steps(2);
    assert!(h.state().dialog.idle(), "no check started");
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy());
    h.run_steps(2);
    assert!(!export_disabled(&h));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_snapshots_choices_and_confirms_overwrite() {
    use egui_kittest::kittest::Queryable;

    use crate::export::{ExportAction, ExportFormat};

    let (root, mut s, mut jobs) = analyzed("mosaic-export-overwrite");
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    drain(&mut jobs, &mut s);

    // One group to an existing file: confirm first, listing it.
    let dest = root.join("ha.fits");
    std::fs::write(&dest, b"keep").unwrap();
    let mut h = dialog_harness(s, jobs, vec!["Ha".into()]);
    h.state_mut().dialog.set_path(&dest.display().to_string());
    h.run_steps(2);
    export_button(&h).click();
    step_until(&mut h, |st| st.dialog.overwrite_shown());
    h.get_by_label_contains(&dest.display().to_string());
    // Back: nothing decided, the file untouched.
    h.get_by_label("Back").click();
    h.run();
    assert!(h.state().actions.is_empty());
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep");
    // Again, then change the choices while the confirmation shows: the
    // export uses what was checked.
    export_button(&h).click();
    step_until(&mut h, |st| st.dialog.overwrite_shown());
    {
        let st = h.state_mut();
        st.dialog
            .set_path(&root.join("other.tif").display().to_string());
        st.dialog.set_format(ExportFormat::Tiff);
    }
    h.get_by_label("Overwrite").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        [ExportAction::Export(
            vec![("Ha".to_string(), dest.clone())],
            ExportFormat::Fits
        )]
    );
    drop(h);

    // Several groups: the derived names are checked too.
    let (root2, mut s, mut jobs) = analyzed("mosaic-export-overwrite2");
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    drain(&mut jobs, &mut s);
    let tag = s.mosaic.config.group_dirs["OIII"].clone();
    let existing = root2.join(format!("m-{tag}.fits"));
    std::fs::write(&existing, b"keep").unwrap();
    let mut h = dialog_harness(s, jobs, vec!["Ha".into(), "OIII".into()]);
    h.state_mut()
        .dialog
        .set_path(&root2.join("m.fits").display().to_string());
    h.run_steps(2);
    export_button(&h).click();
    step_until(&mut h, |st| st.dialog.overwrite_shown());
    h.get_by_label_contains(&existing.display().to_string());
    assert!(
        h.query_by_label_contains(&root2.join("m-Ha").display().to_string())
            .is_none(),
        "only existing files are listed"
    );
    h.get_by_label("Overwrite").click();
    h.run_steps(2);
    assert!(matches!(&h.state().actions[..], [ExportAction::Export(d, _)] if d.len() == 2));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[test]
fn previews_do_not_move_persisted_rois() {
    use mmm_core::mosaic::{FRAME_FILE, Mosaic, MosaicConfig, set_inputs};
    use mmm_core::reference::ReferenceFrame;
    use mmm_core::synth::{SynthWcs, write_xisf_solved};

    use crate::rois::{load_rois, reproject};

    let root = temp_dir("mosaic-roi-frame");
    let (w, h) = (200u64, 150u64);
    let panel = |name: &str, refimg: [f64; 2]| {
        let path = root.join(name);
        let wcs = SynthWcs {
            crval: [83.8, -5.4],
            refimg,
            cd: [[-1e-3, 0.0], [0.0, 1e-3]],
        };
        write_xisf_solved(&path, w, h, 1, &vec![0.1f32; (w * h) as usize], &wcs).unwrap();
        path
    };
    let (a, b) = (
        panel("a.xisf", [100.0, 75.0]),
        panel("b.xisf", [40.0, 45.0]),
    );
    let mut mosaic = Mosaic::create(
        &root.join("s.mmm-session"),
        MosaicConfig::new(vec![a.clone(), b.clone()]),
    )
    .unwrap();
    // The frame of the last analysis (as analysis saves it), and an ROI
    // anchored on the sky in it.
    let input = mosaic.config.analyze.input;
    let analysed = mmm_core::reference::derive(&[a.clone(), b.clone()], input).unwrap();
    analysed.save(&mosaic.dir.join(FRAME_FILE)).unwrap();
    let roi = reproject(&[roi("ROI 1", [20, 20, 80, 60])], Some(&analysed)).remove(0);
    assert!(roi.sky.is_some());
    save_rois(&mut mosaic.config.ui, std::slice::from_ref(&roi));
    // A panel added up and to the left: the panels' frame grows and its
    // origin moves (no analysis yet).
    let c = panel("c.xisf", [250.0, 200.0]);
    let panels = MosaicConfig::new(vec![a.clone(), b.clone(), c.clone()]).panels;
    let grouping = mosaic.config.grouping.clone();
    set_inputs(&mut mosaic, panels, grouping).unwrap();
    let grown = mmm_core::reference::derive(&[a, b, c], input).unwrap();
    assert_ne!(grown, analysed);
    assert_ne!(
        reproject(std::slice::from_ref(&roi), Some(&grown))[0].canvas,
        roi.canvas
    );
    let groups = mosaic.groups().ok();
    let mut s = OpenSession {
        mosaic,
        frame: Some(analysed.clone()),
        groups,
        lock: None,
        summary: None,
        merged: false,
        results: Default::default(),
    };
    // The previews run in the panels' frame...
    let mut jobs = JobRunner::new(egui::Context::default());
    jobs.start_previews(&s);
    drain(&mut jobs, &mut s);
    assert_eq!(
        ReferenceFrame::load(&s.mosaic.dir.join(FRAME_FILE)).unwrap(),
        analysed,
        "previews rewrote the analysed frame"
    );
    // ...and the view's ROI sync, on opening, keeps the ROI where the
    // analysed frame puts it.
    let ctx = egui::Context::default();
    let mut view = MosaicView::new();
    view.rois.load_once(&s.mosaic.config.ui);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        update(&mut view, &ctx, &mut s, &jobs);
        if view.rois.synced() && view.rois.idle() {
            break;
        }
        assert!(Instant::now() < deadline, "the ROI sync never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(view.rois(), std::slice::from_ref(&roi));
    assert_eq!(load_rois(&s.mosaic.config.ui), [roi]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_merge_while_the_settings_tile_count_runs() {
    use crate::blend_settings::SettingsEdit;

    let (root, mut s, mut jobs) = analyzed("mosaic-settings-count");
    jobs.start_blend(&mut s, "Ha", None);
    drain(&mut jobs, &mut s);
    let ctx = egui::Context::default();
    let mut view = MosaicView::new();
    view.select("Ha".into());
    member(&mut view, "Ha").reopen = false;
    let mut e = SettingsEdit::of(&s.mosaic.config);
    e.blend.feather_px = 128.0;
    view.settings.stage(e, &s.mosaic.config);
    // The draft needs its cost: the count starts (its TileReader maps the
    // stores a merge may recreate).
    view.poll_settings(&ctx, &mut s, &mut jobs);
    assert!(!view.settings.idle());
    let why = view.merge_block_reason(&s, &jobs).expect("blocked");
    assert!(why.contains("settings"), "{why}");
    view.merge_groups(&mut s, &mut jobs, vec!["Ha".into()], None);
    assert!(!jobs.busy(), "no merge started during the count");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !view.settings.idle() {
        assert!(Instant::now() < deadline, "the count never ended");
        std::thread::sleep(Duration::from_millis(2));
        view.poll_settings(&ctx, &mut s, &mut jobs);
    }
    assert_eq!(view.merge_block_reason(&s, &jobs), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn settings_edit_waits_for_merges_and_blocks_merging() {
    use crate::blend_settings::SettingsEdit;
    use mmm_core::settings::ChangeImpact;

    let (root, mut s, mut jobs) = analyzed("mosaic-settings-wait");
    let ctx = egui::Context::default();
    let mut view = MosaicView::new();
    view.select("Ha".into());
    member(&mut view, "Ha").reopen = false;
    assert_eq!(view.merge_block_reason(&s, &jobs), None);
    let gate = jobs.hold();
    view.merge_groups(&mut s, &mut jobs, vec!["Ha".into()], None);
    let mut e = SettingsEdit::of(&s.mosaic.config);
    e.blend.feather_px = 128.0;
    view.settings.stage(e, &s.mosaic.config);
    assert_eq!(view.settings.apply(&mut s, &mut jobs), None);
    // Waiting for the merge to stop: no merge may start meanwhile, and the
    // shell keeps the pipeline from starting anything.
    assert!(view.settings.has_pending());
    assert!(view.merge_block_reason(&s, &jobs).is_some());
    gate.open();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut applied = None;
    while applied.is_none() {
        assert!(Instant::now() < deadline, "never applied");
        for e in jobs.poll(&mut s) {
            view.on_event(&e);
        }
        applied = view.poll_settings(&ctx, &mut s, &mut jobs);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(applied, Some(ChangeImpact::Reblend));
    assert_eq!(s.mosaic.config.blend.feather_px, 128.0);
    assert!(!view.settings.has_pending());
    // The view's results, ROI freshness and health are looked at again.
    assert!(view.rois.want_fresh && view.want_health);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn health_waits_for_the_analysis() {
    use egui_kittest::kittest::Queryable;

    let root = temp_dir("mosaic-health-unanalysed");
    let mut state = State::new(
        two_group_session(&root),
        JobRunner::new(egui::Context::default()),
    );
    state.canvas = false;
    state.inspector = true;
    let mut h = harness(state);
    step_until(&mut h, |st| st.view.view().is_some());
    h.run_steps(2);
    h.get_by_label("Shown once the group is analysed.");
    let _ = std::fs::remove_dir_all(&root);
}
