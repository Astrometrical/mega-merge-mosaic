//! Background jobs: analysis and blending on worker threads, with progress,
//! cancellation and "newer work wins" (spec §8).
//!
//! Each job follows the core's concurrency contract
//! ([`mmm_core::mosaic`]): it is *planned* on the UI thread when it starts,
//! *run* on its own `std::thread` with a [`JobCtl`], and its outcome is
//! *applied* on the UI thread by [`JobRunner::poll`]. An analysis first
//! resolves the groups (panel header reads) on a worker over a copy of the
//! mosaic, then plans with [`plan_analyze_scoped`], so the UI thread never
//! reads panels.
//!
//! The UI thread does write a little: planning, applying and discarding
//! save `mosaic.json` (a few kilobytes) and invalidation flushes only the
//! tile state table. That is the accepted exception to "no file I/O on the
//! UI thread" (spec §8): the core's contract is that only the owner thread
//! saves the configuration, and these writes are bookkeeping, not
//! processing — no pixel data is read or written.
//!
//! At most one job runs at a time; newer requests queue behind it:
//!
//! - A new analysis cancels the running job and replaces everything
//!   queued: it re-stales every group anyway. Panel previews are the
//!   exception: they hold no result store and feed the analysis's decode
//!   cache, so the analysis queues behind them instead.
//! - New previews cancel running previews and replace queued ones; they
//!   queue behind any other job. Each preview is delivered as it finishes
//!   ([`JobEvent::Preview`]).
//! - A new blend cancels a running blend of the same group and replaces
//!   that group's queued blends; blends of other groups are kept and run
//!   in order. Behind a running analysis it waits (its group cannot be
//!   blended until the analysis is applied).
//! - An export queues behind everything and cancels nothing; an analysis
//!   cancels a running export and drops queued ones like any other job.
//!   It reads its group's result through a [`mmm_core::tiles::TileReader`]
//!   (never the store's writer lock); since one job runs at a time, no
//!   blend can recreate the store under it.
//!
//! A queued job starts once `poll` has applied (or discarded) its
//! predecessor's outcome; its worker joins the predecessor's thread before
//! running, so that wait never happens on the UI thread.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mega_ui::jobs::JobView;
use mmm_core::export::{ExportFormat, ExportReport, export_group};
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{
    AnalyzeOutcome, AnalyzePlan, AnalyzeScope, BlendPlan, Grouping, Mosaic, PanelEntry,
    invalidate_group, mark_needs_analyze, plan_analyze_scoped, plan_blend_with, run_analyze,
    run_blend,
};
use mmm_core::preview::{Preview, PreviewPlan, PreviewRun, plan_previews, run_previews};
use mmm_core::reference::ReferenceFrame;
use mmm_core::settings::BlendSettings;

use crate::session::OpenSession;

/// Least time between two progress updates sent by a worker (≤ 30/s).
pub const PROGRESS_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Label of an analysis job.
pub const ANALYZE_LABEL: &str = "Analyse";
/// Label of a previews job.
pub const PREVIEWS_LABEL: &str = "Previews";
/// Stage shown while a cancelled job winds down.
const CANCELLING: &str = "Cancelling…";
/// Stage shown for a queued job.
const WAITING: &str = "Waiting…";
/// Stage shown while an analysis resolves its groups.
const RESOLVING: &str = "Resolving groups…";

/// How a job ended (or, for previews, what it delivered), as reported by
/// [`JobRunner::poll`].
#[derive(Clone, Debug, PartialEq)]
pub enum JobEvent {
    /// A previews job finished one panel's preview (shared, never copied).
    Preview(Arc<Preview>),
    /// The job finished and its outcome was applied.
    Finished {
        /// Job id.
        id: u64,
        /// Job label.
        label: String,
    },
    /// The job was cancelled (by the user or by newer work); a cancelled
    /// analysis still keeps the groups it finished.
    Cancelled {
        /// Job id.
        id: u64,
    },
    /// The job could not be planned, failed, or its outcome was refused.
    Failed {
        /// Job id.
        id: u64,
        /// What went wrong, for the user.
        message: String,
    },
}

/// The kind of job that works on one group's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobTarget {
    /// A blend of the group.
    Blend,
    /// An export of the group's result.
    Export,
}

/// Where a run of merges or exports stands ([`JobRunner::run_of`]): the
/// whole-canvas blends, or the exports, since the last time none of them
/// was running or queued (an ROI's blend is in no run).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The job at the head of the run: running, else the first queued.
    pub id: u64,
    /// The head job's group.
    pub group: String,
    /// The head job's place in the run, from 1.
    pub position: usize,
    /// The jobs of the run: those that ended, and those running or queued.
    pub count: usize,
}

/// What a queued job will do.
#[derive(Clone, Debug)]
enum Kind {
    Analyze {
        scope: AnalyzeScope,
    },
    Previews,
    Blend {
        group: String,
        rect: Option<[u64; 4]>,
        /// The committed settings when it was queued: the merge runs with
        /// these, whatever is committed by the time it starts.
        settings: BlendSettings,
    },
    Export(ExportJob),
}

/// An export of one group's blended result to a file.
#[derive(Clone, Debug)]
struct ExportJob {
    group: String,
    out: std::path::PathBuf,
    format: ExportFormat,
}

/// A job waiting for the running one to end.
struct Queued {
    id: u64,
    kind: Kind,
}

/// A planned job, handed to its worker.
enum Planned {
    /// An analysis's first phase: resolve the groups of a copy of the
    /// mosaic (reads panel headers).
    Resolve(Box<Mosaic>, AnalyzeScope),
    Analyze(AnalyzePlan),
    Blend(BlendPlan),
    Previews(PreviewPlan),
    /// An export of a group of the mosaic in `dir`.
    Export {
        dir: std::path::PathBuf,
        job: ExportJob,
    },
}

/// What resolution was based on: the inputs of [`Mosaic::groups`].
#[derive(PartialEq)]
struct GroupInputs {
    panels: Vec<PanelEntry>,
    grouping: Grouping,
}

impl GroupInputs {
    fn of(m: &Mosaic) -> Self {
        GroupInputs {
            panels: m.config.panels.clone(),
            grouping: m.config.grouping.clone(),
        }
    }
}

/// What a worker achieved.
enum Done {
    Resolved(mmm_core::Result<Groups>),
    Analyze {
        outcome: AnalyzeOutcome,
        result: mmm_core::Result<()>,
        /// The analysed frame as saved by the run (or still on disk).
        frame: Option<ReferenceFrame>,
    },
    Blend(mmm_core::Result<[u64; 4]>),
    /// The previews run (its frame and per-panel failures), or why the
    /// whole run failed.
    Previews(mmm_core::Result<PreviewRun>),
    Export(mmm_core::Result<ExportReport>),
}

/// Worker to UI thread.
enum Msg {
    Progress(String, u64, u64),
    Preview(Arc<Preview>),
    Done(Box<Done>),
}

/// The running job.
struct Running {
    id: u64,
    token: CancelToken,
    rx: Receiver<Msg>,
    handle: JoinHandle<()>,
    /// The plan of a blend, kept to apply or discard its outcome.
    blend: Option<BlendPlan>,
    /// While resolving groups: the inputs the resolution is based on, and
    /// which groups the analysis then covers.
    resolving: Option<(GroupInputs, AnalyzeScope)>,
    /// A previews job (analysis queues behind it rather than cancel it).
    previews: bool,
    /// An export job (input edits spare it).
    export: bool,
    stage: String,
    done: u64,
    total: u64,
    /// Progress updates received so far.
    updates: u64,
}

/// Limits progress updates to one per `min` interval; a final update
/// (`done == total`) always passes.
pub struct Throttle {
    min: Duration,
    last: Option<Instant>,
}

impl Throttle {
    /// A throttle letting one update through per `min`.
    pub fn new(min: Duration) -> Self {
        Throttle { min, last: None }
    }

    /// Whether an update at `now` should be sent (and, if so, record it).
    pub fn ready(&mut self, now: Instant, last_of_stage: bool) -> bool {
        let due = self
            .last
            .is_none_or(|t| now.saturating_duration_since(t) >= self.min);
        if due || last_of_stage {
            self.last = Some(now);
        }
        due || last_of_stage
    }
}

/// Runs analysis and blend jobs off the UI thread for one session.
pub struct JobRunner {
    ctx: egui::Context,
    next_id: u64,
    running: Option<Running>,
    queue: VecDeque<Queued>,
    /// The last finished worker's thread, joined by the next worker.
    previous: Option<JoinHandle<()>>,
    /// Every job's label, by id (for notices about ended jobs).
    labels: BTreeMap<u64, String>,
    /// Blend and export jobs' group, by id (see [`JobRunner::target`]).
    targets: BTreeMap<u64, (JobTarget, String)>,
    /// The blends of the whole canvas (not of an ROI), by id.
    whole: BTreeSet<u64>,
    /// Whole-canvas blends and exports that ended (ran to an end, whatever
    /// it was) in the current run of each ([`JobRunner::run_of`]).
    blends_ended: usize,
    exports_ended: usize,
    /// Events raised outside `poll` (queued jobs cancelled), delivered by it.
    pending: Vec<JobEvent>,
    /// The frame of the last previews run that finished with every panel,
    /// until taken ([`JobRunner::take_previews_frame`]).
    previews_frame: Option<ReferenceFrame>,
    #[cfg(test)]
    gate: Option<std::sync::Arc<tests::Gate>>,
    #[cfg(test)]
    stepper: Option<std::sync::Arc<tests::Stepper>>,
    /// Called once [`JobRunner::stop_all`] has cancelled the jobs.
    #[cfg(test)]
    on_stop: Option<Box<dyn FnOnce() + Send>>,
}

impl JobRunner {
    /// A runner that repaints `ctx` whenever a job reports progress or ends.
    pub fn new(ctx: egui::Context) -> Self {
        JobRunner {
            ctx,
            next_id: 1,
            running: None,
            queue: VecDeque::new(),
            previous: None,
            labels: BTreeMap::new(),
            targets: BTreeMap::new(),
            whole: BTreeSet::new(),
            blends_ended: 0,
            exports_ended: 0,
            pending: Vec::new(),
            previews_frame: None,
            #[cfg(test)]
            gate: None,
            #[cfg(test)]
            stepper: None,
            #[cfg(test)]
            on_stop: None,
        }
    }

    /// Analyze every group; returns the job id. Cancels the running job and
    /// replaces anything queued (newer work wins), except previews: the
    /// analysis waits for them.
    ///
    /// The groups are resolved on a worker first ("Resolving groups…").
    /// Planning then happens on the UI thread: it marks and saves
    /// `mosaic.json` and invalidates each group's existing store by flag
    /// (flushing only the state table). That is bookkeeping of a few
    /// kilobytes, not processing: no panel or pixel data is read there, and
    /// the analysis itself runs on the worker.
    pub fn start_analyze(&mut self, session: &mut OpenSession) -> u64 {
        self.start_analyze_scoped(session, AnalyzeScope::All)
    }

    /// [`JobRunner::start_analyze`] limited to `scope`: with
    /// [`AnalyzeScope::Needed`] only the groups marked as needing analysis
    /// (and new ones). If the frame then moves, it restarts with every group
    /// (see [`JobRunner::poll`]), reporting no failure.
    pub fn start_analyze_scoped(&mut self, session: &mut OpenSession, scope: AnalyzeScope) -> u64 {
        let id = self.new_id(ANALYZE_LABEL.into());
        if let Some(r) = self.running.as_ref().filter(|r| !r.previews) {
            r.token.cancel();
        }
        let mut dropped = Vec::new();
        self.queue.retain(|q| {
            let keep = matches!(q.kind, Kind::Previews);
            if !keep {
                dropped.push(q.id);
            }
            keep
        });
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
        self.queue.push_back(Queued {
            id,
            kind: Kind::Analyze { scope },
        });
        self.start_next(session);
        id
    }

    /// Build (or load from the session's cache) every panel's preview in
    /// the shared frame; returns the job id. Each preview is delivered by
    /// [`JobRunner::poll`] as [`JobEvent::Preview`] when it finishes (the
    /// UI is repainted for each). Cancels running previews and replaces
    /// queued ones (newer work wins); behind any other job it waits.
    /// Planning reads nothing (the worker reads the panels).
    pub fn start_previews(&mut self, session: &OpenSession) -> u64 {
        let id = self.new_id(PREVIEWS_LABEL.into());
        if let Some(r) = self.running.as_ref().filter(|r| r.previews) {
            r.token.cancel();
        }
        let mut dropped = Vec::new();
        self.queue.retain(|q| {
            let keep = !matches!(q.kind, Kind::Previews);
            if !keep {
                dropped.push(q.id);
            }
            keep
        });
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
        self.queue.push_back(Queued {
            id,
            kind: Kind::Previews,
        });
        self.start_next(session);
        id
    }

    /// Blend `rect` (level-0 canvas coordinates; `None` = the whole canvas)
    /// of `group`; returns the job id. Cancels a running blend of the same
    /// group and replaces its queued ones (newer work wins); blends of
    /// other groups are left to run, this one queueing behind them, and it
    /// waits behind a running or queued analysis. Planned (on the UI
    /// thread, no I/O) when it starts.
    ///
    /// A blend may recreate (truncate) its group's store, which would fault
    /// (SIGBUS) any [`mmm_core::tiles::TileReader`] still mapping it. The
    /// runner does not check that: callers must not start a blend while a
    /// reader outside the job runner maps the store — the mosaic view gates
    /// it on its tracked sources, the export check and the merge settings
    /// tile count (see [`crate::mosaic_view`] and
    /// [`crate::blend_settings`]).
    pub fn start_blend(
        &mut self,
        session: &mut OpenSession,
        group: &str,
        rect: Option<[u64; 4]>,
    ) -> u64 {
        let id = self.new_id(job_label("Merge", group, session));
        self.begin(JobTarget::Blend, id, group);
        if rect.is_none() {
            self.whole.insert(id);
        }
        if let Some(r) = &self.running
            && r.blend.as_ref().is_some_and(|p| p.group == group)
        {
            r.token.cancel();
        }
        let mut dropped = Vec::new();
        self.queue.retain(|q| match &q.kind {
            Kind::Blend { group: g, .. } if g == group => {
                dropped.push(q.id);
                false
            }
            Kind::Blend { .. } | Kind::Analyze { .. } | Kind::Previews | Kind::Export(_) => true,
        });
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
        self.queue.push_back(Queued {
            id,
            kind: Kind::Blend {
                group: group.to_string(),
                rect,
                settings: session.mosaic.config.settings_for(group).1,
            },
        });
        self.start_next(session);
        id
    }

    /// Export `group`'s blended result to `out` as `format`; returns the
    /// job id. Queues behind every other job and cancels nothing. Stale
    /// tiles are exported as they are and missing ones as zeros (warn the
    /// user first: [`mmm_core::export::export_check`]). Planning does no
    /// I/O.
    pub fn start_export(
        &mut self,
        session: &OpenSession,
        group: &str,
        out: std::path::PathBuf,
        format: ExportFormat,
    ) -> u64 {
        let id = self.new_id(job_label("Export", group, session));
        self.begin(JobTarget::Export, id, group);
        self.queue.push_back(Queued {
            id,
            kind: Kind::Export(ExportJob {
                group: group.to_string(),
                out,
                format,
            }),
        });
        self.start_next(session);
        id
    }

    /// Cancel job `id`: a running job winds down (reported by
    /// [`JobRunner::poll`] when it has); a queued one is dropped at once.
    /// Unknown ids are ignored.
    pub fn cancel(&mut self, id: u64) {
        if let Some(r) = self.running.as_ref().filter(|r| r.id == id) {
            r.token.cancel();
        } else if let Some(i) = self.queue.iter().position(|q| q.id == id) {
            self.queue.remove(i);
            self.pending.push(JobEvent::Cancelled { id });
        }
    }

    /// Cancel every job but exports: the running one winds down (reported
    /// by [`JobRunner::poll`] when it has), queued ones are dropped at once.
    /// For input edits, which exports tolerate (they write stale tiles as
    /// they are).
    pub fn cancel_all_but_exports(&mut self) {
        if let Some(r) = self.running.as_ref().filter(|r| !r.export) {
            r.token.cancel();
        }
        let mut dropped = Vec::new();
        self.queue.retain(|q| {
            let keep = matches!(q.kind, Kind::Export(_));
            if !keep {
                dropped.push(q.id);
            }
            keep
        });
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
    }

    /// Cancel every blend: the running one winds down (reported by
    /// [`JobRunner::poll`] when it has), queued ones are dropped at once.
    /// For blend-local settings edits, which only need the stores' writer
    /// locks.
    pub fn cancel_blends(&mut self) {
        if let Some(r) = self.running.as_ref().filter(|r| r.blend.is_some()) {
            r.token.cancel();
        }
        let mut dropped = Vec::new();
        self.queue.retain(|q| {
            let keep = !matches!(q.kind, Kind::Blend { .. });
            if !keep {
                dropped.push(q.id);
            }
            keep
        });
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
    }

    /// Cancel every job, exports included, and wait up to `wait` for the
    /// running one to stop, applying (or discarding) its outcome as
    /// [`JobRunner::poll`] does (a cancelled analysis keeps the groups it
    /// finished). The events of the jobs that ended, and whether every job
    /// stopped in time (one that did not winds down on its own, and nothing
    /// applies its outcome). Blocks the calling (UI) thread: for leaving the
    /// session only.
    pub fn stop_all(&mut self, session: &mut OpenSession, wait: Duration) -> (Vec<JobEvent>, bool) {
        let deadline = Instant::now() + wait;
        if let Some(r) = &self.running {
            r.token.cancel();
        }
        let dropped: Vec<u64> = self.queue.drain(..).map(|q| q.id).collect();
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
        #[cfg(test)]
        if let Some(f) = self.on_stop.take() {
            f();
        }
        let mut events = self.poll(session);
        while self.busy() {
            if Instant::now() >= deadline {
                return (events, false);
            }
            std::thread::sleep(Duration::from_millis(5));
            events.extend(self.poll(session));
        }
        (events, true)
    }

    /// Let go of the workers, e.g. of a job that did not stop in
    /// [`JobRunner::stop_all`]'s wait: the running job (cancelled) and every
    /// queued one are forgotten, and nothing applies their outcomes. Their
    /// worker threads, and the last one that ended, are returned: once all
    /// have been joined, nothing this runner started runs any more (each
    /// worker joins its predecessor first).
    pub fn detach_workers(&mut self) -> Vec<JoinHandle<()>> {
        self.queue.clear();
        let running = self.running.take().map(|r| {
            r.token.cancel();
            r.handle
        });
        running.into_iter().chain(self.previous.take()).collect()
    }

    /// Whether a blend is running or queued.
    pub fn blending(&self) -> bool {
        self.running.as_ref().is_some_and(|r| r.blend.is_some())
            || self
                .queue
                .iter()
                .any(|q| matches!(q.kind, Kind::Blend { .. }))
    }

    /// Whether an analysis is running (resolving the groups or analysing
    /// them) or queued.
    pub fn analyzing(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.blend.is_none() && !r.previews && !r.export)
            || self
                .queue
                .iter()
                .any(|q| matches!(q.kind, Kind::Analyze { .. }))
    }

    /// Whether events raised outside [`JobRunner::poll`] (queued jobs
    /// cancelled) wait to be delivered by its next call.
    pub fn has_undelivered(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether a job other than an export is running or queued.
    pub fn busy_except_exports(&self) -> bool {
        self.running.as_ref().is_some_and(|r| !r.export)
            || self
                .queue
                .iter()
                .any(|q| !matches!(q.kind, Kind::Export(_)))
    }

    /// Take progress and outcomes from the workers, apply outcomes to
    /// `session` (UI thread), start the next queued job, and return what
    /// ended since the last call.
    pub fn poll(&mut self, session: &mut OpenSession) -> Vec<JobEvent> {
        let mut events = std::mem::take(&mut self.pending);
        while let Some(r) = &mut self.running {
            match r.rx.try_recv() {
                Ok(Msg::Progress(stage, done, total)) => {
                    r.stage = stage;
                    r.done = done;
                    r.total = total;
                    r.updates += 1;
                }
                Ok(Msg::Preview(p)) => events.push(JobEvent::Preview(p)),
                Ok(Msg::Done(done)) => {
                    let r = self.running.take().expect("running job");
                    self.ended(r.id);
                    events.extend(self.finish(r, *done, session));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The worker died without an outcome (a panic).
                    let r = self.running.take().expect("running job");
                    self.ended(r.id);
                    let mut message =
                        format!("{} stopped unexpectedly (see the log)", self.name(r.id));
                    if let Some(plan) = &r.blend
                        && let Err(e) = discard_blend(session, plan)
                    {
                        message = format!("{message}; {e}");
                    }
                    events.push(JobEvent::Failed { id: r.id, message });
                    self.previous = Some(r.handle);
                }
            }
        }
        self.start_next(session);
        events.append(&mut self.pending);
        for e in &events {
            match e {
                JobEvent::Finished { label, .. } => tracing::info!("{label} finished"),
                JobEvent::Cancelled { id } => tracing::info!("{} cancelled", self.name(*id)),
                JobEvent::Failed { message, .. } => tracing::warn!("{message}"),
                JobEvent::Preview(_) => {}
            }
        }
        events
    }

    /// The running job, then the queued ones, for the job panel.
    pub fn views(&self) -> Vec<JobView> {
        let running = self.running.iter().map(|r| {
            let cancelling = r.token.is_cancelled();
            JobView {
                id: r.id,
                label: self.name(r.id).to_string(),
                stage: if cancelling {
                    CANCELLING.into()
                } else {
                    r.stage.clone()
                },
                done: r.done,
                total: r.total,
                cancellable: !cancelling,
            }
        });
        let queued = self.queue.iter().map(|q| JobView {
            id: q.id,
            label: self.name(q.id).to_string(),
            stage: WAITING.into(),
            done: 0,
            total: 0,
            cancellable: true,
        });
        running.chain(queued).collect()
    }

    /// The running blend, if any: `(job id, group, progress updates
    /// received)`. The count changes whenever the blend reports progress
    /// (each update follows newly written tiles); once it is non-zero the
    /// blend has created or reopened its result store.
    pub fn blend_progress(&self) -> Option<(u64, &str, u64)> {
        let r = self.running.as_ref()?;
        let plan = r.blend.as_ref()?;
        Some((r.id, plan.group.as_str(), r.updates))
    }

    /// Whether a blend of `group` is queued, or running but has not
    /// reported progress yet: it may still create or recreate the group's
    /// result store, so the result must not be opened until it reports.
    pub fn blend_unstarted(&self, group: &str) -> bool {
        let running = self
            .running
            .as_ref()
            .is_some_and(|r| r.updates == 0 && r.blend.as_ref().is_some_and(|p| p.group == group));
        running
            || self
                .queue
                .iter()
                .any(|q| matches!(&q.kind, Kind::Blend { group: g, .. } if g == group))
    }

    /// Whether a blend of `group` is queued or running (whatever its
    /// progress): it may (re)create or write the group's result store.
    pub fn blend_pending(&self, group: &str) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.blend.as_ref().is_some_and(|p| p.group == group))
            || self
                .queue
                .iter()
                .any(|q| matches!(&q.kind, Kind::Blend { group: g, .. } if g == group))
    }

    /// The frame of the last previews run that finished and used every
    /// panel (set when its [`JobEvent::Finished`] is delivered), once.
    pub fn take_previews_frame(&mut self) -> Option<ReferenceFrame> {
        self.previews_frame.take()
    }

    /// Whether a job is running or queued.
    pub fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }

    /// What blend or export job `id` was for: its kind and group (also after
    /// it ended); `None` for any other job.
    pub fn target(&self, id: u64) -> Option<(JobTarget, &str)> {
        self.targets.get(&id).map(|(t, g)| (*t, g.as_str()))
    }

    /// Whether job `id` blends the whole canvas (not an ROI); also after it
    /// ended.
    pub fn whole_canvas(&self, id: u64) -> bool {
        self.whole.contains(&id)
    }

    /// Where the run of `target` jobs stands, while one is running or
    /// queued: its head job, and that job's place among the jobs started
    /// since none of them was left (a queued job dropped, e.g. replaced by
    /// newer work, does not count). The blends of a run are those of the
    /// whole canvas: an ROI's blend is in none.
    pub fn run_of(&self, target: JobTarget) -> Option<Run> {
        let live: Vec<u64> = self
            .running
            .iter()
            .map(|r| r.id)
            .chain(self.queue.iter().map(|q| q.id))
            .filter(|id| self.in_run(*id) == Some(target))
            .collect();
        let id = *live.first()?;
        let ended = match target {
            JobTarget::Blend => self.blends_ended,
            JobTarget::Export => self.exports_ended,
        };
        Some(Run {
            id,
            group: self.target(id)?.1.to_string(),
            position: ended + 1,
            count: ended + live.len(),
        })
    }

    /// The label of job `id` (also after it ended).
    pub fn label(&self, id: u64) -> Option<&str> {
        self.labels.get(&id).map(String::as_str)
    }

    /// Record a new `target` job `id` of `group`: a run of them starts
    /// afresh when none is left.
    fn begin(&mut self, target: JobTarget, id: u64, group: &str) {
        if self.run_of(target).is_none() {
            match target {
                JobTarget::Blend => self.blends_ended = 0,
                JobTarget::Export => self.exports_ended = 0,
            }
        }
        self.targets.insert(id, (target, group.to_string()));
    }

    /// The running job `id` has ended: count it in its run.
    fn ended(&mut self, id: u64) {
        match self.in_run(id) {
            Some(JobTarget::Blend) => self.blends_ended += 1,
            Some(JobTarget::Export) => self.exports_ended += 1,
            None => {}
        }
    }

    /// The run job `id` belongs to: a whole-canvas blend's or an export's.
    fn in_run(&self, id: u64) -> Option<JobTarget> {
        match self.target(id)?.0 {
            JobTarget::Blend if !self.whole_canvas(id) => None,
            t => Some(t),
        }
    }

    fn name(&self, id: u64) -> &str {
        self.label(id).unwrap_or("Job")
    }

    fn new_id(&mut self, label: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.labels.insert(id, label);
        id
    }

    /// Start queued jobs until one runs (or the queue is empty); a job that
    /// cannot be planned fails at once.
    fn start_next(&mut self, session: &OpenSession) {
        while self.running.is_none() {
            let Some(q) = self.queue.pop_front() else {
                return;
            };
            let planned = match &q.kind {
                Kind::Analyze { scope } => {
                    Ok(Planned::Resolve(Box::new(session.mosaic.clone()), *scope))
                }
                Kind::Blend {
                    group,
                    rect,
                    settings,
                } => plan_blend_with(&session.mosaic, group, *rect, settings.clone())
                    .map(Planned::Blend),
                Kind::Previews => Ok(Planned::Previews(plan_previews(&session.mosaic))),
                Kind::Export(job) => Ok(Planned::Export {
                    dir: session.mosaic.dir.clone(),
                    job: job.clone(),
                }),
            };
            match planned {
                Ok(planned) => self.spawn(q.id, planned),
                Err(e) => {
                    let message = format!("{}: {}", self.name(q.id), describe(&e));
                    self.pending.push(JobEvent::Failed { id: q.id, message });
                }
            }
        }
    }

    /// Run `planned` on its own thread as job `id`.
    fn spawn(&mut self, id: u64, planned: Planned) {
        let (tx, rx) = mpsc::channel();
        let token = CancelToken::new();
        let blend = match &planned {
            Planned::Blend(plan) => Some(plan.clone()),
            Planned::Analyze(_)
            | Planned::Resolve(..)
            | Planned::Previews(_)
            | Planned::Export { .. } => None,
        };
        let resolving = match &planned {
            Planned::Resolve(m, scope) => Some((GroupInputs::of(m), *scope)),
            Planned::Analyze(_)
            | Planned::Blend(_)
            | Planned::Previews(_)
            | Planned::Export { .. } => None,
        };
        let previews = matches!(planned, Planned::Previews(_));
        let export = matches!(planned, Planned::Export { .. });
        let stage = if resolving.is_some() {
            RESOLVING
        } else {
            "Starting…"
        };
        let worker_token = token.clone();
        let ctx = self.ctx.clone();
        let previous = self.previous.take();
        #[cfg(test)]
        let gate = self.gate.clone();
        #[cfg(test)]
        let stepper = self.stepper.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("mmm-job-{id}"))
            .spawn(move || {
                if let Some(p) = previous {
                    let _ = p.join();
                }
                #[cfg(test)]
                if let Some(g) = gate {
                    g.wait();
                }
                let done = {
                    let throttle = Mutex::new(Throttle::new(PROGRESS_INTERVAL));
                    let progress_tx = tx.clone();
                    let progress_ctx = ctx.clone();
                    let observer = move |stage: &str, done: u64, total: u64| {
                        let send = throttle
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .ready(Instant::now(), done >= total);
                        if send {
                            let _ = progress_tx.send(Msg::Progress(stage.to_string(), done, total));
                            progress_ctx.request_repaint();
                            #[cfg(test)]
                            if let Some(s) = &stepper {
                                s.wait();
                            }
                        }
                    };
                    let ctl = JobCtl::new(worker_token, Some(&observer));
                    match &planned {
                        Planned::Resolve(m, _) => Done::Resolved(m.groups()),
                        Planned::Analyze(plan) => {
                            let (outcome, result) = run_analyze(plan, &ctl);
                            let frame = ReferenceFrame::load(&plan.frame_path).ok();
                            Done::Analyze {
                                outcome,
                                result,
                                frame,
                            }
                        }
                        Planned::Blend(plan) => Done::Blend(run_blend(plan, &ctl)),
                        Planned::Previews(plan) => {
                            let preview_tx = tx.clone();
                            let preview_ctx = ctx.clone();
                            let on_ready = move |p: Preview| {
                                let _ = preview_tx.send(Msg::Preview(Arc::new(p)));
                                preview_ctx.request_repaint();
                            };
                            Done::Previews(run_previews(plan, &ctl, &on_ready))
                        }
                        Planned::Export { dir, job } => {
                            Done::Export(export_group(dir, &job.group, &job.out, job.format, &ctl))
                        }
                    }
                };
                // Everything the job held (store locks included) is
                // released before the outcome is sent.
                let _ = tx.send(Msg::Done(Box::new(done)));
                ctx.request_repaint();
            });
        match spawned {
            Ok(handle) => {
                self.running = Some(Running {
                    id,
                    token,
                    rx,
                    handle,
                    blend,
                    resolving,
                    previews,
                    export,
                    stage: stage.into(),
                    done: 0,
                    total: 0,
                    updates: 0,
                });
            }
            Err(e) => self.pending.push(JobEvent::Failed {
                id,
                message: format!("{}: cannot start a worker thread: {e}", self.name(id)),
            }),
        }
    }

    /// Apply (or discard) a finished worker's outcome on the UI thread; an
    /// analysis's resolved groups start its next phase (no event yet).
    fn finish(&mut self, r: Running, done: Done, session: &mut OpenSession) -> Option<JobEvent> {
        self.previous = Some(r.handle);
        let id = r.id;
        let name = self.name(id).to_string();
        let failed = |message: String| JobEvent::Failed { id, message };
        if let Done::Resolved(resolved) = done {
            let groups = match resolved {
                Ok(g) => g,
                Err(e) => return Some(failed(format!("{name}: {}", describe(&e)))),
            };
            let current = GroupInputs::of(&session.mosaic);
            let (fresh, scope) = match &r.resolving {
                Some((inputs, scope)) => (*inputs == current, *scope),
                None => (false, AnalyzeScope::All),
            };
            if fresh {
                session.groups = Some(groups.clone());
            }
            if r.token.is_cancelled() {
                return Some(JobEvent::Cancelled { id });
            }
            // Panels or grouping changed meanwhile: resolve again.
            let planned = if fresh {
                match plan_analyze_scoped(&mut session.mosaic, groups, scope) {
                    Ok(plan) => Planned::Analyze(plan),
                    Err(e) => return Some(failed(format!("{name}: {}", describe(&e)))),
                }
            } else {
                Planned::Resolve(Box::new(session.mosaic.clone()), scope)
            };
            self.spawn(id, planned);
            return None;
        }
        Some(match (done, &r.blend) {
            (Done::Resolved(_), _) => unreachable!("handled above"),
            (Done::Previews(Ok(run)), _) => {
                if !run.failed.is_empty() {
                    tracing::warn!("{name}: {} panel(s) have no preview", run.failed.len());
                }
                // Only a frame of every panel says where the panels are.
                self.previews_frame = run.complete.then_some(run.frame);
                JobEvent::Finished { id, label: name }
            }
            (Done::Previews(Err(e)), _) if e.is_cancelled() => JobEvent::Cancelled { id },
            (Done::Previews(Err(e)), _) => failed(format!("{name}: {}", describe(&e))),
            (Done::Export(Ok(report)), _) => {
                if let Some(w) = report.warning() {
                    tracing::warn!("{name}: {w}");
                }
                JobEvent::Finished { id, label: name }
            }
            (Done::Export(Err(e)), _) if e.is_cancelled() => JobEvent::Cancelled { id },
            (Done::Export(Err(e)), _) => failed(format!("{name}: {}", describe(&e))),
            (
                Done::Analyze {
                    outcome,
                    result,
                    frame,
                },
                _,
            ) => {
                if frame.is_some() {
                    session.frame = frame;
                }
                // Applied even when cancelled: the finished groups stay.
                let applied = session.mosaic.apply_analyze(&outcome);
                if let (Err(e), Ok(())) = (&result, &applied)
                    && e.is_frame_moved()
                    && !r.token.is_cancelled()
                {
                    // The groups left out were analysed on another canvas:
                    // mark them all and analyse everything, with no failure
                    // to report.
                    let analysed: Vec<String> =
                        session.mosaic.config.group_dirs.keys().cloned().collect();
                    return match mark_needs_analyze(&mut session.mosaic, &analysed) {
                        Ok(()) => {
                            let m = Box::new(session.mosaic.clone());
                            self.spawn(id, Planned::Resolve(m, AnalyzeScope::All));
                            None
                        }
                        Err(e) => Some(failed(format!("{name}: {}", describe(&e)))),
                    };
                }
                match (result, applied) {
                    (_, Err(e)) => failed(format!("{name}: {}", describe(&e))),
                    (Ok(()), Ok(())) => JobEvent::Finished { id, label: name },
                    (Err(e), Ok(())) if e.is_cancelled() => JobEvent::Cancelled { id },
                    (Err(e), Ok(())) => failed(format!("{name}: {}", describe(&e))),
                }
            }
            (Done::Blend(Ok(rect)), Some(plan)) => match session.mosaic.apply_blend(plan, rect) {
                Ok(()) => JobEvent::Finished { id, label: name },
                Err(e) => failed(format!("{name}: {}", describe(&e))),
            },
            (Done::Blend(Err(e)), Some(plan)) => {
                let discarded = discard_blend(session, plan);
                match (e.is_cancelled(), discarded) {
                    (true, Ok(())) => JobEvent::Cancelled { id },
                    (true, Err(d)) => failed(format!("{name} cancelled; {d}")),
                    (false, Ok(())) => failed(format!("{name}: {}", describe(&e))),
                    (false, Err(d)) => failed(format!("{name}: {}; {d}", describe(&e))),
                }
            }
            (Done::Blend(_), None) => unreachable!("a blend outcome always has its plan"),
        })
    }
}

impl Drop for JobRunner {
    /// Cancel the running job: nothing would apply its outcome. The worker
    /// winds down on its own (never joined here, so dropping never waits).
    fn drop(&mut self) {
        if let Some(r) = &self.running {
            r.token.cancel();
        }
    }
}

/// Discard a blend that will never be applied (cancelled or failed). Its
/// tiles were blended with the settings it carried, which a later commit
/// does not make wrong, so they stay; only if `group` now needs analysis is
/// the store invalidated. Owner (UI) thread; the error is a message for the
/// user.
pub fn discard_blend(session: &OpenSession, plan: &BlendPlan) -> Result<(), String> {
    let config = &session.mosaic.config;
    if !config.needs_analyze.contains(&plan.group) {
        return Ok(());
    }
    invalidate_group(&session.mosaic, &plan.group).map_err(|e| {
        format!(
            "could not mark the tiles of group '{}' stale: {}",
            plan.group,
            describe(&e)
        )
    })
}

/// A merge's or an export's label (and so its toasts' title): "{verb}
/// {group}", or `verb` alone in a session of one group (an OSC mosaic:
/// the group name says nothing).
fn job_label(verb: &str, group: &str, session: &OpenSession) -> String {
    if crate::util::group_names(session).len() <= 1 {
        verb.to_string()
    } else {
        format!("{verb} {group}")
    }
}

/// A core error as a message for the user.
fn describe(e: &mmm_core::Error) -> String {
    if e.is_busy() {
        format!("{e} — the result is in use by another job or program; try again when it ends")
    } else {
        e.to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::{Duration, Instant};

    use mmm_core::mosaic::{MosaicConfig, run_blend};
    use mmm_core::tiles::TileStore;

    use super::*;
    use crate::session::OpenSession;
    use crate::start::tests::temp_dir;
    use crate::testutil::{CANVAS, two_group_session};

    /// Holds workers back (before they run) until opened.
    pub(crate) struct Gate {
        open: std::sync::Mutex<bool>,
        cv: std::sync::Condvar,
    }

    impl Gate {
        /// Let every held worker run.
        pub(crate) fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }

        /// Block until opened (bounded: never hang a test).
        pub(crate) fn wait(&self) {
            let open = self.open.lock().unwrap();
            let (open, timeout) = self
                .cv
                .wait_timeout_while(open, crate::testutil::PATIENCE, |o| !*o)
                .unwrap();
            assert!(*open && !timeout.timed_out(), "gate never opened");
        }
    }

    /// Holds a worker after each progress update it sends, until stepped
    /// past it or opened.
    pub(crate) struct Stepper {
        /// Steps granted and not taken yet, and whether it is open.
        state: std::sync::Mutex<(u64, bool)>,
        cv: std::sync::Condvar,
    }

    impl Stepper {
        /// Let the held worker go on to its next progress update.
        pub(crate) fn step(&self) {
            self.state.lock().unwrap().0 += 1;
            self.cv.notify_all();
        }

        /// Hold nothing any more.
        pub(crate) fn open(&self) {
            self.state.lock().unwrap().1 = true;
            self.cv.notify_all();
        }

        /// Block until stepped or opened (bounded: never hang a test).
        pub(crate) fn wait(&self) {
            let state = self.state.lock().unwrap();
            let (mut state, timeout) = self
                .cv
                .wait_timeout_while(state, crate::testutil::PATIENCE, |(steps, open)| {
                    *steps == 0 && !*open
                })
                .unwrap();
            assert!(!timeout.timed_out(), "stepper never stepped");
            if !state.1 {
                state.0 -= 1;
            }
        }
    }

    impl JobRunner {
        /// Hold every worker started from now on until the gate opens.
        pub(crate) fn hold(&mut self) -> std::sync::Arc<Gate> {
            let gate = std::sync::Arc::new(Gate {
                open: std::sync::Mutex::new(false),
                cv: std::sync::Condvar::new(),
            });
            self.gate = Some(gate.clone());
            gate
        }

        /// Hold every worker started from now on after each progress
        /// update it sends (see [`Stepper`]).
        pub(crate) fn hold_progress(&mut self) -> std::sync::Arc<Stepper> {
            let stepper = std::sync::Arc::new(Stepper {
                state: std::sync::Mutex::new((0, false)),
                cv: std::sync::Condvar::new(),
            });
            self.stepper = Some(stepper.clone());
            stepper
        }

        /// Call `f` once [`JobRunner::stop_all`] has cancelled the jobs,
        /// before it waits for them (e.g. to let a held worker go).
        pub(crate) fn on_stop(&mut self, f: impl FnOnce() + Send + 'static) {
            self.on_stop = Some(Box::new(f));
        }
    }

    /// Poll until no job is running or queued (bounded); every event.
    pub(crate) fn drain(runner: &mut JobRunner, s: &mut OpenSession) -> Vec<JobEvent> {
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        let mut events = runner.poll(s);
        while runner.busy() {
            assert!(Instant::now() < deadline, "timed out waiting for jobs");
            std::thread::sleep(Duration::from_millis(5));
            events.extend(runner.poll(s));
        }
        events
    }

    fn runner() -> JobRunner {
        JobRunner::new(egui::Context::default())
    }

    /// An analysed two-group session (`Ha`, `OIII`) and its idle runner.
    pub(crate) fn analyzed(tag: &str) -> (std::path::PathBuf, OpenSession, JobRunner) {
        let root = temp_dir(tag);
        let mut s = two_group_session(&root);
        let mut r = runner();
        let id = r.start_analyze(&mut s);
        let events = drain(&mut r, &mut s);
        assert_eq!(
            events,
            vec![JobEvent::Finished {
                id,
                label: ANALYZE_LABEL.into()
            }]
        );
        (root, s, r)
    }

    fn valid0(path: &std::path::Path) -> usize {
        TileStore::open(path).unwrap().valid_count(0)
    }

    fn actions(s: &OpenSession) -> Vec<String> {
        s.mosaic
            .config
            .provenance
            .iter()
            .map(|p| p.action.clone())
            .collect()
    }

    #[test]
    fn analyzing_follows_running_and_queued_analyses() {
        let root = temp_dir("job-analyzing");
        let mut s = two_group_session(&root);
        let mut r = runner();
        assert!(!r.analyzing());
        let gate = r.hold();
        r.start_previews(&s);
        assert!(!r.analyzing(), "previews are not an analysis");
        r.start_analyze(&mut s);
        assert!(r.analyzing(), "queued behind the previews");
        gate.open();
        let _ = drain(&mut r, &mut s);
        assert!(!r.analyzing());
        let gate = r.hold();
        r.start_analyze(&mut s);
        assert!(r.analyzing(), "running");
        gate.open();
        let _ = drain(&mut r, &mut s);
        let gate = r.hold();
        r.start_blend(&mut s, "Ha", None);
        assert!(!r.analyzing(), "a blend is not an analysis");
        gate.open();
        let _ = drain(&mut r, &mut s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stop_all_cancels_every_job_and_waits_for_it() {
        let (root, mut s, mut r) = analyzed("job-stop-all");
        let gate = r.hold();
        let ha = r.start_blend(&mut s, "Ha", None);
        let export = r.start_export(&s, "OIII", root.join("o.fits"), ExportFormat::Fits);
        // The running merge gets going while `stop_all` waits for it.
        let opener = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            gate.open();
        });
        let (events, stopped) = r.stop_all(&mut s, crate::testutil::PATIENCE);
        opener.join().unwrap();
        assert!(stopped);
        assert!(!r.busy());
        for id in [ha, export] {
            assert!(events.contains(&JobEvent::Cancelled { id }), "{events:?}");
        }
        // A job that ignores the wait is let go of; its worker is handed
        // over, to be joined once it has stopped.
        let gate = r.hold();
        r.start_blend(&mut s, "OIII", None);
        let (_, stopped) = r.stop_all(&mut s, Duration::from_millis(20));
        assert!(!stopped);
        let workers = r.detach_workers();
        assert!(!workers.is_empty());
        assert!(!r.busy(), "nothing left to the runner");
        gate.open();
        for w in workers {
            w.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blend_and_export_jobs_know_their_group() {
        let (root, mut s, mut r) = analyzed("job-target");
        let blend = r.start_blend(&mut s, "Ha", None);
        let out = root.join("o.fits");
        let export = r.start_export(&s, "OIII", out, ExportFormat::Fits);
        assert_eq!(r.target(blend), Some((JobTarget::Blend, "Ha")));
        assert_eq!(r.target(export), Some((JobTarget::Export, "OIII")));
        assert_eq!(r.target(9999), None);
        let _ = drain(&mut r, &mut s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn runs_count_their_jobs() {
        let (root, mut s, mut r) = analyzed("job-runs");
        assert_eq!(r.run_of(JobTarget::Blend), None);
        let gate = r.hold();
        let ha = r.start_blend(&mut s, "Ha", None);
        let oiii = r.start_blend(&mut s, "OIII", None);
        let run = |id, group: &str, position, count| Run {
            id,
            group: group.into(),
            position,
            count,
        };
        assert_eq!(r.run_of(JobTarget::Blend), Some(run(ha, "Ha", 1, 2)));
        assert_eq!(r.run_of(JobTarget::Export), None);
        gate.open();
        // Ha ended: OIII, the second of two, runs.
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while r.run_of(JobTarget::Blend).is_some_and(|x| x.id == ha) {
            assert!(Instant::now() < deadline, "Ha never ended");
            std::thread::sleep(Duration::from_millis(5));
            let _ = r.poll(&mut s);
        }
        assert_eq!(r.run_of(JobTarget::Blend), Some(run(oiii, "OIII", 2, 2)));
        let _ = drain(&mut r, &mut s);
        assert_eq!(r.run_of(JobTarget::Blend), None);
        // The next run starts afresh. An ROI's merge is in none: alone it
        // makes no run, and its end leaves the run's count as it was.
        let gate = r.hold();
        let roi = r.start_blend(&mut s, "Ha", Some([0, 0, 64, 64]));
        assert!(!r.whole_canvas(roi) && r.whole_canvas(oiii));
        assert_eq!(r.run_of(JobTarget::Blend), None);
        let again = r.start_blend(&mut s, "OIII", None);
        assert_eq!(r.run_of(JobTarget::Blend), Some(run(again, "OIII", 1, 1)));
        gate.open();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while r.views().first().is_some_and(|v| v.id == roi) {
            assert!(Instant::now() < deadline, "the ROI's merge never ended");
            std::thread::sleep(Duration::from_millis(5));
            let _ = r.poll(&mut s);
        }
        assert_eq!(r.run_of(JobTarget::Blend), Some(run(again, "OIII", 1, 1)));
        let _ = drain(&mut r, &mut s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blend_job_runs_and_applies() {
        let (root, mut s, mut r) = analyzed("job-blend");
        assert_eq!(s.canvas(), Some(CANVAS), "canvas from the reference frame");
        assert!(s.mosaic.config.needs_analyze.is_empty());
        let id = r.start_blend(&mut s, "Ha", None);
        assert!(r.busy());
        assert_eq!(r.views().len(), 1);
        assert_eq!(r.views()[0].label, "Merge Ha");
        let events = drain(&mut r, &mut s);
        assert_eq!(
            events,
            vec![JobEvent::Finished {
                id,
                label: "Merge Ha".into()
            }]
        );
        assert!(!r.busy());
        assert!(r.views().is_empty());
        assert_eq!(actions(&s), ["analyze", "blend"]);
        // Applied and saved on the UI thread.
        assert_eq!(MosaicConfig::load(&s.mosaic.dir).unwrap(), s.mosaic.config);
        assert!(valid0(&s.mosaic.result_path("Ha")) > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_reports_cancelled_and_session_consistent() {
        let (root, mut s, mut r) = analyzed("job-cancel");
        let gate = r.hold();
        let id = r.start_blend(&mut s, "Ha", None);
        r.cancel(id);
        assert_eq!(r.views()[0].stage, "Cancelling…");
        assert!(!r.views()[0].cancellable);
        gate.open();
        assert_eq!(drain(&mut r, &mut s), vec![JobEvent::Cancelled { id }]);
        assert_eq!(r.label(id), Some("Merge Ha"));
        assert_eq!(actions(&s), ["analyze"]);
        assert_eq!(MosaicConfig::load(&s.mosaic.dir).unwrap(), s.mosaic.config);

        // Cancelled while resolving groups: nothing was planned or changed.
        let config = s.mosaic.config.clone();
        let gate = r.hold();
        let id = r.start_analyze(&mut s);
        r.cancel(id);
        gate.open();
        assert_eq!(drain(&mut r, &mut s), vec![JobEvent::Cancelled { id }]);
        assert_eq!(s.mosaic.config, config);

        // Cancelled once analysing: every unfinished group stays refused.
        let resolve = r.hold();
        let id = r.start_analyze(&mut s);
        let analyze = r.hold();
        resolve.open();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while r.views()[0].stage == "Resolving groups…" {
            assert!(Instant::now() < deadline, "resolution never finished");
            std::thread::sleep(Duration::from_millis(5));
            assert!(r.poll(&mut s).is_empty());
        }
        assert_eq!(s.mosaic.config.needs_analyze.len(), 2, "planned");
        r.cancel(id);
        analyze.open();
        assert_eq!(drain(&mut r, &mut s), vec![JobEvent::Cancelled { id }]);
        assert_eq!(s.mosaic.config.needs_analyze.len(), 2);
        assert_eq!(MosaicConfig::load(&s.mosaic.dir).unwrap(), s.mosaic.config);
        let id = r.start_blend(&mut s, "Ha", None);
        match &drain(&mut r, &mut s)[..] {
            [JobEvent::Failed { id: f, message }] => {
                assert_eq!(*f, id);
                assert!(message.contains("analyze first"), "{message}");
            }
            other => panic!("{other:?}"),
        }

        // The store is free again: a new analysis and blend go through.
        r.start_analyze(&mut s);
        drain(&mut r, &mut s);
        let id = r.start_blend(&mut s, "Ha", None);
        assert_eq!(
            drain(&mut r, &mut s),
            vec![JobEvent::Finished {
                id,
                label: "Merge Ha".into()
            }]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn newer_blend_cancels_older_of_same_group() {
        let (root, mut s, mut r) = analyzed("job-newer");
        let gate = r.hold();
        let old = r.start_blend(&mut s, "Ha", None);
        let other = r.start_blend(&mut s, "OIII", None);
        let superseded = r.start_blend(&mut s, "Ha", Some([0, 0, 512, 512]));
        let new = r.start_blend(&mut s, "Ha", Some([0, 0, 256, 256]));
        // The old Ha job is cancelling and the queued Ha one was replaced at
        // once; the OIII blend still waits its turn.
        let views = r.views();
        assert_eq!(views.len(), 3);
        assert_eq!((views[0].id, views[0].stage.as_str()), (old, "Cancelling…"));
        assert_eq!((views[1].id, views[1].stage.as_str()), (other, "Waiting…"));
        assert_eq!((views[2].id, views[2].stage.as_str()), (new, "Waiting…"));
        gate.open();
        let events = drain(&mut r, &mut s);
        assert_eq!(
            events,
            vec![
                JobEvent::Cancelled { id: superseded },
                JobEvent::Cancelled { id: old },
                JobEvent::Finished {
                    id: other,
                    label: "Merge OIII".into()
                },
                JobEvent::Finished {
                    id: new,
                    label: "Merge Ha".into()
                },
            ]
        );
        assert_eq!(actions(&s), ["analyze", "blend", "blend"]);
        let last = s.mosaic.config.provenance.last().unwrap();
        assert_eq!(last.detail["rect"], serde_json::json!([0, 0, 256, 256]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blends_of_different_groups_both_finish() {
        let (root, mut s, mut r) = analyzed("job-two-groups");
        let gate = r.hold();
        let ha = r.start_blend(&mut s, "Ha", None);
        let oiii = r.start_blend(&mut s, "OIII", None);
        gate.open();
        assert_eq!(
            drain(&mut r, &mut s),
            vec![
                JobEvent::Finished {
                    id: ha,
                    label: "Merge Ha".into()
                },
                JobEvent::Finished {
                    id: oiii,
                    label: "Merge OIII".into()
                },
            ]
        );
        assert!(valid0(&s.mosaic.result_path("Ha")) > 0);
        assert!(valid0(&s.mosaic.result_path("OIII")) > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blend_twice_cancels_the_first() {
        let (root, mut s, mut r) = analyzed("job-twice");
        let gate = r.hold();
        let first = r.start_blend(&mut s, "Ha", None);
        let second = r.start_blend(&mut s, "Ha", None);
        gate.open();
        assert_eq!(
            drain(&mut r, &mut s),
            vec![
                JobEvent::Cancelled { id: first },
                JobEvent::Finished {
                    id: second,
                    label: "Merge Ha".into()
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blend_unstarted_until_first_progress() {
        let (root, mut s, mut r) = analyzed("job-unstarted");
        assert!(!r.blend_unstarted("Ha"));
        let gate = r.hold();
        r.start_blend(&mut s, "Ha", None);
        r.start_blend(&mut s, "OIII", None);
        // Running without progress, and queued.
        assert!(r.blend_unstarted("Ha"));
        assert!(r.blend_unstarted("OIII"));
        assert!(!r.blend_unstarted("SII"));
        gate.open();
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while r
            .blend_progress()
            .is_none_or(|(_, g, n)| g != "Ha" || n == 0)
        {
            assert!(Instant::now() < deadline, "no Ha progress");
            std::thread::sleep(Duration::from_millis(1));
            r.poll(&mut s);
            if !r.busy() {
                break;
            }
        }
        // Ha has reported progress (or ended): its store exists.
        assert!(!r.blend_unstarted("Ha"));
        drain(&mut r, &mut s);
        assert!(!r.blend_unstarted("OIII"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn analysis_cancels_running_blend() {
        let (root, mut s, mut r) = analyzed("job-analyze-wins");
        let gate = r.hold();
        let blend = r.start_blend(&mut s, "Ha", None);
        let analyze = r.start_analyze(&mut s);
        gate.open();
        assert_eq!(
            drain(&mut r, &mut s),
            vec![
                JobEvent::Cancelled { id: blend },
                JobEvent::Finished {
                    id: analyze,
                    label: ANALYZE_LABEL.into()
                },
            ]
        );
        assert_eq!(actions(&s), ["analyze", "analyze"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blend_waits_for_running_analysis() {
        let root = temp_dir("job-blend-waits");
        let mut s = two_group_session(&root);
        let mut r = runner();
        let gate = r.hold();
        let analyze = r.start_analyze(&mut s);
        let blend = r.start_blend(&mut s, "OIII", None);
        assert_eq!(r.views()[1].stage, "Waiting…");
        gate.open();
        assert_eq!(
            drain(&mut r, &mut s),
            vec![
                JobEvent::Finished {
                    id: analyze,
                    label: ANALYZE_LABEL.into()
                },
                JobEvent::Finished {
                    id: blend,
                    label: "Merge OIII".into()
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discarded_blend_is_invalidated_only_when_analysis_is_stale() {
        let (root, mut s, _r) = analyzed("job-discard");
        let plan = mmm_core::mosaic::plan_blend(&s.mosaic, "Ha", None).unwrap();
        // Unchanged settings: the partial tiles of an unapplied blend stay.
        run_blend(&plan, &mmm_core::job::JobCtl::none()).unwrap();
        discard_blend(&s, &plan).unwrap();
        assert!(valid0(&plan.result_path) > 0);
        // A committed settings edit does not make them wrong: they stay.
        let new = BlendSettings {
            feather_px: s.mosaic.config.blend.feather_px + 10.0,
            ..s.mosaic.config.blend.clone()
        };
        mmm_core::mosaic::commit_blend_settings(&mut s.mosaic, new).unwrap();
        discard_blend(&s, &plan).unwrap();
        assert!(valid0(&plan.result_path) > 0);
        // A group that needs analysis invalidates them.
        mmm_core::mosaic::mark_needs_analyze(&mut s.mosaic, &["Ha".to_string()]).unwrap();
        discard_blend(&s, &plan).unwrap();
        assert_eq!(valid0(&plan.result_path), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_queued_merge_keeps_the_settings_it_was_queued_with() {
        let (root, mut s, mut r) = analyzed("job-queued-settings");
        let old = s.mosaic.config.blend.clone();
        let gate = r.hold();
        r.start_previews(&s);
        let id = r.start_blend(&mut s, "Ha", None);
        let new = BlendSettings {
            feather_px: old.feather_px + 10.0,
            ..old.clone()
        };
        mmm_core::mosaic::commit_blend_settings(&mut s.mosaic, new.clone()).unwrap();
        gate.open();
        let events = drain(&mut r, &mut s);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, JobEvent::Finished { id: i, .. } if *i == id)),
            "{events:?}"
        );
        assert_eq!(
            mmm_core::mosaic::last_merge_settings(&s.mosaic.config, "Ha"),
            Some(old)
        );
        assert!(mmm_core::mosaic::settings_changed(
            &s.mosaic.config,
            "Ha",
            &new
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stopped_fresh_merge_leaves_the_group_out_of_date() {
        use mmm_core::session_summary::{ResultState, result_state};
        let (root, mut s, mut r) = analyzed("job-stopped-fresh");
        r.start_blend(&mut s, "Ha", None);
        drain(&mut r, &mut s);
        let store = s.mosaic.result_path("Ha");
        assert_eq!(result_state(&store), ResultState::Complete);
        let new = BlendSettings {
            feather_px: s.mosaic.config.blend.feather_px + 10.0,
            ..s.mosaic.config.blend.clone()
        };
        mmm_core::mosaic::commit_blend_settings(&mut s.mosaic, new).unwrap();
        let gate = r.hold();
        let id = r.start_blend(&mut s, "Ha", None);
        r.cancel(id);
        gate.open();
        let events = drain(&mut r, &mut s);
        assert_eq!(events, vec![JobEvent::Cancelled { id }]);
        assert_ne!(result_state(&store), ResultState::Complete);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_unknown_or_queued_job() {
        let (root, mut s, mut r) = analyzed("job-cancel-queued");
        let gate = r.hold();
        let first = r.start_blend(&mut s, "Ha", None);
        let analyze = r.start_analyze(&mut s);
        r.cancel(analyze);
        r.cancel(9999);
        gate.open();
        assert_eq!(
            drain(&mut r, &mut s),
            vec![
                JobEvent::Cancelled { id: analyze },
                JobEvent::Cancelled { id: first },
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn analysis_resolves_groups_on_a_worker() {
        let root = temp_dir("job-resolve");
        let mut s = two_group_session(&root);
        let before = std::fs::read(s.mosaic.dir.join("mosaic.json")).unwrap();
        let mut r = runner();
        let gate = r.hold();
        let id = r.start_analyze(&mut s);
        assert_eq!(r.views()[0].stage, "Resolving groups…");
        // Nothing planned (or saved) on the UI thread before resolution.
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_eq!(
            std::fs::read(s.mosaic.dir.join("mosaic.json")).unwrap(),
            before
        );
        // The panels' grouping changes while the worker resolves...
        for p in s.mosaic.config.panels.iter_mut().take(4) {
            p.manual_group = Some("Red".into());
        }
        gate.open();
        assert_eq!(
            drain(&mut r, &mut s),
            vec![JobEvent::Finished {
                id,
                label: ANALYZE_LABEL.into()
            }]
        );
        // ...so it resolved again, and analysed the current groups.
        let names: Vec<&String> = s.mosaic.config.group_dirs.keys().collect();
        assert_eq!(names, ["OIII", "Red"]);
        let cached = s.groups.as_ref().unwrap();
        assert_eq!(cached.groups.keys().collect::<Vec<_>>(), ["OIII", "Red"]);
        assert_eq!(cached.processing_groups().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The events other than previews, and the number of previews.
    fn split_previews(events: Vec<JobEvent>) -> (Vec<JobEvent>, usize) {
        let (previews, ended): (Vec<_>, Vec<_>) = events
            .into_iter()
            .partition(|e| matches!(e, JobEvent::Preview(_)));
        (ended, previews.len())
    }

    #[test]
    fn previews_stream_and_analysis_waits_for_them() {
        let root = temp_dir("job-previews");
        let mut s = two_group_session(&root);
        let mut r = runner();
        let gate = r.hold();
        let first = r.start_previews(&s);
        // Newer previews replace running ones...
        let previews = r.start_previews(&s);
        // ...but an analysis waits for them rather than cancel them.
        let analyze = r.start_analyze(&mut s);
        let views = r.views();
        assert_eq!(views.len(), 3);
        assert_eq!(
            (views[0].id, views[0].stage.as_str()),
            (first, "Cancelling…")
        );
        assert_eq!(
            (views[1].id, views[1].label.as_str()),
            (previews, "Previews")
        );
        assert_eq!(
            (views[2].id, views[2].stage.as_str()),
            (analyze, "Waiting…")
        );
        gate.open();
        let (ended, n) = split_previews(drain(&mut r, &mut s));
        assert_eq!(
            ended,
            vec![
                JobEvent::Cancelled { id: first },
                JobEvent::Finished {
                    id: previews,
                    label: "Previews".into()
                },
                JobEvent::Finished {
                    id: analyze,
                    label: ANALYZE_LABEL.into()
                },
            ]
        );
        assert!(n >= 8, "every panel's preview delivered ({n})");
        assert!(s.mosaic.config.needs_analyze.is_empty());
        // Cached now: a second run loads them all again.
        let again = r.start_previews(&s);
        let (ended, n) = split_previews(drain(&mut r, &mut s));
        assert_eq!(
            ended,
            vec![JobEvent::Finished {
                id: again,
                label: "Previews".into()
            }]
        );
        assert_eq!(n, 8);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn progress_is_throttled_but_final_always_passes() {
        let t0 = Instant::now();
        let mut t = Throttle::new(PROGRESS_INTERVAL);
        let sent = (0..1000u64)
            .filter(|i| t.ready(t0 + Duration::from_micros(i * 100), false))
            .count();
        // 100 ms of updates at ≤ 30/s.
        assert!((1..=4).contains(&sent), "{sent}");
        assert!(t.ready(t0 + Duration::from_millis(100), true));
        assert!(t.ready(t0 + Duration::from_millis(140), false));
    }

    #[test]
    fn a_moved_frame_restarts_the_analysis_for_every_group() {
        use mmm_core::session_summary::{ResultState, result_state};
        let root = temp_dir("job-frame-moved");
        let (mut s, extra) = crate::testutil::solved_two_group_session(&root);
        let mut r = runner();
        r.start_analyze(&mut s);
        drain(&mut r, &mut s);
        r.start_blend(&mut s, "OIII", None);
        drain(&mut r, &mut s);
        let frame = s.frame.clone().expect("analysed frame");
        let oiii = s.mosaic.result_path("OIII");
        assert_eq!(result_state(&oiii), ResultState::Complete);

        // An edit that claims to touch only Ha, though its panel grows the
        // canvas: the targeted analysis finds the frame moved.
        let mut panels = s.mosaic.config.panels.clone();
        panels.push(PanelEntry {
            path: extra,
            fingerprint: None,
            manual_group: Some("Ha".into()),
        });
        let touched = std::collections::BTreeSet::from(["Ha".to_string()]);
        let grouping = s.mosaic.config.grouping.clone();
        mmm_core::mosaic::set_inputs_affecting(&mut s.mosaic, panels, grouping, Some(&touched))
            .unwrap();
        assert_eq!(result_state(&oiii), ResultState::Complete, "left alone");
        let id = r.start_analyze_scoped(&mut s, AnalyzeScope::Needed);
        let events = drain(&mut r, &mut s);
        assert_eq!(
            events,
            vec![JobEvent::Finished {
                id,
                label: ANALYZE_LABEL.into()
            }],
            "restarted, no failure"
        );
        assert!(s.mosaic.config.needs_analyze.is_empty());
        assert_ne!(s.frame.as_ref(), Some(&frame), "the new canvas");
        assert_ne!(result_state(&oiii), ResultState::Complete, "re-analysed");
        let _ = std::fs::remove_dir_all(&root);
    }
}
