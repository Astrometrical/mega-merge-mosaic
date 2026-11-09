//! Transient ROI previews in the session window (spec §8.2): each visible
//! ROI's area merged by itself, in memory, with the draft, through the
//! preview runner's deterministic gate.

use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::SystemTime;

use mmm_core::mosaic::{CONFIG_FILE, analysis_stamp, result_store_paths};

use super::*;
use crate::blend_settings::draft_for;
use crate::previews::tests::Gate;
use crate::previews::{Logged, PreviewKey};
use crate::tile_source::{Mix, planar_image};
use mmm_core::stretch::StretchParams;

/// "Core" (id 7) and "Halo" (id 9): both in the first 256 × 256 tile of
/// the 600 × 420 synthetic canvas.
pub(super) const CORE: (u64, &str, [u64; 4]) = (7, "Core", [100, 100, 200, 160]);
pub(super) const HALO: (u64, &str, [u64; 4]) = (9, "Halo", [0, 0, 64, 64]);

/// A merged two-group session with `rois`, its window showing the mosaic of
/// Ha with the ROIs section open (once `prepare` has adjusted it, before
/// the first frame). Every preview started waits for the returned gate.
pub(super) fn shown(
    tag: &str,
    rois: &[(u64, &str, [u64; 4])],
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    shown_on(tag, rois, CanvasMode::Mosaic, prepare)
}

/// [`shown`] on the canvas `mode` (drawn, with the plan's panels placed).
pub(super) fn shown_on(
    tag: &str,
    rois: &[(u64, &str, [u64; 4])],
    mode: CanvasMode,
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    shown_in(merged(tag), rois, mode, prepare)
}

/// [`shown_on`] for the session `built` (analysed and merged).
fn shown_in(
    built: (PathBuf, OpenSession, JobRunner),
    rois: &[(u64, &str, [u64; 4])],
    mode: CanvasMode,
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    let (root, mut s, jobs) = built;
    let rois: Vec<Roi> = rois
        .iter()
        .map(|&(id, name, canvas)| Roi {
            id,
            name: name.into(),
            note: String::new(),
            sky: None,
            canvas,
        })
        .collect();
    save_rois(&mut s.mosaic.config.ui, &rois);
    let mut state = State::new(s, jobs);
    state.w.inspector_mut().reveal(ROIS);
    state.w.set_mode(mode);
    let gate = state.w.previews.hold();
    prepare(&mut state);
    let mut h = harness(state);
    match mode {
        CanvasMode::Mosaic => step_to(&mut h, |st| st.w.mosaic().viewer_frame().is_some()),
        CanvasMode::Plan => step_to(&mut h, plan_placed),
    }
    (root, h, gate)
}

/// The HOO composite (R Ha, G and B OIII), shown.
pub(super) fn show_hoo(st: &mut State) {
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
    ];
    save_compositions(&mut st.session.mosaic.config.ui, &[hoo]);
    st.w.select_view(ViewKind::Composition(1));
}

pub(super) fn log(st: &State) -> Vec<Logged> {
    st.w.previews.log().to_vec()
}

/// The previews started so far, in order: `(roi, group)`.
pub(super) fn started(st: &State) -> Vec<(u64, String)> {
    log(st)
        .into_iter()
        .filter_map(|e| match e {
            Logged::Started(roi, group, _) => Some((roi, group)),
            _ => None,
        })
        .collect()
}

/// The key the last preview started was made with.
pub(super) fn last_started_key(st: &State) -> PreviewKey {
    log(st)
        .into_iter()
        .rev()
        .find_map(|e| match e {
            Logged::Started(_, _, key) => Some(key),
            _ => None,
        })
        .expect("a preview started")
}

/// How many times `(roi, group)`'s preview was delivered into the cache.
pub(super) fn delivered(st: &State, roi: u64, group: &str) -> usize {
    log(st)
        .iter()
        .filter(|e| **e == Logged::Delivered(roi, group.into()))
        .count()
}

/// How many times `(roi, group)`'s preview was cancelled.
pub(super) fn cancelled(st: &State, roi: u64, group: &str) -> usize {
    log(st)
        .iter()
        .filter(|e| **e == Logged::Cancelled(roi, group.into()))
        .count()
}

/// The key a preview of `rect` of `group` is wanted with now.
pub(super) fn wanted(st: &State, group: &str, rect: [u64; 4]) -> PreviewKey {
    let cfg = &st.session.mosaic.config;
    PreviewKey {
        rect,
        settings: draft_for(cfg, group),
        analysis: analysis_stamp(cfg, group).expect("analysed"),
    }
}

/// How many outcomes `(roi, group)`'s previews ended dropped with.
fn dropped(st: &State, roi: u64, group: &str) -> usize {
    log(st)
        .iter()
        .filter(|e| **e == Logged::Dropped(roi, group.into()))
        .count()
}

/// Wait, with no frame (so nothing takes it), until the preview thread
/// has sent `n` outcomes in all.
fn until_sent(h: &Harness<'static, State>, n: usize) {
    let probe = h.state().w.previews.probe();
    let deadline = Instant::now() + PATIENCE;
    while probe.sent.load(AtomicOrdering::SeqCst) < n {
        assert!(Instant::now() < deadline, "the preview never ended");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The draft's feather width.
pub(super) fn draft_feather(st: &State) -> f32 {
    crate::blend_settings::load_draft(&st.session.mosaic.config).feather_px
}

/// Let the held preview go, holding those started from now on.
pub(super) fn release(h: &mut Harness<'static, State>, gate: Arc<Gate>) -> Arc<Gate> {
    let next = h.state_mut().w.previews.hold();
    gate.open();
    next
}

/// Never two previews at once and no queue: each preview started has
/// ended (delivered, dropped or failed) before the next starts.
fn assert_one_at_a_time(log: &[Logged]) {
    let mut live = false;
    for e in log {
        match e {
            Logged::Started(..) => {
                assert!(!live, "two previews at once: {log:?}");
                live = true;
            }
            Logged::Delivered(..) | Logged::Dropped(..) | Logged::Failed(..) => live = false,
            Logged::Cancelled(..) => {}
        }
    }
}

#[test]
fn a_visible_roi_previews_by_itself() {
    let (root, mut h, gate) = shown("previews-by-itself", &[CORE], |_| {});
    step_to(&mut h, |st| !started(st).is_empty());
    let want = wanted(h.state(), "Ha", CORE.2);
    // The group in view only, with the draft and the group's analysis.
    assert_eq!(
        log(h.state()),
        [Logged::Started(7, "Ha".into(), want.clone())]
    );
    // Its own runner: no job, nothing in the strip.
    assert!(!h.state().jobs.busy());
    assert!(labels(&h.state().jobs).is_empty());
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    let (key, patch) = h.state().w.previews.patch(7, "Ha").expect("a patch");
    assert_eq!(key, &want);
    // The tile-aligned rect, at full resolution, in memory.
    assert_eq!(patch.rect, [0, 0, 256, 256]);
    assert_eq!(
        (patch.width, patch.height, patch.channels, patch.downsample),
        (256, 256, 3, 1)
    );
    assert!(patch.data.iter().any(|&v| v > 0.0));
    // Up to date: nothing merges again.
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 1);
    // A composite: its member without a preview merges; Ha's is kept.
    show_hoo(h.state_mut());
    step_to(&mut h, |st| delivered(st, 7, "OIII") == 1);
    h.run_steps(5);
    assert_eq!(
        started(h.state()),
        [(7, "Ha".to_string()), (7, "OIII".to_string())]
    );
    assert!(h.state().events.is_empty(), "{:?}", h.state().events);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hidden_or_off_screen_rois_wait() {
    let (root, mut h, gate) = shown("previews-wait", &[CORE, HALO], |st| {
        st.w.look.rois = false;
    });
    gate.open();
    step_to(&mut h, |st| st.w.mosaic().rois_synced());
    h.run_steps(10);
    // Hidden by the toggle: nothing merges.
    assert!(started(h.state()).is_empty(), "{:?}", log(h.state()));
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    assert!(row_detail(&h, "Halo").ends_with("out of date"));
    // Zoomed in on Halo: Core is off screen. Still hidden.
    h.state_mut().w.frame(HALO.2);
    h.run_steps(5);
    assert!(started(h.state()).is_empty());
    // Shown: Halo merges, Core waits.
    h.key_press(egui::Key::R);
    step_to(&mut h, |st| delivered(st, 9, "Ha") == 1);
    h.run_steps(10);
    assert_eq!(started(h.state()), [(9, "Ha".to_string())]);
    step_until_row(&mut h, "Halo", "up to date");
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    // Core comes into view: it merges in turn.
    h.state_mut().w.frame(CORE.2);
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    step_until_row(&mut h, "Core", "up to date");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nearest_the_centre_goes_first() {
    // Both in view, Halo nearer the centre though Core comes first in the
    // list; a composite of Ha (R) then OIII (G, B).
    let (root, mut h, mut gate) = shown("previews-nearest", &[CORE, HALO], |st| {
        show_hoo(st);
        let c = Camera {
            centre: (60.0, 60.0),
            zoom: 1.5,
        };
        st.w.plan.set_camera(c);
        st.w.mosaic.set_camera(c);
    });
    let mut order: Vec<(u64, String)> = Vec::new();
    for _ in 0..4 {
        let n = order.len() + 1;
        step_to(&mut h, |st| started(st).len() == n);
        order.push(started(h.state())[n - 1].clone());
        // One at a time: nothing else starts while it runs.
        h.run_steps(3);
        assert_eq!(started(h.state()).len(), n);
        gate = release(&mut h, gate);
    }
    // The group in view first (R before G), the nearest ROI first in each.
    let want = [(9, "Ha"), (7, "Ha"), (9, "OIII"), (7, "OIII")];
    let want: Vec<(u64, String)> = want.iter().map(|&(r, g)| (r, g.into())).collect();
    assert_eq!(order, want);
    step_to(&mut h, |st| delivered(st, 7, "OIII") == 1);
    assert_one_at_a_time(&log(h.state()));
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn settings_changed_mid_preview_restarts_it() {
    let (root, mut h, gate) = shown("previews-restart", &[CORE], |_| {});
    step_to(&mut h, |st| started(st).len() == 1);
    // Changed twice, faster than the preview merges: each change cancels
    // the preview running and starts it again with the new settings.
    for (i, px) in [40.0, 55.0].into_iter().enumerate() {
        set_draft_feather(&mut h.state_mut().session, px);
        step_to(&mut h, |st| started(st).len() == i + 2);
        assert_eq!(last_started_key(h.state()).settings.feather_px, px);
        assert_eq!(cancelled(h.state(), 7, "Ha"), i + 1);
    }
    // No queue built up: never two at once.
    assert_one_at_a_time(&log(h.state()));
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    // The preview shown is made from the final settings.
    let want = wanted(h.state(), "Ha", CORE.2);
    assert_eq!(want.settings.feather_px, 55.0);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&want));
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 3);
    step_until_row(&mut h, "Core", "up to date");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_analysis_cancels_and_dirties_previews() {
    let (root, mut h, gate) = shown("previews-analysis", &[CORE], |_| {});
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    step_until_row(&mut h, "Core", "up to date");
    let before = wanted(h.state(), "Ha", CORE.2);
    let feather = draft_feather(h.state());
    // A new preview runs (held)...
    let gate = h.state_mut().w.previews.hold();
    set_draft_feather(&mut h.state_mut().session, feather + 20.0);
    step_to(&mut h, |st| started(st).len() == 2);
    // ...when an analysis starts: the preview is cancelled, and nothing
    // starts while the analysis runs.
    let analysis = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    step_to(&mut h, |st| cancelled(st, 7, "Ha") == 1);
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 2, "{:?}", log(h.state()));
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    // Back to the settings it was made with, hidden: only the analysis can
    // date it now.
    set_draft_feather(&mut h.state_mut().session, feather);
    h.state_mut().w.look.rois = false;
    h.run_steps(3);
    assert!(row_detail(&h, "Core").ends_with("up to date"));
    analysis.open();
    step_to(&mut h, |st| {
        !st.jobs.busy() && analysis_stamp(&st.session.mosaic.config, "Ha") != Some(before.analysis)
    });
    h.run_steps(3);
    // The old patch is kept but no longer up to date.
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&before));
    // Shown again: merged afresh from the new analysis.
    h.state_mut().w.look.rois = true;
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 2);
    let after = wanted(h.state(), "Ha", CORE.2);
    assert_ne!(after.analysis, before.analysis);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&after));
    step_until_row(&mut h, "Core", "up to date");
    // A group needing an analysis is out of date at once and merges
    // nothing, whatever changes.
    h.state_mut()
        .session
        .mosaic
        .config
        .needs_analyze
        .insert("Ha".into());
    h.run_steps(3);
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    set_draft_feather(&mut h.state_mut().session, feather + 7.0);
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 3, "{:?}", log(h.state()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_preview_from_before_a_stopped_reanalysis_is_out_of_date() {
    let (root, mut h, gate) = shown("previews-stopped-analysis", &[CORE], |_| {});
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    step_until_row(&mut h, "Core", "up to date");
    let before = wanted(h.state(), "Ha", CORE.2);
    // Hidden: nothing merges meanwhile, so only the analysis can date it.
    h.state_mut().w.look.rois = false;
    // Both groups analysed again, the run held at each progress update...
    let stepper = h.state_mut().jobs.hold_progress();
    let id = {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
        id
    };
    let stage = |st: &State| {
        st.jobs
            .views()
            .into_iter()
            .find(|v| v.id == id)
            .map(|v| v.stage)
    };
    // ...and stopped once Ha has finished and OIII has begun.
    loop {
        step_to(&mut h, |_| stepper.held());
        h.step();
        if stage(h.state()).is_some_and(|s| s.starts_with("OIII: ")) {
            break;
        }
        stepper.step();
    }
    h.state_mut().jobs.cancel(id);
    stepper.open();
    step_to(&mut h, |st| !st.jobs.busy());
    h.run_steps(3);
    let cfg = &h.state().session.mosaic.config;
    assert!(!cfg.needs_analyze.contains("Ha"), "Ha finished");
    assert!(cfg.needs_analyze.contains("OIII"), "OIII did not");
    // Ha was analysed again: the patch it had is out of date.
    assert_ne!(analysis_stamp(cfg, "Ha"), Some(before.analysis));
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&before));
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    // Shown again: merged afresh from the new analysis.
    h.state_mut().w.look.rois = true;
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 2);
    let after = wanted(h.state(), "Ha", CORE.2);
    assert_ne!(after.analysis, before.analysis);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&after));
    step_until_row(&mut h, "Core", "up to date");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_analysed_group_without_a_stamp_previews() {
    // As in a session written before stopped analyses were recorded: Ha is
    // analysed (it has a directory and needs no analysis) but no "analyze"
    // entry lists it.
    let (root, mut h, gate) = shown("previews-unstamped", &[CORE], |st| {
        let cfg = &mut st.session.mosaic.config;
        cfg.provenance.retain(|e| e.action != "analyze");
    });
    let cfg = &h.state().session.mosaic.config;
    assert_eq!(analysis_stamp(cfg, "Ha"), None);
    let key = PreviewKey::wanted(cfg, "Ha", CORE.2).expect("previewable");
    assert_eq!(key.analysis, crate::previews::UNSTAMPED);
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    step_until_row(&mut h, "Core", "up to date");
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&key));
    // A later analysis records a real stamp, replacing the sentinel: the
    // patch is out of date, and merges again.
    h.state_mut().w.look.rois = false;
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    step_to(&mut h, |st| {
        !st.jobs.busy() && analysis_stamp(&st.session.mosaic.config, "Ha").is_some()
    });
    h.run_steps(3);
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    h.state_mut().w.look.rois = true;
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 2);
    let after = wanted(h.state(), "Ha", CORE.2);
    assert_ne!(after.analysis, crate::previews::UNSTAMPED);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), Some(&after));
    step_until_row(&mut h, "Core", "up to date");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_empty_view_is_out_of_date_and_offers_no_compare() {
    let (root, mut h, gate) = shown("previews-empty-view", &[CORE], |_| {});
    gate.open();
    step_until_row(&mut h, "Core", "up to date");
    h.state_mut().w.mosaic.select_roi(7);
    h.run_steps(3);
    assert!(h.state().w.labelled.contains(&7), "Compare is offered");
    // A composite that maps no group: nothing in view to preview, so no
    // ROI is up to date and none offers Compare (it would do nothing).
    {
        let st = h.state_mut();
        save_compositions(
            &mut st.session.mosaic.config.ui,
            &[Composition::new("Empty")],
        );
        st.w.select_view(ViewKind::Composition(1));
    }
    h.run_steps(5);
    let st = h.state();
    assert!(st.w.mosaic().members().is_empty());
    assert_eq!(
        st.w.previews.state(7, &[], &st.session),
        PreviewState::OutOfDate
    );
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    assert!(!h.state().w.labelled.contains(&7));
    assert!(h.query_by_label("Compare Core").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_coarse_preview_says_so_on_its_row() {
    // A patch of Core's tile, made as wanted now, coarse or not.
    let put = |st: &mut State, coarse: bool| {
        let key = PreviewKey::wanted(&st.session.mosaic.config, "Ha", CORE.2).unwrap();
        let patch = mmm_core::mosaic::PreviewPatch {
            rect: [0, 0, 256, 256],
            width: 32,
            height: 32,
            channels: 3,
            downsample: 8,
            coarse,
            data: vec![0.2; 32 * 32 * 3],
        };
        st.w.previews.insert_patch(7, "Ha", key, patch);
    };
    let (root, mut h, gate) = shown("previews-coarse", &[CORE], |st| put(st, true));
    h.run_steps(3);
    assert!(started(h.state()).is_empty(), "{:?}", log(h.state()));
    let size = crate::rois::roi_size_text(CORE.2);
    assert_eq!(
        row_detail(&h, "Core"),
        format!("{size} · up to date · coarse preview: merge mode not shown")
    );
    // The canvas label is unchanged.
    let rect = h.state().w.mosaic().viewer_frame().unwrap().0;
    assert!(painted_roi(&h, rect, "Core").is_some());
    // A preview that shows the merge mode: no note.
    put(h.state_mut(), false);
    h.run_steps(3);
    assert_eq!(row_detail(&h, "Core"), format!("{size} · up to date"));
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_patch_draws_until_the_rois_are_synced() {
    let (root, mut h, gate) = shown("previews-unsynced", &[CORE], |_| {});
    gate.open();
    step_to(&mut h, |st| st.w.shown_patches().last().len() == 1);
    let conversions = h.state().w.shown_patches().conversions(7);
    // The ROIs asked to be reprojected, as an analysis applying does: until
    // they are, no patch draws (one could land at a rect gone stale).
    h.state_mut().w.mosaic.want_roi_sync();
    h.step();
    assert!(!h.state().w.mosaic().rois_synced());
    assert!(h.state().w.shown_patches().last().is_empty());
    // Synced: it draws again, from the texture it had.
    step_to(&mut h, |st| st.w.shown_patches().last().len() == 1);
    assert!(h.state().w.mosaic().rois_synced());
    assert_eq!(h.state().w.shown_patches().conversions(7), conversions);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_moved_or_deleted_roi_drops_its_preview() {
    let (root, mut h, gate) = shown("previews-moved", &[CORE, HALO], |_| {});
    gate.open();
    step_to(&mut h, |st| {
        delivered(st, 7, "Ha") == 1 && delivered(st, 9, "Ha") == 1
    });
    // The settings change: Core (nearer the centre) merges again, held.
    let gate = h.state_mut().w.previews.hold();
    let feather = draft_feather(h.state());
    set_draft_feather(&mut h.state_mut().session, feather + 20.0);
    step_to(&mut h, |st| started(st).len() == 3);
    assert_eq!(started(h.state())[2].0, 7);
    // Moved meanwhile (as a reprojection moves it): that preview is
    // cancelled, its patch dropped at once, and it merges at its new place.
    let moved = [300, 200, 380, 260];
    h.state_mut().w.mosaic.move_roi(7, moved);
    h.step();
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), None);
    assert_eq!(cancelled(h.state(), 7, "Ha"), 1);
    step_to(&mut h, |st| started(st).len() == 4);
    assert_eq!(started(h.state())[3].0, 7);
    assert_eq!(last_started_key(h.state()).rect, moved);
    // Deleted mid-preview: cancelled, and nothing of it is left.
    hover_row(&mut h, "Core");
    h.get_by_label("Delete Core").click();
    h.run_steps(3);
    assert_eq!(cancelled(h.state(), 7, "Ha"), 2);
    gate.open();
    // Halo merges again for the new settings; Core never shows again.
    step_to(&mut h, |st| delivered(st, 9, "Ha") == 2);
    h.run_steps(10);
    assert_eq!(delivered(h.state(), 7, "Ha"), 1);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), None);
    assert_one_at_a_time(&log(h.state()));
    // Undo puts it back: merged afresh, where it was.
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    let undo = toasts.into_iter().find_map(|t| t.undo).expect("an undo");
    {
        let st = h.state_mut();
        st.w.undo(undo, &mut st.session, &mut st.jobs);
    }
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 2);
    let key = h.state().w.previews.cached_key(7, "Ha").cloned();
    assert_eq!(key.map(|k| k.rect), Some(moved));
    let _ = std::fs::remove_dir_all(&root);
}

/// What a preview must leave as it was: the provenance, `mosaic.json`'s
/// bytes and every other file of the session (bytes and modification
/// time), the decoded-panel cache aside.
#[derive(Debug, PartialEq)]
struct Untouched {
    provenance: String,
    config: Vec<u8>,
    files: Vec<(PathBuf, Vec<u8>, SystemTime)>,
}

fn untouched(s: &OpenSession) -> Untouched {
    fn walk(dir: &std::path::Path, out: &mut Vec<(PathBuf, Vec<u8>, SystemTime)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "decoded") {
                    walk(&p, out);
                }
            } else if p.file_name().is_some_and(|n| n != CONFIG_FILE) {
                let modified = std::fs::metadata(&p).unwrap().modified().unwrap();
                out.push((p.clone(), std::fs::read(&p).unwrap(), modified));
            }
        }
    }
    let dir = &s.mosaic.dir;
    let mut files = Vec::new();
    walk(dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Untouched {
        provenance: serde_json::to_string(&s.mosaic.config.provenance).unwrap(),
        config: std::fs::read(dir.join(CONFIG_FILE)).unwrap(),
        files,
    }
}

#[test]
fn previews_leave_the_session_untouched() {
    let (root, mut h, gate) = shown("previews-untouched", &[CORE, HALO], |_| {});
    step_to(&mut h, |st| started(st).len() == 1);
    {
        let st = h.state_mut();
        st.w.flush_save(&mut st.session);
    }
    h.run_steps(3);
    let before = untouched(&h.state().session);
    let stores = result_store_paths(&h.state().session.mosaic.dir).unwrap();
    assert_eq!(stores.len(), 2, "both groups merged");
    assert!(
        stores
            .iter()
            .all(|p| before.files.iter().any(|(f, ..)| f == p)),
        "the result stores are checked"
    );
    let provenance = h.state().session.mosaic.config.provenance.len();
    h.state_mut().events.clear();
    h.state_mut().toasts.clear();
    gate.open();
    step_to(&mut h, |st| {
        delivered(st, 7, "Ha") == 1 && delivered(st, 9, "Ha") == 1
    });
    h.run_steps(5);
    {
        let st = h.state_mut();
        st.w.flush_save(&mut st.session);
    }
    assert_eq!(h.state().session.mosaic.config.provenance.len(), provenance);
    assert_eq!(untouched(&h.state().session), before);
    // No job event, no toast, no job.
    assert!(h.state().events.is_empty(), "{:?}", h.state().events);
    assert!(h.state().toasts.is_empty(), "{:?}", h.state().toasts);
    assert!(!h.state().jobs.busy());
    assert!(labels(&h.state().jobs).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn row_states_follow_previews() {
    let (root, mut h, gate) = shown("previews-row-states", &[CORE], |_| {});
    step_to(&mut h, |st| started(st).len() == 1);
    h.step();
    // Merging: the row says so, and so does the label on the canvas.
    assert!(row_detail(&h, "Core").ends_with("merging"));
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    assert!(painted(&h, frame, "Core · merging…").is_some());
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    h.step();
    assert!(row_detail(&h, "Core").ends_with("up to date"));
    assert!(painted(&h, frame, "Core").is_some());
    assert!(painted(&h, frame, "Core · merging…").is_none());
    // Hidden, then the settings change: out of date, merging nothing.
    let gate = h.state_mut().w.previews.hold();
    h.key_press(egui::Key::R);
    h.run_steps(2);
    let feather = draft_feather(h.state());
    set_draft_feather(&mut h.state_mut().session, feather + 20.0);
    h.run_steps(3);
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    assert_eq!(started(h.state()).len(), 1);
    // Shown: merging, then up to date.
    h.key_press(egui::Key::R);
    step_to(&mut h, |st| started(st).len() == 2);
    h.step();
    assert!(row_detail(&h, "Core").ends_with("merging"));
    gate.open();
    step_until_row(&mut h, "Core", "up to date");
    // The state is that of the groups in view: a composite's OIII has no
    // preview yet.
    h.state_mut().w.look.rois = false;
    show_hoo(h.state_mut());
    h.run_steps(3);
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_result_landing_as_its_key_goes_stale_is_dropped() {
    let (root, mut h, gate) = shown("previews-same-frame", &[CORE], |_| {});
    step_to(&mut h, |st| started(st).len() == 1);
    // The preview ends and its result waits in the channel; in the very
    // frame that takes it, the settings change: it is dropped, never cached
    // as current, and the new settings' preview starts.
    let next = h.state_mut().w.previews.hold();
    gate.open();
    until_sent(&h, 1);
    let feather = draft_feather(h.state());
    set_draft_feather(&mut h.state_mut().session, feather + 20.0);
    h.step();
    assert_eq!(delivered(h.state(), 7, "Ha"), 0, "{:?}", log(h.state()));
    assert_eq!(dropped(h.state(), 7, "Ha"), 1);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), None);
    assert_eq!(started(h.state()).len(), 2);
    assert_eq!(
        last_started_key(h.state()).settings.feather_px,
        feather + 20.0
    );
    // The same when an analysis starts in that frame: dropped, and nothing
    // starts while it runs.
    let last = h.state_mut().w.previews.hold();
    next.open();
    until_sent(&h, 2);
    let analysis = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    h.step();
    assert_eq!(delivered(h.state(), 7, "Ha"), 0, "{:?}", log(h.state()));
    assert_eq!(dropped(h.state(), 7, "Ha"), 2);
    assert_eq!(h.state().w.previews.cached_key(7, "Ha"), None);
    assert_eq!(started(h.state()).len(), 2);
    assert_one_at_a_time(&log(h.state()));
    analysis.open();
    last.open();
    step_to(&mut h, |st| !st.jobs.busy());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn previews_wait_for_invalid_settings_waiting_edits_and_the_roi_sync() {
    let (root, mut h, gate) = shown("previews-blocked", &[CORE], |_| {});
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    let feather = draft_feather(h.state());
    // Invalid typed settings: nothing merges until they are valid again.
    h.state_mut().w.mosaic.force_feather_error();
    set_draft_feather(&mut h.state_mut().session, feather + 10.0);
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 1, "{:?}", log(h.state()));
    h.state_mut().w.mosaic.clear_feather_error();
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 2);
    // The ROIs not reprojected for the current frame yet: nothing wanted.
    {
        let st = h.state_mut();
        st.w.mosaic.want_roi_sync();
        assert!(!st.w.mosaic().rois_synced());
        assert!(st.w.wanted_previews(&st.session, &st.jobs).is_empty());
    }
    step_to(&mut h, |st| st.w.mosaic().rois_synced());
    let st = h.state();
    assert_eq!(st.w.wanted_previews(&st.session, &st.jobs).len(), 1);
    // A merge running (held) does not stop previews...
    let merge = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "OIII", None);
    }
    step_to(&mut h, |st| st.jobs.busy());
    set_draft_feather(&mut h.state_mut().session, feather + 20.0);
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 3);
    // ...but a panel edit waiting for it does: nothing merges meanwhile.
    {
        let st = h.state_mut();
        let edit = crate::panel_edits::PanelsEdit::of(&st.session);
        st.w.panel_list_mut()
            .submit_edit(edit, &mut st.session, &mut st.jobs);
    }
    h.run_steps(2);
    assert!(h.state().w.panels_editing());
    set_draft_feather(&mut h.state_mut().session, feather + 30.0);
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 3, "{:?}", log(h.state()));
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    merge.open();
    step_to(&mut h, |st| !st.w.panels_editing() && !st.jobs.busy());
    // The edit changed nothing: the preview merges once it has applied.
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 4);
    // An analysis settings edit waiting for a merge: nothing merges
    // meanwhile, nor after (the groups then need a new analysis).
    let merge = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "OIII", None);
    }
    step_to(&mut h, |st| st.jobs.busy());
    {
        let st = h.state_mut();
        let mut a = st.session.mosaic.config.analyze.clone();
        a.gain = mmm_core::photometry::GainMode::Unity;
        assert_eq!(
            st.w.mosaic.edit_analysis(a, &mut st.session, &mut st.jobs),
            None
        );
    }
    assert!(h.state().w.has_waiting_edits());
    set_draft_feather(&mut h.state_mut().session, feather + 40.0);
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 4, "{:?}", log(h.state()));
    merge.open();
    step_to(&mut h, |st| !st.w.has_waiting_edits() && !st.jobs.busy());
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 4, "{:?}", log(h.state()));
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn previews_run_on_the_plan_and_wait_while_its_frame_differs() {
    // The plan shown, zoomed in on Halo: Core is off screen there.
    let (root, mut h, gate) = shown_on("previews-plan", &[CORE, HALO], CanvasMode::Plan, |st| {
        let c = Camera {
            centre: (32.0, 32.0),
            zoom: mega_ui::viewer::MAX_ZOOM,
        };
        st.w.plan.set_camera(c);
        st.w.mosaic.set_camera(c);
    });
    gate.open();
    step_to(&mut h, |st| delivered(st, 9, "Ha") == 1);
    h.run_steps(10);
    assert_eq!(started(h.state()), [(9, "Ha".to_string())]);
    assert_eq!(h.state().w.mode(), CanvasMode::Plan);
    assert!(
        h.state().w.mosaic().viewer_frame().is_none(),
        "the plan's view"
    );
    step_until_row(&mut h, "Halo", "up to date");
    assert!(row_detail(&h, "Core").ends_with("out of date"));
    // A canvas-changing edit: the plan draws in another frame than the
    // analysed one until the analysis catches up, and shows no ROIs: none
    // merges, even one brought into view.
    let analysed = h.state().session.frame.clone();
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.state_mut().w.frame(CORE.2);
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 1, "{:?}", log(h.state()));
    // The frames agree again: Core, now in view, merges.
    h.state_mut().session.frame = analysed;
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    let _ = std::fs::remove_dir_all(&root);
}

// ---- Drawing the patches (plan 4 task 6) ----

/// What converting `group`'s cached patch of `roi` through `stretch` gives.
pub(super) fn converted(
    st: &State,
    roi: u64,
    group: &str,
    stretch: &[StretchParams],
) -> egui::ColorImage {
    let (_, p) = st.w.previews.patch(roi, group).expect("a patch");
    let n = (p.width * p.height) as usize;
    planar_image(
        &p.data,
        n,
        p.width as usize,
        (p.width as usize, p.height as usize),
        p.channels as usize,
        stretch,
    )
}

/// ROI `roi` is drawn with its group's patch through the stretch the canvas
/// has now (the mosaic's member stretch, or the plan's).
pub(super) fn drawn_with_canvas_stretch(st: &State, roi: u64, group: &str) -> bool {
    let stretch = match st.w.mode() {
        CanvasMode::Mosaic => st.w.mosaic().member_stretch(group),
        CanvasMode::Plan => st.w.plan().stretch_for_view().map(<[_]>::to_vec),
    };
    let (Some(stretch), Some(image)) = (stretch, st.w.preview_image(roi)) else {
        return false;
    };
    *image == converted(st, roi, group, &stretch)
}

/// The mosaic's HOO mix of Ha (red) and OIII (green, blue) patches of `roi`
/// through their members' stretches; `None` for a member left out.
fn hoo_mix(st: &State, roi: u64, ha: bool, oiii: bool) -> egui::ColorImage {
    let mut mix = Mix::new([256, 256]);
    for (on, group, tint) in [(ha, "Ha", [1.0, 0.0, 0.0]), (oiii, "OIII", [0.0, 1.0, 1.0])] {
        if on {
            let stretch = st.w.mosaic().member_stretch(group).expect("a stretch");
            mix.add(&converted(st, roi, group, &stretch), tint);
        }
    }
    mix.image()
}

#[test]
fn a_preview_draws_over_the_mosaic_with_its_stretch() {
    let (root, mut h, gate) = shown("draw-mosaic", &[CORE], |_| {});
    // Nothing to draw before a patch exists.
    h.run_steps(3);
    assert!(h.state().w.preview_image(7).is_none());
    gate.open();
    step_to(&mut h, |st| drawn_with_canvas_stretch(st, 7, "Ha"));
    // Over the aligned rect it was blended on, clipped to the ROI.
    let st = h.state();
    let patches = st.w.shown_patches().last();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].rect, [0.0, 0.0, 256.0, 256.0]);
    assert_eq!(patches[0].clip, CORE.2.map(|v| v as f64));
    let image = st.w.preview_image(7).unwrap();
    assert_eq!(image.size, [256, 256]);
    assert!(image.pixels.iter().any(|p| p.a() > 0), "some data");
    // The canvas stretch changes (Strong): the patch is converted again with
    // the new one, and the stretch is the canvas's own.
    let before = h.state().w.mosaic().member_stretch("Ha").unwrap();
    let converted_before = h.state().w.shown_patches().conversions(7);
    h.key_press(egui::Key::S);
    step_to(&mut h, |st| {
        st.w.mosaic().member_stretch("Ha").as_ref() != Some(&before)
            && drawn_with_canvas_stretch(st, 7, "Ha")
    });
    assert!(h.state().w.shown_patches().conversions(7) > converted_before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_preview_draws_over_the_plan() {
    let (root, mut h, gate) = shown_on("draw-plan", &[CORE], CanvasMode::Plan, |st| {
        st.run_pipeline = true
    });
    gate.open();
    step_to(&mut h, |st| drawn_with_canvas_stretch(st, 7, "Ha"));
    let patches = h.state().w.shown_patches().last();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].rect, [0.0, 0.0, 256.0, 256.0]);
    assert_eq!(patches[0].clip, CORE.2.map(|v| v as f64));
    // Nothing is drawn while the plan draws in another frame.
    let analysed = h.state().session.frame.clone();
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    assert!(h.state().w.shown_patches().last().is_empty());
    h.state_mut().session.frame = analysed;
    step_to(&mut h, |st| st.w.shown_patches().last().len() == 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composite_preview_mixes_its_members() {
    let (root, mut h, gate) = shown("draw-composite", &[CORE], show_hoo);
    // Ha's patch first: it alone is drawn, in red.
    step_to(&mut h, |st| started(st).len() == 1);
    let gate = release(&mut h, gate);
    step_to(&mut h, |st| {
        st.w.preview_image(7)
            .is_some_and(|i| *i == hoo_mix(st, 7, true, false))
    });
    // Then OIII's joins it, in green and blue.
    gate.open();
    step_to(&mut h, |st| delivered(st, 7, "OIII") == 1);
    step_to(&mut h, |st| {
        st.w.preview_image(7)
            .is_some_and(|i| *i == hoo_mix(st, 7, true, true))
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_rois_toggle_hides_previews() {
    let (root, mut h, gate) = shown("draw-toggle", &[CORE], |_| {});
    gate.open();
    step_to(&mut h, |st| drawn_with_canvas_stretch(st, 7, "Ha"));
    assert_eq!(h.state().w.shown_patches().last().len(), 1);
    h.key_press(egui::Key::R);
    h.run_steps(3);
    assert!(h.state().w.shown_patches().last().is_empty());
    assert!(h.state().w.preview_image(7).is_none());
    h.key_press(egui::Key::R);
    step_to(&mut h, |st| drawn_with_canvas_stretch(st, 7, "Ha"));
    assert_eq!(h.state().w.shown_patches().last().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn previews_never_change_the_stretch() {
    let (root, mut h, gate) = shown_on("draw-stretch", &[CORE], CanvasMode::Plan, |st| {
        st.run_pipeline = true
    });
    step_to(&mut h, |st| {
        started(st).len() == 1 && st.w.plan().stretch_for_view().is_some()
    });
    let plan = h.state().w.plan().stretch_for_view().unwrap().to_vec();
    gate.open();
    step_to(&mut h, |st| drawn_with_canvas_stretch(st, 7, "Ha"));
    h.run_steps(5);
    assert_eq!(h.state().w.plan().stretch_for_view(), Some(plan.as_slice()));
    // The same on the mosaic.
    h.state_mut().w.set_mode(CanvasMode::Mosaic);
    step_to(&mut h, |st| {
        st.w.mosaic().member_stretch("Ha").is_some() && drawn_with_canvas_stretch(st, 7, "Ha")
    });
    let mosaic = h.state().w.mosaic().member_stretch("Ha").unwrap();
    h.run_steps(5);
    assert_eq!(h.state().w.mosaic().member_stretch("Ha"), Some(mosaic));
    let image = h.state().w.preview_image(7).unwrap();
    assert!(image.pixels.iter().any(|p| p.a() > 0));
    let _ = std::fs::remove_dir_all(&root);
}

/// An analysed two-group session of mono panels with both groups merged.
fn merged_mono(tag: &str) -> (PathBuf, OpenSession, JobRunner) {
    let root = temp_dir(tag);
    let mut s = two_group_mono_session(&root);
    let mut jobs = runner();
    jobs.start_analyze(&mut s);
    let _ = drain(&mut jobs, &mut s);
    jobs.start_blend(&mut s, "Ha", None);
    jobs.start_blend(&mut s, "OIII", None);
    let _ = drain(&mut jobs, &mut s);
    s.merged = any_merged(&s);
    s.results = classify_results(&s);
    (root, s, jobs)
}

/// `patch`'s channel mean through `stretch`, grey (a plan composition
/// channel).
fn mean_image(patch: &mmm_core::mosaic::PreviewPatch, stretch: StretchParams) -> egui::ColorImage {
    let n = (patch.width * patch.height) as usize;
    let mut mean = vec![0.0f32; n];
    for plane in patch.data.chunks_exact(n) {
        for (m, v) in mean.iter_mut().zip(plane) {
            *m += v / patch.channels as f32;
        }
    }
    planar_image(
        &mean,
        n,
        patch.width as usize,
        (patch.width as usize, patch.height as usize),
        1,
        &[stretch],
    )
}

#[test]
fn a_plan_patch_takes_the_group_colour() {
    let (root, mut h, gate) = shown_in(
        merged_mono("draw-plan-colour"),
        &[CORE],
        CanvasMode::Plan,
        |st| st.run_pipeline = true,
    );
    gate.open();
    // The plan draws a mono group with grouping on in its group colour; the
    // patch is tinted the same, so it compares fairly with its surroundings.
    let st = h.state();
    let names = crate::util::group_names(&st.session);
    let colours = crate::panel_edits::group_colors(
        &crate::panel_edits::load_group_colors(&st.session.mosaic.config.ui),
        names.iter().map(String::as_str),
    );
    let c = colours["Ha"];
    let tint = [c.r(), c.g(), c.b()].map(|v| f32::from(v) / 255.0);
    assert!(
        tint[0] != tint[1] || tint[1] != tint[2],
        "a colour, not grey"
    );
    step_to(&mut h, |st| {
        let (Some(stretch), Some(image)) = (st.w.plan().stretch_for_view(), st.w.preview_image(7))
        else {
            return false;
        };
        let Some((_, p)) = st.w.previews.patch(7, "Ha") else {
            return false;
        };
        assert_eq!(p.channels, 1);
        let mut mix = Mix::new([p.width as usize, p.height as usize]);
        mix.add(&converted(st, 7, "Ha", stretch), tint);
        *image == mix.image()
    });
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_plan_composite_patch_mixes_its_slots() {
    let (root, mut h, gate) = shown_on("draw-plan-composite", &[CORE], CanvasMode::Plan, |st| {
        st.run_pipeline = true;
        show_hoo(st);
    });
    gate.open();
    step_to(&mut h, |st| {
        delivered(st, 7, "Ha") == 1 && delivered(st, 7, "OIII") == 1
    });
    // Each slot is its group's channel mean through the plan's stretch for
    // that slot, tinted red, green or blue (OIII is mapped to two slots).
    step_to(&mut h, |st| {
        let (Some(stretch), Some(image)) = (st.w.plan().stretch_for_view(), st.w.preview_image(7))
        else {
            return false;
        };
        if stretch.len() < 3 {
            return false;
        }
        let ha = st.w.previews.patch(7, "Ha").unwrap().1;
        let oiii = st.w.previews.patch(7, "OIII").unwrap().1;
        let mut mix = Mix::new([256, 256]);
        mix.add(&mean_image(&ha, stretch[0]), [1.0, 0.0, 0.0]);
        mix.add(&mean_image(&oiii, stretch[1]), [0.0, 1.0, 0.0]);
        mix.add(&mean_image(&oiii, stretch[2]), [0.0, 0.0, 1.0]);
        *image == mix.image()
    });
    let _ = std::fs::remove_dir_all(&root);
}
