//! Ordered, idempotent schema-migration runner.
//!
//! Migrations are numbered from 1. The runner tracks the current schema
//! version in the `_schema_version` table and applies any migration whose
//! version number is greater than the stored value. Applying the runner a
//! second time is a no-op — every migration is guarded by the version check.
//!
//! # Adding a migration
//!
//! Append a new [`Migration`] to [`MIGRATIONS`]. The version must be exactly
//! one greater than the previous entry's version. Every migration runs inside
//! its own `SAVEPOINT` so a failure leaves the database in the state it was in
//! before that migration started.

use rusqlite::Connection;

use crate::error::StorageError;

/// A single, versioned schema migration.
#[derive(Debug)]
pub struct Migration {
    /// 1-based monotonically increasing version number.
    pub version: u32,
    /// Human-readable description displayed in logs.
    pub description: &'static str,
    /// The SQL to execute when this migration is applied.
    ///
    /// May contain multiple statements separated by semicolons.
    pub sql: &'static str,
}

/// The ordered list of all migrations.
///
/// New migrations **must** be appended — never inserted in the middle or
/// renumbered. The version field must equal the 1-based index in this slice.
pub static MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    description: "create namespaced KV store",
    sql: "
            CREATE TABLE IF NOT EXISTS plugin_storage (
                plugin_name   TEXT    NOT NULL,
                identity_key  TEXT    NOT NULL,
                key           TEXT    NOT NULL,
                value         BLOB    NOT NULL,
                PRIMARY KEY (plugin_name, identity_key, key)
            ) STRICT;
        ",
}];

/// Bootstrap the `_schema_version` tracking table.
///
/// This is called before [`run`] so that the table exists and can be read.
/// It is idempotent.
pub(crate) fn bootstrap(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _schema_version (
            id      INTEGER PRIMARY KEY CHECK (id = 1),
            version INTEGER NOT NULL DEFAULT 0
        ) STRICT;
        INSERT OR IGNORE INTO _schema_version (id, version) VALUES (1, 0);",
    )
    .map_err(StorageError::Sqlite)?;
    Ok(())
}

/// Returns the current schema version stored in the database.
pub(crate) fn current_version(conn: &Connection) -> Result<u32, StorageError> {
    conn.query_row(
        "SELECT version FROM _schema_version WHERE id = 1",
        [],
        |row| row.get::<_, u32>(0),
    )
    .map_err(StorageError::Sqlite)
}

/// Applies all pending migrations in version order.
///
/// Migrations whose version is ≤ the current schema version are skipped.
/// If the stored version exceeds the latest known migration, an error is
/// returned — this protects against a newer database being opened by an older
/// binary.
///
/// Each migration runs inside a `SAVEPOINT` so a partial failure is rolled back
/// without affecting already-applied migrations.
///
/// # Errors
///
/// Returns [`StorageError::SchemaTooNew`] when the database's stored version
/// exceeds the latest migration this binary knows.  Returns
/// [`StorageError::Migration`] when a migration's SQL fails.  Returns
/// [`StorageError::Sqlite`] for any lower-level `rusqlite` error.
pub fn run(conn: &mut Connection) -> Result<(), StorageError> {
    bootstrap(conn)?;

    let current = current_version(conn)?;
    let latest = MIGRATIONS.last().map_or(0, |m| m.version);

    if current > latest {
        return Err(StorageError::SchemaTooNew {
            found: current,
            latest,
        });
    }

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let sp = format!("migration_{}", migration.version);
        conn.execute_batch(&format!("SAVEPOINT {sp};"))
            .map_err(StorageError::Sqlite)?;

        let result = conn
            .execute_batch(migration.sql)
            .map_err(|e| StorageError::Migration {
                version: migration.version,
                source: e,
            });

        if let Err(err) = result {
            let _ = conn.execute_batch(&format!("ROLLBACK TO SAVEPOINT {sp};"));
            return Err(err);
        }

        conn.execute(
            "UPDATE _schema_version SET version = ?1 WHERE id = 1",
            [migration.version],
        )
        .map_err(StorageError::Sqlite)?;

        conn.execute_batch(&format!("RELEASE {sp};"))
            .map_err(StorageError::Sqlite)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> Connection {
        Connection::open_in_memory().expect("in-memory connection")
    }

    #[test]
    fn bootstrap_is_idempotent() {
        let conn = open_in_memory();
        bootstrap(&conn).unwrap();
        // Running a second time must not error.
        bootstrap(&conn).unwrap();
        // Initial version is 0.
        assert_eq!(current_version(&conn).unwrap(), 0);
    }

    #[test]
    fn migrations_advance_version() {
        let mut conn = open_in_memory();
        run(&mut conn).unwrap();
        let expected = MIGRATIONS.last().map_or(0, |m| m.version);
        assert_eq!(current_version(&conn).unwrap(), expected);
    }

    #[test]
    fn running_twice_is_idempotent() {
        let mut conn = open_in_memory();
        run(&mut conn).unwrap();
        let after_first = current_version(&conn).unwrap();
        // Second run must be a no-op.
        run(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), after_first);
    }

    #[test]
    fn versions_are_contiguous_and_start_at_one() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                m.version,
                u32::try_from(i + 1).expect("migration index fits u32"),
                "migration at index {i} has version {} but expected {}",
                m.version,
                i + 1,
            );
        }
    }

    #[test]
    fn schema_too_new_returns_error() {
        let mut conn = open_in_memory();
        // Manually set the version to something above the highest migration.
        bootstrap(&conn).unwrap();
        conn.execute("UPDATE _schema_version SET version = 9999 WHERE id = 1", [])
            .unwrap();

        let result = run(&mut conn);
        assert!(
            matches!(result, Err(StorageError::SchemaTooNew { found: 9999, .. })),
            "expected SchemaTooNew, got {result:?}",
        );
    }
}
