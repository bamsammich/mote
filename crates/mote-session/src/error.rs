//! Error types for `mote-session`.

use thiserror::Error;

/// All errors that `mote-session` can produce.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SessionError {
    /// A storage operation failed.
    #[error("storage error: {0}")]
    Storage(#[from] mote_storage::StorageError),

    /// A value stored in the session database could not be deserialized.
    #[error("session data corrupt at key {key:?}: {source}")]
    Corrupt {
        /// The storage key whose value was corrupt.
        key: String,
        /// The underlying deserialization error.
        #[source]
        source: serde_json::Error,
    },

    /// A tab referenced in session state was not found.
    #[error("tab {0} not found in session")]
    TabNotFound(mote_types::TabId),

    /// A workspace referenced in session state was not found.
    #[error("workspace {0} not found in session")]
    WorkspaceNotFound(mote_types::WorkspaceId),

    /// An internal invariant was violated (should never happen in production).
    #[error("internal session error: {0}")]
    Internal(String),
}
