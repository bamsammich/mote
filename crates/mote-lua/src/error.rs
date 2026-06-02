//! Error types for sandbox construction and declarative plugin loading.

use mote_types::{PluginNameError, SchemaVersionParseError};
use thiserror::Error;

use crate::invoke::HookTable;

/// An error raised while invoking a single declarative Lua handler under a
/// deadline (see [`call_hook_with_deadline`](crate::call_hook_with_deadline)).
///
/// These three outcomes are exactly what the dispatch layer needs to apply the
/// per-hook-type budget contract: a timeout becomes `defer` on a filter chain,
/// a Lua error skips the plugin for that dispatch, and a missing handler is a
/// wiring bug surfaced rather than silently ignored. No variant is a panic.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HookInvokeError {
    /// The handler exceeded its wall-clock deadline and was interrupted
    /// mid-execution. The filter-chain layer treats this as `defer`.
    #[error("hook handler exceeded its deadline and was interrupted")]
    Timeout,

    /// The handler raised a Lua error (caught, not a panic). The plugin is
    /// skipped for this dispatch; others continue.
    #[error("hook handler raised a Lua error: {0}")]
    Lua(#[source] mlua::Error),

    /// No function was found at the requested table/key. Either the declaration
    /// table is absent/ill-typed, or the key holds a non-function value.
    #[error("no handler function at M.{table:?}[{key:?}]")]
    NoSuchHandler {
        /// Which declaration table was addressed (`hooks` or `events`).
        table: HookTable,
        /// The handler key that was missing.
        key: String,
    },
}

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

    /// The `M.rail` top-level field is present but not a Lua table (array).
    #[error("module field `rail` is present but not a table (got {got})")]
    RailNotATable {
        /// The Lua type actually present.
        got: &'static str,
    },

    /// A `M.rail[i]` entry is not a table.
    #[error("module `rail` entry at index {index} is not a table (got {got})")]
    RailEntryNotATable {
        /// The 1-based index of the offending entry.
        index: usize,
        /// The Lua type actually present.
        got: &'static str,
    },

    /// A required field inside a `M.rail[i]` entry is missing or wrong type.
    #[error("rail entry {index} field `{field}` has wrong type: expected {expected}, got {got}")]
    RailEntryFieldType {
        /// The 1-based index of the offending entry.
        index: usize,
        /// The field name.
        field: &'static str,
        /// The expected Lua type.
        expected: &'static str,
        /// The actual Lua type.
        got: &'static str,
    },

    /// A required field inside a `M.rail[i]` entry is absent.
    #[error("rail entry {index} is missing required field `{field}`")]
    RailEntryMissingField {
        /// The 1-based index of the offending entry.
        index: usize,
        /// The absent required key.
        field: &'static str,
    },
}
