//! Small filesystem helpers shared across the crate.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Error, Result};

/// Distinguishes concurrent temporary files of one process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically: into a uniquely named temporary file
/// beside it (process id and a per-process counter), then renamed into
/// place, so a concurrent reader sees the old file or the new one, never a
/// partial write. Creates the parent directory. The temporary file is
/// removed if the rename fails.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let tmp = path.with_file_name(format!(
        "{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(&tmp, e));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(path, e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temporaries() {
        let dir = std::env::temp_dir().join(format!("mmm-fsutil-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested").join("f.json");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("f.json")]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
