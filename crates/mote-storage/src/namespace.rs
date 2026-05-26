//! Per-plugin, identity-scoped key-value storage.
//!
//! A [`Namespace`] is a scoped handle into the `plugin_storage` table. The
//! scope is determined by the plugin name and the [`IdentityScope`]:
//!
//! - **[`IdentityScope::Global`]** — one namespace shared across all identities
//!   for this plugin. The `identity_key` column is set to the literal string
//!   `"global"`.
//! - **[`IdentityScope::PerIdentity`]** — one namespace per `(plugin, identity)`
//!   pair. The `identity_key` column is set to the decimal representation of the
//!   [`IdentityId`]. A plugin in one identity's namespace cannot read or write
//!   another identity's data.
//!
//! Namespace isolation is enforced by the SQL queries — every read/write
//! includes both `plugin_name` and `identity_key` in the `WHERE` clause, so a
//! plugin cannot accidentally (or intentionally) reach another plugin's rows or
//! another identity's rows.

use std::sync::{Arc, Mutex};

use mote_types::{IdentityId, PluginName};
use rusqlite::Connection;

use crate::error::StorageError;

/// Determines whether a plugin's storage is shared across all identities or
/// isolated per identity.
///
/// Mirrors the `identity_scope` manifest field described in
/// DESIGN §Plugin Identity Scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityScope {
    /// One storage namespace per `(plugin, identity)` pair.
    ///
    /// A plugin with this scope running under identity A cannot read data
    /// written by the same plugin running under identity B.
    PerIdentity(IdentityId),

    /// One storage namespace shared across all identities for this plugin.
    ///
    /// Appropriate for purely behavioural plugins (ad-blocker, vim mode) that
    /// need no per-identity isolation, and for password managers that maintain
    /// a single vault and filter at query time.
    Global,
}

impl IdentityScope {
    /// Returns the string used as the `identity_key` column value for this
    /// scope.
    fn identity_key(&self) -> String {
        match self {
            Self::PerIdentity(id) => id.get().to_string(),
            Self::Global => "global".to_owned(),
        }
    }
}

/// A scoped key-value handle for a single `(plugin, identity-scope)` pair.
///
/// Obtain a `Namespace` from [`Store::namespace`](crate::Store::namespace).
/// All operations are synchronous and hold the connection mutex only for the
/// duration of the individual SQL call.
///
/// Keys and values are arbitrary byte sequences (stored as `SQLite` `BLOB`).
/// String convenience wrappers treat strings as their UTF-8 byte representation.
#[derive(Debug, Clone)]
pub struct Namespace {
    conn: Arc<Mutex<Connection>>,
    plugin_name: String,
    identity_key: String,
}

impl Namespace {
    pub(crate) fn new(
        conn: Arc<Mutex<Connection>>,
        plugin: &PluginName,
        scope: IdentityScope,
    ) -> Self {
        Self {
            conn,
            plugin_name: plugin.as_str().to_owned(),
            identity_key: scope.identity_key(),
        }
    }

    /// Stores `value` under `key`.
    ///
    /// Overwrites any existing value for the same key.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQL statement fails.
    pub fn set(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StorageError::Internal("mutex poisoned".into()))?;
        let result = conn
            .execute(
                "INSERT INTO plugin_storage (plugin_name, identity_key, key, value)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (plugin_name, identity_key, key)
             DO UPDATE SET value = excluded.value",
                rusqlite::params![self.plugin_name, self.identity_key, key, value],
            )
            .map_err(StorageError::Sqlite);
        drop(conn);
        result.map(|_| ())
    }

    /// Retrieves the raw bytes stored under `key`, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQL statement fails.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StorageError::Internal("mutex poisoned".into()))?;
        let result = conn.query_row(
            "SELECT value FROM plugin_storage
             WHERE plugin_name = ?1 AND identity_key = ?2 AND key = ?3",
            rusqlite::params![self.plugin_name, self.identity_key, key],
            |row| row.get::<_, Vec<u8>>(0),
        );
        drop(conn);

        match result {
            Ok(bytes) => Ok(Some(bytes)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::Sqlite(e)),
        }
    }

    /// Removes the entry for `key`.
    ///
    /// If the key is absent, this is a no-op (not an error).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQL statement fails.
    pub fn delete(&self, key: &str) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StorageError::Internal("mutex poisoned".into()))?;
        let result = conn
            .execute(
                "DELETE FROM plugin_storage
             WHERE plugin_name = ?1 AND identity_key = ?2 AND key = ?3",
                rusqlite::params![self.plugin_name, self.identity_key, key],
            )
            .map_err(StorageError::Sqlite);
        drop(conn);
        result.map(|_| ())
    }

    /// Returns all keys stored in this namespace, in lexicographic order.
    ///
    /// Keys belonging to other plugins or other identities are never included.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQL statement fails.
    pub fn list_keys(&self) -> Result<Vec<String>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StorageError::Internal("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT key FROM plugin_storage
                 WHERE plugin_name = ?1 AND identity_key = ?2
                 ORDER BY key ASC",
            )
            .map_err(StorageError::Sqlite)?;

        let keys = stmt
            .query_map(
                rusqlite::params![self.plugin_name, self.identity_key],
                |row| row.get::<_, String>(0),
            )
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)?;

        drop(stmt);
        drop(conn);
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::migrations;

    fn open_conn() -> Arc<Mutex<Connection>> {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn plugin(name: &str) -> PluginName {
        PluginName::new(name).unwrap()
    }

    // -----------------------------------------------------------------------
    // Basic round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn set_get_round_trip() {
        let conn = open_conn();
        let ns = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::Global,
        );

        ns.set("greeting", b"hello").unwrap();
        let value = ns.get("greeting").unwrap();
        assert_eq!(value.as_deref(), Some(b"hello".as_slice()));
    }

    #[test]
    fn get_absent_key_returns_none() {
        let conn = open_conn();
        let ns = Namespace::new(conn, &plugin("my-plugin"), IdentityScope::Global);
        assert_eq!(ns.get("missing").unwrap(), None);
    }

    #[test]
    fn set_overwrites_existing_value() {
        let conn = open_conn();
        let ns = Namespace::new(conn, &plugin("my-plugin"), IdentityScope::Global);
        ns.set("k", b"v1").unwrap();
        ns.set("k", b"v2").unwrap();
        assert_eq!(ns.get("k").unwrap().as_deref(), Some(b"v2".as_slice()));
    }

    #[test]
    fn delete_removes_key() {
        let conn = open_conn();
        let ns = Namespace::new(conn, &plugin("my-plugin"), IdentityScope::Global);
        ns.set("bye", b"data").unwrap();
        ns.delete("bye").unwrap();
        assert_eq!(ns.get("bye").unwrap(), None);
    }

    #[test]
    fn delete_absent_key_is_noop() {
        let conn = open_conn();
        let ns = Namespace::new(conn, &plugin("my-plugin"), IdentityScope::Global);
        // Must not error.
        ns.delete("nonexistent").unwrap();
    }

    #[test]
    fn list_keys_returns_sorted_keys_in_scope() {
        let conn = open_conn();
        let ns = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::Global,
        );
        ns.set("c", b"3").unwrap();
        ns.set("a", b"1").unwrap();
        ns.set("b", b"2").unwrap();
        let keys = ns.list_keys().unwrap();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn list_keys_empty_when_no_entries() {
        let conn = open_conn();
        let ns = Namespace::new(conn, &plugin("my-plugin"), IdentityScope::Global);
        assert!(ns.list_keys().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // Plugin-level isolation
    // -----------------------------------------------------------------------

    #[test]
    fn plugin_a_cannot_read_plugin_b_keys() {
        let conn = open_conn();
        let ns_a = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-a"),
            IdentityScope::Global,
        );
        let ns_b = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-b"),
            IdentityScope::Global,
        );

        ns_a.set("secret", b"plugin-a-secret").unwrap();

        // Plugin B must not see plugin A's key.
        assert_eq!(ns_b.get("secret").unwrap(), None);
        assert!(ns_b.list_keys().unwrap().is_empty());
    }

    #[test]
    fn plugin_b_cannot_overwrite_plugin_a_key() {
        let conn = open_conn();
        let ns_a = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-a"),
            IdentityScope::Global,
        );
        let ns_b = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-b"),
            IdentityScope::Global,
        );

        ns_a.set("key", b"original").unwrap();
        ns_b.set("key", b"overwrite").unwrap();

        // Plugin A still sees its original value.
        assert_eq!(
            ns_a.get("key").unwrap().as_deref(),
            Some(b"original".as_slice())
        );
    }

    // -----------------------------------------------------------------------
    // Per-identity isolation
    // -----------------------------------------------------------------------

    #[test]
    fn per_identity_isolates_different_identities() {
        let id1 = IdentityId::new(1);
        let id2 = IdentityId::new(2);
        let conn = open_conn();
        let ns1 = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::PerIdentity(id1),
        );
        let ns2 = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::PerIdentity(id2),
        );

        ns1.set("k", b"from-identity-1").unwrap();

        // Identity 2 must not see identity 1's data.
        assert_eq!(ns2.get("k").unwrap(), None);
        assert!(ns2.list_keys().unwrap().is_empty());
    }

    #[test]
    fn global_namespace_is_shared_across_identities() {
        let id1 = IdentityId::new(1);
        let id2 = IdentityId::new(2);
        let conn = open_conn();

        // Write under id1 using global scope.
        let ns_writer = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            // A global-scoped namespace ignores the identity; even if the
            // IdentityScope::Global is constructed from different call-sites
            // (one per identity), both resolve to the same storage row.
            IdentityScope::Global,
        );

        // Read using a separate Namespace handle that represents "running
        // under identity 2 but global scope".
        let ns_reader = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::Global,
        );

        ns_writer.set("shared", b"value").unwrap();

        // Both identity contexts see the shared value.
        assert_eq!(
            ns_reader.get("shared").unwrap().as_deref(),
            Some(b"value".as_slice())
        );

        // Confirm identities are irrelevant for global scope by also checking
        // that a per-identity scope for id1 and id2 does NOT see the global value.
        let ns_id1 = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::PerIdentity(id1),
        );
        let ns_id2 = Namespace::new(
            Arc::clone(&conn),
            &plugin("my-plugin"),
            IdentityScope::PerIdentity(id2),
        );
        assert_eq!(ns_id1.get("shared").unwrap(), None);
        assert_eq!(ns_id2.get("shared").unwrap(), None);
    }

    #[test]
    fn list_keys_scoped_to_namespace_only() {
        let conn = open_conn();
        let id1 = IdentityId::new(1);
        let id2 = IdentityId::new(2);

        let global_a = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-a"),
            IdentityScope::Global,
        );
        let scoped_a = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-a"),
            IdentityScope::PerIdentity(id1),
        );
        let scoped_b = Namespace::new(
            Arc::clone(&conn),
            &plugin("plugin-b"),
            IdentityScope::PerIdentity(id2),
        );

        global_a.set("global-key", b"g").unwrap();
        scoped_a.set("id1-key", b"i1").unwrap();
        scoped_b.set("b-id2-key", b"bi2").unwrap();

        // plugin-a global only sees its own key.
        assert_eq!(global_a.list_keys().unwrap(), vec!["global-key"]);
        // plugin-a id1 sees only its own key.
        assert_eq!(scoped_a.list_keys().unwrap(), vec!["id1-key"]);
        // plugin-b id2 sees only its own key.
        assert_eq!(scoped_b.list_keys().unwrap(), vec!["b-id2-key"]);
    }
}
