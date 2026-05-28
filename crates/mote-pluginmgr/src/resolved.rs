//! The [`ResolvedPlugin`] list — the composed, reconciled, load-ready view of a
//! profile's plugins.
//!
//! [`crate::PluginManager::resolved_set`] produces an ordered [`Vec`] of
//! [`ResolvedPlugin`]s: every plugin the shell should consider loading
//! (`plugins.lua` + `managed.lua` + the bundled first-party defaults), each
//! resolved to its on-disk directory, parsed manifest, `init.lua` source,
//! provenance, and integrity status — **without** loading anything into a
//! runtime. The shell's plugin host walks this list, classifies each entry
//! through the approval brain, and loads the auto-grant ones.

use std::path::PathBuf;

use mote_lua::Manifest;
use mote_types::PluginName;

use crate::manager::IntegrityStatus;
use crate::provenance::Provenance;

/// One plugin resolved and ready to load, produced by
/// [`crate::PluginManager::resolved_set`].
///
/// All fields reflect already-reconciled state (the manager has fetched/linked
/// and hashed each plugin via `sync` before building this). Nothing here loads
/// Lua into a runtime — that is the shell's responsibility.
#[derive(Debug, Clone)]
pub struct ResolvedPlugin {
    /// The canonical plugin name (the `plugins.lua` key / manifest name).
    pub name: PluginName,
    /// How this plugin's code reached the machine (drives auto-grant vs.
    /// dialog in the approval classifier).
    pub provenance: Provenance,
    /// The resolved active directory: the `plugins/<name>` symlink target (a
    /// cache commit dir for git/bundled, or the user's real dir for `path:`).
    pub dir: PathBuf,
    /// The parsed manifest read from `<dir>/init.lua`.
    pub manifest: Manifest,
    /// The integrity status the manager computed for this plugin during the
    /// reconciling `sync` pass (verified / mismatch / bundled / unknown).
    pub integrity: IntegrityStatus,
    /// The full contents of `<dir>/init.lua` — the source the runtime loads.
    pub init_source: String,
}
