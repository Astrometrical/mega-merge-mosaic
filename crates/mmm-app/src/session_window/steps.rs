//! The step strip's steps (spec §6): **Panels · Checks · Merge · Export**,
//! each with its state worked out from the session and the work running.
//! [`steps`] only reads; the top bar acts on the clicks.
//!
//! | Step | State |
//! |---|---|
//! | Panels | none: "Add panels"; previews running: "Reading previews {done} / {n}" (cancellable); previews stopped short: "Read previews" (next); else "{n} panels" (done) |
//! | Checks | no panels, no checks yet, or an analysis about to start by itself: "Checks"; analysis running or queued: "Analysing…" (cancellable); checks running: "Checking…"; an analysis the pipeline left to the user ([`Pipeline::analysis_stalled`]): "Analyse" (next); "{p} problem(s)" (warn); else "Checks passed" (done) |
//! | Merge | a whole-canvas merge queued while an export runs: "Merge queued after export" (cancellable, no fraction); whole-canvas merges running or queued: "Merging {group} · {i} of {n} · {pct} %" (cancellable; no "{i} of {n}" for one merge, no group name in a session of one group); no groups, or analysis needed or running: "Merge"; nothing ever merged: "Merge" (next); a group out of date ([`out_of_date`]: no complete result, it needs analysis, or a region of it was merged with other settings): "Out of date" (next); a group whose last merge used other settings than the draft ([`draft_changed`]): "Settings changed" (next); else "Merged" (done) |
//! | Export | exports running or queued: "Exporting {i} of {n}" ("Exporting" for one) (cancellable); else "Export" |
//!
//! One step at most is next (highlighted): the first that waits for the
//! user; a later one shows as to do. Steps working by themselves are no
//! one's next action, so a step after one can be next (merging while the
//! previews load). An ROI's merge shows on its ROI (spec §6, §8), never
//! here.

use mega_ui::jobs::JobView;
use mega_ui::widgets::{StepState, StripStep};
use mmm_core::mosaic::{region_differs, settings_changed};
use mmm_core::session_summary::ResultState;

use super::sections::counted;
use crate::blend_settings::load_draft;
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
    let mut steps = [
        panels_step(session, pipeline),
        checks_step(session, jobs, &views, pipeline, checks),
        merge_step(session, jobs, &views),
        export_step(jobs, &views),
    ];
    // One next step: the first.
    let mut next = false;
    for s in &mut steps {
        if s.state == StepState::Next {
            if next {
                s.state = StepState::Todo;
            }
            next = true;
        }
    }
    steps
}

/// Whether the Checks step offers **Analyse** (shown next, or to do when
/// another step is next): the pipeline has left the analysis to the user
/// ([`Pipeline::analysis_stalled`], which stays so while an analysis the
/// user started runs) and the step shows nothing else (no analysis running
/// or queued, no checks running).
pub(super) fn offers_analysis(
    session: &OpenSession,
    jobs: &JobRunner,
    pipeline: &Pipeline,
    checks: &ChecksEngine,
) -> bool {
    !session.mosaic.config.panels.is_empty()
        && !jobs.analyzing()
        && !checks.running()
        && pipeline.analysis_stalled(session)
}

/// Whether `group`'s mosaic is out of date: its result is not complete
/// (none yet, partly merged, a merge that stopped, or one made before the
/// panels changed), the group needs a new analysis, or a region of it was
/// merged with other settings than its last whole merge
/// ([`region_differs`]: the store is complete but not of one merge).
pub fn out_of_date(session: &OpenSession, group: &str) -> bool {
    let cfg = &session.mosaic.config;
    session.results.get(group) != Some(&ResultState::Complete)
        || cfg.needs_analyze.contains(group)
        || region_differs(cfg, group)
}

/// Whether `group` is up to date but its last whole-canvas merge used
/// different effective merge settings from the draft (the draft with the
/// group's override applied). Never blocks export.
pub fn draft_changed(session: &OpenSession, group: &str) -> bool {
    let cfg = &session.mosaic.config;
    !out_of_date(session, group) && settings_changed(cfg, group, &load_draft(cfg))
}

/// Whether a merge is needed: some group is [`out_of_date`] or its
/// [`draft_changed`].
pub fn merge_needed(session: &OpenSession) -> bool {
    !groups_to_merge(session).is_empty()
}

/// The groups a **Merge** merges: the ones out of date or with the draft
/// changed, in order.
pub fn groups_to_merge(session: &OpenSession) -> Vec<String> {
    group_names(session)
        .into_iter()
        .filter(|g| out_of_date(session, g) || draft_changed(session, g))
        .collect()
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
        tooltip: None,
    }
}

fn working(label: String, fraction: Option<f32>) -> StripStep {
    StripStep {
        label,
        state: StepState::Working { fraction },
        cancellable: true,
        tooltip: None,
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
    pipeline: &Pipeline,
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
    if offers_analysis(session, jobs, pipeline, checks) {
        return step("Analyse", StepState::Next);
    }
    if wants_analysis(session) {
        // The pipeline starts it by itself.
        return step("Checks", StepState::Todo);
    }
    match checks.counts(cfg) {
        None => step("Checks", StepState::Todo),
        Some(c) if c.problems > 0 => step(counted(c.problems, "problem"), StepState::Warn),
        Some(_) => step("Checks passed", StepState::Done),
    }
}

fn merge_step(session: &OpenSession, jobs: &JobRunner, views: &[JobView]) -> StripStep {
    if jobs.merge_waits_for_export() {
        return working("Merge queued after export".into(), None);
    }
    if let Some(run) = jobs.run_of(JobTarget::Blend) {
        let f = fraction(views, run.id).unwrap_or(0.0);
        let pct = (f * 100.0).floor() as u32;
        let mut label = "Merging".to_string();
        if group_names(session).len() > 1 {
            label.push_str(&format!(" {}", run.group));
        }
        if run.count > 1 {
            label.push_str(&format!(" · {} of {}", run.position, run.count));
        }
        label.push_str(&format!(" · {pct} %"));
        return working(label, Some(f));
    }
    if group_names(session).is_empty() || wants_analysis(session) || jobs.analyzing() {
        step("Merge", StepState::Todo)
    } else if !session.merged {
        step("Merge", StepState::Next)
    } else if group_names(session).iter().any(|g| out_of_date(session, g)) {
        step("Out of date", StepState::Next)
    } else if group_names(session)
        .iter()
        .any(|g| draft_changed(session, g))
    {
        step("Settings changed", StepState::Next)
    } else {
        step("Merged", StepState::Done)
    }
}

fn export_step(jobs: &JobRunner, views: &[JobView]) -> StripStep {
    match jobs.run_of(JobTarget::Export) {
        Some(run) => working(
            if run.count > 1 {
                format!("Exporting {} of {}", run.position, run.count)
            } else {
                "Exporting".to_string()
            },
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
