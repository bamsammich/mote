//! The [`Store`] — a single SQLite-backed persistent store.
//!
//! Each [`Store`] owns one `SQLite` connection (wrapped in an `Arc<Mutex<_>>` so
//! [`Namespace`] handles can be cloned freely and used from multiple threads).
//!
//! On open:
//! 1. WAL journal mode is enabled.
//! 2. Sane pragmas are set (`foreign_keys`, `synchronous = NORMAL`,
//!    `temp_store = MEMORY`, `mmap_size`).
//! 3. The migration runner is invoked to bring the schema to the latest version.
//!
//! # In-memory stores
//!
//! Pass `":memory:"` (or call [`Store::open_in_memory`]) to get an in-memory
//! database that lives only for the lifetime of the `Store`. Useful for tests
//! and for scratch space that must not persist.

use std::path::Path;
use std::sync::{Arc, Mutex};

use mote_types::PluginName;
use rusqlite::Connection;

use crate::error::StorageError;
use crate::migrations;
use crate::namespace::{IdentityScope, Namespace};

/// A single SQLite-backed persistent store with WAL mode and managed migrations.
///
/// The `Store` is cheap to clone — all clones share the same underlying
/// connection through `Arc<Mutex<Connection>>`.
#[derive(Debug, Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (or creates) a store at `path`.
    ///
    /// Enables WAL mode, sets recommended pragmas, and runs all pending
    /// migrations before returning.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the file cannot be opened, the pragmas
    /// cannot be set, or a migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path).map_err(StorageError::Sqlite)?;
        Self::configure_and_migrate(conn)
    }

    /// Opens an in-memory store that exists only for the lifetime of this
    /// `Store` value.
    ///
    /// Behaves identically to [`open`](Store::open) in every other respect.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if migration fails (should be unreachable for
    /// a freshly created in-memory database with valid migrations).
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory().map_err(StorageError::Sqlite)?;
        Self::configure_and_migrate(conn)
    }

    /// Returns a [`Namespace`] handle scoped to `(plugin, scope)`.
    ///
    /// The handle borrows the store's underlying connection by reference count.
    /// Multiple namespace handles may coexist and operate concurrently (each
    /// call acquires the mutex for the duration of the individual SQL statement).
    #[must_use]
    pub fn namespace(&self, plugin: &PluginName, scope: IdentityScope) -> Namespace {
        Namespace::new(Arc::clone(&self.conn), plugin, scope)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn configure_and_migrate(mut conn: Connection) -> Result<Self, StorageError> {
        apply_pragmas(&conn)?;
        migrations::run(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

/// Applies WAL mode and recommended operational pragmas.
fn apply_pragmas(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        PRAGMA mmap_size = 134217728;   -- 128 MiB read via mmap
        ",
    )
    .map_err(StorageError::Sqlite)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mote_types::IdentityId;

    use super::*;
    use crate::namespace::IdentityScope;

    fn plugin(name: &str) -> PluginName {
        PluginName::new(name).unwrap()
    }

    // -----------------------------------------------------------------------
    // WAL mode
    // -----------------------------------------------------------------------

    #[test]
    fn wal_mode_is_set_on_file_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = Store::open(&path).unwrap();

        // Query journal_mode on the live connection.
        let mode: String = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal", "expected WAL mode, got {mode:?}");
    }

    #[test]
    fn wal_mode_is_set_on_in_memory_db() {
        // In-memory DBs don't use WAL (SQLite ignores the pragma for :memory:
        // and returns "memory"). This test documents the actual behaviour so we
        // don't make incorrect claims in comments.
        let store = Store::open_in_memory().unwrap();
        let mode: String = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        // SQLite silently uses "memory" for :memory: dbs — this is expected.
        assert!(
            mode == "wal" || mode == "memory",
            "unexpected journal mode: {mode:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Namespace convenience
    // -----------------------------------------------------------------------

    #[test]
    fn store_namespace_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let ns = store.namespace(&plugin("test-plugin"), IdentityScope::Global);
        ns.set("foo", b"bar").unwrap();
        assert_eq!(ns.get("foo").unwrap().as_deref(), Some(b"bar".as_slice()));
    }

    // -----------------------------------------------------------------------
    // Persistence across re-open
    // -----------------------------------------------------------------------

    #[test]
    fn data_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist.db");

        {
            let store = Store::open(&path).unwrap();
            let ns = store.namespace(
                &plugin("persistent-plugin"),
                IdentityScope::PerIdentity(IdentityId::new(42)),
            );
            ns.set("durable", b"yes").unwrap();
        } // store (and therefore connection) dropped here

        // Reopen the same file.
        let store2 = Store::open(&path).unwrap();
        let ns2 = store2.namespace(
            &plugin("persistent-plugin"),
            IdentityScope::PerIdentity(IdentityId::new(42)),
        );
        assert_eq!(
            ns2.get("durable").unwrap().as_deref(),
            Some(b"yes".as_slice()),
            "data should survive closing and reopening the store"
        );
    }

    // -----------------------------------------------------------------------
    // Migration idempotency via Store (integration level)
    // -----------------------------------------------------------------------

    #[test]
    fn opening_already_migrated_db_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idempotent.db");

        // First open applies migrations.
        let _ = Store::open(&path).unwrap();
        // Second open must succeed without error.
        let _ = Store::open(&path).unwrap();

        // Verify the file is non-empty (the DB exists).
        let meta = fs::metadata(&path).unwrap();
        assert!(meta.len() > 0);
    }
}
