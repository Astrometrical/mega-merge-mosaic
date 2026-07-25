//! Crate-wide error type: every fallible operation reports the file it was
//! working on plus either the underlying I/O error or a human-readable reason.

use std::path::PathBuf;

/// Crate-wide result alias over [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The error type for all mmm-core operations.
///
/// Deliberately small: everything the pipeline does is either an I/O failure
/// on a specific file or a "this file/value is not what it must be" condition,
/// and both carry the offending path so CLI users can act on the message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An operating-system I/O failure on `path`.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// The file or directory the operation was working on.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// `path` exists but its contents are unsupported, malformed, or
    /// inconsistent with what the pipeline requires; `reason` says how.
    #[error("unsupported or malformed file {path}: {reason}")]
    Format {
        /// The offending file (or context path for non-file validation).
        path: PathBuf,
        /// Human-readable explanation of what is wrong.
        reason: String,
    },
}

impl Error {
    /// Wrap an [`std::io::Error`] with the path it occurred on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    /// Build a [`Error::Format`] from a path and a reason.
    pub fn format(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Error::Format {
            path: path.into(),
            reason: reason.into(),
        }
    }
}
