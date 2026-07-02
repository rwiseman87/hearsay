//! The orchestrator's internal error type. Surfaces to the core's [`LiveError::Internal`] (500)
//! when a lifecycle operation fails.

use hearsay_core::LiveError;

/// An unexpected failure while starting or running a meeting.
#[derive(Debug)]
pub enum OrchestratorError {
    /// A database error.
    Db(sqlx::Error),
    /// An I/O error (e.g. creating the meeting folder, spawning a sidecar).
    Io(std::io::Error),
    /// A capture source or transcriber failed to start.
    Backend(String),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrchestratorError::Db(e) => write!(f, "database error: {e}"),
            OrchestratorError::Io(e) => write!(f, "io error: {e}"),
            OrchestratorError::Backend(m) => write!(f, "backend error: {m}"),
        }
    }
}

impl std::error::Error for OrchestratorError {}

impl From<sqlx::Error> for OrchestratorError {
    fn from(err: sqlx::Error) -> Self {
        OrchestratorError::Db(err)
    }
}

impl From<std::io::Error> for OrchestratorError {
    fn from(err: std::io::Error) -> Self {
        OrchestratorError::Io(err)
    }
}

impl From<OrchestratorError> for LiveError {
    fn from(err: OrchestratorError) -> Self {
        LiveError::Internal(err.to_string())
    }
}
