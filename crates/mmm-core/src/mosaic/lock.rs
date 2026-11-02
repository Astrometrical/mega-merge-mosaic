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
}
