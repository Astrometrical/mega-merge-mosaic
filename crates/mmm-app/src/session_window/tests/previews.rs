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

/// "Core" (id 7) and "Halo" (id 9): both in the first 256 × 256 tile of
/// the 600 × 420 synthetic canvas.
const CORE: (u64, &str, [u64; 4]) = (7, "Core", [100, 100, 200, 160]);
const HALO: (u64, &str, [u64; 4]) = (9, "Halo", [0, 0, 64, 64]);

/// A merged two-group session with `rois`, its window showing the mosaic of
/// Ha with the ROIs section open (once `prepare` has adjusted it, before
/// the first frame). Every preview started waits for the returned gate.
fn shown(
    tag: &str,
    rois: &[(u64, &str, [u64; 4])],
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    shown_on(tag, rois, CanvasMode::Mosaic, prepare)
}

/// [`shown`] on the canvas `mode` (drawn, with the plan's panels placed).
fn shown_on(
    tag: &str,
    rois: &[(u64, &str, [u64; 4])],
    mode: CanvasMode,
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    let (root, mut s, jobs) = merged(tag);
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
fn show_hoo(st: &mut State) {
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
    ];
    save_compositions(&mut st.session.mosaic.config.ui, &[hoo]);
    st.w.select_view(ViewKind::Composition(0));
}

fn log(st: &State) -> Vec<Logged> {
    st.w.previews.log().to_vec()
}

/// The previews started so far, in order: `(roi, group)`.
fn started(st: &State) -> Vec<(u64, String)> {
    log(st)
        .into_iter()
        .filter_map(|e| match e {
            Logged::Started(roi, group, _) => Some((roi, group)),
            _ => None,
        })
        .collect()
}

/// The key the last preview started was made with.
fn last_started_key(st: &State) -> PreviewKey {
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
fn delivered(st: &State, roi: u64, group: &str) -> usize {
    log(st)
        .iter()
        .filter(|e| **e == Logged::Delivered(roi, group.into()))
        .count()
}

/// How many times `(roi, group)`'s preview was cancelled.
fn cancelled(st: &State, roi: u64, group: &str) -> usize {
    log(st)
        .iter()
        .filter(|e| **e == Logged::Cancelled(roi, group.into()))
        .count()
}

/// The key a preview of `rect` of `group` is wanted with now.
fn wanted(st: &State, group: &str, rect: [u64; 4]) -> PreviewKey {
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
fn draft_feather(st: &State) -> f32 {
    crate::blend_settings::load_draft(&st.session.mosaic.config).feather_px
}

/// Let the held preview go, holding those started from now on.
fn release(h: &mut Harness<'static, State>, gate: Arc<Gate>) -> Arc<Gate> {
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
