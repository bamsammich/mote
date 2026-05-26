//! Error types for sandbox construction and declarative plugin loading.

use mote_types::{PluginNameError, SchemaVersionParseError};
use thiserror::Error;

/// An error raised while constructing the sandbox or loading a plugin module.
///
/// These map the two failure surfaces of this crate — building the constrained
/// Lua state, and evaluating a plugin chunk to extract its declarative surface
/// (DESIGN §Enforcement Rules step 2; ADR-0001). A malformed plugin must fail
/// with one of these variants, never a panic.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LuaError {
    /// The constrained Lua state could not be created (e.g. mlua rejected the
    /// requested standard-library subset).
    #[error("failed to construct sandboxed Lua state: {0}")]
    Sandbox(#[source] mlua::Error),

    /// The plugin chunk failed to compile or raised an error while being
    /// evaluated to produce the `M` table.
    ///
    /// Evaluation runs the module body (which builds `M`), but per ADR-0001 it
    /// does **not** call `setup()`.
    #[error("failed to evaluate plugin module: {0}")]
    Evaluate(#[source] mlua::Error),

    /// The chunk did not return a Lua table (`return M`).
    #[error("plugin module did not return a table (expected `return M`); got {got}")]
    NotATable {
        /// The Lua type name actually returned by the chunk.
        got: &'static str,
    },

    /// The module is missing the required `manifest` field, or it is not a
    /// table.
    #[error("plugin module `manifest` field is missing or not a table")]
    MissingManifest,

    /// A manifest field had the wrong Lua type.
    #[error("manifest field `{field}` has wrong type: expected {expected}, got {got}")]
    ManifestFieldType {
        /// The manifest key whose value was ill-typed.
        field: &'static str,
        /// The Lua type the loader required.
        expected: &'static str,
        /// The Lua type actually present.
        got: &'static str,
    },

    /// A required manifest field was absent.
    #[error("manifest is missing required field `{field}`")]
    MissingManifestField {
        /// The absent required key.
        field: &'static str,
    },

    /// One of `hooks`, `events`, or `api` was present but not a table.
    #[error("module field `{field}` is present but not a table (got {got})")]
    NotADeclarationTable {
        /// The offending module key (`hooks`, `events`, or `api`).
        field: &'static str,
        /// The Lua type actually present.
        got: &'static str,
    },

    /// The manifest `name` failed [`PluginName`](mote_types::PluginName)
    /// validation.
    #[error("manifest `name` is not a valid plugin name: {0}")]
    InvalidPluginName(#[source] PluginNameError),

    /// The manifest `schema` was not a recognized
    /// [`SchemaVersion`](mote_types::SchemaVersion).
    #[error("manifest `schema` is not a recognized schema version: {0}")]
    InvalidSchemaVersion(#[source] SchemaVersionParseError),

    /// A Lua operation against the loaded module failed unexpectedly (e.g.
    /// reading a field raised a metamethod error).
    #[error("Lua operation failed while inspecting the module: {0}")]
    Lua(#[source] mlua::Error),
}
