//! Keeps the open session's `summary.json` current (see
//! [`mmm_core::session_summary`]), so the start screen can show the
//! session's facts without opening it.
//!
//! [`SummaryWriter::poll`] runs each frame. It notices changes to what the
//! summary shows (panels, groups, checks, canvas, last view) by itself, and
//! the Shell marks it dirty when a job ends (a result changed). The work
//! (classifying each group's result store, writing the file) runs on a
//! background thread; the UI thread does no file I/O.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use mmm_core::mosaic::MosaicConfig;
use mmm_core::mosaic::grouping::Groups;
use mmm_core::session_summary::{
    GroupSummary, ResultState, SUMMARY_FORMAT, SessionSummary, result_state, write_summary,
};

use crate::checks::CheckCounts;
use crate::session::OpenSession;
use crate::start::unix_now;

/// Per-group `(blended_at, exported_at)` stamps, in unix seconds.
type Stamps = BTreeMap<String, (Option<u64>, Option<u64>)>;

/// The facts of a session as a [`SessionSummary`] (with `written_at` 0: the
/// writer stamps it). Groups follow processing order; a group missing from
/// `results` is [`ResultState::None`] and one missing from `stamps` has no
/// stamps.
pub fn summarize(
    cfg: &MosaicConfig,
    groups: Option<&Groups>,
    checks: Option<CheckCounts>,
    canvas: Option<(u64, u64)>,
    last_view: Option<String>,
    results: &BTreeMap<String, ResultState>,
    stamps: &Stamps,
) -> SessionSummary {
    let groups = groups
        .map(|g| {
            g.processing_groups()
                .into_iter()
                .map(|(name, panels)| {
                    let (blended_at, exported_at) = stamps.get(&name).copied().unwrap_or_default();
                    GroupSummary {
                        result: results.get(&name).copied().unwrap_or_default(),
                        name,
                        panels,
                        blended_at,
                        exported_at,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    SessionSummary {
        format: SUMMARY_FORMAT,
        written_at: 0,
        panels: cfg.panels.len(),
        groups,
        problems: checks.map(|c| c.problems),
        notes: checks.map(|c| c.notes),
        canvas,
        last_view,
    }
}

/// What the summary shows apart from result states: a change to any of it
/// marks the writer dirty.
#[derive(Clone, PartialEq)]
struct Inputs {
    panels: usize,
    groups: Vec<(String, Vec<PathBuf>)>,
    checks: Option<CheckCounts>,
    canvas: Option<(u64, u64)>,
    last_view: Option<String>,
}

/// Writes the session's summary in the background when it has changed.
pub struct SummaryWriter {
    stamps: Stamps,
    dirty: bool,
    /// The last summary written (or found on opening).
    last: Option<SessionSummary>,
    inputs: Option<Inputs>,
    /// The running write: the summary it wrote, or `None` if unchanged.
    running: Option<Receiver<Option<SessionSummary>>>,
}

/// Equal facts, ignoring when they were written.
fn same_facts(a: &SessionSummary, b: &SessionSummary) -> bool {
    SessionSummary {
        written_at: 0,
        ..a.clone()
    } == SessionSummary {
        written_at: 0,
        ..b.clone()
    }
}

impl SummaryWriter {
    /// A writer for a session whose existing summary is `previous`: its
    /// blend and export stamps carry over. Starts dirty.
    pub fn new(previous: Option<SessionSummary>) -> Self {
        let stamps = previous
            .iter()
            .flat_map(|s| &s.groups)
            .map(|g| (g.name.clone(), (g.blended_at, g.exported_at)))
            .collect();
        SummaryWriter {
            stamps,
            dirty: true,
            last: previous,
            inputs: None,
            running: None,
        }
    }

    /// Note that something the summary shows may have changed (a job ended).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// True while a write runs (the caller should keep polling).
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Group `group` was blended at `at` (unix seconds).
    pub fn record_blend(&mut self, group: &str, at: u64) {
        self.stamps.entry(group.to_string()).or_default().0 = Some(at);
        self.dirty = true;
    }

    /// Group `group` was exported at `at` (unix seconds).
    pub fn record_export(&mut self, group: &str, at: u64) {
        self.stamps.entry(group.to_string()).or_default().1 = Some(at);
        self.dirty = true;
    }

    /// Take a finished write's outcome, notice changed inputs, and start a
    /// write when dirty and none is running. Cheap; call every frame.
    pub fn poll(
        &mut self,
        session: &OpenSession,
        checks: Option<CheckCounts>,
        last_view: Option<String>,
    ) {
        if let Some(rx) = &self.running {
            match rx.try_recv() {
                Ok(written) => {
                    if written.is_some() {
                        self.last = written;
                    }
                    self.running = None;
                }
                Err(TryRecvError::Disconnected) => self.running = None,
                Err(TryRecvError::Empty) => return,
            }
        }
        let inputs = Inputs {
            panels: session.mosaic.config.panels.len(),
            groups: session
                .groups
                .as_ref()
                .map(Groups::processing_groups)
                .unwrap_or_default(),
            checks,
            canvas: session.canvas(),
            last_view,
        };
        if self.inputs.as_ref() != Some(&inputs) {
            self.inputs = Some(inputs.clone());
            self.dirty = true;
        }
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let mosaic = session.mosaic.clone();
        let groups = session.groups.clone();
        let stamps = self.stamps.clone();
        let last = self.last.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("mmm-summary".into())
            .spawn(move || {
                let results: BTreeMap<String, ResultState> = groups
                    .iter()
                    .flat_map(Groups::processing_groups)
                    .map(|(g, _)| {
                        let state = result_state(&mosaic.result_path(&g));
                        (g, state)
                    })
                    .collect();
                let mut summary = summarize(
                    &mosaic.config,
                    groups.as_ref(),
                    inputs.checks,
                    inputs.canvas,
                    inputs.last_view,
                    &results,
                    &stamps,
                );
                if last.as_ref().is_some_and(|l| same_facts(l, &summary)) {
                    let _ = tx.send(None);
                    return;
                }
                summary.written_at = unix_now();
                match write_summary(&mosaic.dir, &summary) {
                    Ok(()) => {
                        let _ = tx.send(Some(summary));
                    }
                    Err(e) => {
                        tracing::warn!("cannot write the session summary: {e}");
                        let _ = tx.send(None);
                    }
                }
            });
        match spawned {
            Ok(_) => self.running = Some(rx),
            Err(e) => tracing::warn!("cannot start the summary thread: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn two_groups() -> (MosaicConfig, Groups) {
        let ha = paths(&["h1.xisf", "h2.xisf", "h3.xisf"]);
        let oiii = paths(&["o1.xisf", "o2.xisf"]);
        let cfg = MosaicConfig::new(ha.iter().chain(&oiii).cloned().collect());
        let groups = Groups {
            groups: BTreeMap::from([("Ha".to_string(), ha), ("OIII".to_string(), oiii)]),
            ungrouped: Vec::new(),
        };
        (cfg, groups)
    }

    #[test]
    fn summarize_reports_groups_checks_and_results() {
        let (cfg, groups) = two_groups();
        let results = BTreeMap::from([
            ("Ha".to_string(), ResultState::Complete),
            ("OIII".to_string(), ResultState::Stale),
        ]);
        let checks = CheckCounts {
            problems: 1,
            notes: 2,
            passed: 0,
        };
        let s = summarize(
            &cfg,
            Some(&groups),
            Some(checks),
            Some((600, 420)),
            Some("Ha".into()),
            &results,
            &Stamps::new(),
        );
        assert_eq!(s.panels, 5);
        assert_eq!(s.problems, Some(1));
        assert_eq!(s.notes, Some(2));
        assert_eq!(s.canvas, Some((600, 420)));
        assert_eq!(s.last_view.as_deref(), Some("Ha"));
        let names: Vec<_> = s.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Ha", "OIII"]);
        assert_eq!(s.groups[0].panels.len(), 3);
        assert_eq!(s.groups[1].panels, paths(&["o1.xisf", "o2.xisf"]));
        assert_eq!(s.groups[0].result, ResultState::Complete);
        assert_eq!(s.groups[1].result, ResultState::Stale);

        let unchecked = summarize(
            &cfg,
            None,
            None,
            None,
            None,
            &BTreeMap::new(),
            &Stamps::new(),
        );
        assert_eq!((unchecked.problems, unchecked.notes), (None, None));
        assert!(unchecked.groups.is_empty());
    }

    #[test]
    fn exports_survive_reopening() {
        let (cfg, groups) = two_groups();
        let previous = SessionSummary {
            format: SUMMARY_FORMAT,
            groups: vec![GroupSummary {
                name: "Ha".into(),
                exported_at: Some(100),
                blended_at: Some(50),
                ..GroupSummary::default()
            }],
            ..SessionSummary::default()
        };
        let mut w = SummaryWriter::new(Some(previous));
        let s = summarize(
            &cfg,
            Some(&groups),
            None,
            None,
            None,
            &BTreeMap::new(),
            &w.stamps,
        );
        assert_eq!(s.groups[0].exported_at, Some(100));
        assert_eq!(s.groups[0].blended_at, Some(50));
        assert_eq!(s.groups[1].exported_at, None);
        w.record_export("OIII", 200);
        w.record_blend("Ha", 300);
        let s = summarize(
            &cfg,
            Some(&groups),
            None,
            None,
            None,
            &BTreeMap::new(),
            &w.stamps,
        );
        assert_eq!(s.groups[1].exported_at, Some(200));
        assert_eq!(s.groups[0].blended_at, Some(300));
        assert_eq!(s.groups[0].exported_at, Some(100));
    }
}
