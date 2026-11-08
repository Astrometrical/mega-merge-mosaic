//! The cross-process session lock: one process (or window) owns a mosaic
//! session directory at a time.
//!
//! The lock is an exclusive advisory lock on [`LOCK_FILE`] inside the
//! session directory, released when the [`SessionLock`] drops (or the
//! process exits, however it ends). The file is never deleted and holds no
//! data, so it does not make a directory look like a mosaic or a group.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

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

/// Take the exclusive session lock on `dir` (which must exist), creating
/// [`LOCK_FILE`] if missing. Fails with [`Error::Busy`] (naming `dir`) while
/// another holder, in this process or another, has it.
pub fn lock_session(dir: &Path) -> Result<SessionLock> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;
    match file.try_lock() {
        Ok(()) => Ok(SessionLock {
            dir: dir.to_path_buf(),
            _file: file,
        }),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::busy(dir)),
        Err(std::fs::TryLockError::Error(e)) => Err(Error::io(&path, e)),
    }
}

/// Whether another holder has the session lock on `dir`, without taking it.
///
/// Opens [`LOCK_FILE`] without creating it (absent: `false`) and tries a
/// shared lock, released at once. This microseconds-long probe is the only
/// time a start screen touches the lock; it never holds it.
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
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
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
