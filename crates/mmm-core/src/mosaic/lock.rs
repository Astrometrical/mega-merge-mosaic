//! The cross-process session lock: one process (or window) owns a mosaic
//! session directory at a time.
//!
//! The lock is an exclusive advisory lock on [`LOCK_FILE`] inside the
//! session directory, released when the [`SessionLock`] drops (or the
//! process exits, however it ends). The file is never deleted and holds no
//! data, so it does not make a directory look like a mosaic or a group.
//! Taking the lock waits out brief contention ([`LOCK_CONTENTION_WAIT`]) before
//! it reports the session busy.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Name of the session lock file inside a mosaic directory.
pub const LOCK_FILE: &str = "session.lock";

/// An exclusive hold on a session directory; dropping it releases the lock.
#[derive(Debug)]
pub struct SessionLock {
    dir: PathBuf,
    // Held for the exclusive lock; released on drop.
    _file: File,
}

impl SessionLock {
    /// The session directory this lock covers.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// How long [`lock_session`] keeps retrying a contended lock before it
/// reports [`Error::Busy`]. It rides out the microseconds-long shared probe of
/// [`session_busy`] (and a just-dropped holder); a real holder still yields
/// `Busy`, at most this long after the call.
pub const LOCK_CONTENTION_WAIT: Duration = Duration::from_millis(250);

/// Pause between lock attempts while waiting out contention.
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(5);

/// Take the exclusive session lock on `dir` (which must exist), creating
/// [`LOCK_FILE`] if missing. A contended lock is retried for up to
/// [`LOCK_CONTENTION_WAIT`] so a brief holder (such as a [`session_busy`]
/// probe) does not count; it then fails with [`Error::Busy`] (naming `dir`)
/// while another holder, in this process or another, still has it.
pub fn lock_session(dir: &Path) -> Result<SessionLock> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => {
                return Ok(SessionLock {
                    dir: dir.to_path_buf(),
                    _file: file,
                });
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if start.elapsed() >= LOCK_CONTENTION_WAIT {
                    return Err(Error::busy(dir));
                }
                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::io(&path, e)),
        }
    }
}

/// Whether another holder has the session lock on `dir`, without taking it.
///
/// Opens [`LOCK_FILE`] without creating it (absent: `false`) and tries a
/// shared lock, released at once. This microseconds-long probe is the only
/// time a start screen touches the lock; it never holds it. Contention is
/// retried for up to [`LOCK_CONTENTION_WAIT`] (as [`lock_session`] does), so a
/// holder that lets go at once (a notes save, another window's own probe or
/// open that is about to fail) doesn't read as busy; a real holder still
/// does, at most that long after the call.
///
/// A probe error other than contention (permissions, I/O) returns `false`: it
/// is not evidence of another holder. Live reads go through
/// [`crate::tiles::FileTileReader`], which is safe against a concurrent
/// writer, and opening the session still takes the real lock and reports a
/// real error.
pub fn session_busy(dir: &Path) -> bool {
    let Ok(file) = File::open(dir.join(LOCK_FILE)) else {
        return false;
    };
    let start = Instant::now();
    loop {
        match file.try_lock_shared() {
            Ok(()) => {
                let _ = file.unlock();
                return false;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if start.elapsed() >= LOCK_CONTENTION_WAIT {
                    return true;
                }
                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(std::fs::TryLockError::Error(_)) => return false,
        }
    }
}

/// The message for a refused [`lock_session`] on `dir`.
pub fn busy_message(dir: &Path) -> String {
    format!(
        "{} is open in another window or process. Close it there first, or open a \
         different session.",
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_process_open_is_refused() {
        let dir = std::env::temp_dir().join(format!("mmm-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = lock_session(&dir).unwrap();
        let err = lock_session(&dir).unwrap_err();
        assert!(err.is_busy(), "{err}");
        assert!(busy_message(&dir).contains("open in another window or process"));
        drop(first);
        let again = lock_session(&dir).unwrap();
        assert_eq!(again.dir(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_brief_shared_probe_does_not_make_the_lock_busy() {
        let dir = fresh_dir("probe");
        let path = dir.join(LOCK_FILE);
        std::fs::write(&path, b"").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let prober = std::thread::spawn(move || {
            let file = File::open(&path).unwrap();
            file.lock_shared().unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            file.unlock().unwrap();
        });
        rx.recv().unwrap();
        let lock = lock_session(&dir);
        prober.join().unwrap();
        assert!(lock.is_ok(), "{:?}", lock.err());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_lock_is_still_busy() {
        let dir = fresh_dir("held");
        let held = lock_session(&dir).unwrap();
        let start = std::time::Instant::now();
        let err = lock_session(&dir).unwrap_err();
        assert!(err.is_busy(), "{err}");
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_brief_holder_does_not_make_the_session_busy() {
        let dir = fresh_dir("brief");
        let (tx, rx) = std::sync::mpsc::channel();
        let path = dir.clone();
        let holder = std::thread::spawn(move || {
            let held = lock_session(&path).unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held);
        });
        rx.recv().unwrap();
        let busy = session_busy(&dir);
        holder.join().unwrap();
        assert!(!busy, "a lock held for 50 ms reads as busy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn busy_probe_sees_the_session_lock() {
        let dir = std::env::temp_dir().join(format!("mmm-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!session_busy(&dir), "no lock file");
        assert!(!dir.join(LOCK_FILE).exists(), "the probe creates nothing");
        let held = lock_session(&dir).unwrap();
        assert!(session_busy(&dir));
        drop(held);
        assert!(!session_busy(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
