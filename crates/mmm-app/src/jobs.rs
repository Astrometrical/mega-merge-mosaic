//! Background jobs: analysis and blending on worker threads, with progress,
//! cancellation and "newer work wins" (spec §8).
//!
//! Each job follows the core's concurrency contract
//! ([`mmm_core::mosaic`]): it is *planned* on the UI thread when it starts,
//! *run* on its own `std::thread` with a [`JobCtl`], and its outcome is
//! *applied* on the UI thread by [`JobRunner::poll`]. An analysis first
//! resolves the groups (panel header reads) on a worker over a copy of the
//! mosaic, then plans with [`plan_analyze_with`], so the UI thread never
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
//!
//! A queued job starts once `poll` has applied (or discarded) its
//! predecessor's outcome; its worker joins the predecessor's thread before
//! running, so that wait never happens on the UI thread.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mega_ui::jobs::JobView;
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{
    AnalyzeOutcome, AnalyzePlan, BlendPlan, Grouping, Mosaic, PanelEntry, invalidate_group,
    plan_analyze_with, plan_blend, run_analyze, run_blend,
};
use mmm_core::preview::{Preview, PreviewPlan, plan_previews, run_previews};
use mmm_core::reference::ReferenceFrame;

use crate::session::OpenSession;

/// Least time between two progress updates sent by a worker (≤ 30/s).
pub const PROGRESS_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Label of an analysis job.
pub const ANALYZE_LABEL: &str = "Analyze";
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
    /// A previews job finished one panel's preview.
    Preview(Preview),
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

/// What a queued job will do.
#[derive(Clone, Debug)]
enum Kind {
    Analyze,
    Previews,
    Blend {
        group: String,
        rect: Option<[u64; 4]>,
    },
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
    Resolve(Box<Mosaic>),
    Analyze(AnalyzePlan),
    Blend(BlendPlan),
    Previews(PreviewPlan),
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
        canvas: Option<(u64, u64)>,
    },
    Blend(mmm_core::Result<[u64; 4]>),
    /// The previews run: per-panel failures, or why the whole run failed.
    Previews(mmm_core::Result<Vec<(std::path::PathBuf, String)>>),
}

/// Worker to UI thread.
enum Msg {
    Progress(String, u64, u64),
    Preview(Preview),
    Done(Done),
}

/// The running job.
struct Running {
    id: u64,
    token: CancelToken,
    rx: Receiver<Msg>,
    handle: JoinHandle<()>,
    /// The plan of a blend, kept to apply or discard its outcome.
    blend: Option<BlendPlan>,
    /// While resolving groups: the inputs the resolution is based on.
    resolving: Option<GroupInputs>,
    /// A previews job (analysis queues behind it rather than cancel it).
    previews: bool,
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
    /// Events raised outside `poll` (queued jobs cancelled), delivered by it.
    pending: Vec<JobEvent>,
    #[cfg(test)]
    gate: Option<std::sync::Arc<tests::Gate>>,
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
            pending: Vec::new(),
            #[cfg(test)]
            gate: None,
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
            kind: Kind::Analyze,
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
    pub fn start_blend(
        &mut self,
        session: &mut OpenSession,
        group: &str,
        rect: Option<[u64; 4]>,
    ) -> u64 {
        let id = self.new_id(format!("Blend {group}"));
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
            Kind::Blend { .. } | Kind::Analyze | Kind::Previews => true,
        });
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
        self.queue.push_back(Queued {
            id,
            kind: Kind::Blend {
                group: group.to_string(),
                rect,
            },
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

    /// Cancel every job: the running one winds down (reported by
    /// [`JobRunner::poll`] when it has), queued ones are dropped at once.
    pub fn cancel_all(&mut self) {
        if let Some(r) = &self.running {
            r.token.cancel();
        }
        let dropped: Vec<u64> = self.queue.drain(..).map(|q| q.id).collect();
        self.pending
            .extend(dropped.into_iter().map(|id| JobEvent::Cancelled { id }));
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
                    events.extend(self.finish(r, done, session));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The worker died without an outcome (a panic).
                    let r = self.running.take().expect("running job");
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

    /// Whether a job is running or queued.
    pub fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }

    /// The label of job `id` (also after it ended).
    pub fn label(&self, id: u64) -> Option<&str> {
        self.labels.get(&id).map(String::as_str)
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
                Kind::Analyze => Ok(Planned::Resolve(Box::new(session.mosaic.clone()))),
                Kind::Blend { group, rect } => {
                    plan_blend(&session.mosaic, group, *rect).map(Planned::Blend)
                }
                Kind::Previews => Ok(Planned::Previews(plan_previews(&session.mosaic))),
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
            Planned::Analyze(_) | Planned::Resolve(_) | Planned::Previews(_) => None,
        };
        let resolving = match &planned {
            Planned::Resolve(m) => Some(GroupInputs::of(m)),
            Planned::Analyze(_) | Planned::Blend(_) | Planned::Previews(_) => None,
        };
        let previews = matches!(planned, Planned::Previews(_));
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
                        }
                    };
                    let ctl = JobCtl::new(worker_token, Some(&observer));
                    match &planned {
                        Planned::Resolve(m) => Done::Resolved(m.groups()),
                        Planned::Analyze(plan) => {
                            let (outcome, result) = run_analyze(plan, &ctl);
                            let canvas = ReferenceFrame::load(&plan.frame_path)
                                .ok()
                                .map(|f| f.canvas());
                            Done::Analyze {
                                outcome,
                                result,
                                canvas,
                            }
                        }
                        Planned::Blend(plan) => Done::Blend(run_blend(plan, &ctl)),
                        Planned::Previews(plan) => {
                            let preview_tx = tx.clone();
                            let preview_ctx = ctx.clone();
                            let on_ready = move |p: Preview| {
                                let _ = preview_tx.send(Msg::Preview(p));
                                preview_ctx.request_repaint();
                            };
                            Done::Previews(run_previews(plan, &ctl, &on_ready))
                        }
                    }
                };
                // Everything the job held (store locks included) is
                // released before the outcome is sent.
                let _ = tx.send(Msg::Done(done));
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
            let fresh = r.resolving.as_ref() == Some(&current);
            if fresh {
                session.groups = Some(groups.clone());
            }
            if r.token.is_cancelled() {
                return Some(JobEvent::Cancelled { id });
            }
            // Panels or grouping changed meanwhile: resolve again.
            let planned = if fresh {
                match plan_analyze_with(&mut session.mosaic, groups) {
                    Ok(plan) => Planned::Analyze(plan),
                    Err(e) => return Some(failed(format!("{name}: {}", describe(&e)))),
                }
            } else {
                Planned::Resolve(Box::new(session.mosaic.clone()))
            };
            self.spawn(id, planned);
            return None;
        }
        Some(match (done, &r.blend) {
            (Done::Resolved(_), _) => unreachable!("handled above"),
            (Done::Previews(Ok(failed)), _) => {
                if !failed.is_empty() {
                    tracing::warn!("{name}: {} panel(s) have no preview", failed.len());
                }
                JobEvent::Finished { id, label: name }
            }
            (Done::Previews(Err(e)), _) if e.is_cancelled() => JobEvent::Cancelled { id },
            (Done::Previews(Err(e)), _) => failed(format!("{name}: {}", describe(&e))),
            (
                Done::Analyze {
                    outcome,
                    result,
                    canvas,
                },
                _,
            ) => {
                if canvas.is_some() {
                    session.canvas = canvas;
                }
                // Applied even when cancelled: the finished groups stay.
                let applied = session.mosaic.apply_analyze(&outcome);
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
/// tiles carry no settings guarantee: if `group`'s blend settings changed
/// since `plan` (or it now needs analysis), the store is invalidated so no
/// tile blended with superseded settings stays valid. Owner (UI) thread;
/// the error is a message for the user.
pub fn discard_blend(session: &OpenSession, plan: &BlendPlan) -> Result<(), String> {
    let config = &session.mosaic.config;
    if config.settings_for(&plan.group).1 == plan.settings
        && !config.needs_analyze.contains(&plan.group)
    {
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

    use mmm_core::mosaic::{MosaicConfig, run_blend, set_blend_settings};
    use mmm_core::settings::BlendSettings;
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
                .wait_timeout_while(open, Duration::from_secs(30), |o| !*o)
                .unwrap();
            assert!(*open && !timeout.timed_out(), "gate never opened");
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
    }

    /// Poll until no job is running or queued (bounded); every event.
    pub(crate) fn drain(runner: &mut JobRunner, s: &mut OpenSession) -> Vec<JobEvent> {
        let deadline = Instant::now() + Duration::from_secs(60);
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
                label: "Analyze".into()
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
    fn blend_job_runs_and_applies() {
        let (root, mut s, mut r) = analyzed("job-blend");
        assert_eq!(s.canvas, Some(CANVAS), "canvas from the reference frame");
        assert!(s.mosaic.config.needs_analyze.is_empty());
        let id = r.start_blend(&mut s, "Ha", None);
        assert!(r.busy());
        assert_eq!(r.views().len(), 1);
        assert_eq!(r.views()[0].label, "Blend Ha");
        let events = drain(&mut r, &mut s);
        assert_eq!(
            events,
            vec![JobEvent::Finished {
                id,
                label: "Blend Ha".into()
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
        assert_eq!(r.label(id), Some("Blend Ha"));
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
        let deadline = Instant::now() + Duration::from_secs(30);
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
                label: "Blend Ha".into()
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
                    label: "Blend OIII".into()
                },
                JobEvent::Finished {
                    id: new,
                    label: "Blend Ha".into()
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
                    label: "Blend Ha".into()
                },
                JobEvent::Finished {
                    id: oiii,
                    label: "Blend OIII".into()
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
                    label: "Blend Ha".into()
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
        let deadline = Instant::now() + Duration::from_secs(60);
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
                    label: "Analyze".into()
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
                    label: "Analyze".into()
                },
                JobEvent::Finished {
                    id: blend,
                    label: "Blend OIII".into()
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discarded_blend_after_settings_change_is_invalidated() {
        let (root, mut s, _r) = analyzed("job-discard");
        let plan = mmm_core::mosaic::plan_blend(&s.mosaic, "Ha", None).unwrap();
        // Unchanged settings: the partial tiles of an unapplied blend stay.
        run_blend(&plan, &mmm_core::job::JobCtl::none()).unwrap();
        discard_blend(&s, &plan).unwrap();
        assert!(valid0(&plan.result_path) > 0);
        // A settings edit lands before the blend locks the store...
        let new = BlendSettings {
            feather_px: s.mosaic.config.blend.feather_px + 10.0,
            ..s.mosaic.config.blend.clone()
        };
        set_blend_settings(&mut s.mosaic, new).unwrap();
        run_blend(&plan, &mmm_core::job::JobCtl::none()).unwrap();
        assert!(valid0(&plan.result_path) > 0);
        // ...so discarding the unapplied blend invalidates its tiles.
        discard_blend(&s, &plan).unwrap();
        assert_eq!(valid0(&plan.result_path), 0);
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
                label: "Analyze".into()
            }]
        );
        // ...so it resolved again, and analysed the current groups.
        let names: Vec<&String> = s.mosaic.config.group_dirs.keys().collect();
        assert_eq!(names, ["OIII", "Red"]);
        let cached = s.groups.as_ref().unwrap();
        assert_eq!(cached.groups.keys().collect::<Vec<_>>(), ["OIII", "Red"]);
        assert_eq!(s.group_count(), 2);
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
                    label: "Analyze".into()
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
}
