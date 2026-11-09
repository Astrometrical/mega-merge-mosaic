//! The checks engine (spec §6.1): mmm-core's structured checks, run on a
//! worker, for the UI to render.
//!
//! A run reads every panel's header ([`panel_facts`]), resolves the groups
//! if the session has none resolved, and computes the input checks, the
//! file checks and, for each group with a finished analysis
//! (`session.json` present, not marked [`MosaicConfig::needs_analyze`],
//! so never one being analysed), the analysis checks. Runs start after
//! group resolution (the session's groups changed), after the inputs
//! change, and after each previews or analysis job; a request while one
//! runs starts another when it ends. The last report stays shown
//! meanwhile.
//!
//! Accepting a check records its keys in [`MosaicConfig::accepted_checks`]
//! and saves `mosaic.json` on the UI thread: a small save, the documented
//! exception to "no file I/O on the UI thread"; un-accepting mirrors it.
//!
//! Every analysed group gets its layout, statistic and seam checks. The seam
//! checks measure the seams with the feather of the group's last whole merge
//! when it has one, else the draft's ([`group_feathers`]); the seam lines go
//! into the report for the canvas. A whole-canvas merge ending re-runs the
//! checks once no other whole-canvas merge is queued or running (Merge all
//! runs them once, after its last merge, however that ends), and so does a
//! draft feather change, once typing has paused for [`CHECKS_DEBOUNCE`].
//!
//! Measuring a group's seams is the run's costly part (a health pass over
//! its L8 owner map), so a run measures again only the groups whose
//! [`SeamKey`] changed (their analysis, panels, feather or merged state)
//! and keeps the others' last seams ([`SeamCache`]): a draft feather pause
//! measures the groups that follow the draft, a merge its group.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mmm_core::checks::{
    Check, Severity, analysis_checks, file_checks, input_checks, is_accepted, is_aligned,
    layout_checks, panel_facts, seam_checks, stat_checks,
};
use mmm_core::health::{SeamLine, health_with_lines};
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{Mosaic, MosaicConfig, PanelEntry, analysis_stamp};
use mmm_core::overlap::OverlapGraph;
use mmm_core::photometry::Photometry;
use mmm_core::session::Session;
use mmm_core::surfaces::Surfaces;

use crate::blend_settings::draft_for;
use crate::jobs::{ANALYZE_LABEL, JobEvent, JobRunner, JobTarget, PREVIEWS_LABEL};
use crate::session::OpenSession;
use crate::util::{Poll, Task};

/// How long after the last draft feather change the checks run again.
pub const CHECKS_DEBOUNCE: Duration = Duration::from_millis(750);

/// One group's feather for the seam checks, and whether the group has been
/// merged (the feather is then its last whole merge's, else the draft's).
#[derive(Clone, Copy, Debug, PartialEq)]
struct GroupFeather {
    feather_px: f32,
    merged: bool,
}

/// The feather and merged state of each of `cfg`'s analysed groups
/// (the ones with a session directory).
fn group_feathers(cfg: &MosaicConfig) -> BTreeMap<String, GroupFeather> {
    cfg.group_dirs
        .keys()
        .map(|g| {
            let f = match mmm_core::mosaic::last_merge_settings(cfg, g) {
                Some(b) => GroupFeather {
                    feather_px: b.feather_px,
                    merged: true,
                },
                None => GroupFeather {
                    feather_px: draft_for(cfg, g).feather_px,
                    merged: false,
                },
            };
            (g.clone(), f)
        })
        .collect()
}

/// The groups' draft feathers, for the groups not merged yet (a merged
/// group's seams do not follow the draft).
fn draft_feathers(cfg: &MosaicConfig) -> BTreeMap<String, f32> {
    group_feathers(cfg)
        .into_iter()
        .filter(|(_, f)| !f.merged)
        .map(|(g, f)| (g, f.feather_px))
        .collect()
}

/// What one group's seam checks and lines depend on: when it changes, its
/// seams are measured again (a health pass over its L8 owner map, ~0.9 GB
/// transient for 12 full-size panels); otherwise the last measure is kept.
#[derive(Clone, Debug, PartialEq)]
struct SeamKey {
    /// The group's analysis: its newest `"analyze"` provenance entry
    /// ([`analysis_stamp`]), and its session directory.
    stamp: Option<usize>,
    dir: Option<String>,
    /// Its member panels.
    panels: Vec<PathBuf>,
    /// The feather they are measured with, and whether it is a merge's.
    feather: GroupFeather,
}

/// One group's measured seams: its seam checks and seam lines.
#[derive(Clone, Debug)]
struct MeasuredSeams {
    checks: Vec<Check>,
    lines: Vec<SeamLine>,
}

/// The last measured seams of each group, with what they were measured
/// from. The engine keeps it between runs and hands it to each run's
/// worker (moved, never copied or locked on the UI thread), which hands
/// back the entries of the groups it saw.
type SeamCache = BTreeMap<String, (SeamKey, MeasuredSeams)>;

/// One run's results.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChecksReport {
    /// Every check result, in the core's order.
    pub checks: Vec<Check>,
    /// What could not be checked (an unreadable panel header or analysis),
    /// for the user; each counts as a problem.
    pub errors: Vec<String>,
    /// Each analysed group's seam lines (canvas px), for the canvas; a
    /// group whose seams could not be measured has none.
    pub seams: BTreeMap<String, Vec<SeamLine>>,
}

/// Check results by severity; accepted problems and notes are not counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckCounts {
    /// Unaccepted problems, plus what could not be checked.
    pub problems: usize,
    /// Unaccepted notes.
    pub notes: usize,
    /// Passed checks.
    pub passed: usize,
}

/// A finished run: its report, the seam cache to keep, and the groups
/// whose seams it measured.
struct ChecksRun {
    report: ChecksReport,
    cache: SeamCache,
    /// The groups whose seams were measured, not taken from the cache
    /// (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    measured: Vec<String>,
}

/// Run every check over `mosaic` (worker thread: reads panel headers and
/// the groups' analyses). `groups`: the resolved groups, if known;
/// otherwise they are resolved here. `aligned`: whether the input is
/// pre-aligned (`None`: not known yet). `feathers`: the feather each group's
/// seams are measured with. `cache`: the seams measured before; a group
/// whose [`SeamKey`] is unchanged keeps its entry, any other is measured
/// again. The cache handed back holds the groups seen this run.
fn run_checks(
    mosaic: &Mosaic,
    groups: Option<Groups>,
    aligned: Option<bool>,
    feathers: &BTreeMap<String, GroupFeather>,
    mut cache: SeamCache,
) -> ChecksRun {
    let cfg = &mosaic.config;
    let mut errors = Vec::new();
    let mut seams = BTreeMap::new();
    let mut kept = SeamCache::new();
    let mut measured = Vec::new();
    let mut facts = Vec::new();
    for p in &cfg.panels {
        match panel_facts(&p.path) {
            Ok(f) => facts.push(f),
            // A missing file is reported by the file checks.
            Err(e) if p.path.exists() => {
                errors.push(format!("Cannot read {}: {e}", p.path.display()))
            }
            Err(_) => {}
        }
    }
    let groups = match groups.map(Ok).unwrap_or_else(|| mosaic.groups()) {
        Ok(g) => Some(g),
        Err(e) => {
            errors.push(format!("Cannot resolve the groups: {e}"));
            None
        }
    };
    let mut checks = match &groups {
        Some(g) => input_checks(cfg, &facts, g, aligned),
        None => Vec::new(),
    };
    checks.extend(file_checks(cfg));
    let names = groups.map(|g| g.processing_groups()).unwrap_or_default();
    for (group, panels) in names {
        // Never analysed, out of date, or being analysed right now.
        if !cfg.group_dirs.contains_key(&group) || cfg.needs_analyze.contains(&group) {
            continue;
        }
        let dir = mosaic.group_session_dir(&group);
        if !dir.join("session.json").is_file() {
            continue;
        }
        let loaded = Session::open(&dir).and_then(|session| {
            OverlapGraph::load(&session.overlap_graph_path()).map(|graph| (session, graph))
        });
        match loaded {
            Ok((session, graph)) => {
                checks.extend(analysis_checks(&group, &session, &graph));
                checks.extend(layout_checks(&group, &session, &graph));
                checks.extend(stat_checks(&group, &session));
                let feather = feathers.get(&group).copied().unwrap_or(GroupFeather {
                    feather_px: draft_for(cfg, &group).feather_px,
                    merged: false,
                });
                let key = SeamKey {
                    stamp: analysis_stamp(cfg, &group),
                    dir: cfg.group_dirs.get(&group).cloned(),
                    panels,
                    feather,
                };
                let found = match cache.remove(&group).filter(|(k, _)| *k == key) {
                    Some((_, found)) => Ok(Some(found)),
                    None => {
                        measured.push(group.clone());
                        measure_seams(&session, &graph, feather.feather_px).map(|m| {
                            m.map(|(health, lines)| MeasuredSeams {
                                checks: seam_checks(&group, &session, &health, feather.merged),
                                lines,
                            })
                        })
                    }
                };
                match found {
                    Ok(Some(found)) => {
                        checks.extend(found.checks.iter().cloned());
                        seams.insert(group.clone(), found.lines.clone());
                        kept.insert(group.clone(), (key, found));
                    }
                    // No photometry yet: nothing to measure.
                    Ok(None) => {}
                    Err(e) => errors.push(format!(
                        "Cannot measure the seams of group {group}: {e}. Analyse again."
                    )),
                }
            }
            Err(e) => errors.push(format!(
                "Cannot read the analysis of group {group}: {e}. Analyse again."
            )),
        }
    }
    ChecksRun {
        report: ChecksReport {
            checks,
            errors,
            seams,
        },
        cache: kept,
        measured,
    }
}

/// `session`'s seam health and lines at `feather_px`; `None` without
/// photometry (not an error: the analysis has not got that far).
fn measure_seams(
    session: &Session,
    graph: &OverlapGraph,
    feather_px: f32,
) -> mmm_core::Result<Option<(mmm_core::health::Health, Vec<SeamLine>)>> {
    if !session.photometry_path().is_file() {
        return Ok(None);
    }
    let phot = Photometry::load(&session.photometry_path())?;
    let surfaces = if session.surfaces_path().is_file() {
        Some(Surfaces::load(&session.surfaces_path())?)
    } else {
        None
    };
    health_with_lines(session, graph, &phot, surfaces.as_ref(), feather_px).map(Some)
}

/// Runs the checks off the UI thread and keeps the last report.
pub struct ChecksEngine {
    task: Option<Task<ChecksRun>>,
    /// A run is wanted (once the running one, if any, ends).
    dirty: bool,
    report: Option<ChecksReport>,
    /// The groups and panels the last request was for.
    groups_seen: Option<Groups>,
    panels_seen: Vec<PanelEntry>,
    /// The draft's feathers (unmerged groups) when last looked at.
    drafts_seen: Option<BTreeMap<String, f32>>,
    /// When the draft feather last changed, while a run waits for typing
    /// to pause.
    pending: Option<Instant>,
    /// A whole-canvas merge has finished while others were still queued
    /// or running: run once the last has ended, however it ended.
    merge_waiting: bool,
    /// The groups' last measured seams, handed to each run's worker and
    /// taken back with its report ([`SeamCache`]).
    seam_cache: SeamCache,
    /// How long typing must pause before the checks run again.
    debounce: Duration,
    /// The groups whose seams the last run measured (tests).
    #[cfg(test)]
    measured: Vec<String>,
    /// Runs started (tests).
    #[cfg(test)]
    runs: usize,
}

impl Default for ChecksEngine {
    fn default() -> Self {
        ChecksEngine::new()
    }
}

impl ChecksEngine {
    /// An engine for a session just opened (it runs on the first poll).
    pub fn new() -> Self {
        ChecksEngine {
            task: None,
            dirty: true,
            report: None,
            groups_seen: None,
            panels_seen: Vec::new(),
            drafts_seen: None,
            pending: None,
            merge_waiting: false,
            seam_cache: SeamCache::new(),
            debounce: CHECKS_DEBOUNCE,
            #[cfg(test)]
            runs: 0,
            #[cfg(test)]
            measured: Vec::new(),
        }
    }

    /// Ask for a run.
    #[allow(dead_code)] // used by the tests; no UI asks for a re-run yet
    pub fn request(&mut self) {
        self.dirty = true;
    }

    /// Take a finished run, notice what calls for a new one (resolved
    /// groups or inputs changed, a previews or analysis job ended in
    /// `events`, a whole-canvas merge finished and no other is queued or
    /// running any more, a draft feather changed and typing paused), and
    /// start it. Call every frame after the runner's poll.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        jobs: &JobRunner,
        events: &[JobEvent],
    ) {
        self.poll_at(ctx, session, jobs, events, Instant::now());
    }

    /// [`ChecksEngine::poll`] at time `now` (the debounce's clock).
    fn poll_at(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        jobs: &JobRunner,
        events: &[JobEvent],
        now: Instant,
    ) {
        if let Some(task) = &self.task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(run) => {
                    self.task = None;
                    self.report = Some(run.report);
                    self.seam_cache = run.cache;
                    #[cfg(test)]
                    {
                        self.measured = run.measured;
                    }
                }
                Poll::Lost => {
                    self.task = None;
                    tracing::warn!("the checks stopped unexpectedly (see the log)");
                }
            }
        }
        if session.groups != self.groups_seen {
            self.groups_seen = session.groups.clone();
            self.dirty = true;
        }
        if session.mosaic.config.panels != self.panels_seen {
            self.panels_seen = session.mosaic.config.panels.clone();
            self.dirty = true;
        }
        let pipeline_job =
            |label: Option<&str>| matches!(label, Some(ANALYZE_LABEL | PREVIEWS_LABEL));
        for event in events {
            self.dirty |= match event {
                JobEvent::Finished { id, label } => {
                    let merge = jobs.whole_canvas(*id)
                        && matches!(jobs.target(*id), Some((JobTarget::Blend, _)));
                    self.merge_waiting |= merge;
                    pipeline_job(Some(label))
                }
                JobEvent::Cancelled { id } | JobEvent::Failed { id, .. } => {
                    pipeline_job(jobs.label(*id))
                }
                JobEvent::Preview(_) => false,
            };
        }
        // Merge all: one run when the last whole-canvas merge has ended
        // (finished, cancelled or failed), not one per merge alongside the
        // next.
        if self.merge_waiting && jobs.run_of(JobTarget::Blend).is_none() {
            self.merge_waiting = false;
            self.dirty = true;
        }
        self.watch_draft(ctx, &session.mosaic.config, now);
        if self.dirty && self.task.is_none() {
            self.dirty = false;
            #[cfg(test)]
            {
                self.runs += 1;
            }
            let mosaic = session.mosaic.clone();
            let feathers = group_feathers(&session.mosaic.config);
            let groups = session.groups.clone();
            // No analysed frame yet: the input mode is unknown, so no distortion verdict.
            let aligned = session.frame.as_ref().map(|f| is_aligned(Some(f)));
            let cache = std::mem::take(&mut self.seam_cache);
            self.task = Task::spawn("mmm-checks", ctx, move || {
                run_checks(&mosaic, groups, aligned, &feathers, cache)
            });
        }
    }

    /// Notice a changed draft feather and run again once it has stayed
    /// the same for the debounce.
    fn watch_draft(&mut self, ctx: &egui::Context, cfg: &MosaicConfig, now: Instant) {
        let feathers = draft_feathers(cfg);
        // A group that is new or has been merged since is not a change (its
        // run was asked for by the analysis or the merge); the first look is
        // the session's own state.
        let moved = self.drafts_seen.as_ref().is_some_and(|seen| {
            feathers
                .iter()
                .any(|(g, f)| seen.get(g).is_some_and(|old| old != f))
        });
        self.drafts_seen = Some(feathers);
        if moved {
            self.pending = Some(now);
        }
        if let Some(since) = self.pending {
            let waited = now.saturating_duration_since(since);
            if waited >= self.debounce {
                self.pending = None;
                self.dirty = true;
            } else {
                ctx.request_repaint_after(self.debounce - waited);
            }
        }
    }

    /// The last report, once a run has finished.
    pub fn report(&self) -> Option<&ChecksReport> {
        self.report.as_ref()
    }

    /// Show `report` as the last run's (tests: a run is never started
    /// unless the engine is polled).
    #[cfg(test)]
    pub(crate) fn set_report(&mut self, report: ChecksReport) {
        self.report = Some(report);
    }

    /// A run is in progress.
    pub fn running(&self) -> bool {
        self.task.is_some()
    }

    /// The last report's counts under `cfg`'s accepted checks.
    pub fn counts(&self, cfg: &MosaicConfig) -> Option<CheckCounts> {
        let report = self.report.as_ref()?;
        let mut counts = CheckCounts {
            problems: report.errors.len(),
            ..CheckCounts::default()
        };
        for c in &report.checks {
            match c.severity {
                Severity::Passed => counts.passed += 1,
                _ if is_accepted(cfg, c) => {}
                Severity::Problem => counts.problems += 1,
                Severity::Note => counts.notes += 1,
            }
        }
        Some(counts)
    }

    /// Accept `check`: add its keys to the accepted checks and save
    /// `mosaic.json` (UI thread; module docs). The error is a message for
    /// the user; nothing changes then.
    pub fn accept(&self, session: &mut OpenSession, check: &Check) -> Result<(), String> {
        let mut next = session.mosaic.config.clone();
        let before = next.accepted_checks.len();
        for key in check.keys() {
            if !next.accepted_checks.contains(&key) {
                next.accepted_checks.push(key);
            }
        }
        if next.accepted_checks.len() == before {
            return Ok(());
        }
        next.save(&session.mosaic.dir)
            .map_err(|e| format!("Could not save the accepted check: {e}"))?;
        session.mosaic.config = next;
        Ok(())
    }

    /// Take back accepting `check`: remove its keys from the accepted
    /// checks and save `mosaic.json` (UI thread; module docs). The error is
    /// a message for the user; nothing changes then.
    pub fn unaccept(&self, session: &mut OpenSession, check: &Check) -> Result<(), String> {
        let keys = check.keys();
        let mut next = session.mosaic.config.clone();
        next.accepted_checks.retain(|k| !keys.contains(k));
        if next.accepted_checks == session.mosaic.config.accepted_checks {
            return Ok(());
        }
        next.save(&session.mosaic.dir)
            .map_err(|e| format!("Could not save the change to the accepted checks: {e}"))?;
        session.mosaic.config = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use mmm_core::checks::{CheckCode, is_accepted};
    use mmm_core::mosaic::MosaicConfig;

    use super::*;
    use crate::jobs::tests::analyzed;

    /// Poll until a report for the current state is in (bounded).
    fn settle(e: &mut ChecksEngine, s: &OpenSession, r: &JobRunner) {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        loop {
            e.poll(&ctx, s, r, &[]);
            if e.report().is_some() && !e.running() && !e.dirty && e.pending.is_none() {
                return;
            }
            assert!(Instant::now() < deadline, "checks never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn checks_engine_counts_and_accept_persists() {
        let (root, mut s, r) = analyzed("checks-engine");
        let mut e = ChecksEngine::new();
        assert_eq!(e.counts(&s.mosaic.config), None, "no report yet");
        settle(&mut e, &s, &r);
        // One note each for the unsolved panels; the synthetic three-channel
        // groups each carry one genuine gradient note.
        let solution = |c: &&Check| c.code == CheckCode::SolutionPresent;
        let report = e.report().unwrap().clone();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.checks.iter().filter(solution).count(), 8);
        let gradients = report
            .checks
            .iter()
            .filter(|c| c.code == CheckCode::PanelGradient)
            .count();
        assert_eq!(gradients, 2, "{:?}", report.checks);
        let counts = e.counts(&s.mosaic.config).unwrap();
        assert_eq!(counts.problems, 0, "{:?}", report.checks);
        assert_eq!(counts.notes, 8 + 2, "{:?}", report.checks);

        // The analyses are read: an unreadable one is a problem.
        let ha = Session::open(&s.mosaic.group_session_dir("Ha")).unwrap();
        std::fs::write(ha.overlap_graph_path(), "not a graph").unwrap();
        e.request();
        settle(&mut e, &s, &r);
        let errors = &e.report().unwrap().errors;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("group Ha"), "{errors:?}");
        assert!(errors[0].ends_with("Analyse again."), "{errors:?}");
        assert_eq!(e.counts(&s.mosaic.config).unwrap().problems, 1);

        // Accepting a note saves its keys; it is no longer counted.
        let pick = report.checks.iter().find(solution).unwrap().clone();
        e.accept(&mut s, &pick).unwrap();
        assert!(is_accepted(&s.mosaic.config, &pick));
        let saved = MosaicConfig::load(&s.mosaic.dir).unwrap();
        assert_eq!(saved, s.mosaic.config);
        assert_eq!(saved.accepted_checks, pick.keys());
        // Ha's unreadable analysis lost its gradient note too: 10 - 1 - 1.
        assert_eq!(e.counts(&s.mosaic.config).unwrap().notes, 8);
        // Accepting again changes nothing.
        let before = std::fs::read(s.mosaic.dir.join("mosaic.json")).unwrap();
        e.accept(&mut s, &pick).unwrap();
        assert_eq!(
            std::fs::read(s.mosaic.dir.join("mosaic.json")).unwrap(),
            before
        );

        // Passed checks count as passed, accepted or not; accepted problems
        // are not counted.
        let mut problem = pick.clone();
        problem.severity = Severity::Problem;
        let mut passed = report.checks[1].clone();
        passed.severity = Severity::Passed;
        e.report = Some(ChecksReport {
            checks: vec![problem, passed],
            errors: Vec::new(),
            seams: Default::default(),
        });
        assert_eq!(
            e.counts(&s.mosaic.config).unwrap(),
            CheckCounts {
                problems: 0,
                notes: 0,
                passed: 1
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A problem check for the un-accept test.
    fn problem_check() -> Check {
        Check {
            target: Default::default(),
            rect: None,
            code: CheckCode::NoOverlap,
            severity: Severity::Problem,
            panels: vec![std::path::PathBuf::from("a.xisf")],
            groups: vec!["Ha".into()],
            message: "a.xisf overlaps no other panel.".into(),
            remedy: None,
        }
    }

    #[test]
    fn checks_cover_every_group_with_seams() {
        let (root, s, r) = analyzed("checks-seams");
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        let report = e.report().unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        for group in ["Ha", "OIII"] {
            let lines = report.seams.get(group);
            assert!(
                lines.is_some_and(|l| !l.is_empty()),
                "{group} has seam lines: {:?}",
                report.seams
            );
            let codes: Vec<CheckCode> = report
                .checks
                .iter()
                .filter(|c| c.groups.iter().any(|g| g == group))
                .map(|c| c.code)
                .collect();
            assert!(
                codes.contains(&CheckCode::PanelGradient),
                "{group}: {codes:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_merge_reruns_checks() {
        let (root, mut s, mut r) = analyzed("checks-merge");
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        let before = e.runs;
        let ctx = egui::Context::default();

        // A merge of a region leaves the checks alone.
        r.start_blend(&mut s, "Ha", Some([0, 0, 64, 64]));
        let events = crate::jobs::tests::drain(&mut r, &mut s);
        e.poll(&ctx, &s, &r, &events);
        settle(&mut e, &s, &r);
        assert_eq!(e.runs, before, "a region merge");

        // A whole-canvas merge runs them again.
        r.start_blend(&mut s, "Ha", None);
        let events = crate::jobs::tests::drain(&mut r, &mut s);
        e.poll(&ctx, &s, &r, &events);
        settle(&mut e, &s, &r);
        assert_eq!(e.runs, before + 1, "a whole merge");
        assert!(mmm_core::mosaic::last_merge_settings(&s.mosaic.config, "Ha").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Poll the runner, then the engine with its events, as the shell does
    /// each frame, until no job is left (bounded); or, with `until`, until
    /// job `until` has finished. Whether a checks run started meanwhile
    /// while a whole-canvas merge was still running or queued.
    fn follow_jobs(
        e: &mut ChecksEngine,
        s: &mut OpenSession,
        r: &mut JobRunner,
        until: Option<u64>,
    ) -> bool {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        let mut early = false;
        loop {
            let events = r.poll(s);
            let before = e.runs;
            e.poll(&ctx, s, r, &events);
            early |= e.runs > before && r.run_of(JobTarget::Blend).is_some();
            let done = match until {
                Some(id) => events
                    .iter()
                    .any(|ev| matches!(ev, JobEvent::Finished { id: f, .. } if *f == id)),
                None => !r.busy(),
            };
            if done {
                return early;
            }
            assert!(Instant::now() < deadline, "the jobs never finished");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn merge_all_runs_the_checks_once_after_the_last_merge() {
        let (root, mut s, mut r) = analyzed("checks-merge-all");
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        let before = e.runs;
        // Merge all: each group's whole-canvas merge, queued together.
        r.start_blend(&mut s, "Ha", None);
        r.start_blend(&mut s, "OIII", None);
        let early = follow_jobs(&mut e, &mut s, &mut r, None);
        settle(&mut e, &s, &r);
        assert!(!early, "no run while a merge is still to come");
        assert_eq!(e.runs, before + 1, "one run, after the last merge");
        // A queue that ends cancelled still runs them, once.
        let before = e.runs;
        let ha = r.start_blend(&mut s, "Ha", None);
        let oiii = r.start_blend(&mut s, "OIII", None);
        let early = follow_jobs(&mut e, &mut s, &mut r, Some(ha));
        assert!(!early);
        assert_eq!(e.runs, before, "waiting for OIII's merge");
        r.cancel(oiii);
        follow_jobs(&mut e, &mut s, &mut r, None);
        settle(&mut e, &s, &r);
        assert_eq!(e.runs, before + 1, "one run once the queue has ended");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seams_are_measured_again_only_where_their_inputs_changed() {
        use crate::blend_settings::{load_draft, save_draft};
        use mmm_core::settings::BlendSettings;
        let (root, mut s, mut r) = analyzed("checks-seam-cache");
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        assert_eq!(e.measured, ["Ha", "OIII"], "the first run measures all");
        let first = e.report().unwrap().clone();
        // Nothing changed: nothing measured again, the same report.
        e.request();
        settle(&mut e, &s, &r);
        assert!(e.measured.is_empty(), "{:?}", e.measured);
        assert_eq!(e.report(), Some(&first));
        // Ha merged: its seams describe its merge now; OIII's are kept.
        let ctx = egui::Context::default();
        r.start_blend(&mut s, "Ha", None);
        let events = crate::jobs::tests::drain(&mut r, &mut s);
        e.poll(&ctx, &s, &r, &events);
        settle(&mut e, &s, &r);
        assert_eq!(e.measured, ["Ha"]);
        // A draft feather change, once typing pauses: only the group that
        // follows the draft (OIII; Ha has its merge's).
        let t0 = Instant::now();
        e.poll_at(&ctx, &s, &r, &[], t0);
        let draft = load_draft(&s.mosaic.config);
        let typed = BlendSettings {
            feather_px: draft.feather_px + 8.0,
            ..draft
        };
        save_draft(&mut s.mosaic.config.ui, &typed);
        e.poll_at(&ctx, &s, &r, &[], t0 + Duration::from_millis(10));
        e.poll_at(&ctx, &s, &r, &[], t0 + Duration::from_secs(5));
        settle(&mut e, &s, &r);
        assert_eq!(e.measured, ["OIII"]);
        // Ha analysed again: Ha's.
        let analysed = s.mosaic.config.settings_for("Ha").0;
        s.mosaic.config.push_provenance(
            "analyze",
            None,
            serde_json::json!({ "groups": { "Ha": analysed } }),
        );
        e.request();
        settle(&mut e, &s, &r);
        assert_eq!(e.measured, ["Ha"]);
        // What is kept is what was measured: a fresh engine agrees.
        let mut fresh = ChecksEngine::new();
        settle(&mut fresh, &s, &r);
        assert_eq!(fresh.report(), e.report());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn feather_typing_runs_checks_once() {
        use mmm_core::settings::BlendSettings;
        let (root, mut s, r) = analyzed("checks-feather");
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        let before = e.runs;
        let ctx = egui::Context::default();
        // A clock the test drives: typing, each value within the debounce.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        e.poll_at(&ctx, &s, &r, &[], t0);
        for (i, feather) in [31.0, 32.0, 33.0, 34.0].into_iter().enumerate() {
            let draft = BlendSettings {
                feather_px: feather,
                ..crate::blend_settings::load_draft(&s.mosaic.config)
            };
            crate::blend_settings::save_draft(&mut s.mosaic.config.ui, &draft);
            e.poll_at(&ctx, &s, &r, &[], at(100 * (i as u64 + 1)));
        }
        // 600 ms after the last change: still waiting.
        e.poll_at(&ctx, &s, &r, &[], at(400 + 600));
        assert_eq!(e.runs, before, "nothing runs while typing");
        assert!(e.pending.is_some());
        // 750 ms after the last change: one run.
        e.poll_at(&ctx, &s, &r, &[], at(400 + 750));
        settle(&mut e, &s, &r);
        assert_eq!(e.runs, before + 1, "one run after typing stops");
        e.poll_at(&ctx, &s, &r, &[], at(5000));
        settle(&mut e, &s, &r);
        assert_eq!(e.runs, before + 1, "an unchanged feather asks for nothing");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_non_feather_edit_after_a_merge_runs_nothing() {
        use mmm_core::settings::BlendSettings;
        let (root, mut s, mut r) = analyzed("checks-nonfeather");
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        let ctx = egui::Context::default();
        // Merge Ha: it leaves the draft feathers' map.
        r.start_blend(&mut s, "Ha", None);
        let events = crate::jobs::tests::drain(&mut r, &mut s);
        e.poll(&ctx, &s, &r, &events);
        settle(&mut e, &s, &r);
        let before = e.runs;
        let t0 = Instant::now();
        let draft = crate::blend_settings::load_draft(&s.mosaic.config);
        let edited = BlendSettings {
            defect_veto: !draft.defect_veto,
            ..draft
        };
        crate::blend_settings::save_draft(&mut s.mosaic.config.ui, &edited);
        e.poll_at(&ctx, &s, &r, &[], t0);
        e.poll_at(&ctx, &s, &r, &[], t0 + Duration::from_secs(10));
        assert!(e.pending.is_none() && !e.dirty);
        settle(&mut e, &s, &r);
        assert_eq!(e.runs, before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unaccept_restores_a_problem() {
        let (root, mut s, _r) = analyzed("checks-unaccept");
        let mut e = ChecksEngine::new();
        let pick = problem_check();
        e.set_report(ChecksReport {
            checks: vec![pick.clone()],
            ..ChecksReport::default()
        });
        assert_eq!(e.counts(&s.mosaic.config).unwrap().problems, 1);
        e.accept(&mut s, &pick).unwrap();
        assert_eq!(e.counts(&s.mosaic.config).unwrap().problems, 0);
        e.unaccept(&mut s, &pick).unwrap();
        assert!(!is_accepted(&s.mosaic.config, &pick));
        assert_eq!(e.counts(&s.mosaic.config).unwrap().problems, 1);
        let saved = MosaicConfig::load(&s.mosaic.dir).unwrap();
        assert!(saved.accepted_checks.is_empty());
        assert_eq!(saved, s.mosaic.config);
        // Un-accepting what is not accepted changes nothing.
        let before = std::fs::read(s.mosaic.dir.join("mosaic.json")).unwrap();
        e.unaccept(&mut s, &pick).unwrap();
        assert_eq!(
            std::fs::read(s.mosaic.dir.join("mosaic.json")).unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// End to end, from a run over a synthetic two-group session through
    /// the canvas's marks: a seam line's panel ids (the analysis's
    /// `PanelMeta.id`s, its owner indices) are the ids the plan's layout
    /// reads for its panels, so each line lies in its own pair's overlap,
    /// and a seam problem is framed at its lines, badged at its pair's
    /// overlap and drawn in the warning colour there.
    #[test]
    fn seam_lines_and_badges_land_on_their_pairs_overlap() {
        use mega_ui::theme::Tone;
        use mmm_core::checks::CheckTarget;

        use crate::marks::{covering_px, problem_marks, seam_lines, target_rect};
        use crate::plan_view::load_layout;

        let (root, s, _r) = analyzed("checks-seam-places");
        let cfg = &s.mosaic.config;
        let run = run_checks(
            &s.mosaic,
            s.groups.clone(),
            None,
            &group_feathers(cfg),
            SeamCache::new(),
        );
        let report = run.report;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let groups = ["Ha".to_string(), "OIII".to_string()];
        let dirs: Vec<(String, PathBuf)> = groups
            .iter()
            .map(|g| (g.clone(), s.mosaic.group_session_dir(g)))
            .collect();
        let no_frame = root.join("no-frame.json");
        let layout = load_layout((&no_frame, &no_frame), &dirs);
        let meet = |a: [f64; 4], b: [f64; 4]| {
            [
                a[0].max(b[0]),
                a[1].max(b[1]),
                a[2].min(b[2]),
                a[3].min(b[3]),
            ]
        };
        let bbox = |points: &mut dyn Iterator<Item = &[f64; 2]>| {
            points.fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |r, p| {
                [
                    r[0].min(p[0]),
                    r[1].min(p[1]),
                    r[2].max(p[0]),
                    r[3].max(p[1]),
                ]
            })
        };
        // One L8 cell of slack: the lines run along cell edges.
        let cell = 8.0;
        for g in &groups {
            let lines = &report.seams[g];
            assert!(!lines.is_empty(), "{g} has seams");
            let file = |id: u16| {
                layout.ids[g]
                    .iter()
                    .find(|(_, i)| **i == usize::from(id))
                    .map(|(p, _)| p.clone())
                    .unwrap_or_else(|| panic!("{g} has a panel {id}"))
            };
            let rect = |p: &PathBuf| layout.rects[p];
            for l in lines {
                let o = meet(rect(&file(l.a)), rect(&file(l.b)));
                assert!(
                    l.points.iter().all(|p| {
                        p[0] >= o[0] - cell
                            && p[0] <= o[2] + cell
                            && p[1] >= o[1] - cell
                            && p[1] <= o[3] + cell
                    }),
                    "{g}: the line of {} and {} is off their overlap {o:?}: {:?}",
                    l.a,
                    l.b,
                    l.points
                );
            }
            // A problem at the first line's seam.
            let first = &lines[0];
            let (a, b) = (file(first.a), file(first.b));
            let pair = |l: &SeamLine| (l.a.min(l.b), l.a.max(l.b));
            let its: Vec<&SeamLine> = lines.iter().filter(|l| pair(l) == pair(first)).collect();
            let shows = Check {
                code: CheckCode::ResidualSeam,
                severity: Severity::Problem,
                panels: vec![a.clone(), b.clone()],
                groups: vec![g.clone()],
                message: "A seam shows".into(),
                remedy: None,
                target: CheckTarget::Seam {
                    group: g.clone(),
                    a: a.clone(),
                    b: b.clone(),
                },
                rect: None,
            };
            // Framed at its lines.
            let lines_box = bbox(&mut its.iter().flat_map(|l| &l.points));
            assert_eq!(
                target_rect(&s, &layout, &report.seams, &shows),
                Some(covering_px(lines_box))
            );
            // Badged at its pair's overlap, its lines warned.
            let with = ChecksReport {
                checks: vec![shows],
                ..report.clone()
            };
            let shown = [g.clone()];
            let marks = problem_marks(&s, &layout, Some(&with), &shown);
            assert_eq!(marks.len(), 1);
            let o = meet(rect(&a), rect(&b));
            assert_eq!(
                marks[0].placed_badge().map(|b| b.at),
                Some([(o[0] + o[2]) / 2.0, (o[1] + o[3]) / 2.0])
            );
            let warned: Vec<Vec<[f64; 2]>> = seam_lines(&layout, Some(&with), &shown, &marks)
                .into_iter()
                .filter(|l| l.color == Tone::Warning.color())
                .map(|l| l.points)
                .collect();
            let want: Vec<Vec<[f64; 2]>> = its.iter().map(|l| l.points.clone()).collect();
            assert_eq!(warned, want);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn aligned_sessions_pass_the_distortion_check() {
        use mmm_core::reference::ReferenceFrame;
        let root = crate::start::tests::temp_dir("checks-aligned");
        let (mut s, _extra) = crate::testutil::solved_two_group_session(&root);
        let r = JobRunner::new(egui::Context::default());
        // The solved panels carry linear solutions only.
        s.frame = Some(ReferenceFrame::Aligned {
            width: 10,
            height: 10,
            wcs: None,
        });
        let mut e = ChecksEngine::new();
        settle(&mut e, &s, &r);
        let found: Vec<&Check> = e
            .report()
            .unwrap()
            .checks
            .iter()
            .filter(|c| c.code == CheckCode::DistortionModel)
            .collect();
        assert!(!found.is_empty());
        assert!(
            found.iter().all(|c| c.severity == Severity::Passed
                && c.message == "No distortion model: accepted for pre-aligned panels"),
            "{found:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
