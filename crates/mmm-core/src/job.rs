//! Job control: cooperative cancellation and progress reporting for
//! long-running pipeline stages.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::blend::RowSink;
use crate::error::{Error, Result};

/// A cheaply clonable cancellation flag; clones share state.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation (visible to every clone).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(Error::Cancelled)` once cancelled, otherwise `Ok(())`.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Progress observer: `(stage, done, total)`; same shape as
/// [`crate::analyze::AnalyzeProgress`].
pub type ProgressFn<'a> = &'a (dyn Fn(&str, u64, u64) + Sync);

/// Cancellation token plus optional progress observer for one job.
pub struct JobCtl<'a> {
    cancel: CancelToken,
    progress: Option<ProgressFn<'a>>,
}

impl<'a> JobCtl<'a> {
    /// A control with the given token and optional progress observer.
    pub fn new(cancel: CancelToken, progress: Option<ProgressFn<'a>>) -> Self {
        Self { cancel, progress }
    }

    /// A control that is never cancelled and reports no progress.
    pub fn none() -> JobCtl<'static> {
        JobCtl {
            cancel: CancelToken::new(),
            progress: None,
        }
    }

    /// `Err(Error::Cancelled)` if the job has been cancelled.
    pub fn check(&self) -> Result<()> {
        self.cancel.check()
    }

    /// Check cancellation, then forward `(stage, done, total)` to the observer.
    pub fn report(&self, stage: &str, done: u64, total: u64) -> Result<()> {
        self.cancel.check()?;
        if let Some(p) = self.progress {
            p(stage, done, total);
        }
        Ok(())
    }

    /// The underlying cancellation token.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }
}

/// A [`RowSink`] wrapper that checks cancellation and reports row progress
/// around each band delivered to the inner sink.
pub struct CtlSink<'s, 'c> {
    inner: &'s mut dyn RowSink,
    ctl: &'s JobCtl<'c>,
    stage: &'static str,
    w: u64,
    h: u64,
    ch: u64,
}

impl<'s, 'c> CtlSink<'s, 'c> {
    /// Wrap `inner`, reporting progress under `stage`.
    pub fn new(inner: &'s mut dyn RowSink, ctl: &'s JobCtl<'c>, stage: &'static str) -> Self {
        Self {
            inner,
            ctl,
            stage,
            w: 0,
            h: 0,
            ch: 0,
        }
    }
}

impl RowSink for CtlSink<'_, '_> {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.w = w;
        self.h = h;
        self.ch = ch;
        self.inner.begin(w, h, ch)
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        self.ctl.check()?;
        self.inner.band(y0, rows)?;
        let per_row = (self.ch * self.w).max(1);
        let rows_done = y0 + rows.len() as u64 / per_row;
        self.ctl.report(self.stage, rows_done, self.h)
    }

    fn finish(&mut self) -> Result<()> {
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn token_check_errors_after_cancel() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        assert!(t.check().is_ok());
        t.cancel();
        assert!(t.is_cancelled());
        assert!(t.check().unwrap_err().is_cancelled());
    }

    #[test]
    fn clones_share_state() {
        let t = CancelToken::new();
        let c = t.clone();
        c.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn report_forwards_then_stops() {
        let seen: Mutex<Vec<(String, u64, u64)>> = Mutex::new(Vec::new());
        let obs = |s: &str, d: u64, t: u64| seen.lock().unwrap().push((s.to_string(), d, t));
        let tok = CancelToken::new();
        let ctl = JobCtl::new(tok.clone(), Some(&obs));
        ctl.report("blend", 1, 4).unwrap();
        tok.cancel();
        assert!(matches!(ctl.report("blend", 2, 4), Err(Error::Cancelled)));
        assert_eq!(*seen.lock().unwrap(), vec![("blend".to_string(), 1, 4)]);
        assert!(JobCtl::none().report("x", 0, 1).is_ok());
    }

    #[derive(Default)]
    struct TestSink {
        bands: Vec<u64>,
        finished: bool,
    }

    impl RowSink for TestSink {
        fn begin(&mut self, _w: u64, _h: u64, _ch: u64) -> Result<()> {
            Ok(())
        }
        fn band(&mut self, y0: u64, _rows: &[f32]) -> Result<()> {
            self.bands.push(y0);
            Ok(())
        }
        fn finish(&mut self) -> Result<()> {
            self.finished = true;
            Ok(())
        }
    }

    #[test]
    fn ctl_sink_reports_and_cancels() {
        let seen: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
        let obs = |_: &str, d: u64, t: u64| seen.lock().unwrap().push((d, t));
        let tok = CancelToken::new();
        let ctl = JobCtl::new(tok.clone(), Some(&obs));
        let mut inner = TestSink::default();
        let mut sink = CtlSink::new(&mut inner, &ctl, "blend");
        sink.begin(4, 4, 1).unwrap();
        let rows = vec![0.0f32; 8];
        sink.band(0, &rows).unwrap();
        sink.band(2, &rows).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![(2, 4), (4, 4)]);
        assert_eq!(inner.bands, vec![0, 2]);

        // Cancelled before the second band: it never reaches the inner sink.
        let tok = CancelToken::new();
        let ctl = JobCtl::new(tok.clone(), None);
        let mut inner = TestSink::default();
        let mut sink = CtlSink::new(&mut inner, &ctl, "blend");
        sink.begin(4, 4, 1).unwrap();
        sink.band(0, &rows).unwrap();
        tok.cancel();
        assert!(sink.band(2, &rows).unwrap_err().is_cancelled());
        sink.finish().unwrap();
        assert_eq!(inner.bands, vec![0]);
    }
}
