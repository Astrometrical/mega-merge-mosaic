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
//! exception to "no file I/O on the UI thread".

use mmm_core::checks::{
    Check, Severity, analysis_checks, file_checks, input_checks, is_accepted, panel_facts,
};
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{Mosaic, MosaicConfig, PanelEntry};
use mmm_core::overlap::OverlapGraph;
use mmm_core::session::Session;

use crate::blend_tab::{Poll, Task};
use crate::jobs::{ANALYZE_LABEL, JobEvent, JobRunner, PREVIEWS_LABEL};
use crate::session::OpenSession;

/// One run's results.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChecksReport {
    /// Every check result, in the core's order.
    pub checks: Vec<Check>,
    /// What could not be checked (an unreadable panel header or analysis),
    /// for the user; each counts as a problem.
    pub errors: Vec<String>,
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

/// Run every check over `mosaic` (worker thread: reads panel headers and
/// the groups' analyses). `groups`: the resolved groups, if known;
/// otherwise they are resolved here.
pub fn run_checks(mosaic: &Mosaic, groups: Option<Groups>) -> ChecksReport {
    let cfg = &mosaic.config;
    let mut errors = Vec::new();
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
        Some(g) => input_checks(cfg, &facts, g),
        None => Vec::new(),
    };
    checks.extend(file_checks(cfg));
    let names = groups.map(|g| g.processing_groups()).unwrap_or_default();
    for (group, _) in names {
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
            Ok((session, graph)) => checks.extend(analysis_checks(&group, &session, &graph)),
            Err(e) => errors.push(format!(
                "Cannot read the analysis of group {group}: {e}. Analyze again."
            )),
        }
    }
    ChecksReport { checks, errors }
}

/// Runs the checks off the UI thread and keeps the last report.
pub struct ChecksEngine {
    task: Option<Task<ChecksReport>>,
    /// A run is wanted (once the running one, if any, ends).
    dirty: bool,
    report: Option<ChecksReport>,
    /// The groups and panels the last request was for.
    groups_seen: Option<Groups>,
    panels_seen: Vec<PanelEntry>,
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
        }
    }

    /// Ask for a run.
    #[allow(dead_code)] // used by the tests; no UI asks for a re-run yet
    pub fn request(&mut self) {
        self.dirty = true;
    }

    /// Take a finished run, notice what calls for a new one (resolved
    /// groups or inputs changed, a previews or analysis job ended in
    /// `events`), and start it. Call every frame after the runner's poll.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        jobs: &JobRunner,
        events: &[JobEvent],
    ) {
        if let Some(task) = &self.task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(report) => {
                    self.task = None;
                    self.report = Some(report);
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
                JobEvent::Finished { label, .. } => pipeline_job(Some(label)),
                JobEvent::Cancelled { id } | JobEvent::Failed { id, .. } => {
                    pipeline_job(jobs.label(*id))
                }
                JobEvent::Preview(_) => false,
            };
        }
        if self.dirty && self.task.is_none() {
            self.dirty = false;
            let mosaic = session.mosaic.clone();
            let groups = session.groups.clone();
            self.task = Task::spawn("mmm-checks", ctx, move || run_checks(&mosaic, groups));
        }
    }

    /// The last report, once a run has finished.
    pub fn report(&self) -> Option<&ChecksReport> {
        self.report.as_ref()
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
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            e.poll(&ctx, s, r, &[]);
            if e.report().is_some() && !e.running() && !e.dirty {
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
        // The synthetic panels are registered, unsolved: one note each.
        let report = e.report().unwrap().clone();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.checks.len(), 8);
        assert!(
            report
                .checks
                .iter()
                .all(|c| c.code == CheckCode::SolutionPresent)
        );
        let counts = e.counts(&s.mosaic.config).unwrap();
        assert_eq!(
            counts,
            CheckCounts {
                problems: 0,
                notes: 8,
                passed: 0
            }
        );

        // The analyses are read: an unreadable one is a problem.
        let ha = Session::open(&s.mosaic.group_session_dir("Ha")).unwrap();
        std::fs::write(ha.overlap_graph_path(), "not a graph").unwrap();
        e.request();
        settle(&mut e, &s, &r);
        let errors = &e.report().unwrap().errors;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("group Ha"), "{errors:?}");
        assert_eq!(e.counts(&s.mosaic.config).unwrap().problems, 1);

        // Accepting a note saves its keys; it is no longer counted.
        let pick = report.checks[0].clone();
        e.accept(&mut s, &pick).unwrap();
        assert!(is_accepted(&s.mosaic.config, &pick));
        let saved = MosaicConfig::load(&s.mosaic.dir).unwrap();
        assert_eq!(saved, s.mosaic.config);
        assert_eq!(saved.accepted_checks, pick.keys());
        assert_eq!(e.counts(&s.mosaic.config).unwrap().notes, 7);
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
}
