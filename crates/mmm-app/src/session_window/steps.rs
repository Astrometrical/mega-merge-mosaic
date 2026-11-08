//! The step strip's steps (spec §6): **Panels · Checks · Merge · Export**,
//! each with its state worked out from the session and the work running.
//! [`steps`] only reads; the top bar acts on the clicks.
//!
//! | Step | State |
//! |---|---|
//! | Panels | none: "Add panels"; previews running: "Reading previews {done} / {n}" (cancellable); previews stopped short: "Read previews" (next); else "{n} panels" (done) |
//! | Checks | no panels or no checks yet: "Checks"; analysis running or queued: "Analysing…" (cancellable); checks running: "Checking…"; analysis needed: "Analyse" (next); "{p} problem(s)" (warn); else "Checks passed" (done) |
//! | Merge | merges running or queued: "Merging {group} · {i} of {n} · {pct} %" (cancellable); no groups: "Merge"; a group without a complete result: "Merge" (next); else "Merged" (done) |
//! | Export | exports running or queued: "Exporting {i} of {n}" (cancellable); else "Export" |
//!
//! Every merge counts, ROI merges too, until an ROI's label shows its own
//! progress (spec §8).

use mega_ui::jobs::JobView;
use mega_ui::widgets::{StepState, StripStep};
use mmm_core::session_summary::ResultState;

use super::sections::counted;
use crate::checks::ChecksEngine;
use crate::jobs::{ANALYZE_LABEL, JobRunner, JobTarget, PREVIEWS_LABEL};
use crate::pipeline::{Pipeline, Step, wants_analysis};
use crate::session::OpenSession;
use crate::util::group_names;

/// The Panels step's place in the strip.
pub(super) const PANELS_STEP: usize = 0;
/// The Checks step's place.
pub(super) const CHECKS_STEP: usize = 1;
/// The Merge step's place.
pub(super) const MERGE_STEP: usize = 2;
/// The Export step's place.
pub(super) const EXPORT_STEP: usize = 3;

/// The strip's four steps for `session` and its work (see the module docs).
pub fn steps(
    session: &OpenSession,
    jobs: &JobRunner,
    pipeline: &Pipeline,
    checks: &ChecksEngine,
) -> [StripStep; 4] {
    let views = jobs.views();
    [
        panels_step(session, pipeline),
        checks_step(session, jobs, &views, checks),
        merge_step(session, jobs, &views),
        export_step(jobs, &views),
    ]
}

/// Whether a merge is needed: the session has groups and one of them has
/// no complete result (none yet, partly merged, or stale: merged before
/// the panels, the analysis or the settings changed).
pub fn needs_merge(session: &OpenSession) -> bool {
    group_names(session)
        .iter()
        .any(|g| session.results.get(g) != Some(&ResultState::Complete))
}

/// The jobs `views` shows that `is` picks, in order (running first).
pub(super) fn jobs_where(views: &[JobView], is: impl Fn(&JobView) -> bool) -> Vec<u64> {
    views.iter().filter(|v| is(v)).map(|v| v.id).collect()
}

fn step(label: impl Into<String>, state: StepState) -> StripStep {
    StripStep {
        label: label.into(),
        state,
        cancellable: false,
    }
}

fn working(label: String, fraction: Option<f32>) -> StripStep {
    StripStep {
        label,
        state: StepState::Working { fraction },
        cancellable: true,
    }
}

/// Job `id`'s progress through its stage, if it has reported any.
fn fraction(views: &[JobView], id: u64) -> Option<f32> {
    let v = views.iter().find(|v| v.id == id)?;
    (v.total > 0).then(|| (v.done.min(v.total) as f64 / v.total as f64) as f32)
}

fn panels_step(session: &OpenSession, pipeline: &Pipeline) -> StripStep {
    let n = session.mosaic.config.panels.len();
    if n == 0 {
        step("Add panels", StepState::Todo)
    } else if pipeline.step() == Some(Step::Previews) {
        let done = pipeline.previews().len().min(n);
        let label = format!("Reading previews {done} / {n}");
        working(label, Some(done as f32 / n as f32))
    } else if pipeline.previews_paused() {
        step("Read previews", StepState::Next)
    } else {
        step(counted(n, "panel"), StepState::Done)
    }
}

fn checks_step(
    session: &OpenSession,
    jobs: &JobRunner,
    views: &[JobView],
    checks: &ChecksEngine,
) -> StripStep {
    let cfg = &session.mosaic.config;
    if cfg.panels.is_empty() {
        return step("Checks", StepState::Todo);
    }
    if jobs.analyzing() {
        let analysis = jobs_where(views, is_analysis);
        let f = analysis.first().and_then(|id| fraction(views, *id));
        return working("Analysing…".into(), f);
    }
    if checks.running() {
        return step("Checking…", StepState::Working { fraction: None });
    }
    if wants_analysis(session) {
        return step("Analyse", StepState::Next);
    }
    match checks.counts(cfg) {
        None => step("Checks", StepState::Todo),
        Some(c) if c.problems > 0 => step(counted(c.problems, "problem"), StepState::Warn),
        Some(_) => step("Checks passed", StepState::Done),
    }
}

fn merge_step(session: &OpenSession, jobs: &JobRunner, views: &[JobView]) -> StripStep {
    if let Some(run) = jobs.run_of(JobTarget::Blend) {
        let f = fraction(views, run.id).unwrap_or(0.0);
        let pct = (f * 100.0).floor() as u32;
        let label = format!(
            "Merging {} · {} of {} · {pct} %",
            run.group, run.position, run.count
        );
        return working(label, Some(f));
    }
    if group_names(session).is_empty() {
        step("Merge", StepState::Todo)
    } else if needs_merge(session) {
        step("Merge", StepState::Next)
    } else {
        step("Merged", StepState::Done)
    }
}

fn export_step(jobs: &JobRunner, views: &[JobView]) -> StripStep {
    match jobs.run_of(JobTarget::Export) {
        Some(run) => working(
            format!("Exporting {} of {}", run.position, run.count),
            Some(fraction(views, run.id).unwrap_or(0.0)),
        ),
        None => step("Export", StepState::Todo),
    }
}

/// The previews and analysis jobs, by their labels (they have no group).
pub(super) fn is_previews(v: &JobView) -> bool {
    v.label == PREVIEWS_LABEL
}

/// See [`is_previews`].
pub(super) fn is_analysis(v: &JobView) -> bool {
    v.label == ANALYZE_LABEL
}
