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
//!   groups never analysed) start an analysis while no job runs, unless
//!   automatic analysis is paused ([`auto_analyze`], `ui.auto_analyze`).
//! - An input edit is coordinated with the Panels tab: while one is
//!   pending the pipeline starts nothing (the tab cancels every job); once
//!   it applies, the debounce starts again. A changed input also cancels
//!   any pipeline job still running.
//! - An analysis that ends (finished, failed, or cancelled by the user),
//!   whether the pipeline or the Blend tab started it, is not started
//!   again automatically until the inputs, the analysis settings or the
//!   groups to analyse change (no retry loops). Previews that end are not
//!   repeated for the same inputs either. Previews the user cancelled (or
//!   that failed) stay incomplete ([`Pipeline::previews_complete`]) until
//!   the inputs change or [`Pipeline::request_previews`] asks again;
//!   analysis may still follow them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mmm_core::mosaic::{Grouping, MosaicConfig};
use mmm_core::preview::Preview;
use mmm_core::settings::{AnalyzeSettings, GroupOverride};

use crate::jobs::{ANALYZE_LABEL, JobEvent, JobRunner};
use crate::session::OpenSession;

/// Wait after the last input edit before the pipeline starts again.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);
/// Key in `MosaicConfig.ui` of the automatic analysis switch.
pub const AUTO_ANALYZE_KEY: &str = "auto_analyze";

/// Whether automatic analysis is on (`ui.auto_analyze`, default on).
pub fn auto_analyze(cfg: &MosaicConfig) -> bool {
    cfg.ui
        .get(AUTO_ANALYZE_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// Turn automatic analysis on or off and save `mosaic.json` (a small save
/// on the UI thread: the documented exception). The error is a message for
/// the user; nothing changes then.
pub fn set_auto_analyze(session: &mut OpenSession, on: bool) -> Result<(), String> {
    let mut next = session.mosaic.config.clone();
    if !next.ui.is_object() {
        next.ui = serde_json::json!({});
    }
    next.ui[AUTO_ANALYZE_KEY] = serde_json::Value::Bool(on);
    next.save(&session.mosaic.dir)
        .map_err(|e| format!("Could not save the automatic analysis setting: {e}"))?;
    session.mosaic.config = next;
    Ok(())
}

/// Whether some group needs analysis: one is marked
/// [`MosaicConfig::needs_analyze`], or a resolved group was never analysed
/// (has no directory yet).
fn wants_analysis(session: &OpenSession) -> bool {
    let cfg = &session.mosaic.config;
    !cfg.needs_analyze.is_empty()
        || session.groups.as_ref().is_some_and(|g| {
            g.processing_groups()
                .iter()
                .any(|(name, _)| !cfg.group_dirs.contains_key(name))
        })
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
    /// An automatic analysis ended with this key: not again until it
    /// changes.
    stalled: Option<Key>,
    /// Automatic analysis was on at the last poll.
    auto_was: bool,
    previews: BTreeMap<PathBuf, Arc<Preview>>,
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
    /// next stage when due. `editing`: the Panels tab has an edit waiting
    /// (or had one when `events` were taken). Call every frame after the
    /// runner's and the Panels tab's polls; `now` is the frame time.
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
                // An analysis started elsewhere (the Blend tab's Analyze):
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
                Step::Previews if finished => self.previews_for = Some(job.inputs),
                // The user's Cancel (or a failure) is final for these
                // inputs; the previews stay incomplete.
                Step::Previews => self.previews_cancelled_for = Some(job.inputs),
                Step::Analyze => self.stalled = Some(Key::of(cfg)),
            }
        }

        let auto = auto_analyze(cfg);
        if auto && !self.auto_was {
            // Switched (back) on: try again whatever ended before.
            self.stalled = None;
        }
        self.auto_was = auto;

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
        if let Some(due) = self.due
            && now < due
        {
            ctx.request_repaint_after(due - now);
            return;
        }
        if cfg.panels.is_empty() {
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
        if auto
            && !jobs.busy()
            && wants_analysis(session)
            && self.stalled.as_ref() != Some(&Key::of(&session.mosaic.config))
        {
            let id = jobs.start_analyze(session);
            self.job = Some(Job {
                id,
                step: Step::Analyze,
                inputs: current,
            });
            self.started_analysis = Some(id);
        }
    }

    /// The pipeline stage running (or queued), if any.
    #[allow(dead_code)] // read by the Plan tab (next task)
    pub fn step(&self) -> Option<Step> {
        self.job.as_ref().map(|j| j.step)
    }

    /// The previews received for the current inputs, by panel path (Plan
    /// reads these).
    #[allow(dead_code)] // read by the Plan tab (next task)
    pub fn previews(&self) -> &BTreeMap<PathBuf, Arc<Preview>> {
        &self.previews
    }

    /// Changes whenever the preview cache is dropped (inputs changed), so a
    /// reader knows to drop what it derived from it.
    #[allow(dead_code)] // read by the Plan tab (next task)
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether every preview of the current inputs has been run (the last
    /// previews run finished). False while they run, and after the user
    /// cancelled them or they failed.
    #[allow(dead_code)] // read by the Plan tab (next task)
    pub fn previews_complete(&self) -> bool {
        self.previews_for.is_some() && self.previews_for == self.seen
    }

    /// Run the previews again (the user asked, e.g. after cancelling
    /// them): they start at the next poll once nothing else of the
    /// pipeline runs.
    #[allow(dead_code)] // a Plan tab button (next task)
    pub fn request_previews(&mut self) {
        self.previews_cancelled_for = None;
        self.previews_for = None;
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
    use crate::panels_tab::{Applied, PanelsEdit};
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
        let deadline = Instant::now() + Duration::from_secs(60);
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
        assert_eq!(seen.started, ["Previews", "Analyze"]);
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
        assert_eq!(seen.started, ["Previews", "Analyze"]);
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
        set_auto_analyze(&mut s, false).unwrap();
        let t0 = Instant::now();
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        set_auto_analyze(&mut s, true).unwrap();
        // The user analyses from the Blend tab before the pipeline polls,
        // then cancels it.
        let gate = r.hold();
        let id = r.start_analyze(&mut s);
        r.cancel(id);
        gate.open();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert!(seen.events.contains(&JobEvent::Cancelled { id }));
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

    /// A session with automatic analysis off whose first previews run the
    /// user cancelled; returns the cancelled job's id.
    fn cancel_first_previews(
        p: &mut Pipeline,
        s: &mut OpenSession,
        r: &mut JobRunner,
        t0: Instant,
        seen: &mut Seen,
    ) -> u64 {
        set_auto_analyze(s, false).unwrap();
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
        assert_eq!(seen.started, ["Previews"]);
        assert!(!p.previews_complete());
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
    fn auto_analyze_toggle_respected() {
        let root = temp_dir("pipe-toggle");
        let mut s = two_group_session(&root);
        assert!(auto_analyze(&s.mosaic.config), "on by default");
        set_auto_analyze(&mut s, false).unwrap();
        assert!(!auto_analyze(&MosaicConfig::load(&s.mosaic.dir).unwrap()));
        let (mut p, mut r) = (Pipeline::new(), runner());
        let t0 = Instant::now();
        let mut seen = Seen::default();
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        for _ in 0..20 {
            run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        }
        // Previews still run; analysis waits for the switch.
        assert_eq!(seen.started, ["Previews"]);
        assert!(s.mosaic.config.group_dirs.is_empty());
        set_auto_analyze(&mut s, true).unwrap();
        let ctx = egui::Context::default();
        p.poll(&ctx, t0, &mut s, &mut r, &[], false);
        assert_eq!(p.step(), Some(Step::Analyze));
        run_until(&mut p, &mut s, &mut r, t0, false, &mut seen, idle);
        assert_eq!(seen.started, ["Previews", "Analyze"]);
        assert_eq!(s.mosaic.config.group_dirs.len(), 2);
        assert!(s.mosaic.config.needs_analyze.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
