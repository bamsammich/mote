//! Plugin management and provenance for Mote.
//!
//! Owns `plugins.lua` + `plugins.lock` parse/resolve; source types (`github:`,
//! `git+https://`, `path:`, `bundled`); the content-addressed cache; BLAKE3
//! directory-hash computation per the documented spec; the full `mote plugin`
//! CLI surface; capability-contract dependency resolution (dangling-consumer
//! detection per ADR-0002); the update flow with permission-change surfacing;
//! implicit-local detection; dev mode; and first-party bundled distribution.
//! Stores per-plugin approved permission/capability/consumes/`identity_scope`
//! hashes.
//!
//! # Phase 3 foundation (this wave)
//!
//! This crate currently implements the management *foundation* — the pure,
//! engine-free layer the CLI and shell wiring build on:
//!
//! - [`source`] — the [`Source`] enum and `plugins.lua` source-string grammar.
//! - [`dirhash`] — the BLAKE3 directory hash ([`hash_dir`]), the integrity
//!   anchor.
//! - [`cache`] — the content-addressed [`Cache`] and the symlink-vs-real
//!   plugins-directory scheme.
//! - [`lock`] — the `plugins.lock` ([`LockFile`]) serde + TOML model.
//! - [`bundle`] — the binary-embedded first-party bundle and its offline unpack.
//! - [`fetch`] — the [`gix`]-backed git [`fetch`](fetch()) (fetch-at-commit).

pub mod bundle;
pub mod cache;
pub mod dirhash;
pub mod fetch;
pub mod lock;
pub mod managed;
pub mod source;

pub use bundle::{BundleError, bundled_names, bundled_version, is_bundled, unpack_into_cache};
pub use cache::{Cache, CacheError, CacheKey};
pub use dirhash::{DirHashError, hash_dir};
pub use fetch::{FetchError, Fetched, fetch};
pub use lock::{LockEntry, LockError, LockFile};
pub use managed::{ManagedEntry, ManagedError, ManagedFile};
pub use source::{Source, SourceParseError};
