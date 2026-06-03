//! Error types for the load pipeline and lifecycle operations.

use mote_types::PluginName;
use thiserror::Error;

/// A failure in the four-step load pipeline (DESIGN §Enforcement Rules) or a
/// lifecycle operation.
///
/// Each variant names the step that failed, so the integrity panel and the CLI
/// can surface a precise reason. A failed load never runs the plugin's
/// `setup()`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoadError {
    /// **Step 1 — schema validation.** A `permissions` / `capabilities` /
    /// `consumes` term referenced an unknown registry entry or had bad grammar.
    #[error("schema validation failed (step 1): {0}")]
    Schema(#[from] mote_registry::SchemaValidationError),

    /// **Step 1 — dangling consumer.** The plugin consumes a capability that no
    /// loaded plugin currently fulfills (DESIGN §Resolution at load time).
    #[error(
        "cannot load {plugin}: consumes capability `{capability}`, but no plugin currently \
         fulfills it"
    )]
    DanglingConsumer {
        /// The plugin that failed to load.
        plugin: PluginName,
        /// The unfulfilled consumed capability.
        capability: String,
    },

    /// **Step 2 — module load.** The Lua module failed to evaluate or its
    /// declarative surface could not be extracted.
    #[error("module load failed (step 2): {0}")]
    Module(#[from] mote_lua::LuaError),

    /// **Step 3 — contract conformance.** A claimed capability's required API /
    /// event surface was not declared by the module.
    #[error("contract conformance failed (step 3): {0}")]
    Conformance(#[from] mote_registry::ConformanceError),

    /// **Step 4 — permission approval.** The user (or the injected
    /// [`ApprovalPolicy`](crate::ApprovalPolicy)) denied the plugin.
    #[error("permission approval denied (step 4): {reason}")]
    ApprovalDenied {
        /// Why approval was refused.
        reason: String,
    },

    /// An exclusive capability is already claimed by another loaded plugin
    /// (DESIGN §Resolution at load time: "the user enables exactly one
    /// fulfiller").
    #[error(
        "cannot load {plugin}: exclusive capability `{capability}` is already fulfilled by \
         `{existing}`"
    )]
    ExclusiveCapabilityConflict {
        /// The plugin that failed to load.
        plugin: PluginName,
        /// The contested exclusive capability.
        capability: String,
        /// The plugin already fulfilling it.
        existing: PluginName,
    },

    /// A capability term appeared in a manifest but is unknown to the registry
    /// (caught while resolving fulfillment, after schema validation).
    #[error("capability `{capability}` is not a known registry capability")]
    UnknownCapability {
        /// The unknown capability name.
        capability: String,
    },

    /// The plugin is already loaded under the same name; use `reload` instead.
    #[error("plugin `{plugin}` is already loaded")]
    AlreadyLoaded {
        /// The duplicate plugin name.
        plugin: PluginName,
    },

    /// A narrowing/grant computation produced an invalid resource glob.
    #[error("failed to build effective grants: {0}")]
    Grant(#[from] mote_types::GlobParseError),

    /// The plugin's effective permissions could not be registered with the
    /// dispatch engine (hook type contradicted a prior registration).
    #[error("hook registration failed: {0}")]
    Register(#[from] mote_dispatch::RegisterError),

    /// A host-API table could not be installed into the plugin's Lua state.
    #[error("failed to install the mote.* host API: {0}")]
    HostApi(String),

    /// Storage access failed while preparing the plugin's namespace.
    #[error("storage error: {0}")]
    Storage(#[from] mote_storage::StorageError),

    /// `setup()` raised a Lua error while binding handlers.
    #[error("plugin setup() failed: {0}")]
    Setup(String),

    /// **Step 3 — rail binding validation.** A `M.rail` entry declared an icon
    /// that the ADR-0013 registry rejects, or a capability that the plugin did
    /// not declare in its manifest (ADR-0014).
    #[error("rail binding validation failed (step 3): {reason}")]
    RailBinding {
        /// The 1-based index of the offending rail entry.
        index: usize,
        /// A human-readable description of the validation failure.
        reason: String,
    },

    /// **Step 3 — statusline element validation.** A `M.statusline` entry
    /// failed semantic validation (unknown lucide icon, missing required field
    /// for the declared kind, duplicate id, etc.) — ADR-0016.
    #[error("statusline element validation failed (step 3, entry {index}): {reason}")]
    StatusLine {
        /// The 1-based index of the offending statusline entry.
        index: usize,
        /// A human-readable description of the validation failure.
        reason: String,
    },
}

/// A failure when reloading or unloading a plugin that was not loaded.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LifecycleError {
    /// No plugin with this name is currently loaded.
    #[error("plugin `{plugin}` is not loaded")]
    NotLoaded {
        /// The name that was not found.
        plugin: PluginName,
    },

    /// The reload's load pipeline failed.
    #[error(transparent)]
    Load(#[from] LoadError),
}
