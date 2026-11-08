//! Small helpers shared by the app's views: background tasks, save
//! debouncing and the session's group names.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::session::OpenSession;

/// Least time between two saves of the display stretch.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(1);

/// Rate limit for saving: a change saves at once, then at most once per
/// [`SAVE_INTERVAL`].
#[derive(Debug, Default)]
pub struct Debounce {
    dirty: bool,
    last: Option<Instant>,
}

impl Debounce {
    /// Something changed that needs saving.
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    /// Whether a change is waiting to be saved (due or not).
    pub fn pending(&self) -> bool {
        self.dirty
    }

    /// Whether to save at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.dirty && self.wait(now).is_none()
    }

    /// Time left before a pending change may be saved (`None` if none is
    /// pending or it may be saved now).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        let last = self.last?;
        let left = SAVE_INTERVAL.saturating_sub(now.saturating_duration_since(last));
        (self.dirty && !left.is_zero()).then_some(left)
    }

    /// A save happened at `now`.
    pub fn saved(&mut self, now: Instant) {
        self.dirty = false;
        self.last = Some(now);
    }
}

/// A computation on its own thread; the UI is repainted when it ends.
pub(crate) struct Task<T> {
    rx: Receiver<T>,
}

/// What a [`Task`] has to say.
pub(crate) enum Poll<T> {
    Pending,
    Done(T),
    /// The thread ended without a result (it panicked).
    Lost,
}

impl<T: Send + 'static> Task<T> {
    pub(crate) fn spawn(
        name: &str,
        ctx: &egui::Context,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = tx.send(f());
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => Some(Task { rx }),
            Err(e) => {
                tracing::error!("cannot start thread {name}: {e}");
                None
            }
        }
    }

    pub(crate) fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(v) => Poll::Done(v),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }
}

/// The processing groups of `session`: resolved ones if known, else those
/// known from analysis.
pub fn group_names(session: &OpenSession) -> Vec<String> {
    match &session.groups {
        Some(g) => g.processing_groups().into_iter().map(|(n, _)| n).collect(),
        None => session.mosaic.config.group_dirs.keys().cloned().collect(),
    }
}
