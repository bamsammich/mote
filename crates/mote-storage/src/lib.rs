//! SQLite-backed persistence primitives for Mote.
//!
//! The single owner of all `rusqlite` access: connection pooling, WAL mode,
//! migrations, and the storage-namespace abstraction. Hands out per-plugin
//! namespaces (partitioned per-identity when `identity_scope = per_identity`),
//! and backs plugin persistent storage, the audit-log sink, the plugin cache
//! index, and session state. Centralizing the `SQLite` dependency keeps
//! schema/migration discipline in one place.
//!
//! This crate is a stub awaiting the `1A.4` implementation wave; the heavy
//! `rusqlite` dependency lands with that work.
