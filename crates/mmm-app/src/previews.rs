//! Transient ROI previews (spec §8.2): each visible ROI's area merged with
//! the merge settings draft, by itself, in memory.
//!
//! - **Transient.** A preview is made by [`run_preview`] on the preview
//!   runner's own worker thread ("mmm-preview"), never through the
//!   [`crate::jobs::JobRunner`]: it opens, creates and writes no result
//!   store, writes no provenance and no `mosaic.json`, and raises no job
//!   event and no toast. Its patch lives in memory for the app's run only.
//!   ([`plan_preview`], on the UI thread, reads no file.)
//! - **Keys.** A patch is cached by `(ROI id, group)` with the
//!   [`PreviewKey`] it was made from: the ROI's canvas rect, the draft as
//!   the group merges with it ([`draft_for`]) and the group's analysis
//!   stamp ([`analysis_stamp`]: a re-analysis moves it, also one that
//!   stopped part-way once it had finished the group; an analysed group no
//!   entry lists, from an older session, has the sentinel [`UNSTAMPED`]). A
//!   cached key that differs from the key wanted now makes the ROI out of
//!   date; so does a group needing an analysis, or never analysed.
//! - **The scheduler** ([`Previews::schedule`], every frame, with what the
//!   window wants: the visible ROIs of the groups in view): a running
//!   preview no longer wanted with its key is cancelled (so settings changed
//!   faster than previews finish never build a queue); with the runner idle
//!   (a cancelled preview counts until it has ended) the most urgent wanted
//!   entry whose cached key differs starts ([`Wanted::priority`]: the group
//!   in view first, then the ROI nearest the centre of the view). At most
//!   one preview is in flight, and nothing is queued behind it.
//! - **Delivery** ([`Previews::poll`], with what is wanted this frame,
//!   before scheduling): a finished patch goes into the cache only if it
//!   was not cancelled and its key is wanted now; a late result for an old
//!   key is dropped, even one that ended as its key went stale. A failed
//!   preview is logged (no toast) and not tried again with the same key.
//! - **Stopping** ([`Previews::stop`], on leaving the session): the preview
//!   in flight is cancelled and the worker thread's handle handed over, so
//!   the session's lock is kept until it has ended.
//! - **ROIs moved or deleted** ([`Previews::keep_rois`],
//!   [`Previews::forget_roi`]): their running preview is cancelled and their
//!   patches dropped, so a stale patch never shows.
//! - **The cache** keeps at most [`PREVIEW_CACHE_BYTES`] of patch samples:
//!   the least recently drawn ([`Previews::patch`] marks a patch drawn) go
//!   first, never the patch just made.
//! - **Compare A / B** (spec §8.3; [`Previews::start_compare`]): one ROI at
//!   a time. Its patches of the groups in view, up to date, are frozen as
//!   side **A** with the draft they were made from ([`Compare`]): held
//!   outside the cache, so the LRU never evicts them (they count towards
//!   its cap, once when the cache shares them) and nothing merges them
//!   again. **B** is the live preview, cached and scheduled as any. The
//!   selected side shows; a momentary switch ([`Peek`]) held shows the
//!   other ([`Compare::shown`]). The compare ends when its ROI is deleted
//!   or moved ([`Previews::forget_roi`], [`Previews::keep_rois`]), on
//!   [`Previews::stop`], on [`Previews::use_a`] (A's patches go back into
//!   the cache: with A's settings back in the draft they are the previews
//!   wanted, so nothing merges) and when the window ends it
//!   ([`Previews::end_compare`]).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::{
    MosaicConfig, PreviewPatch, PreviewPlan, analysis_stamp, plan_preview, run_preview,
};
use mmm_core::settings::BlendSettings;

use crate::blend_settings::draft_for;
use crate::rois::Roi;
use crate::session::OpenSession;

/// Record what the scheduler did in its log (tests; nothing otherwise).
macro_rules! note {
    ($previews:expr, $what:expr) => {
        #[cfg(test)]
        $previews.log.push($what);
    };
}

/// Most bytes of patch samples the preview cache keeps.
pub const PREVIEW_CACHE_BYTES: usize = 1 << 30;

/// The analysis stamp ([`PreviewKey::analysis`]) of a group that is
/// analysed (it has a directory and needs no analysis) but that no
/// `"analyze"` provenance entry lists: a session written before analyses
/// that stopped part-way recorded the groups they finished. Such a group
/// previews under this fixed sentinel; the next analysis of it records an
/// entry, whose index (the real stamp, [`analysis_stamp`]) never equals
/// it, so a patch made under the sentinel then reads out of date.
pub const UNSTAMPED: usize = usize::MAX;

/// What a preview of an ROI and a group is made from.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewKey {
    /// The ROI's canvas rect (`[x0, y0, x1, y1]`, level-0 pixels).
    pub rect: [u64; 4],
    /// The merge settings: the draft with the group's override.
    pub settings: BlendSettings,
    /// The group's analysis stamp ([`analysis_stamp`]), or [`UNSTAMPED`]
    /// for an analysed group no `"analyze"` entry lists.
    pub analysis: usize,
}

impl PreviewKey {
    /// The key a preview of `rect` of `group` is made with now: the draft
    /// as the group merges with it and the group's analysis stamp
    /// ([`UNSTAMPED`] when it is analysed but no entry lists it). `None`
    /// when the group cannot be previewed: never analysed, or needing a new
    /// analysis.
    pub fn wanted(cfg: &MosaicConfig, group: &str, rect: [u64; 4]) -> Option<Self> {
        if !cfg.group_dirs.contains_key(group) || cfg.needs_analyze.contains(group) {
            return None;
        }
        Some(PreviewKey {
            rect,
            settings: draft_for(cfg, group),
            analysis: analysis_stamp(cfg, group).unwrap_or(UNSTAMPED),
        })
    }
}

/// Where an ROI's preview stands for the groups in view (its row says so).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewState {
    /// A preview of it is being merged.
    Merging,
    /// Every group in view has a patch made from what it would be made from
    /// now.
    UpToDate,
    /// A group in view has no patch, or one made from other settings, an
    /// older analysis or another rect, or needs an analysis.
    OutOfDate,
}

impl PreviewState {
    /// What the ROI's row says: "merging", "up to date" or "out of date".
    pub fn text(self) -> &'static str {
        match self {
            PreviewState::Merging => "merging",
            PreviewState::UpToDate => "up to date",
            PreviewState::OutOfDate => "out of date",
        }
    }
}

/// A side of a compare (spec §8.3): **A**, the preview frozen when the
/// compare began, or **B**, the live preview (what every ROI not compared
/// shows too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    /// The preview as it was when the compare began.
    A,
    /// The live preview, following the draft.
    B,
}

impl Side {
    /// The other side.
    pub fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }

    /// "A" or "B".
    pub fn letter(self) -> &'static str {
        match self {
            Side::A => "A",
            Side::B => "B",
        }
    }
}

/// A momentary switch of a compare: while one is held, the side not
/// selected shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Peek {
    /// Space, held (not while typing or in a dialog).
    Space,
    /// The selected A / B button, pressed and held.
    Button,
    /// The left mouse button held on the compared ROI's preview (until it
    /// drags).
    Preview,
}

/// The ROI being compared (spec §8.3).
#[derive(Clone, Debug)]
pub struct Compare {
    /// The ROI's id.
    pub roi: u64,
    /// Side A: per group in view when the compare began, its patch and
    /// what it was made from. Pinned: never evicted, never merged again.
    pub a: BTreeMap<String, (PreviewKey, Arc<PreviewPatch>)>,
    /// The draft A was made from (before any group's override): what
    /// **Use A** puts back.
    pub a_settings: BlendSettings,
    /// The side the A / B toggle has chosen.
    pub selected: Side,
    /// The momentary switches held.
    pub peeks: BTreeSet<Peek>,
}

impl Compare {
    /// The side shown: the one selected, or the other while a momentary
    /// switch is held.
    pub fn shown(&self) -> Side {
        if self.peeks.is_empty() {
            self.selected
        } else {
            self.selected.other()
        }
    }

    /// Whether a momentary switch is held.
    pub fn peeking(&self) -> bool {
        !self.peeks.is_empty()
    }

    /// The ROI's canvas rect A was made over.
    pub fn rect(&self) -> Option<[u64; 4]> {
        self.a.values().next().map(|(k, _)| k.rect)
    }
}

/// A preview the window wants this frame (an ROI in view, a group in view).
#[derive(Clone, Debug, PartialEq)]
pub struct Wanted {
    /// The ROI's id.
    pub roi: u64,
    /// The group.
    pub group: String,
    /// What the preview would be made from now.
    pub key: PreviewKey,
    /// Lower goes first: the group's place in the view (a composite's
    /// members in R, G, B order), then the squared distance (canvas pixels)
    /// from the ROI's centre to the centre of the view.
    pub priority: (u8, u64),
}

/// A cached patch.
struct Entry {
    key: PreviewKey,
    patch: Arc<PreviewPatch>,
    /// When it was last drawn (or made), on the cache's clock.
    drawn: Cell<u64>,
}

/// The preview in flight.
struct Job {
    id: u64,
    roi: u64,
    group: String,
    key: PreviewKey,
    token: CancelToken,
    /// Cancelled: its outcome is dropped when it comes.
    cancelled: bool,
}

/// A preview for the worker to run.
struct Request {
    id: u64,
    plan: PreviewPlan,
    token: CancelToken,
    ctx: egui::Context,
    #[cfg(test)]
    gate: Option<Arc<tests::Gate>>,
}

/// What a preview gave.
struct Outcome {
    id: u64,
    result: mmm_core::Result<PreviewPatch>,
}

/// The worker thread's ends of the channels.
struct Worker {
    tx: Sender<Request>,
    rx: Receiver<Outcome>,
    /// The thread: it ends once `tx` is dropped and its preview has ended.
    handle: JoinHandle<()>,
}

impl Worker {
    /// The "mmm-preview" thread: runs each request in turn and sends its
    /// outcome back, repainting the UI. It ends when the sender is dropped.
    fn spawn(#[cfg(test)] probe: Arc<tests::Probe>) -> Option<Self> {
        let (tx, requests) = mpsc::channel::<Request>();
        let (done, rx) = mpsc::channel::<Outcome>();
        #[cfg(test)]
        probe.alive.store(true, std::sync::atomic::Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name("mmm-preview".into())
            .spawn(move || {
                #[cfg(test)]
                let _alive = tests::Alive(probe.clone());
                for req in requests {
                    let run = || {
                        #[cfg(test)]
                        if let Some(g) = &req.gate {
                            g.wait(&req.token);
                        }
                        run_preview(&req.plan, &JobCtl::new(req.token.clone(), None))
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                        .unwrap_or_else(|_| {
                            Err(mmm_core::Error::compute("the preview failed (see the log)"))
                        });
                    let _ = done.send(Outcome { id: req.id, result });
                    #[cfg(test)]
                    probe.sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    req.ctx.request_repaint();
                }
            });
        match spawned {
            Ok(handle) => Some(Worker { tx, rx, handle }),
            Err(e) => {
                tracing::warn!("cannot start the preview thread: {e}");
                None
            }
        }
    }
}

/// The ROIs' transient previews: the runner (one worker thread, one
/// preview in flight with its own cancel token), the cache by `(ROI id,
/// group)` and the scheduler (see the module docs).
pub struct Previews {
    worker: Option<Worker>,
    running: Option<Job>,
    next_id: u64,
    cache: BTreeMap<(u64, String), Entry>,
    /// The cache's clock: ticks on every patch made or drawn.
    clock: Cell<u64>,
    /// Most bytes of samples kept.
    cap: usize,
    /// Each ROI's canvas rect, as last seen ([`Previews::keep_rois`]).
    rects: BTreeMap<u64, [u64; 4]>,
    /// What the window wanted at the last [`Previews::schedule`].
    wanted: Vec<Wanted>,
    /// Keys that failed, not tried again until the key changes.
    failed: BTreeMap<(u64, String), PreviewKey>,
    /// The ROI being compared, with its side A.
    compare: Option<Compare>,
    /// Hold the previews started from now on (tests).
    #[cfg(test)]
    gate: Option<Arc<tests::Gate>>,
    /// What the scheduler did, in order (tests).
    #[cfg(test)]
    log: Vec<Logged>,
    /// What the worker thread did (tests).
    #[cfg(test)]
    probe: Arc<tests::Probe>,
}

impl Default for Previews {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Previews {
    /// Stop the preview in flight: nothing takes it any more (the session
    /// was left), and it must not go on reading the session's files.
    fn drop(&mut self) {
        if let Some(job) = &self.running {
            job.token.cancel();
        }
    }
}

impl Previews {
    /// No preview made yet; the worker thread starts with the first.
    pub fn new() -> Self {
        Previews {
            worker: None,
            running: None,
            next_id: 1,
            cache: BTreeMap::new(),
            clock: Cell::new(0),
            cap: PREVIEW_CACHE_BYTES,
            rects: BTreeMap::new(),
            wanted: Vec::new(),
            failed: BTreeMap::new(),
            compare: None,
            #[cfg(test)]
            gate: None,
            #[cfg(test)]
            log: Vec::new(),
            #[cfg(test)]
            probe: Arc::default(),
        }
    }

    /// Every frame, with what the window wants now (empty while nothing may
    /// be previewed): cancel the running preview if it is no longer wanted
    /// with its key; with the runner idle, start the most urgent wanted
    /// preview whose cached key differs (a group [`plan_preview`] refuses
    /// is logged and skipped). `ctx` is repainted when a preview ends.
    pub fn schedule(&mut self, ctx: &egui::Context, session: &OpenSession, wanted: &[Wanted]) {
        self.wanted = wanted.to_vec();
        if let Some(job) = &self.running
            && !job.cancelled
            && !self.is_wanted(job.roi, &job.group, &job.key)
        {
            self.cancel_running();
        }
        while self.running.is_none() {
            let next = self
                .wanted
                .iter()
                .filter(|w| {
                    let at = (w.roi, w.group.clone());
                    self.cache.get(&at).map(|e| &e.key) != Some(&w.key)
                        && self.failed.get(&at) != Some(&w.key)
                })
                .min_by_key(|w| w.priority)
                .cloned();
            let Some(w) = next else { break };
            match plan_preview(
                &session.mosaic,
                &w.group,
                w.key.rect,
                w.key.settings.clone(),
            ) {
                Ok(plan) => self.start(ctx, w, plan),
                Err(e) => {
                    tracing::warn!("no preview of {} in ROI {}: {e}", w.group, w.roi);
                    self.fail(w.roi, w.group, w.key);
                }
            }
        }
    }

    /// Take the outcome of the preview in flight, if it has ended: its
    /// patch goes into the cache unless it was cancelled or its key is not
    /// in `wanted` (dropped). Call every frame with what the window wants
    /// now, after [`Previews::keep_rois`] and before [`Previews::schedule`]
    /// (given the same list): a result that ends as its key goes stale (the
    /// settings changed, an analysis started) is never cached as current.
    pub fn poll(&mut self, wanted: &[Wanted]) {
        self.wanted = wanted.to_vec();
        let Some(worker) = &self.worker else { return };
        let mut outcomes = Vec::new();
        let mut lost = false;
        loop {
            match worker.rx.try_recv() {
                Ok(o) => outcomes.push(o),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    lost = true;
                    break;
                }
            }
        }
        for o in outcomes {
            self.finish(o);
        }
        if lost {
            // The thread is gone (it never panics out, but a send can
            // fail): start another with the next preview.
            self.worker = None;
            if let Some(_job) = self.running.take() {
                note!(self, Logged::Failed(_job.roi, _job.group));
            }
        }
    }

    /// The ROIs as they are now: the running preview of one gone or moved
    /// (another canvas rect, e.g. reprojected after a frame change) is
    /// cancelled, and its patches are dropped (its compare ends). Call every
    /// frame before [`Previews::schedule`].
    pub fn keep_rois(&mut self, rois: &[Roi]) {
        self.rects = rois.iter().map(|r| (r.id, r.canvas)).collect();
        if let Some(job) = &self.running
            && !job.cancelled
            && self.rects.get(&job.roi) != Some(&job.key.rect)
        {
            self.cancel_running();
        }
        let rects = &self.rects;
        if self
            .compare
            .as_ref()
            .is_some_and(|c| c.rect().is_none_or(|r| rects.get(&c.roi) != Some(&r)))
        {
            self.compare = None;
        }
        self.cache
            .retain(|(roi, _), e| rects.get(roi) == Some(&e.key.rect));
        self.failed
            .retain(|(roi, _), key| rects.get(roi) == Some(&key.rect));
    }

    /// The patch of ROI `roi` and `group`, with what it was made from (up
    /// to date or not), for drawing: it counts as drawn now (the cache drops
    /// the least recently drawn first).
    pub fn patch(&self, roi: u64, group: &str) -> Option<(&PreviewKey, Arc<PreviewPatch>)> {
        let e = self.cache.get(&(roi, group.to_string()))?;
        e.drawn.set(self.tick());
        Some((&e.key, e.patch.clone()))
    }

    /// What the cached patch of ROI `roi` and `group` was made from, if
    /// there is one (it does not count as drawn).
    pub fn cached_key(&self, roi: u64, group: &str) -> Option<&PreviewKey> {
        self.cache.get(&(roi, group.to_string())).map(|e| &e.key)
    }

    /// Where ROI `roi`'s preview stands for `groups` (the groups in view):
    /// merging while a preview of it runs, out of date when a group's
    /// cached key differs from the key it would be made with now (or it
    /// has none, or cannot be previewed now) or no group is in view (a
    /// composite mapping none: nothing shows), else up to date.
    pub fn state(&self, roi: u64, groups: &[String], session: &OpenSession) -> PreviewState {
        if self
            .running
            .as_ref()
            .is_some_and(|j| j.roi == roi && !j.cancelled)
        {
            return PreviewState::Merging;
        }
        let Some(&rect) = self.rects.get(&roi) else {
            return PreviewState::OutOfDate;
        };
        if groups.is_empty() {
            return PreviewState::OutOfDate;
        }
        let cfg = &session.mosaic.config;
        let current = groups.iter().all(|g| {
            let wanted = PreviewKey::wanted(cfg, g, rect);
            wanted.is_some() && self.cached_key(roi, g) == wanted.as_ref()
        });
        if current {
            PreviewState::UpToDate
        } else {
            PreviewState::OutOfDate
        }
    }

    /// Whether ROI `roi`'s preview for `groups` (the groups in view) is
    /// coarse ([`PreviewPatch::coarse`]: blended from the 1/8 summaries, so
    /// it does not show the merge mode): the cached patch of one of them, or
    /// a compare's side A of it, is.
    pub fn coarse(&self, roi: u64, groups: &[String]) -> bool {
        groups.iter().any(|g| {
            self.cache
                .get(&(roi, g.clone()))
                .is_some_and(|e| e.patch.coarse)
                || self.a_patch(roi, g).is_some_and(|p| p.coarse)
        })
    }

    /// End the previews (leaving the session): a compare ends, the one in
    /// flight is cancelled and the worker thread let go of; it ends once
    /// that preview has (a decode in progress is not interrupted). Its
    /// handle, if there is one, for the caller to wait for: nothing may read
    /// or write the session's files once its lock is released. A later
    /// [`Previews::schedule`] would start a new thread.
    pub fn stop(&mut self) -> Option<JoinHandle<()>> {
        self.compare = None;
        if let Some(job) = self.running.take() {
            job.token.cancel();
            note!(self, Logged::Cancelled(job.roi, job.group));
        }
        self.worker.take().map(|w| w.handle)
    }

    /// ROI `roi` was deleted: its running preview is cancelled, its
    /// patches dropped and its compare ended.
    pub fn forget_roi(&mut self, roi: u64) {
        if self.compare.as_ref().is_some_and(|c| c.roi == roi) {
            self.compare = None;
        }
        if self
            .running
            .as_ref()
            .is_some_and(|j| j.roi == roi && !j.cancelled)
        {
            self.cancel_running();
        }
        self.cache.retain(|(r, _), _| *r != roi);
        self.failed.retain(|(r, _), _| *r != roi);
        self.rects.remove(&roi);
    }

    /// Bytes of samples held: the cache's patches, and a compare's side A
    /// (a patch the cache holds too is counted once).
    pub fn bytes(&self) -> usize {
        let size = |p: &PreviewPatch| p.data.len() * size_of::<f32>();
        let cached: usize = self.cache.values().map(|e| size(&e.patch)).sum();
        let pinned: usize = self
            .compare
            .iter()
            .flat_map(|c| c.a.values())
            .filter(|(_, p)| !self.cache.values().any(|e| Arc::ptr_eq(&e.patch, p)))
            .map(|(_, p)| size(p))
            .sum();
        cached + pinned
    }

    /// Compare ROI `roi` (spec §8.3): its cached patches of `groups` (the
    /// groups in view) become side A, made from the draft `settings`,
    /// pinned; B, the live preview, is selected. A compare running ends
    /// first (one ROI at a time). False, with no compare, when a group has
    /// no patch of the ROI. Offer it only for an ROI up to date for
    /// `groups` ([`Previews::state`]): A is then what the draft makes now.
    pub fn start_compare(&mut self, roi: u64, groups: &[String], settings: BlendSettings) -> bool {
        self.compare = None;
        let mut a = BTreeMap::new();
        for g in groups {
            let Some(e) = self.cache.get(&(roi, g.clone())) else {
                return false;
            };
            a.insert(g.clone(), (e.key.clone(), e.patch.clone()));
        }
        if a.is_empty() {
            return false;
        }
        self.compare = Some(Compare {
            roi,
            a,
            a_settings: settings,
            selected: Side::B,
            peeks: BTreeSet::new(),
        });
        true
    }

    /// End the compare, if any (**Done**, or its view no longer holds): A
    /// is let go of, B stays.
    pub fn end_compare(&mut self) {
        self.compare = None;
    }

    /// **Use A**: end the compare, A's patches going back into the cache as
    /// the ROI's previews (each made from what its key says, so once the
    /// caller has put A's settings back in the draft they are what is
    /// wanted, and nothing merges again). A's draft settings, for the
    /// caller to put back; `None` when not comparing.
    pub fn use_a(&mut self) -> Option<BlendSettings> {
        let c = self.compare.take()?;
        for (group, (key, patch)) in c.a {
            if self.rects.get(&c.roi) == Some(&key.rect) {
                self.insert_arc(c.roi, group, key, patch);
            }
        }
        Some(c.a_settings)
    }

    /// The compare running, if any.
    pub fn compare(&self) -> Option<&Compare> {
        self.compare.as_ref()
    }

    /// The side shown now: [`Compare::shown`], or B outside compare.
    pub fn shown_side(&self) -> Side {
        self.compare.as_ref().map_or(Side::B, Compare::shown)
    }

    /// Choose `side` with the A / B toggle (nothing outside compare).
    pub fn select_side(&mut self, side: Side) {
        if let Some(c) = &mut self.compare {
            c.selected = side;
        }
    }

    /// Momentary switch `peek` is held now, or not (nothing outside
    /// compare). Whether the side shown changed.
    pub fn set_peek(&mut self, peek: Peek, held: bool) -> bool {
        let Some(c) = &mut self.compare else {
            return false;
        };
        let before = c.shown();
        if held {
            c.peeks.insert(peek);
        } else {
            c.peeks.remove(&peek);
        }
        c.shown() != before
    }

    /// Side A's patch of ROI `roi` and `group`, while it is compared.
    pub fn a_patch(&self, roi: u64, group: &str) -> Option<Arc<PreviewPatch>> {
        let c = self.compare.as_ref().filter(|c| c.roi == roi)?;
        c.a.get(group).map(|(_, p)| p.clone())
    }

    /// Whether `(roi, group)` is wanted with `key` (as last scheduled).
    fn is_wanted(&self, roi: u64, group: &str, key: &PreviewKey) -> bool {
        self.wanted
            .iter()
            .any(|w| w.roi == roi && w.group == group && &w.key == key)
    }

    /// The cache's clock, moved on.
    fn tick(&self) -> u64 {
        let t = self.clock.get() + 1;
        self.clock.set(t);
        t
    }

    /// Cancel the preview in flight; the runner stays busy until it has
    /// ended (no second preview meanwhile).
    fn cancel_running(&mut self) {
        let Some(job) = &mut self.running else { return };
        job.token.cancel();
        job.cancelled = true;
        note!(self, Logged::Cancelled(job.roi, job.group.clone()));
    }

    /// Hand `w`'s preview (planned as `plan`) to the worker.
    fn start(&mut self, ctx: &egui::Context, w: Wanted, plan: PreviewPlan) {
        if self.worker.is_none() {
            self.worker = Worker::spawn(
                #[cfg(test)]
                self.probe.clone(),
            );
        }
        let Some(worker) = &self.worker else {
            self.fail(w.roi, w.group, w.key);
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let token = CancelToken::new();
        let req = Request {
            id,
            plan,
            token: token.clone(),
            ctx: ctx.clone(),
            #[cfg(test)]
            gate: self.gate.clone(),
        };
        if worker.tx.send(req).is_err() {
            self.worker = None;
            self.fail(w.roi, w.group, w.key);
            return;
        }
        note!(self, Logged::Started(w.roi, w.group.clone(), w.key.clone()));
        self.running = Some(Job {
            id,
            roi: w.roi,
            group: w.group,
            key: w.key,
            token,
            cancelled: false,
        });
    }

    /// The preview with `o`'s id has ended: keep its patch if it is still
    /// wanted, else drop it.
    fn finish(&mut self, o: Outcome) {
        let Some(job) = self.running.take_if(|j| j.id == o.id) else {
            return;
        };
        let current = !job.cancelled && self.is_wanted(job.roi, &job.group, &job.key);
        match o.result {
            Ok(patch) if current => {
                note!(self, Logged::Delivered(job.roi, job.group.clone()));
                self.insert(job.roi, job.group, job.key, patch);
            }
            Err(e) if current && !e.is_cancelled() => {
                tracing::warn!("no preview of {} in ROI {}: {e}", job.group, job.roi);
                self.fail(job.roi, job.group, job.key);
            }
            _ => {
                note!(self, Logged::Dropped(job.roi, job.group));
            }
        }
    }

    /// Note that `key` failed for `(roi, group)`: not tried again with it.
    fn fail(&mut self, roi: u64, group: String, key: PreviewKey) {
        note!(self, Logged::Failed(roi, group.clone()));
        self.failed.insert((roi, group), key);
    }

    /// Cache `patch` ([`Previews::insert_arc`]).
    fn insert(&mut self, roi: u64, group: String, key: PreviewKey, patch: PreviewPatch) {
        self.insert_arc(roi, group, key, Arc::new(patch));
    }

    /// Cache `patch`, then drop the least recently drawn patches (never
    /// this one, never a compare's A) while more than the cap is held
    /// ([`Previews::bytes`]).
    fn insert_arc(&mut self, roi: u64, group: String, key: PreviewKey, patch: Arc<PreviewPatch>) {
        let at = (roi, group);
        self.failed.remove(&at);
        let entry = Entry {
            key,
            patch,
            drawn: Cell::new(self.tick()),
        };
        self.cache.insert(at.clone(), entry);
        while self.bytes() > self.cap {
            let oldest = self
                .cache
                .iter()
                .filter(|(k, _)| **k != at)
                .min_by_key(|(_, e)| e.drawn.get())
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.cache.remove(&k);
                }
                None => break,
            }
        }
    }
}

/// What the scheduler did (tests read [`Previews`]'s log): `(roi, group)`.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Logged {
    /// A preview started, made from this key.
    Started(u64, String, PreviewKey),
    /// The running preview was cancelled (it ends, dropped, later).
    Cancelled(u64, String),
    /// A patch went into the cache.
    Delivered(u64, String),
    /// A preview ended cancelled, or no longer wanted: its outcome went.
    Dropped(u64, String),
    /// A preview could not be made (planned, started or run).
    Failed(u64, String),
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::*;

    /// Holds previews back (before they run) until opened or cancelled.
    pub(crate) struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
        /// A cancel does not let the preview go (like a decode, which
        /// nothing interrupts).
        through_cancel: bool,
    }

    /// What the worker thread did.
    #[derive(Default)]
    pub(crate) struct Probe {
        /// The thread is running.
        pub(crate) alive: AtomicBool,
        /// Outcomes it has sent.
        pub(crate) sent: AtomicUsize,
    }

    /// Clears [`Probe::alive`] when the worker thread ends, however.
    pub(super) struct Alive(pub(super) Arc<Probe>);

    impl Drop for Alive {
        fn drop(&mut self) {
            self.0.alive.store(false, Ordering::SeqCst);
        }
    }

    impl Gate {
        /// Let every preview it holds run.
        pub(crate) fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }

        /// Block until opened or `token` is cancelled (bounded: never hang
        /// a test).
        pub(super) fn wait(&self, token: &CancelToken) {
            let deadline = Instant::now() + crate::testutil::PATIENCE;
            let mut open = self.open.lock().unwrap();
            while !*open && (self.through_cancel || !token.is_cancelled()) {
                assert!(Instant::now() < deadline, "preview gate never opened");
                open = self
                    .cv
                    .wait_timeout(open, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        }
    }

    impl Previews {
        /// Hold every preview started from now on until the gate returned
        /// opens (or the preview is cancelled).
        pub(crate) fn hold(&mut self) -> Arc<Gate> {
            self.hold_with(false)
        }

        /// [`Previews::hold`], but a cancel does not let a held preview go:
        /// only the gate opening does.
        pub(crate) fn hold_through_cancel(&mut self) -> Arc<Gate> {
            self.hold_with(true)
        }

        fn hold_with(&mut self, through_cancel: bool) -> Arc<Gate> {
            let gate = Arc::new(Gate {
                open: Mutex::new(false),
                cv: Condvar::new(),
                through_cancel,
            });
            self.gate = Some(gate.clone());
            gate
        }

        /// What the worker thread did (shared with it).
        pub(crate) fn probe(&self) -> Arc<Probe> {
            self.probe.clone()
        }

        /// What the scheduler did, in order.
        pub(crate) fn log(&self) -> &[Logged] {
            &self.log
        }

        /// Keep at most `bytes` of samples.
        pub(crate) fn set_cap(&mut self, bytes: usize) {
            self.cap = bytes;
        }

        /// Cache `patch` as a delivery does.
        pub(crate) fn insert_patch(
            &mut self,
            roi: u64,
            group: &str,
            key: PreviewKey,
            patch: PreviewPatch,
        ) {
            self.insert(roi, group.to_string(), key, patch);
        }
    }

    /// A one-channel patch of `n` samples.
    fn patch_of(n: usize) -> PreviewPatch {
        PreviewPatch {
            rect: [0, 0, 256, 256],
            width: n as u32,
            height: 1,
            channels: 1,
            downsample: 1,
            coarse: false,
            data: vec![0.5; n],
        }
    }

    fn key(analysis: usize) -> PreviewKey {
        PreviewKey {
            rect: [0, 0, 64, 64],
            settings: BlendSettings::default(),
            analysis,
        }
    }

    fn held(p: &Previews) -> Vec<u64> {
        p.cache.keys().map(|(roi, _)| *roi).collect()
    }

    #[test]
    fn the_cache_stays_under_its_cap() {
        assert_eq!(Previews::new().cap, PREVIEW_CACHE_BYTES);
        let mut p = Previews::new();
        // Room for three 1000-sample patches.
        p.set_cap(3 * 1000 * size_of::<f32>());
        for roi in 1..=3 {
            p.insert_patch(roi, "Ha", key(0), patch_of(1000));
        }
        assert_eq!(held(&p), [1, 2, 3]);
        assert_eq!(p.bytes(), 12_000);
        // A patch replaced is counted once.
        p.insert_patch(3, "Ha", key(1), patch_of(1000));
        assert_eq!((held(&p), p.bytes()), (vec![1, 2, 3], 12_000));
        // 1 drawn since: 2 is now the least recently drawn, and goes first.
        assert!(p.patch(1, "Ha").is_some());
        p.insert_patch(4, "Ha", key(0), patch_of(1000));
        assert_eq!(held(&p), [1, 3, 4]);
        assert!(p.bytes() <= 12_000);
        // Looking at a key is not drawing: 3 is older than 1 and goes next.
        assert!(p.cached_key(3, "Ha").is_some());
        p.insert_patch(5, "Ha", key(0), patch_of(1000));
        assert_eq!(held(&p), [1, 4, 5]);
        // One patch over the cap by itself: everything else goes, never the
        // patch just made.
        p.insert_patch(6, "OIII", key(0), patch_of(5000));
        assert_eq!(held(&p), [6]);
        assert!(p.patch(6, "OIII").is_some());
        // The next one pushes it out.
        p.insert_patch(7, "Ha", key(0), patch_of(10));
        assert_eq!(held(&p), [7]);
        assert_eq!(p.bytes(), 40);
    }

    #[test]
    fn a_is_never_evicted_while_comparing() {
        let mut p = Previews::new();
        let ha = ["Ha".to_string()];
        // No patch to freeze: no compare.
        assert!(!p.start_compare(1, &ha, BlendSettings::default()));
        assert!(p.compare().is_none());
        p.insert_patch(1, "Ha", key(0), patch_of(1000));
        assert!(p.start_compare(1, &ha, BlendSettings::default()));
        let a = p.compare().unwrap().a["Ha"].1.clone();
        // Room for two patches in all; A, the cached patch itself, counts
        // once.
        p.set_cap(2 * 1000 * size_of::<f32>());
        assert_eq!(p.bytes(), 4000);
        // B merged again with other settings: a patch of its own, and A
        // still counts.
        p.insert_patch(1, "Ha", key(1), patch_of(1000));
        assert_eq!(p.bytes(), 8000);
        // Another ROI's patch: the least recently drawn cached patch goes
        // (B), never A.
        p.insert_patch(2, "Ha", key(0), patch_of(1000));
        assert_eq!(held(&p), [2]);
        assert!(Arc::ptr_eq(&p.compare().unwrap().a["Ha"].1, &a));
        assert_eq!(p.bytes(), 8000);
        // More, drawn or not: A stays.
        for roi in 3..6 {
            p.insert_patch(roi, "Ha", key(0), patch_of(1000));
            assert!(Arc::ptr_eq(&p.compare().unwrap().a["Ha"].1, &a));
            assert!(p.bytes() <= 8000);
        }
        // Compare on another ROI ends this one: A goes.
        assert!(p.start_compare(5, &ha, BlendSettings::default()));
        assert_eq!(p.compare().unwrap().roi, 5);
        assert_eq!(p.bytes(), 4000);
        p.end_compare();
        assert!(p.compare().is_none());
    }
}
