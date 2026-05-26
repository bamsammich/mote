//! Shared vocabulary for Mote.
//!
//! Zero-domain-logic primitives used across the whole workspace: the validated
//! [`PluginName`] identifier, [`Origin`] and the [`Glob`]/[`GlobSet`]
//! permission-pattern matcher (with `!` negation and deny-precedence), the
//! [`SchemaVersion`] selector, the BLAKE3 [`Checksum`] newtype, and the
//! [`IdentityId`]/[`WorkspaceId`]/[`TabId`] id newtypes.
//!
//! This crate holds *vocabulary*, not behavior that belongs to a domain layer.
//! Higher crates (`mote-permissions`, `mote-storage`, `mote-registry`, …) build
//! their logic on top of these types. It depends only on `blake3` and
//! `thiserror`; it has no in-workspace dependencies.
//!
//! ## Permission patterns
//!
//! Permissions use an IAM-style `domain:action[:resource]` syntax (DESIGN
//! §Permission Primitives). The *resource* component is matched with [`Glob`],
//! which supports `*` wildcards and a leading `!` for negation. A [`GlobSet`]
//! resolves a collection of allow/deny patterns with **deny precedence**: any
//! matching negated pattern denies, regardless of matching allow patterns.
//!
//! ## Checksums
//!
//! [`Checksum`] is a BLAKE3 digest rendered and parsed as `blake3:<hex>`
//! (DESIGN §Integrity verification). It is an integrity primitive, not a trust
//! primitive.

mod checksum;
mod glob;
mod ids;
mod origin;
mod plugin_name;
mod schema_version;

pub use checksum::{Checksum, ChecksumParseError};
pub use glob::{Glob, GlobParseError, GlobSet, Match};
pub use ids::{IdentityId, TabId, WorkspaceId};
pub use origin::Origin;
pub use plugin_name::{PluginName, PluginNameError};
pub use schema_version::{SchemaVersion, SchemaVersionParseError};
