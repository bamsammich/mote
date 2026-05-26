//! Error types for `mote-audit`.

use thiserror::Error;

/// All errors that can be produced by `mote-audit`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuditError {
    /// A [`mote_storage`] operation failed.
    #[error("storage error: {0}")]
    Storage(#[from] mote_storage::StorageError),

    /// JSON serialization or deserialization of an [`crate::AuditEvent`]
    /// failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The audit thread panicked or was otherwise lost.
    ///
    /// This is a fatal condition; the `AuditLog` should be dropped.
    #[error("audit thread failed: {0}")]
    ThreadFailed(String),

    /// [`AuditLog::shutdown`](crate::AuditLog::shutdown) was called more than
    /// once, or the thread had already exited.
    #[error("audit log is already shut down")]
    AlreadyShutDown,
}
