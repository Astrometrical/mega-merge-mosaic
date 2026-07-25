use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unsupported or malformed file {path}: {reason}")]
    Format { path: PathBuf, reason: String },
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub fn format(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Error::Format {
            path: path.into(),
            reason: reason.into(),
        }
    }
}
