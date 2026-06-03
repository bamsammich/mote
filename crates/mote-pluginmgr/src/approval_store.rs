//! Persisted per-plugin approval state.
//!
//! [`ApprovalStore`] records the last-approved [`ApprovalHash`] for each
//! plugin in a reserved [`mote_storage::Namespace`]. On every load or reload
//! the plugin manager reads the stored hash, compares it against the
//! candidate manifest's hash, and decides whether re-approval is required
//! (DISCIPLINES §9; plan §6.2).
//!
//! ## Storage layout
//!
//! Entries are stored in a single global namespace (the approval record is
//! per-plugin code, not per-identity — the same manifest is approved for all
//! identities; see R8 in `03-risks.md`). The reserved "plugin name" used to
//! open the namespace is [`STORE_PLUGIN_NAME`]; the identity scope is
//! [`mote_storage::IdentityScope::Global`], which is the cross-identity,
//! shared scope — exactly right for an approval record that is
//! identity-independent.
//!
//! Each entry is stored under the plugin's canonical [`PluginName`] as key,
//! serialised to JSON via a thin versioned wrapper ([`StoredEntry`]) so the
//! on-disk format can evolve without breaking existing approval records
//! (DISCIPLINES §2).

use mote_runtime::ApprovalHash;
use mote_storage::{IdentityScope, Store};
use mote_types::PluginName;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The reserved plugin-namespace name used to open the global approval store.
///
/// This is not a real plugin; it is a Mote-internal reservation. The name is
/// intentionally prefixed with `mote-` to stay in the reserved namespace
/// (DESIGN §Reserved names). It must be a valid [`PluginName`].
const STORE_PLUGIN_NAME: &str = "mote-approval-store";

/// Current schema version embedded in every serialised entry.
const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when reading from or writing to the approval store.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ApprovalStoreError {
    /// The underlying storage layer returned an error.
    #[error("storage error: {0}")]
    Storage(#[from] mote_storage::StorageError),

    /// The serialised entry could not be decoded.
    #[error("approval store entry does not parse: {0}")]
    Decode(#[from] serde_json::Error),

    /// The stored entry uses a schema version this code does not understand.
    #[error(
        "approval store entry for plugin '{plugin}' has schema version {found}, \
         expected {SCHEMA_VERSION}: forward-migration required"
    )]
    UnknownSchemaVersion {
        /// The plugin whose entry has an unexpected schema version.
        plugin: String,
        /// The schema version number found in the stored bytes.
        found: u32,
    },

    /// A stored key is not a valid [`PluginName`].
    ///
    /// This should not occur in normal operation; it indicates store
    /// corruption or a key written by a different version of Mote.
    #[error("stored approval key '{key}' is not a valid plugin name: {reason}")]
    InvalidStoredKey {
        /// The raw key string that failed validation.
        key: String,
        /// The validation error message.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// On-disk format
// ---------------------------------------------------------------------------

/// A versioned wrapper around the stored [`ApprovalHash`].
///
/// The `schema_version` field allows future migrations without silent data
/// corruption: if a future Mote version needs to change the layout, it can
/// increment the version and migrate on read (DISCIPLINES §2).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StoredEntry {
    /// Incremented whenever the JSON layout of this struct changes in a
    /// backward-incompatible way. Currently `1`.
    schema_version: u32,
    /// The last-approved hash for this plugin.
    hash: ApprovalHash,
}

// ---------------------------------------------------------------------------
// ApprovalStore
// ---------------------------------------------------------------------------

/// Persists the last-approved [`ApprovalHash`] for each plugin.
///
/// Backed by a [`mote_storage::Store`] (`SQLite`, WAL mode). All entries are
/// stored in a single global namespace — approval is per-plugin code, not
/// per-identity (R8).
///
/// # Usage
///
/// ```ignore
/// let store = Store::open_in_memory()?;
/// let approval = ApprovalStore::new(&store);
///
/// let plugin = PluginName::new("adblock").unwrap();
/// let hash   = ApprovalHash::of(&manifest);
///
/// approval.put(&plugin, &hash)?;
/// assert_eq!(approval.get(&plugin)?, Some(hash));
/// ```
#[derive(Debug)]
pub struct ApprovalStore {
    namespace: mote_storage::Namespace,
}

impl ApprovalStore {
    /// Creates an [`ApprovalStore`] backed by `store`.
    ///
    /// Uses a single reserved global namespace; the underlying connection is
    /// shared with the caller's `Store` (cheap clone via `Arc`).
    ///
    /// # Panics
    ///
    /// Panics if [`STORE_PLUGIN_NAME`] is not a valid [`PluginName`]. This
    /// constant is validated at compile time and the panic should be
    /// unreachable in practice; it guards against accidental constant changes.
    #[must_use]
    pub fn new(store: &Store) -> Self {
        // STORE_PLUGIN_NAME is a compile-time constant validated at startup.
        // It deliberately lives in the reserved `mote-*` namespace (an
        // internal Mote pseudo-plugin for the per-identity approval store),
        // so it must use `new_internal` which skips the user-namespace
        // reservation. Panic on invalid is the intended "programmer error"
        // guard at initialisation time.
        let reserved = PluginName::new_internal(STORE_PLUGIN_NAME)
            .expect("STORE_PLUGIN_NAME is a valid PluginName");
        Self {
            namespace: store.namespace(&reserved, IdentityScope::Global),
        }
    }

    /// Retrieves the last-approved [`ApprovalHash`] for `plugin`, or `None`
    /// if no approval has been recorded yet.
    ///
    /// A `None` result means this is the plugin's first install: a full
    /// approval dialog is required before it can load.
    ///
    /// # Errors
    ///
    /// Returns [`ApprovalStoreError`] if the storage layer fails or if the
    /// stored bytes cannot be decoded (corrupt or from an unknown schema
    /// version).
    pub fn get(&self, plugin: &PluginName) -> Result<Option<ApprovalHash>, ApprovalStoreError> {
        let Some(bytes) = self.namespace.get(plugin.as_str())? else {
            return Ok(None);
        };
        let entry: StoredEntry = serde_json::from_slice(&bytes)?;
        if entry.schema_version != SCHEMA_VERSION {
            return Err(ApprovalStoreError::UnknownSchemaVersion {
                plugin: plugin.as_str().to_owned(),
                found: entry.schema_version,
            });
        }
        Ok(Some(entry.hash))
    }

    /// Records (or replaces) the approved [`ApprovalHash`] for `plugin`.
    ///
    /// Call this after the user grants approval in the install/update dialog,
    /// or after `mote plugin review` / `mote plugin pin` approves headlessly.
    ///
    /// # Errors
    ///
    /// Returns [`ApprovalStoreError`] if serialisation or the storage layer
    /// fails.
    pub fn put(
        &self,
        plugin: &PluginName,
        approved: &ApprovalHash,
    ) -> Result<(), ApprovalStoreError> {
        let entry = StoredEntry {
            schema_version: SCHEMA_VERSION,
            hash: approved.clone(),
        };
        let bytes = serde_json::to_vec(&entry)?;
        self.namespace.set(plugin.as_str(), &bytes)?;
        Ok(())
    }

    /// Removes the approval record for `plugin`.
    ///
    /// Returns `true` if an entry existed and was removed, `false` if no
    /// entry was present (i.e. the plugin was never approved).
    ///
    /// Use this when revoking a plugin: removing its approval ensures that if
    /// it is re-installed later, the full approval dialog will be shown again.
    ///
    /// # Errors
    ///
    /// Returns [`ApprovalStoreError`] if the storage layer fails.
    pub fn remove(&self, plugin: &PluginName) -> Result<bool, ApprovalStoreError> {
        // Check existence first so we can return an accurate bool without a
        // second round-trip: get + delete is two SQL statements but both are
        // fast indexed lookups.
        let existed = self.namespace.get(plugin.as_str())?.is_some();
        if existed {
            self.namespace.delete(plugin.as_str())?;
        }
        Ok(existed)
    }

    /// Lists the [`PluginName`]s of all plugins that have a stored approval
    /// record.
    ///
    /// The returned list is in lexicographic order (inherited from
    /// [`mote_storage::Namespace::list_keys`]).
    ///
    /// # Errors
    ///
    /// Returns [`ApprovalStoreError`] if the storage layer fails or if any
    /// key is not a valid [`PluginName`].
    pub fn list(&self) -> Result<Vec<PluginName>, ApprovalStoreError> {
        let keys = self.namespace.list_keys()?;
        keys.into_iter()
            .map(|k| {
                PluginName::new(&k).map_err(|e| ApprovalStoreError::InvalidStoredKey {
                    key: k.clone(),
                    reason: e.to_string(),
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use mote_lua::{IdentityScope as LuaScope, Manifest, load_plugin};
    use mote_storage::Store;

    use super::*;

    fn make_manifest(src: &str) -> Manifest {
        load_plugin(src, "test").unwrap().manifest().clone()
    }

    fn plugin(name: &str) -> PluginName {
        PluginName::new(name).unwrap()
    }

    const BASE_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "adblock",
            version = "1",
            permissions = { "storage:persistent", "tabs:list" },
            identity_scope = "global",
        }
        return M
    "#;

    const EXPANDED_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "adblock",
            version = "2",
            permissions = { "storage:persistent", "tabs:list", "history:read" },
            identity_scope = "global",
        }
        return M
    "#;

    const SCOPED_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "adblock",
            version = "3",
            permissions = { "storage:persistent", "tabs:list" },
            identity_scope = "per_identity",
        }
        return M
    "#;

    // -----------------------------------------------------------------------
    // Basic round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn put_then_get_round_trips_hash() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("adblock");
        let hash = ApprovalHash::of(&make_manifest(BASE_SRC));

        approval.put(&p, &hash).unwrap();
        let retrieved = approval.get(&p).unwrap();

        assert_eq!(
            retrieved,
            Some(hash),
            "get must return exactly the hash that was put"
        );
    }

    #[test]
    fn put_then_get_preserves_identity_scope() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("adblock");
        let hash = ApprovalHash::of(&make_manifest(BASE_SRC));

        approval.put(&p, &hash).unwrap();
        let retrieved = approval.get(&p).unwrap().unwrap();

        assert_eq!(
            retrieved.identity_scope(),
            Some(LuaScope::Global),
            "identity_scope must survive the round-trip"
        );
    }

    // -----------------------------------------------------------------------
    // Absent key
    // -----------------------------------------------------------------------

    #[test]
    fn get_absent_plugin_returns_none() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("never-approved");

        assert_eq!(
            approval.get(&p).unwrap(),
            None,
            "absent plugin must return None"
        );
    }

    // -----------------------------------------------------------------------
    // Remove
    // -----------------------------------------------------------------------

    #[test]
    fn remove_returns_true_when_entry_existed() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("adblock");
        let hash = ApprovalHash::of(&make_manifest(BASE_SRC));

        approval.put(&p, &hash).unwrap();
        assert!(
            approval.remove(&p).unwrap(),
            "remove must return true when the entry existed"
        );
        assert_eq!(
            approval.get(&p).unwrap(),
            None,
            "entry must be absent after remove"
        );
    }

    #[test]
    fn remove_returns_false_when_entry_absent() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("never-approved");

        assert!(
            !approval.remove(&p).unwrap(),
            "remove must return false when no entry existed"
        );
    }

    // -----------------------------------------------------------------------
    // Persistence across re-open (on-disk store)
    // -----------------------------------------------------------------------

    #[test]
    fn entries_persist_across_store_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("approval.db");

        let hash = ApprovalHash::of(&make_manifest(BASE_SRC));
        let p = plugin("adblock");

        // Write using the first store handle.
        {
            let store = Store::open(&db_path).unwrap();
            let approval = ApprovalStore::new(&store);
            approval.put(&p, &hash).unwrap();
        }

        // Re-open and verify the entry survived.
        {
            let store2 = Store::open(&db_path).unwrap();
            let approval2 = ApprovalStore::new(&store2);
            assert_eq!(
                approval2.get(&p).unwrap(),
                Some(hash),
                "entry must survive closing and reopening the store"
            );
        }
    }

    // -----------------------------------------------------------------------
    // R8: global approval — same record regardless of identity context
    // -----------------------------------------------------------------------

    #[test]
    fn approval_is_global_not_per_identity() {
        // Approval state is stored in a single global namespace. Even if the
        // ApprovalStore is constructed from different Store handles (or on
        // behalf of different identities), the same plugin name resolves to
        // the same record.
        let store = Store::open_in_memory().unwrap();
        let approval_a = ApprovalStore::new(&store);
        let approval_b = ApprovalStore::new(&store);

        let p = plugin("adblock");
        let hash = ApprovalHash::of(&make_manifest(BASE_SRC));

        approval_a.put(&p, &hash).unwrap();

        // A second handle to the same store must see the same record,
        // proving there is no per-identity partition.
        assert_eq!(
            approval_b.get(&p).unwrap(),
            Some(hash),
            "approval must be retrievable through any handle to the same store"
        );
    }

    // -----------------------------------------------------------------------
    // put replaces an existing entry
    // -----------------------------------------------------------------------

    #[test]
    fn put_replaces_previous_approval() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("adblock");

        let hash_v1 = ApprovalHash::of(&make_manifest(BASE_SRC));
        let hash_v2 = ApprovalHash::of(&make_manifest(EXPANDED_SRC));

        approval.put(&p, &hash_v1).unwrap();
        approval.put(&p, &hash_v2).unwrap();

        assert_eq!(
            approval.get(&p).unwrap(),
            Some(hash_v2),
            "put must overwrite the previous approval hash"
        );
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------

    #[test]
    fn list_returns_all_approved_plugins_sorted() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);

        let hash = ApprovalHash::of(&make_manifest(BASE_SRC));
        for name in ["zeta-plugin", "alpha-plugin", "mid-plugin"] {
            approval.put(&plugin(name), &hash).unwrap();
        }

        let listed = approval.list().unwrap();
        let names: Vec<&str> = listed.iter().map(PluginName::as_str).collect();

        assert_eq!(
            names,
            vec!["alpha-plugin", "mid-plugin", "zeta-plugin"],
            "list must return plugins in lexicographic order"
        );
    }

    #[test]
    fn list_empty_when_no_approvals() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        assert!(
            approval.list().unwrap().is_empty(),
            "list must be empty when no approvals have been recorded"
        );
    }

    // -----------------------------------------------------------------------
    // Expanded hash: accessors survive round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn all_field_lists_survive_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        let p = plugin("adblock");
        let scoped = ApprovalHash::of(&make_manifest(SCOPED_SRC));

        approval.put(&p, &scoped).unwrap();
        let back = approval.get(&p).unwrap().unwrap();

        assert_eq!(
            back.permissions(),
            scoped.permissions(),
            "permissions must survive the store round-trip"
        );
        assert_eq!(
            back.capabilities(),
            scoped.capabilities(),
            "capabilities must survive the store round-trip"
        );
        assert_eq!(
            back.consumes(),
            scoped.consumes(),
            "consumes must survive the store round-trip"
        );
        assert_eq!(
            back.identity_scope(),
            Some(LuaScope::PerIdentity),
            "identity_scope must survive the store round-trip"
        );
    }
}
