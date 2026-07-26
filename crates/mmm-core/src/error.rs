//! Crate-wide error type: every fallible operation reports the file it was
//! working on plus either the underlying I/O error or a human-readable reason.

use std::path::PathBuf;

/// Crate-wide result alias over [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The error type for all mmm-core operations.
///
/// Deliberately small: an I/O failure on a file, a malformed/unsupported file,
/// or a computation that cannot proceed (a numerical failure, or an operation
/// the caller asked for that the data does not support).
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
        /// The offending file.
        path: PathBuf,
        /// Human-readable explanation of what is wrong.
        reason: String,
    },

    /// A computation could not be completed — a numerical failure (e.g. a
    /// singular linear system) or an operation the input does not support
    /// (e.g. refusing to flatten a signal-dominated mosaic). Carries no path
    /// because it is not about a specific file.
    #[error("{reason}")]
    Compute {
        /// Human-readable explanation of what went wrong.
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

    /// Build a [`Error::Compute`] from a reason (no path — see the variant).
    pub fn compute(reason: impl Into<String>) -> Self {
        Error::Compute {
            reason: reason.into(),
        }
    }
}
