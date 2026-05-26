//! Error types for `mote-storage`.

use thiserror::Error;

/// All errors that can be produced by `mote-storage`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    /// A `rusqlite` operation failed.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A migration failed to apply.
    #[error("migration {version} failed: {source}")]
    Migration {
        /// The version number of the migration that failed.
        version: u32,
        /// The underlying `SQLite` error.
        #[source]
        source: rusqlite::Error,
    },

    /// The stored schema version is newer than what this binary knows about.
    ///
    /// This happens when a newer Mote binary wrote to the database and an older
    /// binary tries to open it. The forward-compatibility guarantee only covers
    /// reads of *older* schemas by *newer* binaries.
    #[error("database schema version {found} is newer than the latest known migration {latest}")]
    SchemaTooNew {
        /// Schema version found in the database.
        found: u32,
        /// Latest migration version this binary knows.
        latest: u32,
    },

    /// An internal invariant was violated (should never happen in production).
    #[error("internal storage error: {0}")]
    Internal(String),
}
