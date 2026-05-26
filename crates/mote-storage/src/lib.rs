//! SQLite-backed persistence primitives for Mote.
//!
//! `mote-storage` is the **single owner** of all `rusqlite` access in the
//! workspace. No other crate may depend on `rusqlite` directly — they reach
//! `SQLite` exclusively through the types exported here.
//!
//! # Core concepts
//!
//! ## [`Store`]
//!
//! A [`Store`] wraps a single `SQLite` connection.  On open it:
//! 1. Enables **WAL journal mode** for concurrent-reader durability.
//! 2. Applies sane operational pragmas (`foreign_keys`, `synchronous = NORMAL`,
//!    `temp_store = MEMORY`, `mmap_size`).
//! 3. Runs the **migration runner** to bring the schema to the latest version.
//!
//! ```no_run
//! use mote_storage::Store;
//!
//! let store = Store::open("/path/to/plugin-data.db")?;
//! # Ok::<(), mote_storage::StorageError>(())
//! ```
//!
//! ## [`Namespace`]
//!
//! A [`Namespace`] is a scoped key-value handle over the `plugin_storage`
//! table, identified by a `(plugin_name, identity_key)` pair. Isolation is
//! enforced at the SQL layer — every read/write includes both columns in its
//! `WHERE` clause.
//!
//! ```no_run
//! use mote_storage::{IdentityScope, Store};
//! use mote_types::PluginName;
//!
//! let store = Store::open_in_memory()?;
//! let plugin = PluginName::new("adblock")?;
//! let ns = store.namespace(&plugin, IdentityScope::Global);
//!
//! ns.set("filter-list-version", b"20260525")?;
//! let v = ns.get("filter-list-version")?; // Some(b"20260525")
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## [`IdentityScope`]
//!
//! Controls whether a plugin's storage is shared across all identities
//! ([`IdentityScope::Global`]) or isolated per-identity
//! ([`IdentityScope::PerIdentity`]). Mirrors the `identity_scope` manifest
//! field from DESIGN §Plugin Identity Scope.
//!
//! ## Migrations
//!
//! The [`migrations`] module owns the ordered list of schema migrations and the
//! runner that applies pending ones idempotently. Adding a new table to Mote's
//! storage schema means appending a new [`migrations::Migration`] entry to
//! [`migrations::MIGRATIONS`] — the runner handles the rest.

mod error;
pub mod migrations;
mod namespace;
mod store;

pub use error::StorageError;
pub use namespace::{IdentityScope, Namespace};
pub use store::Store;
