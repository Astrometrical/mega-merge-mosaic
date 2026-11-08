//! The automatic pipeline (spec §5.2, "analysis runs by itself"): panel
//! previews, then analysis, started by the shell without a button.
//!
//! - On session open, and [`DEBOUNCE`] after the last input edit (panels or
//!   grouping), it starts previews; when they end, analysis.
//! - Previews come first: they are cheap and feed Plan. Each one is kept in
//!   an in-memory cache ([`Pipeline::previews`]) as it arrives; the cache is
//!   dropped when the inputs change (the shared frame may move) and filled
//!   again by the next run, from the session's preview cache on disk.
//! - Groups needing analysis ([`MosaicConfig::needs_analyze`], or resolved
//!   groups never analysed) start an analysis while no job runs. Analysis
//!   is always automatic: an `ui.auto_analyze` switch saved by earlier
//!   versions is ignored (and left in `mosaic.json` as it is).
//! - An input edit is coordinated with the Panels tab: while one is
//!   pending the pipeline starts nothing (the tab cancels every job but
//!   exports); once it applies, the debounce starts again. A changed input also cancels
//!   any pipeline job still running. A Blend settings edit waiting to
//!   apply ([`crate::blend_settings`]) counts as an edit too; once a global
//!   one applies, the groups it marked are analysed again.
//! - An analysis that ends (finished, failed, or cancelled by the user),
//!   whether the pipeline or something else started it, is not started
//!   again automatically until the inputs, the analysis settings or the
//!   groups to analyse change (no retry loops): the analysis is then
//!   [`Pipeline::analysis_stalled`], left to the user. Previews that end are not
//!   repeated for the same inputs either. Previews the user cancelled (or
//!   that failed) stay incomplete ([`Pipeline::previews_complete`]) until
//!   the inputs change or [`Pipeline::request_previews`] asks again;
//!   analysis may still follow them.
//! - Previews keep their own frame; `reference.mmm-frame.json` is the
//!   analysed frame ([`OpenSession::frame`]). When a complete previews run's
//!   frame differs from it (panels changed on disk since the analysis) and
//!   nothing but exports runs, every analysed group is marked as needing
//!   analysis ([`mark_needs_analyze`]), once per previews run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mmm_core::mosaic::{AnalyzeScope, Grouping, MosaicConfig, mark_needs_analyze};
use mmm_core::preview::Preview;
use mmm_core::reference::ReferenceFrame;
use mmm_core::settings::{AnalyzeSettings, GroupOverride};

use crate::jobs::{ANALYZE_LABEL, JobEvent, JobRunner};
use crate::session::OpenSession;

/// Wait after the last input edit before the pipeline starts again.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);

/// Whether some group needs analysis: one is marked
/// [`MosaicConfig::needs_analyze`], or a resolved group was never analysed
/// (has no directory yet).
pub fn wants_analysis(session: &OpenSession) -> bool {
    let cfg = &session.mosaic.config;
    !cfg.needs_analyze.is_empty()
        || session.groups.as_ref().is_some_and(|g| {
            g.processing_groups()
                .iter()
                .any(|(name, _)| !cfg.group_dirs.contains_key(name))
        })
}

/// The groups the automatic analysis covers: only those needing it
/// ([`AnalyzeScope::Needed`]) when a frame was analysed and some resolved
/// group is analysed and still valid (so analysing everything would redo
/// it), else every group. A moved frame turns the former into the latter
/// ([`crate::jobs::JobRunner`]).
fn analysis_scope(session: &OpenSession) -> AnalyzeScope {
    let cfg = &session.mosaic.config;
    let some_valid = session.groups.as_ref().is_some_and(|g| {
        g.processing_groups()
            .iter()
            .any(|(n, _)| cfg.group_dirs.contains_key(n) && !cfg.needs_analyze.contains(n))
    });
    if session.frame.is_some() && some_valid {
        AnalyzeScope::Needed
    } else {
        AnalyzeScope::All
    }
}

/// A pipeline stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Building or loading the panel previews.
    Previews,
    /// Analysing the groups.
    Analyze,
}

/// What the previews depend on: the panel files, their manual groups and
/// the grouping (not the fingerprints, which each analysis refreshes).
#[derive(Clone, Debug, PartialEq)]
struct Inputs {
    panels: Vec<(PathBuf, Option<String>)>,
    grouping: Grouping,
}

impl Inputs {
    fn of(cfg: &MosaicConfig) -> Self {
        Inputs {
            panels: cfg
                .panels
                .iter()
                .map(|p| (p.path.clone(), p.manual_group.clone()))
                .collect(),
            grouping: cfg.grouping.clone(),
        }
    }
}

/// What an automatic analysis depends on; an ended analysis is not
/// started again until this changes.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    inputs: Inputs,
    needs: BTreeSet<String>,
    analyze: AnalyzeSettings,
    overrides: BTreeMap<String, GroupOverride>,
}

impl Key {
    fn of(cfg: &MosaicConfig) -> Self {
        Key {
            inputs: Inputs::of(cfg),
            needs: cfg.needs_analyze.clone(),
            analyze: cfg.analyze.clone(),
            overrides: cfg.group_overrides.clone(),
        }
    }
}

/// The pipeline job in flight.
#[derive(Debug)]
struct Job {
    id: u64,
    step: Step,
    /// The inputs it started for.
    inputs: Inputs,
}

/// The automatic preview and analysis pipeline of one open session (owned
/// by the shell; module docs).
#[derive(Default)]
pub struct Pipeline {
    /// The inputs seen at the last poll (`None` before the first).
    seen: Option<Inputs>,
    /// When the next run may start.
    due: Option<Instant>,
    job: Option<Job>,
    /// The inputs whose previews have all been run (finished).
    previews_for: Option<Inputs>,
    /// The inputs whose previews the user cancelled (or that failed): not
    /// run again until the inputs change or previews are requested.
    previews_cancelled_for: Option<Inputs>,
    /// An analysis ended with this key: not again until it changes.
    stalled: Option<Key>,
    previews: BTreeMap<PathBuf, Arc<Preview>>,
    /// The frame of the current inputs' last complete previews run, and
    /// whether it has been compared with the analysed frame yet.
    previews_frame: Option<(ReferenceFrame, bool)>,
    generation: u64,
    /// An analysis started since [`Pipeline::take_started_analysis`].
    started_analysis: Option<u64>,
}

impl Pipeline {
    /// A pipeline for a session just opened (it starts on the first poll).
    pub fn new() -> Self {
        Pipeline::default()
    }

    /// Take the ended jobs from `events`, notice input edits, and start the
    /// next stage when due. `editing`: the Panels tab has an edit waiting,
    /// or a global Blend settings edit waits or has jobs it cancelled still
    /// to be reported ([`crate::blend_settings::SettingsPanel::editing`]) —
    /// now or when `events` were taken. Jobs ending while editing are
    /// restarted, not taken for the user's cancels. Call every frame after
    /// the runner's, the Panels tab's and the Blend settings' polls; `now`
    /// is the frame time.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        now: Instant,
        session: &mut OpenSession,
        jobs: &mut JobRunner,
        events: &[JobEvent],
        editing: bool,
    ) {
        let cfg = &session.mosaic.config;
        let current = Inputs::of(cfg);
        for event in events {
            let id = match event {
                JobEvent::Preview(p) => {
                    // Only panels in the current inputs; the cache is
                    // cleared below if the inputs changed.
                    if current.panels.iter().any(|(path, _)| path == &p.path) {
                        self.previews.insert(p.path.clone(), Arc::clone(p));
                    }
                    continue;
                }
                JobEvent::Finished { id, .. } | JobEvent::Cancelled { id } => *id,
                JobEvent::Failed { id, .. } => *id,
            };
            let finished = matches!(event, JobEvent::Finished { .. });
            let Some(job) = self.job.take_if(|j| j.id == id) else {
                // An analysis started elsewhere (not by the pipeline):
                // once it has ended, failed or been cancelled by the user,
                // it is not repeated automatically either.
                if !editing
                    && self.seen.as_ref() == Some(&current)
                    && jobs.label(id) == Some(ANALYZE_LABEL)
                {
                    self.stalled = Some(Key::of(cfg));
                }
                continue;
            };
            // Ended by an edit (pending, or the inputs moved on): the run
            // starts again for the new inputs. Otherwise an analysis is
            // not repeated for these inputs (finished, failed, or
            // cancelled by the user).
            if editing || job.inputs != current {
                continue;
            }
            match job.step {
                // Cancelled previews are incomplete: run them again.
                Step::Previews if finished => {
                    self.previews_for = Some(job.inputs);
                    if let Some(frame) = jobs.take_previews_frame() {
                        self.previews_frame = Some((frame, false));
                    }
                }
                // The user's Cancel (or a failure) is final for these
                // inputs; the previews stay incomplete.
                Step::Previews => self.previews_cancelled_for = Some(job.inputs),
                Step::Analyze => self.stalled = Some(Key::of(cfg)),
            }
        }

        if self.seen.as_ref() != Some(&current) {
            // Opening starts at once; an edit waits for the debounce.
            let delay = if self.seen.is_some() {
                DEBOUNCE
            } else {
                Duration::ZERO
            };
            self.due = Some(now + delay);
            self.seen = Some(current.clone());
            self.previews.clear();
            self.previews_frame = None;
            self.generation += 1;
            self.previews_for = None;
            self.previews_cancelled_for = None;
            if let Some(job) = &self.job {
                jobs.cancel(job.id);
            }
        }
        if editing {
            // The Panels tab has cancelled every job and applies the edit
            // once they have stopped; the debounce counts from then.
            self.due = Some(now + DEBOUNCE);
            return;
        }
        if self.job.is_some() {
            return;
        }
        self.check_frame(session, jobs);
        if let Some(due) = self.due
            && now < due
        {
            ctx.request_repaint_after(due - now);
            return;
        }
        if session.mosaic.config.panels.is_empty() {
            return;
        }
        if self.previews_for.as_ref() != Some(&current)
            && self.previews_cancelled_for.as_ref() != Some(&current)
        {
            let id = jobs.start_previews(session);
            self.job = Some(Job {
                id,
                step: Step::Previews,
                inputs: current,
            });
            return;
        }
        if !jobs.busy() && wants_analysis(session) && !self.analysis_stalled(session) {
            let id = jobs.start_analyze_scoped(session, analysis_scope(session));
            self.job = Some(Job {
                id,
                step: Step::Analyze,
                inputs: current,
            });
            self.started_analysis = Some(id);
        }
    }

    /// Once nothing but exports runs (no analysis pending or running),
    /// compare the previews' frame with the analysed frame, once per
    /// previews run: when they differ (the panels changed on disk since the
    /// analysis), every analysed group is marked as needing analysis. A
    /// failure (a store held elsewhere) is logged, not retried.
    fn check_frame(&mut self, session: &mut OpenSession, jobs: &JobRunner) {
        let Some((frame, checked)) = &mut self.previews_frame else {
            return;
        };
        if *checked || jobs.busy_except_exports() {
            return;
        }
        *checked = true;
        if session
            .frame
            .as_ref()
            .is_none_or(|analysed| analysed == frame)
        {
            return;
        }
        let cfg = &session.mosaic.config;
        let groups: Vec<String> = cfg
            .group_dirs
            .keys()
            .filter(|g| !cfg.needs_analyze.contains(*g))
            .cloned()
            .collect();
        if groups.is_empty() {
            return;
        }
        tracing::info!("the panels' frame differs from the analysed frame: analysis needed");
        if let Err(e) = mark_needs_analyze(&mut session.mosaic, &groups) {
            tracing::warn!("could not mark the groups as needing analysis: {e}");
        }
    }

    /// The pipeline stage running (or queued), if any.
    pub fn step(&self) -> Option<Step> {
        self.job.as_ref().map(|j| j.step)
    }

    /// The previews received for the current inputs, by panel path (Plan
    /// reads these).
    pub fn previews(&self) -> &BTreeMap<PathBuf, Arc<Preview>> {
        &self.previews
    }

    /// Changes whenever the preview cache is dropped (inputs changed), so a
    /// reader knows to drop what it derived from it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether every preview of the current inputs has been run (the last
    /// previews run finished). False while they run, and after the user
    /// cancelled them or they failed.
    pub fn previews_complete(&self) -> bool {
        self.previews_for.is_some() && self.previews_for == self.seen
    }

    /// Whether `session` needs an analysis the pipeline will not start by
    /// itself: one ended (cancelled by the user, failed, or left groups
    /// still to analyse) and nothing it depends on has changed since. It is
    /// left to the user then.
    pub fn analysis_stalled(&self, session: &OpenSession) -> bool {
        wants_analysis(session) && self.stalled.as_ref() == Some(&Key::of(&session.mosaic.config))
    }

    /// Whether the previews of the current inputs stopped short (the user
    /// cancelled them, or they failed) and wait for
    /// [`Pipeline::request_previews`]: the pipeline does not run them again
    /// by itself.
    pub fn previews_paused(&self) -> bool {
        self.previews_cancelled_for.is_some() && self.previews_cancelled_for == self.seen
    }

    /// Run the previews again (the user asked, e.g. after cancelling
    /// them): they start at the next poll once nothing else of the
    /// pipeline runs.
    pub fn request_previews(&mut self) {
        self.previews_cancelled_for = None;
        self.previews_for = None;
    }

    /// Whether the pipeline is waiting to start (the debounce after an
    /// input edit, or the first poll) with no job of its own running.
    pub fn waiting(&self, now: Instant) -> bool {
        self.job.is_none() && self.due.is_some_and(|due| now < due)
    }

    /// The id of an analysis the pipeline started since the last call.
    pub fn take_started_analysis(&mut self) -> Option<u64> {
        self.started_analysis.take()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use mmm_core::mosaic::MosaicConfig;
    use mmm_core::tiles::TileStore;

    use super::*;
    use crate::jobs::tests::drain;
    use crate::panel_edits::{Applied, PanelsEdit};
    use crate::start::tests::temp_dir;
    use crate::testutil::two_group_session;

    /// What the test saw while the pipeline ran.
    #[derive(Default)]
    struct Seen {
        /// Labels of the jobs that ran, in order (consecutive repeats
        /// collapsed).
        started: Vec<String>,
        previews: usize,
        events: Vec<JobEvent>,
    }

    impl Seen {
        fn note(&mut self, r: &JobRunner, events: &[JobEvent]) {
            for e in events {
                match e {
                    JobEvent::Preview(_) => self.previews += 1,
                    other => self.events.push(other.clone()),
                }
            }
            if let Some(v) = r.views().first()
                && self.started.last() != Some(&v.label)
            {
                self.started.push(v.label.clone());
            }
        }
    }

    /// Poll runner and pipeline at time `at` until `done` (bounded).
    fn run_until(
        p: &mut Pipeline,
        s: &mut OpenSession,
        r: &mut JobRunner,
        at: Instant,
        editing: bool,
        seen: &mut Seen,
        done: impl Fn(&Pipeline, &JobRunner) -> bool,
    ) {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        loop {
            let events = r.poll(s);
            p.poll(&ctx, at, s, r, &events, editing);
            seen.note(r, &events);
            if done(p, r) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out; saw {:?}",
                seen.started
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn idle(_: &Pipeline, r: &JobRunner) -> bool {
        !r.busy()
    }

    fn runner() -> JobRunner {
        JobRunner::new(egui::Context::default())
    }

    fn valid0(path: &std::path::Path) -> usize {
        TileStore::open(path).unwrap().valid_count(0)
    }

    #[test]
    fn pipeline_runs_previews_then_analysis_on_open() {
        let root = temp_dir("pipe-open");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        // On open the pipeline starts at once (no debounce), previews first.
        p.poll(&egui::Context::default(), t0, &mut s, &mut r, &[], false);
        assert_eq!(p.step(), Some(Step::Previews));
        assert_eq!(r.views()[0].label, "Previews");
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert_eq!(seen.started, ["Previews", "Analyse"]);
        assert_eq!(seen.previews, 8, "each preview delivered");
        assert_eq!(p.previews().len(), 8);
        assert!(p.previews_complete());
        assert!(
            p.previews()
                .keys()
                .all(|k| s.mosaic.config.panels.iter().any(|e| &e.path == k))
        );
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_eq!(s.mosaic.config.group_dirs.len(), 2);
        assert_eq!(p.step(), None);
        assert!(p.take_started_analysis().is_some());
        // Nothing more to do: it stays idle.
        for _ in 0..20 {
            let events = r.poll(&mut s);
            p.poll(
                &egui::Context::default(),
                t0,
                &mut s,
                &mut r,
                &events,
                false,
            );
            assert!(!r.busy());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The session with one panel fewer (the last).
    fn drop_last(s: &OpenSession) -> PanelsEdit {
        let mut edit = PanelsEdit::of(s);
        edit.panels.pop();
        edit
    }

    #[test]
    fn edit_during_analysis_restarts_pipeline() {
        let root = temp_dir("pipe-edit");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let mut seen = Seen::default();
        let mut t = Instant::now();
        run_until(&mut p, &mut s, &mut r, t, false, &mut seen, idle);
        let id = r.start_blend(&mut s, "Ha", None);
        drain(&mut r, &mut s);
        assert!(valid0(&s.mosaic.result_path("Ha")) > 0, "blend {id}");

        // A first edit while idle applies at once and stales the tiles;
        // the pipeline waits for the debounce.
        assert!(matches!(drop_last(&s).apply(&mut s, &mut r), Applied::Done));
        assert_eq!(valid0(&s.mosaic.result_path("Ha")), 0);
        let previews = r.hold();
        let ctx = egui::Context::default();
        p.poll(&ctx, t, &mut s, &mut r, &[], false);
        assert!(p.previews().is_empty(), "previews dropped on an edit");
        p.poll(&ctx, t + Duration::from_secs(1), &mut s, &mut r, &[], false);
        assert!(!r.busy(), "debounce");
        t += DEBOUNCE + Duration::from_millis(100);
        p.poll(&ctx, t, &mut s, &mut r, &[], false);
        assert_eq!(p.step(), Some(Step::Previews));

        // Previews, then the analysis resolves and plans (stores locked
        // and invalidated, groups marked)...
        let resolve = r.hold();
        previews.open();
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t, false, &mut seen, |p, _| {
            p.step() == Some(Step::Analyze)
        });
        let analyze = r.hold();
        resolve.open();
        run_until(&mut p, &mut s, &mut r, t, false, &mut seen, |_, r| {
            r.views()[0].stage != "Resolving groups…"
        });
        assert_eq!(s.mosaic.config.needs_analyze.len(), 2, "planned");
        let analysis = p.take_started_analysis().unwrap();

        // ...when a second edit lands: it cancels the analysis and waits.
        let Applied::Queued(edit) = drop_last(&s).apply(&mut s, &mut r) else {
            panic!("the edit should wait for the jobs");
        };
        analyze.open();
        run_until(&mut p, &mut s, &mut r, t, true, &mut seen, idle);
        assert!(seen.events.contains(&JobEvent::Cancelled { id: analysis }));
        assert_eq!(s.mosaic.config.panels.len(), 7, "not applied yet");
        // Nothing restarts while the edit is pending.
        assert_eq!(p.step(), None);
        assert!(matches!(edit.apply(&mut s, &mut r), Applied::Done));
        assert_eq!(valid0(&s.mosaic.result_path("Ha")), 0, "never stale-valid");

        // The debounce restarts from the edit, then the whole pipeline runs.
        p.poll(&ctx, t, &mut s, &mut r, &[], false);
        p.poll(&ctx, t + Duration::from_secs(1), &mut s, &mut r, &[], false);
        assert!(!r.busy());
        let t = t + DEBOUNCE + Duration::from_millis(100);
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t, false, &mut seen, idle);
        assert_eq!(seen.started, ["Previews", "Analyse"]);
        assert!(
            seen.events
                .iter()
                .all(|e| matches!(e, JobEvent::Finished { .. }))
        );
        assert_eq!(p.previews().len(), 6);
        assert_eq!(s.mosaic.config.panels.len(), 6);
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_eq!(valid0(&s.mosaic.result_path("Ha")), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_analysis_cancelled_is_not_restarted() {
        let root = temp_dir("pipe-manual");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        let mut seen = Seen::default();
        // The pipeline's previews run (held); the user analyses (not the
        // pipeline), queued behind them, then cancels it.
        let gate = r.hold();
        p.poll(&egui::Context::default(), t0, &mut s, &mut r, &[], false);
        assert_eq!(p.step(), Some(Step::Previews));
        let id = r.start_analyze(&mut s);
        r.cancel(id);
        gate.open();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert!(seen.events.contains(&JobEvent::Cancelled { id }));
        assert!(p.previews_complete());
        assert!(p.analysis_stalled(&s), "left to the user");
        for _ in 0..20 {
            let events = r.poll(&mut s);
            p.poll(
                &egui::Context::default(),
                t0,
                &mut s,
                &mut r,
                &events,
                false,
            );
            assert!(!r.busy(), "a cancelled analysis was restarted");
        }
        assert_eq!(p.step(), None);
        assert_eq!(p.take_started_analysis(), None, "the pipeline analysed");
        assert!(s.mosaic.config.group_dirs.is_empty(), "nothing analysed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A session whose first previews run the user cancelled (the analysis
    /// follows all the same); returns the cancelled job's id.
    fn cancel_first_previews(
        p: &mut Pipeline,
        s: &mut OpenSession,
        r: &mut JobRunner,
        t0: Instant,
        seen: &mut Seen,
    ) -> u64 {
        let gate = r.hold();
        p.poll(&egui::Context::default(), t0, s, r, &[], false);
        assert_eq!(p.step(), Some(Step::Previews));
        let first = r.views()[0].id;
        r.cancel(first);
        gate.open();
        run_until(p, s, r, t0, false, seen, idle);
        assert!(seen.events.contains(&JobEvent::Cancelled { id: first }));
        first
    }

    #[test]
    fn user_cancelled_previews_stay_cancelled() {
        let root = temp_dir("pipe-prev-cancel");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        let mut seen = Seen::default();
        cancel_first_previews(&mut p, &mut s, &mut r, t0, &mut seen);
        for _ in 0..20 {
            let events = r.poll(&mut s);
            p.poll(
                &egui::Context::default(),
                t0,
                &mut s,
                &mut r,
                &events,
                false,
            );
            assert!(!r.busy(), "cancelled previews were restarted");
        }
        assert_eq!(seen.started, ["Previews", "Analyse"]);
        assert!(!p.previews_complete());
        assert!(p.previews_paused());
        assert_eq!(p.step(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn request_previews_restarts_after_cancel() {
        let root = temp_dir("pipe-prev-request");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        let mut seen = Seen::default();
        let first = cancel_first_previews(&mut p, &mut s, &mut r, t0, &mut seen);
        p.request_previews();
        p.poll(&egui::Context::default(), t0, &mut s, &mut r, &[], false);
        assert_eq!(p.step(), Some(Step::Previews));
        assert_ne!(r.views()[0].id, first);
        assert!(!p.previews_complete(), "running");
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert_eq!(p.previews().len(), 8);
        assert!(p.previews_complete());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn frame_mismatch_marks_groups_needs_analyze() {
        use mmm_core::mosaic::FRAME_FILE;
        use mmm_core::preview::preview_frame_path;
        use mmm_core::reference::ReferenceFrame;

        let root = temp_dir("pipe-frame");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert!(s.mosaic.config.needs_analyze.is_empty());
        r.start_blend(&mut s, "Ha", None);
        drain(&mut r, &mut s);
        assert!(valid0(&s.mosaic.result_path("Ha")) > 0);
        // The panels changed on disk since that analysis: its frame no
        // longer matches theirs (an analysed frame of another size).
        let Some(ReferenceFrame::Aligned { width, height, wcs }) = s.frame.clone() else {
            panic!("an aligned analysed frame");
        };
        let old = ReferenceFrame::Aligned {
            width: width + 8,
            height,
            wcs,
        };
        old.save(&s.mosaic.dir.join(FRAME_FILE)).unwrap();
        s.frame = Some(old.clone());
        // Reopened: the previews find the mismatch and every group is
        // marked (and its result staled) before the analysis, held here,
        // has done anything.
        let mut p = Pipeline::new();
        let previews = r.hold();
        p.poll(&egui::Context::default(), t0, &mut s, &mut r, &[], false);
        let analysis = r.hold();
        previews.open();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, |p, _| {
            p.step() == Some(Step::Analyze)
        });
        assert_eq!(
            s.mosaic
                .config
                .needs_analyze
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            ["Ha", "OIII"]
        );
        assert_eq!(MosaicConfig::load(&s.mosaic.dir).unwrap(), s.mosaic.config);
        assert_eq!(valid0(&s.mosaic.result_path("Ha")), 0);
        assert_eq!(
            ReferenceFrame::load(&s.mosaic.dir.join(FRAME_FILE)).unwrap(),
            old,
            "previews never write the analysed frame"
        );
        // The analysis runs, and the frames agree again.
        analysis.open();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert!(s.mosaic.config.needs_analyze.is_empty());
        let previews = ReferenceFrame::load(&preview_frame_path(&s.mosaic.dir)).unwrap();
        assert_eq!(s.frame.as_ref(), Some(&previews));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stored_auto_analyze_off_is_ignored() {
        let root = temp_dir("pipe-auto-off");
        let mut s = two_group_session(&root);
        // An earlier version saved automatic analysis off.
        s.mosaic.config.ui["auto_analyze"] = serde_json::Value::Bool(false);
        s.mosaic.save().unwrap();
        let (mut p, mut r) = (Pipeline::new(), runner());
        let mut seen = Seen::default();
        run_until(
            &mut p,
            &mut s,
            &mut r,
            Instant::now(),
            false,
            &mut seen,
            idle,
        );
        // Analysed by itself all the same; the key is left as it was.
        assert_eq!(seen.started, ["Previews", "Analyse"]);
        assert_eq!(s.mosaic.config.group_dirs.len(), 2);
        let saved = MosaicConfig::load(&s.mosaic.dir).unwrap();
        assert_eq!(saved.ui["auto_analyze"], serde_json::Value::Bool(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_changes_reanalyse_only_when_global() {
        use crate::blend_settings::SettingsPanel;
        use mmm_core::photometry::GainMode;
        use mmm_core::settings::ChangeImpact;

        let root = temp_dir("pipe-settings");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert_eq!(seen.started, ["Previews", "Analyse"]);
        let mut panel = SettingsPanel::new();

        // A merge setting is a draft: nothing is applied, nothing reruns.
        let mut draft = s.mosaic.config.blend.clone();
        draft.feather_px = 100.0;
        panel.edit_draft(&mut s.mosaic.config, draft);
        for _ in 0..20 {
            run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        }
        assert_eq!(seen.started, ["Previews", "Analyse"]);

        // Global: the pipeline analyses again by itself.
        let mut a = s.mosaic.config.analyze.clone();
        a.gain = GainMode::Unity;
        assert_eq!(
            panel.edit_analysis(a, &mut s, &mut r),
            Some(ChangeImpact::Reanalyze)
        );
        assert_eq!(s.mosaic.config.needs_analyze.len(), 2);
        p.poll(&egui::Context::default(), t0, &mut s, &mut r, &[], false);
        assert_eq!(p.step(), Some(Step::Analyze));
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert!(s.mosaic.config.needs_analyze.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reverted_settings_edit_restarts_the_cancelled_analysis() {
        use crate::blend_settings::SettingsPanel;
        use mmm_core::photometry::GainMode;

        let root = temp_dir("pipe-settings-revert");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let ctx = egui::Context::default();
        let t0 = Instant::now();
        // Previews first, with the analysis held back.
        let mut seen = Seen::default();
        let previews = r.hold();
        p.poll(&ctx, t0, &mut s, &mut r, &[], false);
        let gate = r.hold();
        previews.open();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, |p, _| {
            p.step() == Some(Step::Analyze)
        });
        let first = p.take_started_analysis().unwrap();

        // A global edit cancels the automatic analysis, then is reverted
        // before the cancel has been reported.
        let mut panel = SettingsPanel::new();
        let original = s.mosaic.config.analyze.clone();
        let mut a = original.clone();
        a.gain = GainMode::Unity;
        assert_eq!(panel.edit_analysis(a, &mut s, &mut r), None);
        assert!(panel.editing(&s.mosaic.config));
        // The user puts the old value back (nothing differs: no edit).
        assert_eq!(panel.edit_analysis(original, &mut s, &mut r), None);
        assert!(panel.editing(&s.mosaic.config), "until the cancel lands");
        gate.open();

        // The shell's order: runner, then `editing`, then the panel, then
        // the pipeline.
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        let mut ended = Vec::new();
        let mut again = None;
        let mut t = t0;
        loop {
            let events = r.poll(&mut s);
            let editing = panel.editing(&s.mosaic.config);
            panel.poll(&ctx, &mut s, &mut r);
            let editing = editing || panel.editing(&s.mosaic.config);
            p.poll(&ctx, t, &mut s, &mut r, &events, editing);
            ended.extend(events);
            again = again.or(p.take_started_analysis());
            if let Some(id) = again
                && ended.contains(&JobEvent::Finished {
                    id,
                    label: "Analyse".into(),
                })
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "analysis never re-run: {ended:?}"
            );
            // Simulated time moves on, past the debounce.
            t += Duration::from_millis(200);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(ended.contains(&JobEvent::Cancelled { id: first }));
        assert_ne!(again, Some(first));
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_eq!(s.mosaic.config.group_dirs.len(), 2);
        assert_eq!(s.mosaic.config.analyze.gain, GainMode::Fit, "reverted");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every file under `dir` with its modification time (a group's
    /// analysis and result files): equal before and after means untouched.
    fn stamp(dir: &std::path::Path) -> Vec<(PathBuf, std::time::SystemTime)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let meta = e.metadata().unwrap();
                if meta.is_dir() {
                    todo.push(e.path());
                } else {
                    out.push((e.path(), meta.modified().unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// Submit `edit` as the Panels tab does and poll until it has applied.
    fn apply_edit(edit: PanelsEdit, s: &mut OpenSession, r: &mut JobRunner) {
        let ctx = egui::Context::default();
        let mut edits = crate::panel_edits::PanelEdits::default();
        edits.poll(&ctx, s, r);
        edits.submit(edit, s, r);
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while edits.has_pending() {
            assert!(Instant::now() < deadline, "the edit never applied");
            edits.poll(&ctx, s, r);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(edits.take_errors().is_empty());
    }

    /// Run the pipeline after an edit (debounce included) until idle.
    fn run_after_edit(p: &mut Pipeline, s: &mut OpenSession, r: &mut JobRunner) -> Seen {
        let t = Instant::now();
        let ctx = egui::Context::default();
        p.poll(&ctx, t, s, r, &[], false);
        let mut seen = Seen::default();
        run_until(
            p,
            s,
            r,
            t + DEBOUNCE + Duration::from_millis(100),
            false,
            &mut seen,
            idle,
        );
        seen
    }

    #[test]
    fn adding_a_panel_to_one_group_keeps_the_others_merged() {
        use mmm_core::session_summary::{ResultState, result_state};
        let root = temp_dir("pipe-scoped-add");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let mut seen = Seen::default();
        run_until(
            &mut p,
            &mut s,
            &mut r,
            Instant::now(),
            false,
            &mut seen,
            idle,
        );
        r.start_blend(&mut s, "Ha", None);
        r.start_blend(&mut s, "OIII", None);
        drain(&mut r, &mut s);
        let (ha, oiii) = (
            s.mosaic.group_session_dir("Ha"),
            s.mosaic.group_session_dir("OIII"),
        );
        let (ha_before, oiii_before) = (stamp(&ha), stamp(&oiii));
        assert_eq!(
            result_state(&s.mosaic.result_path("OIII")),
            ResultState::Complete
        );

        // One more Ha panel (a copy of the first: same canvas).
        let first = s.mosaic.config.panels[0].path.clone();
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Ha")
        );
        let extra = first.with_file_name("Ha_extra.xisf");
        std::fs::copy(&first, &extra).unwrap();
        let mut edit = PanelsEdit::of(&s);
        edit.panels.push(mmm_core::mosaic::PanelEntry {
            path: extra,
            fingerprint: None,
            manual_group: None,
        });
        apply_edit(edit, &mut s, &mut r);
        let marked: Vec<&str> = s
            .mosaic
            .config
            .needs_analyze
            .iter()
            .map(|g| g.as_str())
            .collect();
        assert_eq!(marked, ["Ha"]);
        assert_eq!(
            result_state(&s.mosaic.result_path("OIII")),
            ResultState::Complete
        );
        assert_ne!(
            result_state(&s.mosaic.result_path("Ha")),
            ResultState::Complete
        );

        let seen = run_after_edit(&mut p, &mut s, &mut r);
        assert!(
            seen.events
                .iter()
                .all(|e| matches!(e, JobEvent::Finished { .. }))
        );
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_eq!(
            stamp(&oiii),
            oiii_before,
            "OIII was neither re-analysed nor re-merged"
        );
        assert_eq!(
            result_state(&s.mosaic.result_path("OIII")),
            ResultState::Complete
        );
        assert_ne!(stamp(&ha), ha_before, "Ha was re-analysed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_frame_moving_add_reanalyses_everything() {
        let root = temp_dir("pipe-frame-moves");
        let (mut s, extra) = crate::testutil::solved_two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let mut seen = Seen::default();
        run_until(
            &mut p,
            &mut s,
            &mut r,
            Instant::now(),
            false,
            &mut seen,
            idle,
        );
        assert!(
            seen.events
                .iter()
                .all(|e| matches!(e, JobEvent::Finished { .. })),
            "{:?}",
            seen.events
        );
        let frame = s.frame.clone().expect("analysed frame");
        let (ha, oiii) = (
            s.mosaic.group_session_dir("Ha"),
            s.mosaic.group_session_dir("OIII"),
        );
        let (ha_before, oiii_before) = (stamp(&ha), stamp(&oiii));

        let mut edit = PanelsEdit::of(&s);
        edit.panels.push(mmm_core::mosaic::PanelEntry {
            path: extra,
            fingerprint: None,
            manual_group: Some("Ha".into()),
        });
        apply_edit(edit, &mut s, &mut r);
        let marked: Vec<&str> = s
            .mosaic
            .config
            .needs_analyze
            .iter()
            .map(|g| g.as_str())
            .collect();
        assert_eq!(
            marked,
            ["Ha", "OIII"],
            "the moved frame touches every group"
        );

        let seen = run_after_edit(&mut p, &mut s, &mut r);
        assert!(
            seen.events
                .iter()
                .all(|e| matches!(e, JobEvent::Finished { .. })),
            "{:?}",
            seen.events
        );
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_ne!(s.frame.as_ref(), Some(&frame), "the canvas grew");
        assert_ne!(stamp(&ha), ha_before);
        assert_ne!(stamp(&oiii), oiii_before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_a_panel_from_one_group_keeps_the_others_merged() {
        use mmm_core::session_summary::{ResultState, result_state};
        let root = temp_dir("pipe-scoped-remove");
        let mut s = two_group_session(&root);
        let (mut p, mut r) = (Pipeline::new(), runner());
        let mut seen = Seen::default();
        run_until(
            &mut p,
            &mut s,
            &mut r,
            Instant::now(),
            false,
            &mut seen,
            idle,
        );
        r.start_blend(&mut s, "Ha", None);
        r.start_blend(&mut s, "OIII", None);
        drain(&mut r, &mut s);
        let (ha, oiii) = (
            s.mosaic.group_session_dir("Ha"),
            s.mosaic.group_session_dir("OIII"),
        );
        let (ha_before, oiii_before) = (stamp(&ha), stamp(&oiii));

        let mut edit = PanelsEdit::of(&s);
        let last_ha = edit
            .panels
            .iter()
            .rposition(|e| {
                e.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("Ha")
            })
            .unwrap();
        edit.panels.remove(last_ha);
        apply_edit(edit, &mut s, &mut r);
        let marked: Vec<&str> = s
            .mosaic
            .config
            .needs_analyze
            .iter()
            .map(|g| g.as_str())
            .collect();
        assert_eq!(marked, ["Ha"]);
        assert_eq!(
            result_state(&s.mosaic.result_path("OIII")),
            ResultState::Complete
        );

        let seen = run_after_edit(&mut p, &mut s, &mut r);
        assert!(
            seen.events
                .iter()
                .all(|e| matches!(e, JobEvent::Finished { .. }))
        );
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_eq!(stamp(&oiii), oiii_before, "OIII untouched");
        assert_eq!(
            result_state(&s.mosaic.result_path("OIII")),
            ResultState::Complete
        );
        assert_ne!(stamp(&ha), ha_before, "Ha was re-analysed");
        let _ = std::fs::remove_dir_all(&root);
    }
}
