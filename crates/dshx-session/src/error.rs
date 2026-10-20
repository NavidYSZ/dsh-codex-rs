//! Typed failures of the session store, mirroring dsh's persistence error vocabulary.
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Filesystem failure; the path names the file or directory involved.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Committed bytes that cannot be interpreted (dsh `SessionPersistenceCorruptionError`).
    #[error("corrupt session log: {0}")]
    Corrupt(String),
    /// A format version or required event this build does not read
    /// (dsh `SessionFormatUnsupportedError`): never "repair" such a log.
    #[error("unsupported session log: {0}")]
    Unsupported(String),
    #[error("session \"{0}\" not found")]
    NotFound(String),
    #[error("session \"{0}\" already exists")]
    AlreadyExists(String),
    /// Another process (or handle) holds the session's write lock (dsh `SessionAlreadyOwnedError`).
    #[error("session \"{0}\" is already owned by another writer")]
    Locked(String),
    /// An event the official dsh would refuse to load; nothing was appended.
    #[error("invalid session event: {0}")]
    InvalidEvent(String),
    /// The handle was closed, or an earlier write failure left it unusable.
    #[error("session \"{0}\" is closed")]
    Closed(String),
}

impl SessionError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        SessionError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = SessionError> = std::result::Result<T, E>;

/// Attaches a path to `std::io::Result`s.
pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| SessionError::io(path, e))
    }
}
