//! Keeps the open session's `summary.json` current (see
//! [`mmm_core::session_summary`]), so the start screen can show the
//! session's facts without opening it.
//!
//! [`SummaryWriter::poll`] runs each frame. It notices changes to what the
//! summary shows (panels, groups, checks, canvas, last view) and to the
//! settings the results depend on (a settings edit marks them stale) by
//! itself, and the Shell marks it dirty when a job ends, cancelled ones
//! included ([`SummaryWriter::on_job_event`]: a result changed). The work
//! (classifying each group's result store, writing the file) runs on a
//! background thread; the UI thread does no file I/O. The classified
//! result states are handed back ([`SummaryWriter::take_results`]) unless
//! something changed while they were read. Leaving the session closes the
//! writer ([`SummaryWriter::close`]: no background write lands after it)
//! and writes the summary one last time: on the UI thread
//! ([`SummaryWriter::finish`]), or, when a job outlives the leaving, on the
//! thread that waits for it ([`FinalWrite`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{Mosaic, MosaicConfig, region_differs, settings_changed};
use mmm_core::session_summary::{
    GroupSummary, ResultState, SUMMARY_FORMAT, SessionSummary, result_state, write_summary,
};
use mmm_core::settings::{AnalyzeSettings, BlendSettings, GroupOverride};

use crate::blend_settings::load_draft;
use crate::checks::CheckCounts;
use crate::jobs::{JobEvent, JobTarget};
use crate::session::OpenSession;
use crate::start::unix_now;

/// Per-group `(blended_at, exported_at)` stamps, in unix seconds.
type Stamps = BTreeMap<String, (Option<u64>, Option<u64>)>;

/// The facts of a session as a [`SessionSummary`] (with `written_at` 0: the
/// writer stamps it). Groups follow processing order; a group missing from
/// `results` is [`ResultState::None`] and one missing from `stamps` has no
/// stamps. A group a region of which was merged with other settings
/// ([`region_differs`]) is recorded so (`GroupSummary::region_differs`) and
/// never as settings changed: it is out of date, as the window says.
pub fn summarize(
    cfg: &MosaicConfig,
    groups: Option<&Groups>,
    checks: Option<CheckCounts>,
    canvas: Option<(u64, u64)>,
    last_view: Option<String>,
    results: &BTreeMap<String, ResultState>,
    stamps: &Stamps,
) -> SessionSummary {
    let draft = load_draft(cfg);
    let groups = groups
        .map(|g| {
            g.processing_groups()
                .into_iter()
                .map(|(name, panels)| {
                    let (blended_at, exported_at) = stamps.get(&name).copied().unwrap_or_default();
                    let result = results.get(&name).copied().unwrap_or_default();
                    // Out of date though complete: never settings changed.
                    let region_differs = region_differs(cfg, &name);
                    let settings_changed = result == ResultState::Complete
                        && !cfg.needs_analyze.contains(&name)
                        && !region_differs
                        && settings_changed(cfg, &name, &draft);
                    GroupSummary {
                        result,
                        settings_changed,
                        region_differs,
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

/// What the summary shows apart from result states, and the settings those
/// states depend on: a change to any of it marks the writer dirty.
#[derive(Clone, PartialEq)]
struct Inputs {
    panels: usize,
    groups: Vec<(String, Vec<PathBuf>)>,
    checks: Option<CheckCounts>,
    canvas: Option<(u64, u64)>,
    last_view: Option<String>,
    settings: Settings,
}

/// The settings the group results depend on. Applying an edit
/// ([`mmm_core::mosaic::set_settings`], a CLI override, an analysis
/// planned) marks results stale on disk without any job ending, so a
/// change here re-reads every group's result state.
#[derive(Clone, PartialEq)]
struct Settings {
    analyze: AnalyzeSettings,
    blend: BlendSettings,
    overrides: BTreeMap<String, GroupOverride>,
    needs_analyze: BTreeSet<String>,
    /// The merge draft (edits to it change which groups read "settings
    /// changed").
    draft: BlendSettings,
    /// Merges committed so far (each adds a provenance entry, which is what
    /// "the last merge's settings" is read from).
    provenance: usize,
}

impl Settings {
    fn of(cfg: &MosaicConfig) -> Self {
        Settings {
            analyze: cfg.analyze.clone(),
            blend: cfg.blend.clone(),
            overrides: cfg.group_overrides.clone(),
            needs_analyze: cfg.needs_analyze.clone(),
            draft: load_draft(cfg),
            provenance: cfg.provenance.len(),
        }
    }
}

impl Inputs {
    /// What `session` shows now, with its check counts and last view.
    fn of(session: &OpenSession, checks: Option<CheckCounts>, last_view: Option<String>) -> Self {
        Inputs {
            panels: session.mosaic.config.panels.len(),
            groups: session
                .groups
                .as_ref()
                .map(Groups::processing_groups)
                .unwrap_or_default(),
            checks,
            canvas: session.canvas(),
            last_view,
            settings: Settings::of(&session.mosaic.config),
        }
    }
}

/// What a write found: each group's result, and the summary it wrote
/// (`None` if unchanged, the write failed, or the writer was closed).
struct Written {
    results: BTreeMap<String, ResultState>,
    summary: Option<SessionSummary>,
}

/// Classify each processing group's result store, then write the summary of
/// `inputs` and those results unless `last` already says the same, or
/// `closed` (held around the write) says the writer is closed. Blocking
/// file I/O.
fn classify_and_write(
    mosaic: &Mosaic,
    groups: Option<&Groups>,
    inputs: &Inputs,
    stamps: &Stamps,
    last: Option<&SessionSummary>,
    closed: Option<&Mutex<bool>>,
) -> Written {
    let results: BTreeMap<String, ResultState> = groups
        .iter()
        .flat_map(|g| g.processing_groups())
        .map(|(g, _)| {
            let state = result_state(&mosaic.result_path(&g));
            (g, state)
        })
        .collect();
    let mut summary = summarize(
        &mosaic.config,
        groups,
        inputs.checks,
        inputs.canvas,
        inputs.last_view.clone(),
        &results,
        stamps,
    );
    if last.is_some_and(|l| same_facts(l, &summary)) {
        return Written {
            results,
            summary: None,
        };
    }
    let closed = closed.map(|c| c.lock().unwrap_or_else(PoisonError::into_inner));
    if closed.as_deref() == Some(&true) {
        return Written {
            results,
            summary: None,
        };
    }
    summary.written_at = unix_now();
    let summary = match write_summary(&mosaic.dir, &summary) {
        Ok(()) => Some(summary),
        Err(e) => {
            tracing::warn!("cannot write the session summary: {e}");
            None
        }
    };
    Written { results, summary }
}

/// The summary's last write, from [`SummaryWriter::close`].
pub struct FinalWrite {
    mosaic: Mosaic,
    groups: Option<Groups>,
    inputs: Inputs,
    stamps: Stamps,
    last: Option<SessionSummary>,
}

impl FinalWrite {
    /// Classify each group's result and write the summary, unless its
    /// facts are unchanged. Blocking file I/O; run it once no job writes
    /// the session's results any more (a store a job holds reads as no
    /// result).
    pub fn write(self) {
        classify_and_write(
            &self.mosaic,
            self.groups.as_ref(),
            &self.inputs,
            &self.stamps,
            self.last.as_ref(),
            None,
        );
    }
}

/// Writes the session's summary in the background when it has changed.
pub struct SummaryWriter {
    stamps: Stamps,
    dirty: bool,
    /// The last summary written (or found on opening).
    last: Option<SessionSummary>,
    inputs: Option<Inputs>,
    /// The running write.
    running: Option<Receiver<Written>>,
    /// Result states classified and not taken yet.
    results: Option<BTreeMap<String, ResultState>>,
    /// Set (under the lock) by [`SummaryWriter::close`]: background writes
    /// check it, holding the lock while they write.
    closed: Arc<Mutex<bool>>,
    /// How many summaries have been written (for tests).
    #[cfg(test)]
    writes: usize,
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
            results: None,
            closed: Arc::new(Mutex::new(false)),
            #[cfg(test)]
            writes: 0,
        }
    }

    /// Close the writer, the session being left: no background write lands
    /// once this returns (one under way finishes first; one that has not
    /// written yet never does). Returns the summary's last write, of
    /// `session` as it is now, to run once nothing writes its results any
    /// more ([`FinalWrite::write`]).
    pub fn close(
        &mut self,
        session: &OpenSession,
        checks: Option<CheckCounts>,
        last_view: Option<String>,
    ) -> FinalWrite {
        *self.closed.lock().unwrap_or_else(PoisonError::into_inner) = true;
        if let Some(rx) = self.running.take()
            && let Ok(Written {
                summary: Some(s), ..
            }) = rx.try_recv()
        {
            self.last = Some(s);
        }
        self.dirty = false;
        FinalWrite {
            mosaic: session.mosaic.clone(),
            groups: session.groups.clone(),
            inputs: Inputs::of(session, checks, last_view),
            stamps: self.stamps.clone(),
            last: self.last.clone(),
        }
    }

    /// [`SummaryWriter::close`], then the last write on this thread: for a
    /// session left with every job stopped. Blocking file I/O on the UI
    /// thread, on the user's ‹ only.
    pub fn finish(
        &mut self,
        session: &OpenSession,
        checks: Option<CheckCounts>,
        last_view: Option<String>,
    ) {
        self.close(session, checks, last_view).write();
    }

    /// Note that something the summary shows may have changed (a job ended).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// True while a write runs. A change that lands meanwhile is picked up
    /// by the next poll (any later frame; no repaint is forced for it).
    #[cfg(test)]
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

    /// Note a job event: a finished blend or export (`target`) stamps its
    /// group at `at` (unix seconds); a job that ended marks the writer
    /// dirty (its result may have changed). A cancelled one too: discarding
    /// a cancelled blend may invalidate its store (`jobs::discard_blend`),
    /// and on Windows a store a blend holds reads as no result until the
    /// blend lets go of it (see [`mmm_core::tiles::FileTileReader`]).
    pub fn on_job_event(&mut self, event: &JobEvent, target: Option<(JobTarget, &str)>, at: u64) {
        match event {
            JobEvent::Finished { .. } => {
                self.mark_dirty();
                match target {
                    Some((JobTarget::Blend, group)) => self.record_blend(group, at),
                    Some((JobTarget::Export, group)) => self.record_export(group, at),
                    None => {}
                }
            }
            JobEvent::Failed { .. } | JobEvent::Cancelled { .. } => self.mark_dirty(),
            JobEvent::Preview(_) => {}
        }
    }

    /// The result states the last write classified, once, if nothing that
    /// could change them happened while they were read (a job ended, the
    /// settings, panels or groups changed): the session's
    /// [`OpenSession::results`].
    pub fn take_results(&mut self) -> Option<BTreeMap<String, ResultState>> {
        self.results.take()
    }

    /// Take a finished write's outcome, notice changed inputs, and start a
    /// write when dirty and none is running. Cheap; call every frame. A
    /// finished write asks `ctx` for one repaint, so a change that landed
    /// meanwhile is flushed by the next frame's poll even when idle.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        checks: Option<CheckCounts>,
        last_view: Option<String>,
    ) {
        let mut classified = None;
        if let Some(rx) = &self.running {
            match rx.try_recv() {
                Ok(written) => {
                    if written.summary.is_some() {
                        #[cfg(test)]
                        {
                            self.writes += 1;
                        }
                        self.last = written.summary;
                    }
                    classified = Some(written.results);
                    self.running = None;
                }
                Err(TryRecvError::Disconnected) => self.running = None,
                Err(TryRecvError::Empty) => return,
            }
        }
        let inputs = Inputs::of(session, checks, last_view);
        if self.inputs.as_ref() != Some(&inputs) {
            self.inputs = Some(inputs.clone());
            self.dirty = true;
        }
        // States read before a change may be out of date: the write the
        // change starts reads them again.
        if let Some(results) = classified
            && !self.dirty
        {
            self.results = Some(results);
        }
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let mosaic = session.mosaic.clone();
        let groups = session.groups.clone();
        let stamps = self.stamps.clone();
        let last = self.last.clone();
        let closed = Arc::clone(&self.closed);
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("mmm-summary".into())
            .spawn(move || {
                let written = classify_and_write(
                    &mosaic,
                    groups.as_ref(),
                    &inputs,
                    &stamps,
                    last.as_ref(),
                    Some(&closed),
                );
                let _ = tx.send(written);
                ctx.request_repaint();
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
    use mmm_core::job::JobCtl;
    use mmm_core::mosaic::{analyze_all, blend_group, invalidate_group, set_settings};
    use mmm_core::session_summary::{SUMMARY_FILE, read_summary};

    use crate::start::tests::temp_dir;

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

    /// Poll `w` until no write runs and nothing is dirty (bounded by
    /// [`PATIENCE`](crate::testutil::PATIENCE)).
    fn settle(w: &mut SummaryWriter, ctx: &egui::Context, s: &OpenSession) {
        let deadline = std::time::Instant::now() + crate::testutil::PATIENCE;
        for _ in 0..3 {
            loop {
                w.poll(ctx, s, None, None);
                if !w.busy() {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "write never landed");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn poll_writes_stamps_and_skips_unchanged() {
        let root = temp_dir("summary-poll");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        let ctx = egui::Context::default();
        let mut w = SummaryWriter::new(None);
        w.record_blend("Ha", 111);
        w.record_export("Ha", 222);
        settle(&mut w, &ctx, &s);
        let written = read_summary(&dir).unwrap();
        assert_eq!(written.groups[0].name, "Ha");
        assert_eq!(written.groups[0].blended_at, Some(111));
        assert_eq!(written.groups[0].exported_at, Some(222));
        assert_eq!(w.writes, 1);
        assert!(ctx.has_requested_repaint(), "a finished write repaints");
        // Nothing changed: marking dirty and polling again writes nothing.
        w.mark_dirty();
        settle(&mut w, &ctx, &s);
        assert_eq!(w.writes, 1, "unchanged facts are not rewritten");
        // A real change is written.
        w.record_blend("OIII", 333);
        settle(&mut w, &ctx, &s);
        assert_eq!(w.writes, 2);
        assert_eq!(read_summary(&dir).unwrap().groups[1].blended_at, Some(333));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finish_writes_at_once_and_nothing_lands_after() {
        let root = temp_dir("summary-finish");
        let s = crate::testutil::two_group_session(&root);
        let dir = s.mosaic.dir.clone();
        let ctx = egui::Context::default();
        let mut w = SummaryWriter::new(None);
        // A background write starts, then the session is left at once.
        w.poll(&ctx, &s, None, Some("Ha".into()));
        w.record_blend("Ha", 111);
        w.finish(&s, None, Some("OIII".into()));
        let written = read_summary(&dir).unwrap();
        assert_eq!(written.last_view.as_deref(), Some("OIII"));
        assert_eq!(written.groups[0].blended_at, Some(111));
        let bytes = std::fs::read(dir.join(SUMMARY_FILE)).unwrap();
        // Nothing is written after it, by the write under way or a later one.
        w.record_blend("OIII", 222);
        settle(&mut w, &ctx, &s);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(std::fs::read(dir.join(SUMMARY_FILE)).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A two-group session in `root`, analysed, with Ha blended.
    fn ha_merged(root: &std::path::Path) -> OpenSession {
        let mut s = crate::testutil::two_group_session(root);
        s.groups = Some(analyze_all(&mut s.mosaic, &JobCtl::none()).unwrap());
        blend_group(&mut s.mosaic, "Ha", None, &JobCtl::none()).unwrap();
        s
    }

    /// The result state the written summary gives group `i`.
    fn written_result(s: &OpenSession, i: usize) -> ResultState {
        read_summary(&s.mosaic.dir).unwrap().groups[i].result
    }

    #[test]
    fn settings_change_rewrites_stale_results() {
        let root = temp_dir("summary-settings");
        let mut s = ha_merged(&root);
        let ctx = egui::Context::default();
        let mut w = SummaryWriter::new(None);
        settle(&mut w, &ctx, &s);
        assert_eq!(written_result(&s, 0), ResultState::Complete);
        // The states read are handed back, once.
        let states = |ha, oiii| {
            Some(BTreeMap::from([
                ("Ha".to_string(), ha),
                ("OIII".to_string(), oiii),
            ]))
        };
        assert_eq!(
            w.take_results(),
            states(ResultState::Complete, ResultState::None)
        );
        assert_eq!(w.take_results(), None);
        // A blend-local change marks every result stale (no job ends).
        let mut blend = s.mosaic.config.blend.clone();
        blend.feather_px *= 2.0;
        let analyze = s.mosaic.config.analyze.clone();
        set_settings(&mut s.mosaic, analyze, blend).unwrap();
        settle(&mut w, &ctx, &s);
        assert_eq!(written_result(&s, 0), ResultState::Stale);
        assert_eq!(
            w.take_results(),
            states(ResultState::Stale, ResultState::None)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn summary_records_settings_changed() {
        let root = temp_dir("summary-settings-changed");
        let mut s = ha_merged(&root);
        let ctx = egui::Context::default();
        let mut w = SummaryWriter::new(None);
        settle(&mut w, &ctx, &s);
        let dir = s.mosaic.dir.clone();
        let flags = || {
            read_summary(&dir)
                .unwrap()
                .groups
                .iter()
                .map(|g| g.settings_changed)
                .collect::<Vec<_>>()
        };
        assert_eq!(written_result(&s, 0), ResultState::Complete);
        assert_eq!(flags(), [false, false]);
        // Editing the draft (it lives in `ui`) marks the merged group.
        let mut draft = crate::blend_settings::load_draft(&s.mosaic.config);
        draft.feather_px *= 2.0;
        crate::blend_settings::save_draft(&mut s.mosaic.config.ui, &draft);
        settle(&mut w, &ctx, &s);
        assert_eq!(written_result(&s, 0), ResultState::Complete);
        assert_eq!(flags(), [true, false], "only the merged group");
        // Merging with the draft clears it (a committed merge adds provenance).
        let a = s.mosaic.config.analyze.clone();
        set_settings(&mut s.mosaic, a, draft.clone()).unwrap();
        blend_group(&mut s.mosaic, "Ha", None, &JobCtl::none()).unwrap();
        settle(&mut w, &ctx, &s);
        assert_eq!(flags(), [false, false]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_region_merged_with_other_settings_is_out_of_date_not_settings_changed() {
        use crate::start::facts::{CardState, card_state};
        use mmm_core::mosaic::{plan_blend_with, run_blend};
        let root = temp_dir("summary-region-differs");
        let mut s = ha_merged(&root);
        blend_group(&mut s.mosaic, "OIII", None, &JobCtl::none()).unwrap();
        let ctx = egui::Context::default();
        let mut w = SummaryWriter::new(None);
        // The draft changed: settings changed, not out of date.
        let mut draft = crate::blend_settings::load_draft(&s.mosaic.config);
        draft.feather_px *= 2.0;
        crate::blend_settings::save_draft(&mut s.mosaic.config.ui, &draft);
        settle(&mut w, &ctx, &s);
        let dir = s.mosaic.dir.clone();
        let written = || read_summary(&dir).unwrap();
        let card = |sum: &SessionSummary| card_state(sum.panels, true, Some(sum));
        assert_eq!(card(&written()), CardState::SettingsChanged);
        // A region of Ha merged with the draft: Ha's store stays complete,
        // but it is out of date, and the card no longer reads Merged or
        // Settings changed.
        let plan = plan_blend_with(&s.mosaic, "Ha", Some([0, 0, 64, 64]), draft).unwrap();
        let aligned = run_blend(&plan, &JobCtl::none()).unwrap();
        s.mosaic.apply_blend(&plan, aligned).unwrap();
        settle(&mut w, &ctx, &s);
        let sum = written();
        let ha = &sum.groups[0];
        assert_eq!((ha.name.as_str(), ha.result), ("Ha", ResultState::Complete));
        assert!(ha.region_differs && !ha.settings_changed, "{ha:?}");
        let oiii = &sum.groups[1];
        assert!(!oiii.region_differs && oiii.settings_changed, "{oiii:?}");
        assert_eq!(card(&sum), CardState::ReMergeNeeded);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_job_rewrites_the_summary() {
        let root = temp_dir("summary-cancelled");
        let s = ha_merged(&root);
        let ctx = egui::Context::default();
        let mut w = SummaryWriter::new(None);
        settle(&mut w, &ctx, &s);
        assert_eq!(written_result(&s, 0), ResultState::Complete);
        // A cancelled blend whose settings were superseded invalidates its
        // store (`jobs::discard_blend`), then reports Cancelled.
        invalidate_group(&s.mosaic, "Ha").unwrap();
        w.on_job_event(&JobEvent::Cancelled { id: 1 }, None, 0);
        settle(&mut w, &ctx, &s);
        assert_eq!(written_result(&s, 0), ResultState::Stale);
        let _ = std::fs::remove_dir_all(&root);
    }
}
