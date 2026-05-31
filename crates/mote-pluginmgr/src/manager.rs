//! The [`PluginManager`] façade — the single entry-point the CLI (3.3b) and
//! shell call for all plugin lifecycle operations.
//!
//! # Scope
//!
//! This module manages files, the cache, the lock, `managed.lua`, and
//! approval state. It does **not** drive the runtime, load Lua, or touch any
//! CEF / shell types — those integrations land in Phase-3 work unit 3.6.
//!
//! # Path model
//!
//! Two injected roots (tempdirs in tests; real home dirs in production):
//!
//! - **`config_dir`** — e.g. `~/.config/mote`
//!   - `<config_dir>/plugins.lua` (user-authored, read-only to Mote)
//!   - `<config_dir>/managed.lua` (Mote-owned, generated)
//!   - `<config_dir>/plugins.lock`
//!   - `<config_dir>/plugins/<name>` — symlink (git/bundled) or real dir
//!     (`path:` / implicit)
//!
//! - **`cache_dir`** — e.g. `~/.cache/mote/plugins`
//!   - `<cache_dir>/<name>/<commit>/` — content-addressed trees
//!
//! # Integrity rules (plan §3.5)
//!
//! - `github:`/`git+https:` with a lock entry: mismatch → hard [`IntegrityStatus::Mismatch`],
//!   refused in sync (recorded in [`SyncOutcome`], never committed to the lock).
//! - `path:` and implicit-local: mismatches are informational only
//!   (`IntegrityStatus::Mismatch` flagged but not blocking).
//! - No lock entry yet → [`IntegrityStatus::Unknown`].
//!
//! # Network policy (R5)
//!
//! Fetch failures are recoverable per-plugin errors. `sync` collects failures
//! into [`SyncReport::failed`] and leaves the existing cache/symlink/lock
//! untouched. An offline machine with everything already cached must succeed
//! without any fetch attempts.

use std::fs;
use std::path::{Path, PathBuf};

use mote_lua::DevModeConfig;
use mote_runtime::ApprovalHash;
use mote_storage::Store;
use mote_types::{Checksum, IdentityId, PluginName};
use thiserror::Error;

use crate::approval_store::{ApprovalStore, ApprovalStoreError};
use crate::bundle::{BundleError, bundled_names, unpack_into_cache};
use crate::cache::{Cache, CacheError};
use crate::diff::{DiffReport, diff};
use crate::dirhash::{DirHashError, hash_dir};
use crate::fetch::{FetchError, fetch};
use crate::lock::{LockEntry, LockError, LockFile};
use crate::managed::{ManagedError, ManagedFile};
use crate::provenance::Provenance;
use crate::resolve::{ResolveError, compose};
use crate::resolved::ResolvedPlugin;
use crate::source::{Source, SourceParseError};

// ---------------------------------------------------------------------------
// Outcome / report types
// ---------------------------------------------------------------------------

/// What `sync` found for a single plugin's integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityStatus {
    /// The on-disk dir hash matches the lock entry — clean.
    Verified,
    /// The on-disk hash **differs** from the lock. For git sources this is a
    /// hard failure; for `path:` sources it is informational.
    Mismatch {
        /// Hash currently on disk.
        actual: Checksum,
        /// Hash recorded in the lock.
        expected: Checksum,
    },
    /// No lock entry exists yet (freshly added or never synced).
    Unknown,
    /// Source is `bundled` — hash trustworthy by construction.
    Bundled,
    /// Source is `path:` or implicit-local (informational hash changes expected).
    PathLocal,
}

/// The per-plugin outcome recorded in [`SyncReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    /// The plugin name.
    pub name: PluginName,
    /// The integrity state after this sync.
    pub integrity: IntegrityStatus,
}

/// What `sync` found across all plugins.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Plugins that synced (or were already up-to-date).
    pub ok: Vec<SyncOutcome>,
    /// Plugins that could not be synced, with an error per plugin.
    pub failed: Vec<(PluginName, ManagerError)>,
}

/// The result of `remove`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// The plugin was in `managed.lua` — it has been removed.
    Removed,
    /// The plugin was found **only** in the user's `plugins.lua` (not in
    /// `managed.lua`). Mote did not modify `plugins.lua`; the user must remove
    /// it by hand.
    UserConfigOnly,
    /// The plugin was not found in either config.
    NotFound,
}

/// The result of `import`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// The snippet was returned without modifying `plugins.lua`.
    Snippet(String),
    /// The snippet was appended to `plugins.lua`; the entry was dropped from
    /// `managed.lua`.
    Written,
    /// `plugins.lua` does not parse; fell back to returning the snippet
    /// without modifying the file.
    PluginsLuaDoesNotParse(String),
}

/// The result of `update`.
#[derive(Debug)]
pub enum UpdateOutcome {
    /// The plugin's permissions expanded — re-approval required before
    /// relinking. The diff is included for display.
    NeedsReapproval {
        /// The diff showing what changed.
        report: DiffReport,
    },
    /// No expansion detected; the plugin was relinked to the new commit and the
    /// lock updated.
    Applied {
        /// The new commit SHA.
        commit: String,
    },
}

/// Report produced by [`PluginManager::gc`].
#[derive(Debug, Default)]
pub struct GcReport {
    /// Cache entries (`<name>/<commit>`) reclaimed by gc.
    pub reclaimed: Vec<(PluginName, String)>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by [`PluginManager`] operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ManagerError {
    /// A source string could not be parsed.
    #[error("invalid source string: {0}")]
    Source(#[from] SourceParseError),

    /// I/O error on a path.
    #[error("I/O error on {path:?}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// `plugins.lua` / `managed.lua` does not parse as valid config Lua.
    #[error("config file does not parse: {0}")]
    Config(#[from] mote_lua::ConfigError),

    /// A plugin module could not be loaded (manifest extraction failed).
    #[error("plugin module error: {0}")]
    PluginLoad(#[from] mote_lua::LuaError),

    /// `plugins.lock` could not be read or written.
    #[error("lock error: {0}")]
    Lock(#[from] LockError),

    /// A `managed.lua` operation failed.
    #[error("managed.lua error: {0}")]
    Managed(#[from] ManagedError),

    /// Cache operation failed.
    #[error("cache error: {0}")]
    Cache(#[from] CacheError),

    /// Directory hash error.
    #[error("integrity hash error: {0}")]
    Hash(#[from] DirHashError),

    /// A Git fetch failed (recoverable; see R5).
    #[error("fetch error: {0}")]
    Fetch(#[from] FetchError),

    /// Bundle extraction failed.
    #[error("bundle error: {0}")]
    Bundle(#[from] BundleError),

    /// Approval store error.
    #[error("approval store error: {0}")]
    Approval(#[from] ApprovalStoreError),

    /// Resolve / compose error.
    #[error("resolve error: {0}")]
    Resolve(#[from] ResolveError),

    /// The named plugin was not found in any config layer.
    #[error("plugin {0:?} not found")]
    NotFound(PluginName),

    /// Rollback requested but no previous commit is cached.
    #[error("plugin {name:?} has no previous commit to roll back to (active: {active:?})")]
    NoPreviousCommit {
        /// Plugin name.
        name: PluginName,
        /// Currently active commit.
        active: Option<String>,
    },

    /// A path: source has an empty/invalid expanded path.
    #[error("path: source for {name:?} resolved to an invalid directory: {path:?}")]
    BadPath {
        /// Plugin name.
        name: PluginName,
        /// The problematic path.
        path: PathBuf,
    },
}

impl ManagerError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

/// The management façade the CLI and shell call.
///
/// All fields are **injected** so tests can pass temp directories and in-memory
/// stores without touching real home directories.
///
/// # Construction
///
/// ```ignore
/// let store = Store::open_in_memory()?;
/// let mgr = PluginManager::new(config_dir, cache_dir, &store);
/// ```
///
/// For production use, [`PluginManager::default_dirs`] returns the
/// conventional `~/.config/mote` / `~/.cache/mote/plugins` pair; the
/// manager methods operate on the injected paths regardless.
#[derive(Debug)]
pub struct PluginManager {
    /// The user config directory, e.g. `~/.config/mote`.
    config_dir: PathBuf,
    /// The plugin cache root, e.g. `~/.cache/mote/plugins`.
    cache_dir: PathBuf,
    /// The content-addressed cache + active-link scheme.
    cache: Cache,
    /// Per-plugin approval state.
    approval: ApprovalStore,
}

impl PluginManager {
    /// Creates a [`PluginManager`] using the given injected directories and store.
    ///
    /// The plugins directory used for active symlinks is
    /// `<config_dir>/plugins`. Tests pass temp dirs so no real home paths are
    /// touched.
    #[must_use]
    pub fn new(
        config_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        store: &Store,
    ) -> Self {
        let config_dir: PathBuf = config_dir.into();
        let cache_dir: PathBuf = cache_dir.into();
        let plugins_dir = config_dir.join("plugins");
        let cache = Cache::new(cache_dir.clone(), plugins_dir);
        let approval = ApprovalStore::new(store);
        Self {
            config_dir,
            cache_dir,
            cache,
            approval,
        }
    }

    /// Returns the canonical production `(config_dir, cache_dir)` pair.
    ///
    /// This is the **single source of truth** for where Mote's plugin state
    /// lives, shared by the CLI (`mote_cli::resolve_dirs` delegates here) and
    /// the GUI shell so an approval recorded via `mote plugin` is visible to the
    /// running browser and vice versa. Resolution follows the XDG Base Directory
    /// specification:
    ///
    /// - **`config_dir`**: `$XDG_CONFIG_HOME/mote` if `XDG_CONFIG_HOME` is set,
    ///   otherwise `$HOME/.config/mote`.
    /// - **`cache_dir`**: `$XDG_CACHE_HOME/mote/plugins` if `XDG_CACHE_HOME` is
    ///   set, otherwise `$HOME/.cache/mote/plugins`.
    ///
    /// Does **not** create either directory. The returned pair can be passed to
    /// [`PluginManager::new`].
    ///
    /// Returns `None` if a needed environment variable is missing (i.e. neither
    /// `XDG_CONFIG_HOME` nor `HOME` for the config dir, or neither
    /// `XDG_CACHE_HOME` nor `HOME` for the cache dir).
    #[must_use]
    pub fn default_dirs() -> Option<(PathBuf, PathBuf)> {
        Self::resolve_dirs_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_CACHE_HOME"),
            std::env::var_os("HOME").as_deref(),
        )
    }

    /// Pure resolver behind [`PluginManager::default_dirs`].
    ///
    /// Takes the relevant environment values explicitly so the XDG/HOME
    /// precedence can be unit-tested without mutating process-global env (which
    /// is `unsafe` in edition 2024 and racy across parallel tests).
    #[must_use]
    fn resolve_dirs_from(
        xdg_config_home: Option<std::ffi::OsString>,
        xdg_cache_home: Option<std::ffi::OsString>,
        home: Option<&std::ffi::OsStr>,
    ) -> Option<(PathBuf, PathBuf)> {
        let config = if let Some(xdg) = xdg_config_home {
            PathBuf::from(xdg).join("mote")
        } else {
            PathBuf::from(home?).join(".config").join("mote")
        };
        let cache = if let Some(xdg) = xdg_cache_home {
            PathBuf::from(xdg).join("mote").join("plugins")
        } else {
            PathBuf::from(home?)
                .join(".cache")
                .join("mote")
                .join("plugins")
        };
        Some((config, cache))
    }

    /// Borrows the manager's per-plugin [`ApprovalStore`].
    ///
    /// Exposed so the shell's load coordinator can read prior approvals (to
    /// classify auto-grant vs. dialog) and record new ones, sharing the exact
    /// same store the manager's own `approve`/`pin` operations write to — there
    /// is one approval record per plugin regardless of who wrote it.
    #[must_use]
    pub const fn approval_store(&self) -> &ApprovalStore {
        &self.approval
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Path of the user's read-only `plugins.lua`.
    fn plugins_lua_path(&self) -> PathBuf {
        self.config_dir.join("plugins.lua")
    }

    /// Path of Mote-owned `managed.lua`.
    fn managed_lua_path(&self) -> PathBuf {
        self.config_dir.join("managed.lua")
    }

    /// Path of `plugins.lock`.
    fn lock_path(&self) -> PathBuf {
        self.config_dir.join("plugins.lock")
    }

    /// Loads `plugins.lock` or returns an empty [`LockFile`] if it does not
    /// exist yet.
    fn load_lock(&self) -> Result<LockFile, ManagerError> {
        let p = self.lock_path();
        if !p.exists() {
            return Ok(LockFile::default());
        }
        let text = fs::read_to_string(&p).map_err(|e| ManagerError::io(&p, e))?;
        Ok(LockFile::from_toml(&text)?)
    }

    /// Atomically writes `lock` to `plugins.lock`.
    fn write_lock(&self, lock: &LockFile) -> Result<(), ManagerError> {
        let p = self.lock_path();
        let parent = p.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|e| ManagerError::io(parent, e))?;

        let text = lock.to_toml()?;
        // Atomic write via temp-file + rename.
        let mut tmp = tempfile::Builder::new()
            .prefix(".lock_tmp")
            .tempfile_in(parent)
            .map_err(|e| ManagerError::io(parent, e))?;
        std::io::Write::write_all(&mut tmp, text.as_bytes())
            .map_err(|e| ManagerError::io(tmp.path(), e))?;
        tmp.into_temp_path()
            .persist(&p)
            .map_err(|e| ManagerError::io(&p, e.error))?;
        Ok(())
    }

    /// Loads `managed.lua` or returns an empty [`ManagedFile`] if absent.
    fn load_managed(&self) -> Result<ManagedFile, ManagerError> {
        let p = self.managed_lua_path();
        if !p.exists() {
            return Ok(ManagedFile::new());
        }
        Ok(ManagedFile::load(&p)?)
    }

    /// Path of a per-identity plugin overlay, `<config>/identities/<id>/plugins.lua`.
    ///
    /// The directory component is the identity's raw `u64` rendered as a decimal
    /// string (`IdentityId`'s `Display`), matching the DESIGN.md convention
    /// `~/.config/mote/identities/<identity>/plugins.lua`. The shell's session
    /// identity is `0`, so its overlay lives at `identities/0/plugins.lua`.
    fn identity_plugins_lua_path(&self, identity: IdentityId) -> PathBuf {
        self.config_dir
            .join("identities")
            .join(identity.to_string())
            .join("plugins.lua")
    }

    /// Builds the composed [`crate::resolve::PluginSpecSet`] from whichever
    /// config files exist (`plugins.lua`, then `managed.lua`).
    ///
    /// Identity-agnostic: used by [`sync`](Self::sync) and other reconciling
    /// operations that do not vary by identity. Delegates to
    /// [`composed_config`](Self::composed_config) with `identity = None`,
    /// discarding the merged [`DevModeConfig`] (only the specs matter here).
    fn composed_spec_set(&self) -> Result<crate::resolve::PluginSpecSet, ManagerError> {
        Ok(self.composed_config(None)?.0)
    }

    /// Evaluates and composes the config layers for `identity`, returning both
    /// the merged [`crate::resolve::PluginSpecSet`] and the unioned
    /// [`DevModeConfig`].
    ///
    /// ## Layers (applied in order)
    ///
    /// 1. global `<config>/plugins.lua` (user-authored)
    /// 2. global `<config>/managed.lua` (Mote-owned)
    /// 3. per-identity `<config>/identities/<id>/plugins.lua` — **only** when
    ///    `identity` is `Some` and the file exists.
    ///
    /// Specs are composed via [`compose`], which applies **last-writer-wins per
    /// key**: the identity overlay (last layer) overrides the global layers for
    /// any plugin it re-declares, while disjoint keys from earlier layers are
    /// preserved.
    ///
    /// ## Dev-mode merge
    ///
    /// [`compose`] only merges plugin specs, not the per-layer [`DevModeConfig`].
    /// This method **unions** the `directories` and `plugins` lists across every
    /// present layer (deduplicated, order-preserving) so a `mote.dev_mode {…}`
    /// block in any of `plugins.lua` / `managed.lua` / the identity overlay
    /// contributes. The merged config feeds dev-mode marking in
    /// [`resolved_set`](Self::resolved_set).
    ///
    /// (The sibling `updates` config is **not** merged here — only the global
    /// `plugins.lua` value would win today; cross-layer `updates` merge is a
    /// tracked follow-up, not required by the dev-mode marking path.)
    fn composed_config(
        &self,
        identity: Option<&IdentityId>,
    ) -> Result<(crate::resolve::PluginSpecSet, DevModeConfig), ManagerError> {
        let user_path = self.plugins_lua_path();
        let managed_path = self.managed_lua_path();
        let identity_path = identity.map(|id| self.identity_plugins_lua_path(*id));

        let user_spec = if user_path.exists() {
            let src =
                fs::read_to_string(&user_path).map_err(|e| ManagerError::io(&user_path, e))?;
            Some(mote_lua::eval_config(&src, "plugins.lua")?)
        } else {
            None
        };

        let managed_spec = if managed_path.exists() {
            let src = fs::read_to_string(&managed_path)
                .map_err(|e| ManagerError::io(&managed_path, e))?;
            Some(mote_lua::eval_config(&src, "managed.lua")?)
        } else {
            None
        };

        // Per-identity overlay: only when an identity is supplied AND its
        // overlay file exists. Absent overlay → no extra layer (identity-less
        // resolution is the global set).
        let identity_spec = if let Some(p) = identity_path.as_ref().filter(|p| p.exists()) {
            let src = fs::read_to_string(p).map_err(|e| ManagerError::io(p, e))?;
            Some(mote_lua::eval_config(&src, "identities/<id>/plugins.lua")?)
        } else {
            None
        };

        // Build the layers in precedence order (overlay last → overlay wins).
        let mut layers: Vec<&mote_lua::ConfigSpec> = Vec::with_capacity(3);
        if let Some(u) = &user_spec {
            layers.push(u);
        }
        if let Some(m) = &managed_spec {
            layers.push(m);
        }
        if let Some(i) = &identity_spec {
            layers.push(i);
        }
        let empty = mote_lua::ConfigSpec::default();
        if layers.is_empty() {
            layers.push(&empty);
        }

        let spec_set = compose(&layers)?;

        // Union dev_mode across every present layer (compose ignores it). Dedup
        // while preserving first-seen order.
        let mut dev_mode = DevModeConfig::default();
        for spec in [&user_spec, &managed_spec, &identity_spec]
            .into_iter()
            .flatten()
        {
            for dir in &spec.dev_mode.directories {
                if !dev_mode.directories.contains(dir) {
                    dev_mode.directories.push(dir.clone());
                }
            }
            for plugin in &spec.dev_mode.plugins {
                if !dev_mode.plugins.contains(plugin) {
                    dev_mode.plugins.push(plugin.clone());
                }
            }
        }

        Ok((spec_set, dev_mode))
    }

    /// Expands a `~`-prefixed path to an absolute path.
    fn expand_tilde(path: &Path) -> PathBuf {
        let s = path.to_string_lossy();
        if let Some(rest) = s.strip_prefix("~/")
            && let Some(home) = std::env::var_os("HOME")
        {
            return PathBuf::from(home).join(rest);
        }
        path.to_path_buf()
    }

    /// Canonicalizes every `dev_mode.directories` entry (expanding `~`) for
    /// prefix matching in [`is_dev_mode`](Self::is_dev_mode).
    ///
    /// An entry that does not exist on disk (so cannot be canonicalized) is
    /// retained as its `~`-expanded form, so a configured-but-missing dev dir
    /// still matches a child resolved by a literal path. Canonicalizing both
    /// sides where possible defeats `..`/symlink aliasing in the common case.
    fn canonical_dev_dirs(dev_mode: &DevModeConfig) -> Vec<PathBuf> {
        dev_mode
            .directories
            .iter()
            .map(|d| {
                let expanded = Self::expand_tilde(Path::new(d));
                expanded.canonicalize().unwrap_or(expanded)
            })
            .collect()
    }

    /// Whether the plugin `name` resolved at `dir` is a dev-mode plugin per the
    /// merged `dev_mode` config: its manifest name appears in
    /// `dev_mode.plugins`, OR its canonicalized resolved dir is at or under one
    /// of `dev_dirs` (already canonicalized by
    /// [`canonical_dev_dirs`](Self::canonical_dev_dirs)).
    ///
    /// `dir` is the `plugins/<name>` active link; canonicalizing it follows the
    /// symlink to the real path-source / cache dir so the prefix match is
    /// against the plugin's true location, not the slot path.
    fn is_dev_mode(
        name: &PluginName,
        dir: &Path,
        dev_mode: &DevModeConfig,
        dev_dirs: &[PathBuf],
    ) -> bool {
        if dev_mode.plugins.iter().any(|p| p == name.as_str()) {
            return true;
        }
        let real = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        dev_dirs.iter().any(|root| real.starts_with(root))
    }

    /// Resolves a `path:` [`Source`] to the canonical, real directory.
    fn resolve_path_source(name: &PluginName, raw: &Path) -> Result<PathBuf, ManagerError> {
        let expanded = Self::expand_tilde(raw);
        // canonicalize requires the path to exist; we only error if it does not.
        if !expanded.exists() {
            return Err(ManagerError::BadPath {
                name: name.clone(),
                path: expanded,
            });
        }
        expanded
            .canonicalize()
            .map_err(|e| ManagerError::io(&expanded, e))
    }

    /// Expands and canonicalizes a raw path without requiring a [`PluginName`].
    ///
    /// Used by `add` before the manifest name is known.
    fn canonicalize_path(raw: &Path) -> Result<PathBuf, ManagerError> {
        let expanded = Self::expand_tilde(raw);
        if !expanded.exists() {
            return Err(ManagerError::Io {
                path: expanded,
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "path: source directory does not exist",
                ),
            });
        }
        let path = expanded.clone();
        expanded
            .canonicalize()
            .map_err(|e| ManagerError::io(path, e))
    }

    /// Ensures the `plugins/<name>` slot in the config dir points at (or is)
    /// `dir`. For git/bundled: creates a symlink. For `path:`: creates a real
    /// directory entry by making the slot point to the canonical path (also
    /// symlink, but distinct: target is the user's real path).
    ///
    /// Uses the cache's `link` method for git/bundled (which creates a proper
    /// symlink); for `path:` we just create a symlink to the canonical path
    /// through the same mechanism.
    fn ensure_link(&self, name: &PluginName, target: &Path) -> Result<(), ManagerError> {
        let link = self.cache.active_link(name);
        let plugins_dir = self.config_dir.join("plugins");
        fs::create_dir_all(&plugins_dir).map_err(|e| ManagerError::io(&plugins_dir, e))?;

        // Remove whatever is at the link path first.
        remove_link_or_dir(&link)?;

        // Create the symlink.
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &link).map_err(|e| ManagerError::io(&link, e))?;

        #[cfg(not(unix))]
        {
            // On non-Unix, fall back to cache's link for git targets only;
            // on Windows this would need a junction. Not a v0.1 target.
            let _ = target;
            return Err(ManagerError::Io {
                path: link.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "path: symlinks unsupported on this platform",
                ),
            });
        }

        Ok(())
    }

    /// Removes a plugin's `plugins/<name>` slot (symlink or real dir).
    fn remove_slot(&self, name: &PluginName) -> Result<(), ManagerError> {
        let link = self.cache.active_link(name);
        remove_link_or_dir(&link)
    }

    /// Returns an [`ApprovalHash`] of an empty (permission-less) manifest.
    ///
    /// Used as the "no prior approval" baseline for first-install diffs so that
    /// any non-empty manifest reads as an expansion.
    fn empty_approval_hash() -> Result<ApprovalHash, ManagerError> {
        let src = r#"
            local M = {}
            M.manifest = {
                schema = "v1",
                name = "empty",
                version = "0",
                permissions = {},
            }
            return M
        "#;
        let loaded = mote_lua::load_plugin(src, "empty").map_err(ManagerError::PluginLoad)?;
        Ok(ApprovalHash::of(loaded.manifest()))
    }

    /// Reads `<dir>/init.lua` and extracts the [`mote_lua::Manifest`].
    fn load_manifest_from_dir(
        name: &PluginName,
        dir: &Path,
    ) -> Result<mote_lua::Manifest, ManagerError> {
        let init = dir.join("init.lua");
        let source = fs::read_to_string(&init).map_err(|e| ManagerError::io(&init, e))?;
        let loaded =
            mote_lua::load_plugin(&source, name.as_str()).map_err(ManagerError::PluginLoad)?;
        Ok(loaded.manifest().clone())
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Reconciles the cache and plugin symlinks with the declared specs.
    ///
    /// For each plugin in the composed `plugins.lua` + `managed.lua` spec set:
    ///
    /// - `path:` → expand `~`, verify exists, hash in place, ensure link.
    /// - `github:`/`git+https:` → fetch if the pinned commit is not cached,
    ///   store into cache, link, verify hash.
    /// - `bundled` → unpack into cache (idempotent), link, verify hash.
    ///
    /// Hash verification (plan §3.5): git sources with a lock entry whose hash
    /// does not match record a [`IntegrityStatus::Mismatch`] and do **not**
    /// update the lock (keeping existing state intact). `path:` mismatches are
    /// informational. The lock is written **once** at the end if anything
    /// changed (atomic write; on error the old lock is preserved).
    ///
    /// Fetch failures are recoverable: a failed plugin is added to
    /// [`SyncReport::failed`]; the rest continue.
    ///
    /// Resolves the composed spec set (`plugins.lua` + `managed.lua` + the
    /// bundled first-party defaults) to an ordered list of plugins ready to
    /// load — **without** loading them into any runtime.
    ///
    /// Bundled first-party plugins are seeded (unpacked + linked) if the user's
    /// config declares none of them, so a fresh profile still gets the
    /// urlbar + workspace-manager defaults. Seeding is filesystem-only — it does
    /// **not** write `managed.lua`.
    ///
    /// This method first runs [`sync`](Self::sync) to reconcile the declared
    /// specs (fetch/link/hash + integrity), then reads the already-linked dirs
    /// and the per-plugin integrity outcomes — it does not re-fetch. The result
    /// is ordered by [`load_order`](crate::load_order) so every capability
    /// fulfiller precedes its consumers.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] only for **profile-wide** fatal errors:
    /// config/lock parse failure or a capability cycle / dangling consumer
    /// surfaced by [`load_order`](crate::load_order).
    ///
    /// **Per-plugin failures are non-fatal** (mirrors `sync`'s R5 resilience
    /// contract — the shell's `boot` must not abort startup because of a single
    /// bad plugin). The following are logged and the offending plugin is
    /// silently omitted from the returned [`Vec`]:
    ///
    /// - a spec whose source string does not parse;
    /// - a bundled seed entry that fails to unpack or link;
    /// - an entry whose active-link dir / `init.lua` cannot be read (typically
    ///   a git plugin in `sync.failed` with no prior cache to fall back on).
    ///
    /// ## Identity overlay
    ///
    /// When `identity` is `Some`, a per-identity `plugins.lua` overlay
    /// (`<config>/identities/<id>/plugins.lua`) is composed **last** so it
    /// overrides the global layers for any plugin it re-declares
    /// (see [`composed_config`](Self::composed_config)). The shell threads its
    /// session identity here; identity-agnostic callers pass `None` (the global
    /// set). The overlay also contributes to the merged dev-mode config that
    /// drives dev-mode marking below.
    pub fn resolved_set(
        &self,
        identity: Option<&IdentityId>,
    ) -> Result<Vec<ResolvedPlugin>, ManagerError> {
        // Compose the identity-aware spec set (global + overlay) and the merged
        // dev_mode FIRST, then reconcile *that* set so overlay-added or
        // overlay-overridden plugins are fetched/linked/hashed before we read
        // their active-link dirs. The public `sync()` reconciles only the global
        // set; resolved_set must reconcile the identity set it actually loads.
        //
        // `dev_mode` drives the dev-mode marking pass below (sub-task 6c): any
        // plugin named in it or living under one of its directories is marked
        // [`Provenance::DevMode`].
        let (spec_set, dev_mode) = self.composed_config(identity)?;
        let dev_dirs = Self::canonical_dev_dirs(&dev_mode);

        let sync_report = self.sync_specs(&spec_set)?;
        let integrity_by_name: std::collections::BTreeMap<PluginName, IntegrityStatus> =
            sync_report
                .ok
                .into_iter()
                .map(|o| (o.name, o.integrity))
                .collect();

        // Bundled-defaults seeding: if NONE of the embedded first-party plugins
        // is declared, seed them so a fresh profile still gets urlbar +
        // workspace-manager. Filesystem-only (no managed.lua write).
        let bundled = bundled_names()?;
        let any_bundled_declared = bundled.iter().any(|b| spec_set.specs.contains_key(b));
        let seed: Vec<PluginName> = if any_bundled_declared {
            Vec::new()
        } else {
            bundled
        };

        // Collect (name, provenance) for every plugin to resolve: declared
        // specs first, then any seeded bundled defaults.
        //
        // Per-plugin failures are NON-FATAL (the shell's `boot` contract: a
        // single bad plugin does not abort startup). A spec whose source string
        // fails to parse is logged and skipped here; a bundled-seed entry whose
        // unpack/link fails is logged and skipped below; entries whose active
        // dir is unreadable (e.g. a git plugin in `sync.failed` with no prior
        // cache) are logged and skipped at the resolution step.
        let mut entries: Vec<(PluginName, Provenance)> = Vec::new();
        for spec in spec_set.specs.values() {
            let source: Source = match spec.source.parse() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "mote-pluginmgr: skipping `{}` — source {:?} did not parse: {e}",
                        spec.name, spec.source
                    );
                    continue;
                }
            };
            let provenance = match source {
                Source::Bundled => Provenance::Bundled,
                Source::Github { .. } | Source::Git { .. } => Provenance::DeclaredGit,
                Source::Path(_) => Provenance::Path,
            };
            entries.push((spec.name.clone(), provenance));
        }
        for name in &seed {
            // Seed the bundled default into the cache + active link so its dir
            // resolves below, exactly as `sync_bundled` would for a declared
            // bundled source. Failure here is non-fatal: log + skip this seed,
            // continue with everything else.
            match unpack_into_cache(name, &self.cache)
                .map_err(ManagerError::from)
                .and_then(|key| {
                    self.cache
                        .link(name, &key.commit)
                        .map_err(ManagerError::from)
                }) {
                Ok(()) => entries.push((name.clone(), Provenance::Bundled)),
                Err(e) => eprintln!(
                    "mote-pluginmgr: skipping bundled seed `{name}` — unpack/link failed: {e}"
                ),
            }
        }

        // Implicit-local detection (6a): plugin dirs the user dropped straight
        // into <config>/plugins/<name>/ that are NOT cache-managed (cache slots
        // are symlinks; an implicit dir is a REAL directory) and NOT already a
        // declared/seeded entry. Each flows through `classify` shell-side, so a
        // first detection prompts the dialog and a prior-approved one is silent.
        let already: std::collections::BTreeSet<PluginName> =
            entries.iter().map(|(n, _)| n.clone()).collect();
        entries.extend(self.detect_implicit_local(&already));

        // Resolve each entry to a ResolvedPlugin (applying the dev-mode override
        // + integrity), then order by capability-contract dependency order
        // (fulfillers first).
        let (mut resolved, manifests) =
            self.resolve_entries(entries, &integrity_by_name, &dev_mode, &dev_dirs);

        let order = crate::resolve::load_order(&manifests)?;
        let position = |n: &PluginName| order.iter().position(|o| o == n).unwrap_or(usize::MAX);
        resolved.sort_by_key(|rp| position(&rp.name));

        Ok(resolved)
    }

    /// Resolves each `(name, provenance)` entry to a [`ResolvedPlugin`],
    /// reading its active-link dir / manifest / `init.lua`, applying the
    /// dev-mode provenance override (6c), and assigning the integrity status.
    ///
    /// Returns the resolved plugins alongside their manifests (parallel, used by
    /// the caller for `load_order`). An entry whose active-link dir or `init.lua`
    /// is unreadable (typically a git plugin in `sync.failed` with no prior
    /// cache) is logged and skipped — mirroring sync's resilience contract.
    fn resolve_entries(
        &self,
        entries: Vec<(PluginName, Provenance)>,
        integrity_by_name: &std::collections::BTreeMap<PluginName, IntegrityStatus>,
        dev_mode: &DevModeConfig,
        dev_dirs: &[PathBuf],
    ) -> (Vec<ResolvedPlugin>, Vec<mote_lua::Manifest>) {
        let mut resolved: Vec<ResolvedPlugin> = Vec::with_capacity(entries.len());
        let mut manifests: Vec<mote_lua::Manifest> = Vec::with_capacity(entries.len());
        for (name, provenance) in entries {
            let dir = self.cache.active_link(&name);
            let manifest = match Self::load_manifest_from_dir(&name, &dir) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "mote-pluginmgr: skipping `{name}` — manifest at {} unreadable: {e}",
                        dir.display()
                    );
                    continue;
                }
            };
            let init = dir.join("init.lua");
            let init_source = match fs::read_to_string(&init) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "mote-pluginmgr: skipping `{name}` — init.lua at {} unreadable: {e}",
                        init.display()
                    );
                    continue;
                }
            };
            // Dev-mode override (6c): a plugin named in `dev_mode.plugins`, or
            // whose resolved dir lives under a `dev_mode.directories` entry, is
            // the developer's own working copy — mark it `DevMode` regardless of
            // its declared source (path/git/implicit/bundled). DevMode takes
            // precedence; `classify` then auto-grants it (ADR-0008).
            let provenance = if Self::is_dev_mode(&name, &dir, dev_mode, dev_dirs) {
                Provenance::DevMode
            } else {
                provenance
            };
            let integrity = if matches!(provenance, Provenance::Bundled) {
                IntegrityStatus::Bundled
            } else {
                integrity_by_name
                    .get(&name)
                    .cloned()
                    .unwrap_or(IntegrityStatus::Unknown)
            };
            manifests.push(manifest.clone());
            resolved.push(ResolvedPlugin {
                name,
                provenance,
                dir,
                manifest,
                integrity,
                init_source,
            });
        }
        (resolved, manifests)
    }

    /// Scans `<config>/plugins/` for implicitly-local plugins (6a): real
    /// directories the user dropped in by hand, not declared anywhere.
    ///
    /// A slot is implicit-local iff it is a **real directory** (distinguished
    /// from a cache-managed slot via [`fs::symlink_metadata`] — every
    /// Mote-managed slot for a `path:`/git/bundled plugin is a *symlink*, so a
    /// symlink is skipped), its directory name is a valid [`PluginName`] not in
    /// `already` (the declared + seeded set, so a declared plugin is never
    /// double-counted), and it contains a readable `init.lua`.
    ///
    /// Returns `(name, Provenance::ImplicitLocal)` for each detected plugin, in
    /// lexicographic name order (a [`BTreeSet`](std::collections::BTreeSet) read
    /// of the directory). The caller appends these to the resolve set; a
    /// dev-mode override may still upgrade an implicit dir to
    /// [`Provenance::DevMode`] in [`resolve_entries`](Self::resolve_entries).
    fn detect_implicit_local(
        &self,
        already: &std::collections::BTreeSet<PluginName>,
    ) -> Vec<(PluginName, Provenance)> {
        let plugins_dir = self.config_dir.join("plugins");
        // No plugins/ dir yet (fresh profile) → nothing to detect.
        let Ok(read) = fs::read_dir(&plugins_dir) else {
            return Vec::new();
        };

        let mut detected: std::collections::BTreeMap<PluginName, Provenance> =
            std::collections::BTreeMap::new();
        for entry in read.flatten() {
            // symlink_metadata does NOT follow the link: a cache-managed slot is
            // a symlink and must be skipped; only a REAL directory is implicit.
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.is_dir() || meta.file_type().is_symlink() {
                continue;
            }
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(name) = PluginName::new(file_name) else {
                continue;
            };
            if already.contains(&name) {
                continue;
            }
            // Must contain a readable init.lua to be a loadable plugin.
            if !entry.path().join("init.lua").is_file() {
                continue;
            }
            detected.insert(name, Provenance::ImplicitLocal);
        }
        detected.into_iter().collect()
    }

    /// # Errors
    ///
    /// Returns [`ManagerError`] only for fatal errors (lock parse/write, config
    /// parse). Per-plugin failures go into [`SyncReport::failed`].
    pub fn sync(&self) -> Result<SyncReport, ManagerError> {
        // Public sync is identity-agnostic: it reconciles the *global* composed
        // set (plugins.lua + managed.lua). Per-identity overlay plugins are
        // reconciled by `resolved_set` (which syncs the identity-composed set),
        // not here — the CLI and other callers operate on the shared global set.
        let spec_set = self.composed_spec_set()?;
        self.sync_specs(&spec_set)
    }

    /// Reconciles every spec in `spec_set` (fetch/link/hash + integrity),
    /// writing the lock once at the end if anything changed.
    ///
    /// Shared by [`sync`](Self::sync) (global set) and
    /// [`resolved_set`](Self::resolved_set) (identity-composed set), so an
    /// overlay-added or overlay-overridden plugin is linked before
    /// `resolved_set` reads its active-link dir.
    ///
    // KNOWN LIMITATION (multi-identity follow-up): there is exactly one global
    // `plugins.lock`. When `resolved_set(Some(identity))` reconciles the
    // identity-composed set, a plugin that exists ONLY in a per-identity overlay
    // (`identities/<id>/plugins.lua`, not in global plugins.lua/managed.lua) gets
    // its lock entry written into that single global lock. This is benign today:
    // the sole session identity is `0` and no overlay file is created by default,
    // so the identity-composed set equals the global set. Multi-identity support
    // (a real per-identity overlay) must NOT let the global lock carry
    // per-identity-overlay entries — track a per-identity lock (or scoped
    // entries) when that phase lands. Not changed here.
    fn sync_specs(
        &self,
        spec_set: &crate::resolve::PluginSpecSet,
    ) -> Result<SyncReport, ManagerError> {
        let mut lock = self.load_lock()?;
        let mut report = SyncReport::default();
        let mut lock_dirty = false;

        for spec in spec_set.specs.values() {
            let name = &spec.name;
            let source: Source = match spec.source.parse() {
                Ok(s) => s,
                Err(e) => {
                    report.failed.push((name.clone(), ManagerError::Source(e)));
                    continue;
                }
            };

            match self.sync_one(name, &source, spec.version.as_deref(), &mut lock) {
                Ok((outcome, changed)) => {
                    if changed {
                        lock_dirty = true;
                    }
                    report.ok.push(outcome);
                }
                Err(e) => {
                    report.failed.push((name.clone(), e));
                }
            }
        }

        if lock_dirty {
            self.write_lock(&lock)?;
        }

        Ok(report)
    }

    /// Syncs a single plugin: resolves its source to a dir, hashes it, checks
    /// integrity, updates the lock. Returns the outcome + whether the lock changed.
    fn sync_one(
        &self,
        name: &PluginName,
        source: &Source,
        version: Option<&str>,
        lock: &mut LockFile,
    ) -> Result<(SyncOutcome, bool), ManagerError> {
        match source {
            Source::Path(raw_path) => self.sync_path(name, raw_path, lock),
            Source::Bundled => self.sync_bundled(name, lock),
            Source::Github { .. } | Source::Git { .. } => {
                self.sync_git(name, source, version, lock)
            }
        }
    }

    /// Syncs a `path:` plugin.
    fn sync_path(
        &self,
        name: &PluginName,
        raw_path: &Path,
        lock: &mut LockFile,
    ) -> Result<(SyncOutcome, bool), ManagerError> {
        let dir = Self::resolve_path_source(name, raw_path)?;

        // For path: sources, ensure the slot in plugins/ points to the dir.
        // It's a real dir / symlink to the user's dev dir.
        self.ensure_link(name, &dir)?;

        let actual = hash_dir(&dir)?;
        let integrity;
        let mut changed = false;

        if let Some(entry) = lock.plugins.get(name) {
            if entry.checksum == actual {
                integrity = IntegrityStatus::Verified;
            } else {
                // Informational for path: sources.
                integrity = IntegrityStatus::Mismatch {
                    actual,
                    expected: entry.checksum,
                };
                // Update the lock for path: sources (informational, no refusal).
                lock.plugins.insert(
                    name.clone(),
                    LockEntry {
                        source: Source::Path(raw_path.to_path_buf()),
                        commit: None,
                        checksum: actual,
                    },
                );
                changed = true;
            }
        } else {
            // No entry yet: record hash.
            lock.plugins.insert(
                name.clone(),
                LockEntry {
                    source: Source::Path(raw_path.to_path_buf()),
                    commit: None,
                    checksum: actual,
                },
            );
            changed = true;
            integrity = IntegrityStatus::Unknown;
        }

        Ok((
            SyncOutcome {
                name: name.clone(),
                integrity,
            },
            changed,
        ))
    }

    /// Syncs a `bundled` plugin.
    fn sync_bundled(
        &self,
        name: &PluginName,
        lock: &mut LockFile,
    ) -> Result<(SyncOutcome, bool), ManagerError> {
        let key = unpack_into_cache(name, &self.cache)?;
        self.cache.link(name, &key.commit)?;

        let dir = self.cache.commit_dir(name, &key.commit);
        let actual = hash_dir(&dir)?;
        let mut changed = false;

        let current_entry = lock.plugins.get(name).cloned();
        let matches = current_entry
            .as_ref()
            .is_some_and(|e| e.checksum == actual && e.commit.as_deref() == Some(&key.commit));

        if !matches {
            lock.plugins.insert(
                name.clone(),
                LockEntry {
                    source: Source::Bundled,
                    commit: Some(key.commit),
                    checksum: actual,
                },
            );
            changed = true;
        }

        Ok((
            SyncOutcome {
                name: name.clone(),
                integrity: IntegrityStatus::Bundled,
            },
            changed,
        ))
    }

    /// Syncs a git-backed plugin.
    fn sync_git(
        &self,
        name: &PluginName,
        source: &Source,
        version: Option<&str>,
        lock: &mut LockFile,
    ) -> Result<(SyncOutcome, bool), ManagerError> {
        // If we already have a lock entry with a pinned commit, prefer
        // reusing the cached copy to avoid a fetch (offline-safe, R5).
        let pinned_commit = lock.plugins.get(name).and_then(|e| e.commit.clone());

        let commit = if let Some(ref c) = pinned_commit {
            // Check whether the pinned commit is in the cache.
            let dir = self.cache.commit_dir(name, c);
            if dir.is_dir() {
                c.clone()
            } else {
                // Not cached: fetch.
                let fetched = fetch(source, version)?;
                let c2 = fetched.commit.clone();
                self.cache.store(name, &c2, fetched.tree.path())?;
                c2
            }
        } else {
            // No lock entry: must fetch.
            let fetched = fetch(source, version)?;
            let c = fetched.commit.clone();
            self.cache.store(name, &c, fetched.tree.path())?;
            c
        };

        let dir = self.cache.commit_dir(name, &commit);
        let actual = hash_dir(&dir)?;

        // Integrity check (plan §3.5).
        let integrity = if let Some(entry) = lock.plugins.get(name) {
            if entry.checksum == actual {
                IntegrityStatus::Verified
            } else {
                // Hard mismatch for git sources: do NOT update lock, leave
                // existing state, report mismatch.
                return Ok((
                    SyncOutcome {
                        name: name.clone(),
                        integrity: IntegrityStatus::Mismatch {
                            actual,
                            expected: entry.checksum,
                        },
                    },
                    false,
                ));
            }
        } else {
            IntegrityStatus::Unknown
        };

        // Link and update lock.
        self.cache.link(name, &commit)?;
        let mut changed = false;
        let current_matches = lock
            .plugins
            .get(name)
            .is_some_and(|e| e.commit.as_deref() == Some(&commit) && e.checksum == actual);

        if !current_matches {
            lock.plugins.insert(
                name.clone(),
                LockEntry {
                    source: source.clone(),
                    commit: Some(commit),
                    checksum: actual,
                },
            );
            changed = true;
        }

        Ok((
            SyncOutcome {
                name: name.clone(),
                integrity,
            },
            changed,
        ))
    }

    /// Adds a plugin from `source_str` to `managed.lua`.
    ///
    /// Parses the source, resolves/fetches the plugin dir (for git: fetches;
    /// for path: verifies), computes the dir hash, writes a `managed.lua` entry
    /// and a `plugins.lock` entry, and creates the active link.
    ///
    /// Does **not** approve — approval happens at load time.
    ///
    /// Returns `(name, commit_or_path)`.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] on any I/O, parse, fetch, or integrity failure.
    pub fn add(
        &self,
        source_str: &str,
        version: Option<String>,
    ) -> Result<(PluginName, String), ManagerError> {
        let source: Source = source_str.parse()?;
        let mut lock = self.load_lock()?;
        let mut managed = self.load_managed()?;

        let (name, dir, commit_label) = match &source {
            Source::Path(raw) => {
                // Name is not known yet; resolve the path first, then read from manifest.
                let dir = Self::canonicalize_path(raw)?;
                // Derive the canonical name from the plugin's manifest.
                let init = dir.join("init.lua");
                let source_code =
                    fs::read_to_string(&init).map_err(|e| ManagerError::io(&init, e))?;
                let loaded =
                    mote_lua::load_plugin(&source_code, "add").map_err(ManagerError::PluginLoad)?;
                let name = loaded.manifest().name.clone();
                let label = dir.to_string_lossy().into_owned();
                (name, dir, label)
            }
            Source::Github { .. } | Source::Git { .. } => {
                let fetched = fetch(&source, version.as_deref())?;
                let commit = fetched.commit.clone();
                // Derive name from the manifest in the fetched tree.
                let tree_root = fetched.tree.path().join("checkout");
                let init = tree_root.join("init.lua");
                let source_code =
                    fs::read_to_string(&init).map_err(|e| ManagerError::io(&init, e))?;
                let loaded =
                    mote_lua::load_plugin(&source_code, "add").map_err(ManagerError::PluginLoad)?;
                let name = loaded.manifest().name.clone();
                let key = self.cache.store(&name, &commit, &tree_root)?;
                let dir = self.cache.commit_dir(&name, &key.commit);
                (name, dir, commit)
            }
            Source::Bundled => {
                // Bundled plugins: derive name from bundle names.
                // For `add bundled` we don't know which one; this is a
                // usage error. Return the first bundled name as a hint.
                return Err(ManagerError::Source(SourceParseError::UnknownScheme(
                    "bundled sources are resolved by name; use `add bundled` only \
                         via the shell integration"
                        .into(),
                )));
            }
        };

        let checksum = hash_dir(&dir)?;

        // Write lock entry.
        lock.plugins.insert(
            name.clone(),
            LockEntry {
                source: source.clone(),
                commit: if source.is_git() {
                    Some(commit_label.clone())
                } else {
                    None
                },
                checksum,
            },
        );
        self.write_lock(&lock)?;

        // Write managed.lua entry.
        managed.upsert(name.clone(), source_str.to_owned(), version);
        managed.write_atomic(&self.managed_lua_path())?;

        // Link the plugin.
        match &source {
            Source::Path(_) => {
                self.ensure_link(&name, &dir)?;
            }
            Source::Github { .. } | Source::Git { .. } => {
                self.cache.link(&name, &commit_label)?;
            }
            Source::Bundled => {}
        }

        Ok((name, commit_label))
    }

    /// Removes a plugin.
    ///
    /// If the plugin is in `managed.lua`: drops it from `managed.lua`, drops
    /// the lock entry, and removes the symlink. **Cache entry is retained** (gc
    /// reclaims later).
    ///
    /// If the plugin is **only** in the user's `plugins.lua` (not managed):
    /// returns [`RemoveOutcome::UserConfigOnly`] and does NOT touch `plugins.lua`.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] on I/O, parse, or write errors.
    pub fn remove(&self, name: &PluginName) -> Result<RemoveOutcome, ManagerError> {
        let mut managed = self.load_managed()?;
        let was_managed = managed.remove(name);

        if was_managed {
            managed.write_atomic(&self.managed_lua_path())?;

            let mut lock = self.load_lock()?;
            lock.plugins.remove(name);
            self.write_lock(&lock)?;

            // Remove the symlink/slot but leave the cache.
            self.remove_slot(name)?;

            return Ok(RemoveOutcome::Removed);
        }

        // Check if it's in the user's plugins.lua.
        let user_path = self.plugins_lua_path();
        if user_path.exists() {
            let src =
                fs::read_to_string(&user_path).map_err(|e| ManagerError::io(&user_path, e))?;
            let spec = mote_lua::eval_config(&src, "plugins.lua")?;
            let in_user = spec.plugins.iter().any(|e| e.key == name.as_str());
            if in_user {
                return Ok(RemoveOutcome::UserConfigOnly);
            }
        }

        Ok(RemoveOutcome::NotFound)
    }

    /// Changes the source of a managed plugin.
    ///
    /// Updates `managed.lua`, re-fetches/re-resolves, re-hashes, re-links,
    /// and updates the lock.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] if the plugin is not in `managed.lua`, or on
    /// I/O / fetch / hash failure.
    pub fn set_source(&self, name: &PluginName, new_source: &str) -> Result<(), ManagerError> {
        let mut managed = self.load_managed()?;
        let existing_version = managed
            .entries()
            .find(|e| &e.name == name)
            .map(|e| e.version.clone());

        if existing_version.is_none() && !managed.entries().any(|e| &e.name == name) {
            return Err(ManagerError::NotFound(name.clone()));
        }

        let version = existing_version.flatten();

        // Remove old entry and re-add with new source.
        managed.remove(name);
        managed.upsert(name.clone(), new_source.to_owned(), version.clone());
        managed.write_atomic(&self.managed_lua_path())?;

        // Now sync just this plugin.
        let source: Source = new_source.parse()?;
        let mut lock = self.load_lock()?;
        self.sync_one(name, &source, version.as_deref(), &mut lock)?;
        self.write_lock(&lock)?;

        Ok(())
    }

    /// Rolls back a plugin to the previously-cached commit.
    ///
    /// Relinks `plugins/<name>` to the second-most-recent commit in the cache
    /// (the one before the current active commit). Updates the lock's commit
    /// pointer. Does not fetch.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError::NoPreviousCommit`] if there is no cached commit
    /// to roll back to.
    pub fn rollback(&self, name: &PluginName) -> Result<(), ManagerError> {
        let active = self.cache.resolve_active(name);
        let mut commits = self.cache.list_commits(name)?;

        // Remove the active commit from the list to find "previous".
        if let Some(ref a) = active {
            commits.retain(|c| c != a);
        }

        // The "previous" commit is the last remaining (highest sorted, since
        // list_commits returns them sorted lexicographically).
        let prev = commits
            .last()
            .cloned()
            .ok_or_else(|| ManagerError::NoPreviousCommit {
                name: name.clone(),
                active: active.clone(),
            })?;

        self.cache.link(name, &prev)?;

        // Update the lock's commit pointer.
        let mut lock = self.load_lock()?;
        if let Some(entry) = lock.plugins.get_mut(name) {
            entry.commit = Some(prev);
        }
        self.write_lock(&lock)?;

        Ok(())
    }

    /// Returns the diff between the candidate manifest on disk and the stored
    /// approved hash.
    ///
    /// If the plugin has never been approved, the diff treats the prior as
    /// "empty" — the entire manifest is an expansion (all-additions).
    ///
    /// The resolved plugin directory is read from the active link.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] if the plugin directory cannot be found, the
    /// manifest cannot be loaded, or the approval store fails.
    pub fn diff(&self, name: &PluginName) -> Result<DiffReport, ManagerError> {
        let link = self.cache.active_link(name);
        if !link.exists() {
            return Err(ManagerError::NotFound(name.clone()));
        }

        let manifest = Self::load_manifest_from_dir(name, &link)?;
        let candidate_hash = ApprovalHash::of(&manifest);

        let prior = if let Some(h) = self.approval.get(name)? {
            h
        } else {
            Self::empty_approval_hash()?
        };
        Ok(diff(&prior, &candidate_hash))
    }

    /// Migrates a plugin from `managed.lua` to `plugins.lua`.
    ///
    /// - `write == false`: returns a snippet that can be pasted into
    ///   `plugins.lua` ([`ImportOutcome::Snippet`]).
    /// - `write == true`: verifies `plugins.lua` parses, then appends the
    ///   snippet (append-only, never touches existing lines), drops the entry
    ///   from `managed.lua`. If `plugins.lua` does not parse, falls back to
    ///   [`ImportOutcome::PluginsLuaDoesNotParse`] with the snippet.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] if the plugin is not in `managed.lua`, or on
    /// I/O / write errors.
    pub fn import(&self, name: &PluginName, write: bool) -> Result<ImportOutcome, ManagerError> {
        let managed = self.load_managed()?;
        let entry = managed
            .entries()
            .find(|e| &e.name == name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?
            .clone();

        // Generate the snippet.
        let snippet = match &entry.version {
            None => format!(
                "mote.plugins({{\n  [\"{name}\"] = {{ source = \"{src}\" }},\n}})\n",
                name = entry.name.as_str(),
                src = entry.source
            ),
            Some(v) => format!(
                "mote.plugins({{\n  [\"{name}\"] = {{ source = \"{src}\", version = \"{ver}\" }},\n}})\n",
                name = entry.name.as_str(),
                src = entry.source,
                ver = v,
            ),
        };

        if !write {
            return Ok(ImportOutcome::Snippet(snippet));
        }

        // write=true: try to append.
        let user_path = self.plugins_lua_path();

        // If plugins.lua exists, verify it parses first.
        if user_path.exists() {
            let existing =
                fs::read_to_string(&user_path).map_err(|e| ManagerError::io(&user_path, e))?;
            if mote_lua::eval_config(&existing, "plugins.lua").is_err() {
                // Does not parse: return snippet, do not touch the file.
                return Ok(ImportOutcome::PluginsLuaDoesNotParse(snippet));
            }
        }

        // Append the snippet to plugins.lua.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&user_path)
            .map_err(|e| ManagerError::io(&user_path, e))?;
        std::io::Write::write_all(&mut file, format!("\n{snippet}").as_bytes())
            .map_err(|e| ManagerError::io(&user_path, e))?;

        // Drop from managed.lua.
        let mut managed_mut = self.load_managed()?;
        managed_mut.remove(name);
        managed_mut.write_atomic(&self.managed_lua_path())?;

        Ok(ImportOutcome::Written)
    }

    /// Reclaims cache entries not referenced by any lock entry and not the
    /// immediately-previous commit kept for rollback (R13: retain active +
    /// one previous commit per plugin).
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] on I/O errors during cache enumeration or
    /// removal.
    pub fn gc(&self) -> Result<GcReport, ManagerError> {
        let lock = self.load_lock()?;
        let mut report = GcReport::default();

        // Collect all plugin names that have cache entries.
        let cache_root = &self.cache_dir;
        if !cache_root.exists() {
            return Ok(report);
        }

        let mut names: Vec<PluginName> = Vec::new();
        for entry in fs::read_dir(cache_root).map_err(|e| ManagerError::io(cache_root, e))? {
            let entry = entry.map_err(|e| ManagerError::io(cache_root, e))?;
            if entry.path().is_dir()
                && let Some(n) = entry.file_name().to_str()
                && let Ok(name) = PluginName::new(n)
            {
                names.push(name);
            }
        }

        for name in names {
            let commits = self.cache.list_commits(&name)?;
            let active = lock.plugins.get(&name).and_then(|e| e.commit.clone());

            // Determine the set to retain: active + one previous.
            let mut retain: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            if let Some(ref a) = active {
                retain.insert(a.clone());
            }

            // Previous: the commit just before the active one in sorted order.
            // list_commits returns lexicographic order; find the one before active.
            if let Some(ref a) = active {
                if let Some(pos) = commits.iter().position(|c| c == a)
                    && pos > 0
                {
                    retain.insert(commits[pos - 1].clone());
                }
            } else if let Some(last) = commits.last() {
                // No active lock entry; retain the most recent cached commit.
                retain.insert(last.clone());
            }

            for commit in &commits {
                if !retain.contains(commit) {
                    self.cache.gc_commit(&name, commit)?;
                    report.reclaimed.push((name.clone(), commit.clone()));
                }
            }
        }

        Ok(report)
    }

    /// Pins a plugin's current state: computes the current dir hash, writes it
    /// to the lock (resolving a [`IntegrityStatus::Mismatch`]), and records the
    /// current [`ApprovalHash`] as approved.
    ///
    /// This is the "I edited this on purpose" escape hatch for `path:` sources
    /// and for git sources after a deliberate local edit.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] if the plugin's active link cannot be found or
    /// any I/O / storage operation fails.
    pub fn pin(&self, name: &PluginName) -> Result<(), ManagerError> {
        let link = self.cache.active_link(name);
        if !link.exists() {
            return Err(ManagerError::NotFound(name.clone()));
        }

        // Recompute dir hash.
        let actual = hash_dir(&link)?;

        // Update the lock.
        let mut lock = self.load_lock()?;
        if let Some(entry) = lock.plugins.get_mut(name) {
            entry.checksum = actual;
        } else {
            // No lock entry yet: need source info from managed or user config.
            let spec_set = self.composed_spec_set()?;
            if let Some(spec) = spec_set.specs.get(name) {
                let source: Source = spec.source.parse()?;
                lock.plugins.insert(
                    name.clone(),
                    LockEntry {
                        source,
                        commit: self.cache.resolve_active(name),
                        checksum: actual,
                    },
                );
            } else {
                return Err(ManagerError::NotFound(name.clone()));
            }
        }
        self.write_lock(&lock)?;

        // Record the current manifest as approved.
        let manifest = Self::load_manifest_from_dir(name, &link)?;
        let hash = ApprovalHash::of(&manifest);
        self.approval.put(name, &hash)?;

        Ok(())
    }

    /// Fetches the latest commit for a git-backed plugin and checks whether the
    /// new manifest expands the approved permissions.
    ///
    /// - If the diff [`DiffReport::is_expansion`]: returns
    ///   [`UpdateOutcome::NeedsReapproval`] and does **not** relink (the running
    ///   instance is preserved).
    /// - Otherwise: relinks to the new commit and updates the lock →
    ///   [`UpdateOutcome::Applied`].
    ///
    /// The actual runtime reload is the shell's responsibility (3.6), not this
    /// method.
    ///
    /// For `path:` sources this is equivalent to a re-hash + re-link (no
    /// fetch). For `bundled` sources it re-unpacks if the bundled version
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] on source-parse, fetch, hash, or approval-store
    /// errors.
    pub fn update(&self, name: &PluginName) -> Result<UpdateOutcome, ManagerError> {
        let spec_set = self.composed_spec_set()?;
        let spec = spec_set
            .specs
            .get(name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        let source: Source = spec.source.parse()?;
        let mut lock = self.load_lock()?;

        let (new_commit, dir) = match &source {
            Source::Path(raw) => {
                let dir = Self::resolve_path_source(name, raw)?;
                let label = dir.to_string_lossy().into_owned();
                (label, dir)
            }
            Source::Bundled => {
                let key = unpack_into_cache(name, &self.cache)?;
                let dir = self.cache.commit_dir(name, &key.commit);
                (key.commit, dir)
            }
            Source::Github { .. } | Source::Git { .. } => {
                // Fetch latest (no pinned version for update).
                let fetched = fetch(&source, spec.version.as_deref())?;
                let commit = fetched.commit.clone();
                self.cache.store(name, &commit, fetched.tree.path())?;
                let dir = self.cache.commit_dir(name, &commit);
                (commit, dir)
            }
        };

        let new_checksum = hash_dir(&dir)?;
        let manifest = Self::load_manifest_from_dir(name, &dir)?;
        let candidate_hash = ApprovalHash::of(&manifest);

        // Compare against stored approved hash.
        let prior = if let Some(h) = self.approval.get(name)? {
            h
        } else {
            Self::empty_approval_hash()?
        };
        let report = diff(&prior, &candidate_hash);

        if report.is_expansion() {
            return Ok(UpdateOutcome::NeedsReapproval { report });
        }

        // Non-expansion: relink and update lock.
        match &source {
            Source::Path(_) => {
                self.ensure_link(name, &dir)?;
            }
            Source::Bundled | Source::Github { .. } | Source::Git { .. } => {
                self.cache.link(name, &new_commit)?;
            }
        }

        lock.plugins.insert(
            name.clone(),
            LockEntry {
                source: source.clone(),
                commit: if source.is_git() {
                    Some(new_commit.clone())
                } else {
                    None
                },
                checksum: new_checksum,
            },
        );
        self.write_lock(&lock)?;

        Ok(UpdateOutcome::Applied { commit: new_commit })
    }

    /// Stores the candidate manifest's approval hash as approved.
    ///
    /// This is the "approve for next launch" operation for `mote plugin review`.
    /// It records the current on-disk manifest's [`ApprovalHash`] so the plugin
    /// loads without prompting on next launch.
    ///
    /// # Errors
    ///
    /// Returns [`ManagerError`] if the plugin's active link is missing or the
    /// approval store fails.
    pub fn approve(&self, name: &PluginName) -> Result<(), ManagerError> {
        let link = self.cache.active_link(name);
        if !link.exists() {
            return Err(ManagerError::NotFound(name.clone()));
        }
        let manifest = Self::load_manifest_from_dir(name, &link)?;
        let hash = ApprovalHash::of(&manifest);
        self.approval.put(name, &hash)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

/// Removes whatever is at `path`: a symlink, a real directory, or nothing.
fn remove_link_or_dir(path: &Path) -> Result<(), ManagerError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || meta.is_file() {
                fs::remove_file(path).map_err(|e| ManagerError::io(path, e))
            } else {
                fs::remove_dir_all(path).map_err(|e| ManagerError::io(path, e))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ManagerError::io(path, e)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;

    use mote_storage::Store;
    use mote_types::PluginName;

    use super::*;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// A test fixture holding the injected temp dirs and a manager.
    struct Fixture {
        _config: tempfile::TempDir,
        _cache: tempfile::TempDir,
        config_dir: PathBuf,
        mgr: PluginManager,
        store: Store,
    }

    fn fixture() -> Fixture {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let config_dir = config.path().to_path_buf();
        let cache_dir = cache.path().to_path_buf();
        let store = Store::open_in_memory().unwrap();
        let mgr = PluginManager::new(&config_dir, &cache_dir, &store);
        Fixture {
            _config: config,
            _cache: cache,
            config_dir,
            mgr,
            store,
        }
    }

    fn name(s: &str) -> PluginName {
        PluginName::new(s).unwrap()
    }

    /// Writes a minimal valid plugin dir to `path`.
    fn write_plugin(dir: &Path, plugin_name: &str, permissions: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        let perms = permissions
            .iter()
            .map(|p| format!(r#""{p}""#))
            .collect::<Vec<_>>()
            .join(", ");
        let lua = format!(
            r#"
local M = {{}}
M.manifest = {{
    schema = "v1",
    name = "{plugin_name}",
    version = "1",
    permissions = {{ {perms} }},
    identity_scope = "global",
}}
return M
"#
        );
        fs::write(dir.join("init.lua"), lua).unwrap();
    }

    /// Writes a `plugins.lua` with the given entries.
    fn write_plugins_lua(config_dir: &Path, entries: &[(&str, &str)]) {
        let body = entries
            .iter()
            .map(|(k, src)| format!(r#"  ["{k}"] = {{ source = "{src}" }},"#))
            .collect::<Vec<_>>()
            .join("\n");
        let lua = format!("mote.plugins({{\n{body}\n}})\n");
        fs::write(config_dir.join("plugins.lua"), lua).unwrap();
    }

    /// Writes a per-identity overlay at `<config>/identities/<id>/plugins.lua`.
    fn write_identity_plugins_lua(config_dir: &Path, identity: u64, entries: &[(&str, &str)]) {
        let dir = config_dir.join("identities").join(identity.to_string());
        fs::create_dir_all(&dir).unwrap();
        let body = entries
            .iter()
            .map(|(k, src)| format!(r#"  ["{k}"] = {{ source = "{src}" }},"#))
            .collect::<Vec<_>>()
            .join("\n");
        let lua = format!("mote.plugins({{\n{body}\n}})\n");
        fs::write(dir.join("plugins.lua"), lua).unwrap();
    }

    // -----------------------------------------------------------------------
    // default_dirs / resolve_dirs_from: canonical XDG resolver
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_dirs_prefers_xdg_when_set() {
        let home = std::ffi::OsString::from("/home/user");
        let (config, cache) = PluginManager::resolve_dirs_from(
            Some("/xdg/config".into()),
            Some("/xdg/cache".into()),
            Some(home.as_os_str()),
        )
        .unwrap();
        assert_eq!(config, PathBuf::from("/xdg/config/mote"));
        assert_eq!(cache, PathBuf::from("/xdg/cache/mote/plugins"));
    }

    #[test]
    fn resolve_dirs_falls_back_to_home_when_xdg_unset() {
        let home = std::ffi::OsString::from("/home/user");
        let (config, cache) =
            PluginManager::resolve_dirs_from(None, None, Some(home.as_os_str())).unwrap();
        assert_eq!(config, PathBuf::from("/home/user/.config/mote"));
        assert_eq!(cache, PathBuf::from("/home/user/.cache/mote/plugins"));
    }

    #[test]
    fn resolve_dirs_mixes_xdg_config_and_home_cache() {
        // XDG_CONFIG_HOME set but XDG_CACHE_HOME unset: each axis resolves
        // independently.
        let home = std::ffi::OsString::from("/home/user");
        let (config, cache) = PluginManager::resolve_dirs_from(
            Some("/xdg/config".into()),
            None,
            Some(home.as_os_str()),
        )
        .unwrap();
        assert_eq!(config, PathBuf::from("/xdg/config/mote"));
        assert_eq!(cache, PathBuf::from("/home/user/.cache/mote/plugins"));
    }

    #[test]
    fn resolve_dirs_none_when_no_home_and_no_xdg() {
        assert!(PluginManager::resolve_dirs_from(None, None, None).is_none());
    }

    // -----------------------------------------------------------------------
    // resolved_set: composes declared + seeded bundled defaults
    // -----------------------------------------------------------------------

    #[test]
    fn resolved_set_includes_path_plugin_and_seeded_bundled_defaults() {
        let f = fixture();

        // A user-declared path: plugin.
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "my-plugin", &["storage:persistent"]);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("my-plugin", &src)]);

        let resolved = f.mgr.resolved_set(None).unwrap();

        let by_name: std::collections::BTreeMap<&str, &ResolvedPlugin> =
            resolved.iter().map(|r| (r.name.as_str(), r)).collect();

        // The path plugin resolves with Path provenance + a populated manifest.
        let mine = by_name.get("my-plugin").expect("my-plugin resolved");
        assert_eq!(mine.provenance, Provenance::Path);
        assert_eq!(mine.manifest.name.as_str(), "my-plugin");
        assert!(
            mine.init_source.contains("M.manifest"),
            "init_source must be the real init.lua"
        );

        // Bundled first-party defaults are seeded (none were declared).
        // urlbar was removed in Phase 5a; history owns ui:urlbar_provider.
        // Use bookmarks as a representative bundled plugin.
        let bm = by_name.get("bookmarks").expect("bookmarks seeded");
        assert_eq!(bm.provenance, Provenance::Bundled);
        assert_eq!(bm.integrity, IntegrityStatus::Bundled);
        assert!(!bm.init_source.is_empty());
        let wsm = by_name
            .get("workspace-manager")
            .expect("workspace-manager seeded");
        assert_eq!(wsm.provenance, Provenance::Bundled);
    }

    #[test]
    fn resolved_set_skips_plugin_with_unparsable_source() {
        // Per the resilience contract, a single bad plugin must not abort the
        // whole resolution — it logs + skips and the rest continue. We exercise
        // the source-parse skip path (unknown scheme), which is deterministic
        // and exercises no network.
        let f = fixture();
        write_plugins_lua(&f.config_dir, &[("bad-plugin", "nonsense:whatever")]);

        // Must NOT error — the bad plugin is skipped, the bundled defaults seed.
        let resolved = f
            .mgr
            .resolved_set(None)
            .expect("resolved_set must not fail fatally");
        let names: Vec<&str> = resolved.iter().map(|r| r.name.as_str()).collect();

        assert!(
            !names.contains(&"bad-plugin"),
            "plugin with unparsable source must be omitted: {names:?}"
        );
        assert!(
            names.contains(&"bookmarks"),
            "bundled defaults must still seed when a sibling fails: {names:?}"
        );
        assert!(
            names.contains(&"workspace-manager"),
            "bundled defaults must still seed when a sibling fails: {names:?}"
        );
    }

    #[test]
    fn resolved_set_does_not_seed_when_a_bundled_default_is_declared() {
        let f = fixture();
        // Declaring even one bundled default suppresses auto-seeding.
        // Use bookmarks as a representative bundled plugin (urlbar was removed
        // in Phase 5a; history owns ui:urlbar_provider from this point on).
        write_plugins_lua(&f.config_dir, &[("bookmarks", "bundled")]);

        let resolved = f.mgr.resolved_set(None).unwrap();
        let names: Vec<&str> = resolved.iter().map(|r| r.name.as_str()).collect();

        assert!(names.contains(&"bookmarks"), "declared bookmarks present");
        assert!(
            !names.contains(&"workspace-manager"),
            "workspace-manager must NOT be auto-seeded once a bundled default is declared"
        );
        let bm = resolved
            .iter()
            .find(|r| r.name.as_str() == "bookmarks")
            .unwrap();
        assert_eq!(bm.provenance, Provenance::Bundled);
    }

    // -----------------------------------------------------------------------
    // 6b: per-identity overlay composes; dev_mode unions across layers
    // -----------------------------------------------------------------------

    #[test]
    fn identity_overlay_adds_plugin_only_for_that_identity() {
        let f = fixture();

        // Global plugins.lua declares plugin-a.
        let p_a = tempfile::tempdir().unwrap();
        write_plugin(p_a.path(), "plugin-a", &[]);
        let src_a = format!("path:{}", p_a.path().display());
        write_plugins_lua(&f.config_dir, &[("plugin-a", &src_a)]);

        // Identity 0's overlay ADDS plugin-b.
        let p_b = tempfile::tempdir().unwrap();
        write_plugin(p_b.path(), "plugin-b", &[]);
        let src_b = format!("path:{}", p_b.path().display());
        write_identity_plugins_lua(&f.config_dir, 0, &[("plugin-b", &src_b)]);

        // resolved_set(Some(0)) includes the overlay's plugin-b.
        let id = IdentityId::new(0);
        let with_overlay: Vec<String> = f
            .mgr
            .resolved_set(Some(&id))
            .unwrap()
            .iter()
            .map(|r| r.name.to_string())
            .collect();
        assert!(
            with_overlay.contains(&"plugin-a".to_owned()),
            "global plugin-a present: {with_overlay:?}"
        );
        assert!(
            with_overlay.contains(&"plugin-b".to_owned()),
            "overlay plugin-b present for identity 0: {with_overlay:?}"
        );

        // resolved_set(None) does NOT include the overlay's plugin-b.
        let without_overlay: Vec<String> = f
            .mgr
            .resolved_set(None)
            .unwrap()
            .iter()
            .map(|r| r.name.to_string())
            .collect();
        assert!(
            without_overlay.contains(&"plugin-a".to_owned()),
            "global plugin-a present without overlay: {without_overlay:?}"
        );
        assert!(
            !without_overlay.contains(&"plugin-b".to_owned()),
            "overlay plugin-b must be absent without an identity: {without_overlay:?}"
        );
    }

    #[test]
    fn identity_overlay_overrides_global_plugin_source() {
        let f = fixture();

        // Global declares "shared" pointing at dir A (version "1").
        let dir_a = tempfile::tempdir().unwrap();
        write_plugin(dir_a.path(), "shared", &[]);
        let src_a = format!("path:{}", dir_a.path().display());
        write_plugins_lua(&f.config_dir, &[("shared", &src_a)]);

        // Identity 0 overrides "shared" to point at dir B, whose init.lua is
        // distinguishable (a unique marker permission) so we can prove the
        // overlay's source — not the global one — was resolved.
        let dir_b = tempfile::tempdir().unwrap();
        write_plugin(dir_b.path(), "shared", &["events:emit"]);
        let src_b = format!("path:{}", dir_b.path().display());
        write_identity_plugins_lua(&f.config_dir, 0, &[("shared", &src_b)]);

        // The overlay (last layer) wins: the resolved manifest is B's, so it
        // carries B's distinguishing permission.
        let id = IdentityId::new(0);
        let resolved = f.mgr.resolved_set(Some(&id)).unwrap();
        let shared = resolved
            .iter()
            .find(|r| r.name.as_str() == "shared")
            .expect("shared resolved");
        assert!(
            shared
                .manifest
                .permissions
                .iter()
                .any(|p| p == "events:emit"),
            "overlay source (dir B) must override the global one (overlay wins); \
             resolved perms: {:?}",
            shared.manifest.permissions
        );
    }

    #[test]
    fn dev_mode_unions_across_plugins_lua_and_managed_lua() {
        let f = fixture();

        // plugins.lua contributes one dev_mode directory + one dev_mode plugin.
        fs::write(
            f.config_dir.join("plugins.lua"),
            "mote.dev_mode({ directories = { \"/dev/from-user\" }, plugins = { \"user-dev\" } })\n",
        )
        .unwrap();

        // managed.lua contributes a different dev_mode directory + plugin.
        fs::write(
            f.config_dir.join("managed.lua"),
            "mote.dev_mode({ directories = { \"/dev/from-managed\" }, plugins = { \"managed-dev\" } })\n",
        )
        .unwrap();

        let (_specs, dev_mode) = f.mgr.composed_config(None).unwrap();
        assert!(
            dev_mode.directories.iter().any(|d| d == "/dev/from-user"),
            "user dev dir merged: {:?}",
            dev_mode.directories
        );
        assert!(
            dev_mode
                .directories
                .iter()
                .any(|d| d == "/dev/from-managed"),
            "managed dev dir merged: {:?}",
            dev_mode.directories
        );
        assert!(dev_mode.plugins.iter().any(|p| p == "user-dev"));
        assert!(dev_mode.plugins.iter().any(|p| p == "managed-dev"));
    }

    #[test]
    fn dev_mode_unions_identity_overlay_and_dedups() {
        let f = fixture();

        // plugins.lua: one dir, shared between layers (must dedup).
        fs::write(
            f.config_dir.join("plugins.lua"),
            "mote.dev_mode({ directories = { \"/dev/shared\" } })\n",
        )
        .unwrap();

        // Identity overlay: the shared dir again + an identity-only one.
        let dir = f.config_dir.join("identities").join("0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugins.lua"),
            "mote.dev_mode({ directories = { \"/dev/shared\", \"/dev/identity-only\" } })\n",
        )
        .unwrap();

        let id = IdentityId::new(0);
        let (_specs, dev_mode) = f.mgr.composed_config(Some(&id)).unwrap();
        let shared_count = dev_mode
            .directories
            .iter()
            .filter(|d| *d == "/dev/shared")
            .count();
        assert_eq!(shared_count, 1, "shared dir must be deduplicated");
        assert!(
            dev_mode
                .directories
                .iter()
                .any(|d| d == "/dev/identity-only"),
            "identity-overlay dev dir merged: {:?}",
            dev_mode.directories
        );
    }

    // -----------------------------------------------------------------------
    // 6c: dev-mode marking — provenance overridden to DevMode
    // -----------------------------------------------------------------------

    #[test]
    fn plugin_under_dev_mode_directory_resolves_as_dev_mode() {
        let f = fixture();

        // A workspace dir that holds the dev plugin in a sub-directory; the
        // dev_mode.directories entry is the workspace root.
        let workspace = tempfile::tempdir().unwrap();
        let plugin_dir = workspace.path().join("under-dev");
        write_plugin(&plugin_dir, "under-dev", &[]);
        let src = format!("path:{}", plugin_dir.display());

        // plugins.lua declares the plugin AND a dev_mode directory covering it.
        fs::write(
            f.config_dir.join("plugins.lua"),
            format!(
                "mote.plugins({{ [\"under-dev\"] = {{ source = \"{src}\" }} }})\n\
                 mote.dev_mode({{ directories = {{ \"{}\" }} }})\n",
                workspace.path().display()
            ),
        )
        .unwrap();

        let resolved = f.mgr.resolved_set(None).unwrap();
        let under_dev = resolved
            .iter()
            .find(|r| r.name.as_str() == "under-dev")
            .expect("under-dev resolved");
        assert_eq!(
            under_dev.provenance,
            Provenance::DevMode,
            "a path plugin whose dir is under a dev_mode directory must be DevMode"
        );
    }

    #[test]
    fn plugin_named_in_dev_mode_plugins_resolves_as_dev_mode() {
        let f = fixture();

        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "named-dev", &[]);
        let src = format!("path:{}", plugin_dir.path().display());

        // plugins.lua declares the plugin AND lists it by name in dev_mode.
        fs::write(
            f.config_dir.join("plugins.lua"),
            format!(
                "mote.plugins({{ [\"named-dev\"] = {{ source = \"{src}\" }} }})\n\
                 mote.dev_mode({{ plugins = {{ \"named-dev\" }} }})\n"
            ),
        )
        .unwrap();

        let resolved = f.mgr.resolved_set(None).unwrap();
        let named_dev = resolved
            .iter()
            .find(|r| r.name.as_str() == "named-dev")
            .expect("named-dev resolved");
        assert_eq!(
            named_dev.provenance,
            Provenance::DevMode,
            "a plugin named in dev_mode.plugins must be DevMode"
        );
    }

    #[test]
    fn normal_path_plugin_is_not_dev_mode() {
        let f = fixture();

        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "plain", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        // A dev_mode block exists but does NOT cover this plugin.
        fs::write(
            f.config_dir.join("plugins.lua"),
            format!(
                "mote.plugins({{ [\"plain\"] = {{ source = \"{src}\" }} }})\n\
                 mote.dev_mode({{ directories = {{ \"/some/other/dir\" }} }})\n"
            ),
        )
        .unwrap();

        let resolved = f.mgr.resolved_set(None).unwrap();
        let plain = resolved
            .iter()
            .find(|r| r.name.as_str() == "plain")
            .expect("plain resolved");
        assert_eq!(
            plain.provenance,
            Provenance::Path,
            "a path plugin not covered by dev_mode must stay Path"
        );
    }

    // -----------------------------------------------------------------------
    // 6a: implicit-local detection — real dir in plugins/, not declared
    // -----------------------------------------------------------------------

    #[test]
    fn real_dir_in_plugins_is_detected_as_implicit_local() {
        let f = fixture();
        // No plugins.lua at all — but the user dropped a real plugin dir into
        // <config>/plugins/dropped-in/ with a valid init.lua.
        let dropped = f.config_dir.join("plugins").join("dropped-in");
        write_plugin(&dropped, "dropped-in", &[]);

        let resolved = f.mgr.resolved_set(None).unwrap();
        let dropped_in = resolved
            .iter()
            .find(|r| r.name.as_str() == "dropped-in")
            .expect("dropped-in detected");
        assert_eq!(
            dropped_in.provenance,
            Provenance::ImplicitLocal,
            "a real dir under plugins/ that isn't declared is ImplicitLocal"
        );
        assert_eq!(dropped_in.manifest.name.as_str(), "dropped-in");
    }

    #[test]
    fn cache_symlink_in_plugins_is_not_implicit_local() {
        let f = fixture();
        // A declared path: plugin produces a SYMLINK at plugins/<name>; the
        // implicit scan must skip symlinks (they are cache-managed, not dropped
        // in), so it does not double-count the declared plugin as implicit.
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "declared", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("declared", &src)]);

        let resolved = f.mgr.resolved_set(None).unwrap();
        let declared: Vec<&ResolvedPlugin> = resolved
            .iter()
            .filter(|r| r.name.as_str() == "declared")
            .collect();
        assert_eq!(
            declared.len(),
            1,
            "declared plugin must not be double-counted"
        );
        assert_eq!(
            declared[0].provenance,
            Provenance::Path,
            "a declared path plugin stays Path — its slot is a symlink, not implicit"
        );

        // Sanity: the slot really is a symlink (so the scan correctly skipped it).
        let slot = f.config_dir.join("plugins").join("declared");
        let meta = fs::symlink_metadata(&slot).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "declared path plugin slot must be a symlink"
        );
    }

    #[test]
    fn implicit_local_under_dev_mode_directory_is_dev_mode() {
        let f = fixture();
        // An implicit dir that ALSO falls under a dev_mode directory resolves as
        // DevMode (dev-mode precedence over implicit-local).
        let plugins_dir = f.config_dir.join("plugins");
        let dropped = plugins_dir.join("dev-dropped");
        write_plugin(&dropped, "dev-dropped", &[]);

        // dev_mode covers the whole plugins/ dir.
        fs::write(
            f.config_dir.join("plugins.lua"),
            format!(
                "mote.dev_mode({{ directories = {{ \"{}\" }} }})\n",
                plugins_dir.display()
            ),
        )
        .unwrap();

        let resolved = f.mgr.resolved_set(None).unwrap();
        let dev_dropped = resolved
            .iter()
            .find(|r| r.name.as_str() == "dev-dropped")
            .expect("dev-dropped detected");
        assert_eq!(
            dev_dropped.provenance,
            Provenance::DevMode,
            "an implicit dir under a dev_mode directory becomes DevMode"
        );
    }

    #[test]
    fn implicit_local_named_in_dev_mode_plugins_is_dev_mode() {
        let f = fixture();
        // An implicit dir whose NAME is listed in dev_mode.plugins resolves as
        // DevMode (name path, not directory path — mirror of the dir test).
        let dropped = f.config_dir.join("plugins").join("dev-named-drop");
        write_plugin(&dropped, "dev-named-drop", &[]);

        // dev_mode lists the plugin by name; no dev_mode.directories at all.
        fs::write(
            f.config_dir.join("plugins.lua"),
            "mote.dev_mode({ plugins = { \"dev-named-drop\" } })\n",
        )
        .unwrap();

        let resolved = f.mgr.resolved_set(None).unwrap();
        let dev_named = resolved
            .iter()
            .find(|r| r.name.as_str() == "dev-named-drop")
            .expect("dev-named-drop detected");
        assert_eq!(
            dev_named.provenance,
            Provenance::DevMode,
            "an implicit dir named in dev_mode.plugins becomes DevMode, not ImplicitLocal"
        );
    }

    // -----------------------------------------------------------------------
    // sync: path: plugin resolves, hashes, writes lock, links
    // -----------------------------------------------------------------------

    #[test]
    fn sync_path_plugin_writes_lock_and_link() {
        let f = fixture();

        // Write a plugin to a separate temp dir.
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "my-plugin", &["storage:persistent"]);

        // Declare it in plugins.lua.
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("my-plugin", &src)]);

        let report = f.mgr.sync().unwrap();

        assert!(
            report.failed.is_empty(),
            "no failures expected: {:?}",
            report.failed
        );
        assert_eq!(report.ok.len(), 1);

        // Lock must have an entry.
        let lock = f.mgr.load_lock().unwrap();
        assert!(
            lock.plugins.contains_key(&name("my-plugin")),
            "lock must have my-plugin"
        );
        let entry = &lock.plugins[&name("my-plugin")];
        assert!(entry.commit.is_none(), "path: source has no commit");

        // The plugins/ slot must exist.
        let link = f.config_dir.join("plugins").join("my-plugin");
        assert!(link.exists(), "plugins/my-plugin must exist after sync");
    }

    // -----------------------------------------------------------------------
    // sync: idempotent — re-sync leaves lock and link unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn sync_is_idempotent() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "my-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("my-plugin", &src)]);

        f.mgr.sync().unwrap();
        let lock1 = f.mgr.load_lock().unwrap().to_toml().unwrap();

        f.mgr.sync().unwrap();
        let lock2 = f.mgr.load_lock().unwrap().to_toml().unwrap();

        assert_eq!(lock1, lock2, "second sync must not change the lock");
    }

    // -----------------------------------------------------------------------
    // sync: managed.lua entries are also synced (compose works)
    // -----------------------------------------------------------------------

    #[test]
    fn sync_composes_plugins_lua_and_managed_lua() {
        let f = fixture();

        let p1 = tempfile::tempdir().unwrap();
        write_plugin(p1.path(), "plugin-a", &[]);
        let p2 = tempfile::tempdir().unwrap();
        write_plugin(p2.path(), "plugin-b", &[]);

        // plugin-a in plugins.lua; plugin-b in managed.lua.
        let src_a = format!("path:{}", p1.path().display());
        write_plugins_lua(&f.config_dir, &[("plugin-a", &src_a)]);

        let src_b = format!("path:{}", p2.path().display());
        let mut managed = ManagedFile::new();
        managed.upsert(name("plugin-b"), src_b, None);
        managed
            .write_atomic(&f.config_dir.join("managed.lua"))
            .unwrap();

        let report = f.mgr.sync().unwrap();

        assert!(
            report.failed.is_empty(),
            "both plugins must sync: {:?}",
            report.failed
        );
        assert_eq!(report.ok.len(), 2);

        let lock = f.mgr.load_lock().unwrap();
        assert!(lock.plugins.contains_key(&name("plugin-a")));
        assert!(lock.plugins.contains_key(&name("plugin-b")));
    }

    // -----------------------------------------------------------------------
    // add: writes managed.lua + lock + link
    // -----------------------------------------------------------------------

    #[test]
    fn add_writes_managed_and_lock_and_link() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "new-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        let (returned_name, _) = f.mgr.add(&src, None).unwrap();
        assert_eq!(returned_name, name("new-plugin"));

        // managed.lua must have the entry.
        let managed = ManagedFile::load(&f.config_dir.join("managed.lua")).unwrap();
        let entries: Vec<_> = managed.entries().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, name("new-plugin"));
        assert_eq!(entries[0].source, src);

        // lock must have the entry.
        let lock = f.mgr.load_lock().unwrap();
        assert!(lock.plugins.contains_key(&name("new-plugin")));

        // plugins/<name> must exist.
        let link = f.config_dir.join("plugins").join("new-plugin");
        assert!(link.exists(), "plugins/new-plugin must exist after add");

        // plugins.lua must NOT have been modified (ADR-0006).
        assert!(
            !f.config_dir.join("plugins.lua").exists(),
            "plugins.lua must not be created by add"
        );
    }

    // -----------------------------------------------------------------------
    // remove: managed entry drops from managed.lua + lock + symlink; cache retained
    // -----------------------------------------------------------------------

    #[test]
    fn remove_managed_drops_entry_and_symlink_keeps_cache() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "my-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // Cache dir doesn't apply to path: plugins but the source dir still exists.
        let outcome = f.mgr.remove(&name("my-plugin")).unwrap();
        assert_eq!(outcome, RemoveOutcome::Removed);

        // managed.lua must not have the entry.
        let managed = ManagedFile::load(&f.config_dir.join("managed.lua")).unwrap();
        assert_eq!(managed.entries().count(), 0);

        // lock must not have the entry.
        let lock = f.mgr.load_lock().unwrap();
        assert!(!lock.plugins.contains_key(&name("my-plugin")));

        // The symlink must be gone.
        let link = f.config_dir.join("plugins").join("my-plugin");
        assert!(!link.exists(), "symlink must be removed");

        // The original plugin dir (the "cache" in path: terms) is intact.
        assert!(
            plugin_dir.path().exists(),
            "plugin source dir must be retained"
        );
    }

    // -----------------------------------------------------------------------
    // remove: user plugins.lua-only entry returns UserConfigOnly
    // -----------------------------------------------------------------------

    #[test]
    fn remove_user_only_plugin_returns_user_config_only() {
        let f = fixture();

        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "user-plugin", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("user-plugin", &src)]);
        // Do NOT add to managed.

        let outcome = f.mgr.remove(&name("user-plugin")).unwrap();
        assert_eq!(outcome, RemoveOutcome::UserConfigOnly);

        // plugins.lua must be untouched.
        let content = fs::read_to_string(f.config_dir.join("plugins.lua")).unwrap();
        assert!(
            content.contains("user-plugin"),
            "plugins.lua must not be modified"
        );
    }

    // -----------------------------------------------------------------------
    // import: write=false returns snippet; snippet itself parses via eval_config
    // -----------------------------------------------------------------------

    #[test]
    fn import_write_false_returns_parseable_snippet() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "snap-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        let outcome = f.mgr.import(&name("snap-plugin"), false).unwrap();
        match outcome {
            ImportOutcome::Snippet(snippet) => {
                // The snippet itself must parse without error.
                mote_lua::eval_config(&snippet, "snippet").unwrap();
            }
            other => panic!("expected Snippet, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // import: write=true appends to plugins.lua, original lines unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn import_write_true_appends_to_plugins_lua() {
        let f = fixture();

        // Write an initial plugins.lua.
        let p1 = tempfile::tempdir().unwrap();
        write_plugin(p1.path(), "existing", &[]);
        let src_existing = format!("path:{}", p1.path().display());
        write_plugins_lua(&f.config_dir, &[("existing", &src_existing)]);
        let original = fs::read_to_string(f.config_dir.join("plugins.lua")).unwrap();

        // Add snap-plugin via managed.
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "snap-plugin", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        let outcome = f.mgr.import(&name("snap-plugin"), true).unwrap();
        assert_eq!(outcome, ImportOutcome::Written);

        let new_content = fs::read_to_string(f.config_dir.join("plugins.lua")).unwrap();

        // Original lines must be a byte-identical prefix.
        assert!(
            new_content.starts_with(&original),
            "original content must be unchanged:\noriginal:\n{original}\nnew:\n{new_content}"
        );

        // The snippet must be appended.
        assert!(
            new_content.contains("snap-plugin"),
            "appended content must contain snap-plugin"
        );

        // Entry must be dropped from managed.lua.
        let managed = ManagedFile::load(&f.config_dir.join("managed.lua")).unwrap();
        assert!(
            !managed.entries().any(|e| e.name == name("snap-plugin")),
            "snap-plugin must be removed from managed.lua"
        );
    }

    // -----------------------------------------------------------------------
    // import: write=true on a non-parsing plugins.lua falls back to snippet
    // -----------------------------------------------------------------------

    #[test]
    fn import_write_true_fallback_on_invalid_plugins_lua() {
        let f = fixture();

        // Write a broken plugins.lua.
        fs::write(
            f.config_dir.join("plugins.lua"),
            b"this is not valid lua @@@@",
        )
        .unwrap();

        // Add snap-plugin via managed.
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "snap-plugin", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        let outcome = f.mgr.import(&name("snap-plugin"), true).unwrap();
        match outcome {
            ImportOutcome::PluginsLuaDoesNotParse(snippet) => {
                // The snippet must still be parseable.
                mote_lua::eval_config(&snippet, "snippet").unwrap();
                // The broken file must be unchanged.
                let content = fs::read_to_string(f.config_dir.join("plugins.lua")).unwrap();
                assert_eq!(
                    content, "this is not valid lua @@@@",
                    "broken plugins.lua must not be modified"
                );
            }
            other => panic!("expected PluginsLuaDoesNotParse, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // integrity: pin recomputes hash and resolves mismatch
    // -----------------------------------------------------------------------

    #[test]
    fn pin_resolves_mismatch_and_records_approval() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "local-plugin", &["storage:persistent"]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // Simulate a content change (the user edited their plugin).
        fs::write(
            plugin_dir.path().join("init.lua"),
            b"local M = {}\nM.manifest = { schema='v1', name='local-plugin', version='2', permissions={}, identity_scope='global' }\nreturn M\n",
        )
        .unwrap();

        // Before pin: sync would show mismatch for the path: plugin (informational).
        // After pin: hash is updated and approval recorded.
        f.mgr.pin(&name("local-plugin")).unwrap();

        // The lock's checksum must match the new hash.
        let new_hash = hash_dir(plugin_dir.path()).unwrap();
        let lock = f.mgr.load_lock().unwrap();
        let entry = &lock.plugins[&name("local-plugin")];
        assert_eq!(
            entry.checksum, new_hash,
            "lock checksum must be updated after pin"
        );

        // The approval store must have an entry (recorded by pin into f.store).
        let approval = ApprovalStore::new(&f.store);
        assert!(
            approval.get(&name("local-plugin")).unwrap().is_some(),
            "pin must record approval"
        );
    }

    // -----------------------------------------------------------------------
    // gc: retains active + previous commit; reclaims older
    // -----------------------------------------------------------------------

    #[test]
    fn gc_reclaims_old_commits_keeps_active_and_previous() {
        let f = fixture();
        let pname = name("gc-plugin");

        // Simulate three cached commits for the plugin.
        let commit_dirs: Vec<(String, tempfile::TempDir)> = ["c1", "c2", "c3"]
            .iter()
            .map(|c| {
                let td = tempfile::tempdir().unwrap();
                write_plugin(td.path(), "gc-plugin", &[]);
                (c.to_string(), td)
            })
            .collect();

        for (commit, tree) in &commit_dirs {
            f.mgr.cache.store(&pname, commit, tree.path()).unwrap();
        }

        // Active commit is c3; link to it.
        f.mgr.cache.link(&pname, "c3").unwrap();

        // Write a lock with c3 as active.
        // Use the cached copy's path (the original temp dirs were moved into the cache).
        let cached_c3 = f.mgr.cache.commit_dir(&pname, "c3");
        let checksum = hash_dir(&cached_c3).unwrap();
        let mut lock = f.mgr.load_lock().unwrap();
        lock.plugins.insert(
            pname.clone(),
            LockEntry {
                source: Source::Git {
                    url: "https://example.com/gc-plugin.git".into(),
                },
                commit: Some("c3".into()),
                checksum,
            },
        );
        f.mgr.write_lock(&lock).unwrap();

        // gc should reclaim c1 (the third oldest), keep c2 (previous) + c3 (active).
        let gc_report = f.mgr.gc().unwrap();
        assert_eq!(gc_report.reclaimed.len(), 1, "only c1 should be reclaimed");
        assert_eq!(gc_report.reclaimed[0].1, "c1");

        // c2 and c3 must still exist.
        assert!(
            f.mgr.cache.commit_dir(&pname, "c2").is_dir(),
            "c2 must be retained (previous)"
        );
        assert!(
            f.mgr.cache.commit_dir(&pname, "c3").is_dir(),
            "c3 must be retained (active)"
        );
        assert!(
            !f.mgr.cache.commit_dir(&pname, "c1").is_dir(),
            "c1 must be reclaimed"
        );
    }

    // -----------------------------------------------------------------------
    // update: expansion triggers NeedsReapproval; contraction applies
    // -----------------------------------------------------------------------

    #[test]
    fn update_expansion_returns_needs_reapproval() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();

        // Initial plugin: no permissions.
        write_plugin(plugin_dir.path(), "my-plugin", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // Approve current state.
        f.mgr.approve(&name("my-plugin")).unwrap();

        // Expand: add a permission.
        write_plugin(plugin_dir.path(), "my-plugin", &["storage:persistent"]);

        let outcome = f.mgr.update(&name("my-plugin")).unwrap();
        match outcome {
            UpdateOutcome::NeedsReapproval { report } => {
                assert!(
                    report.is_expansion(),
                    "added permission must be an expansion"
                );
            }
            UpdateOutcome::Applied { .. } => {
                panic!("expected NeedsReapproval for permission expansion")
            }
        }
    }

    #[test]
    fn update_code_only_applies_directly() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();

        // Plugin with permissions.
        write_plugin(plugin_dir.path(), "my-plugin", &["storage:persistent"]);
        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // Approve current state.
        f.mgr.approve(&name("my-plugin")).unwrap();

        // "Update" to the same permissions (only version string changed in manifest
        // but that does not affect ApprovalHash). Re-write with same perms.
        write_plugin(plugin_dir.path(), "my-plugin", &["storage:persistent"]);

        let outcome = f.mgr.update(&name("my-plugin")).unwrap();
        assert!(
            matches!(outcome, UpdateOutcome::Applied { .. }),
            "non-expanding update must be Applied"
        );
    }

    // -----------------------------------------------------------------------
    // offline sync: everything cached → sync succeeds without fetch
    // -----------------------------------------------------------------------

    #[test]
    fn offline_sync_with_path_source_needs_no_network() {
        // path: sources never fetch; verifying that sync completes without any
        // network operation is inherent — there is no fetch code path for path:.
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "offline-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("offline-plugin", &src)]);

        let report = f.mgr.sync().unwrap();
        assert!(
            report.failed.is_empty(),
            "path: sync must succeed offline: {:?}",
            report.failed
        );
        assert_eq!(report.ok.len(), 1);
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------
    // import --write regression: appending to a plugins.lua that already
    // declares a different plugin produces the UNION via eval_config
    // -----------------------------------------------------------------------

    /// Regression guard for the `import --write` accumulation bug:
    /// after `import(name, write=true)` appends a second `mote.plugins({…})`
    /// call to a `plugins.lua` that already contains a different plugin, the
    /// file must parse to the **union** of both declarations — neither the
    /// pre-existing plugin nor the imported one is silently dropped.
    #[test]
    fn import_write_true_union_via_eval_config() {
        let f = fixture();

        // Write a plugins.lua that already declares "existing-plugin".
        let p_existing = tempfile::tempdir().unwrap();
        write_plugin(p_existing.path(), "existing-plugin", &[]);
        let src_existing = format!("path:{}", p_existing.path().display());
        write_plugins_lua(&f.config_dir, &[("existing-plugin", &src_existing)]);

        // Add "imported-plugin" via managed.lua.
        let p_import = tempfile::tempdir().unwrap();
        write_plugin(p_import.path(), "imported-plugin", &[]);
        let src_import = format!("path:{}", p_import.path().display());
        f.mgr.add(&src_import, None).unwrap();

        // import --write appends the snippet for "imported-plugin" to plugins.lua.
        let outcome = f.mgr.import(&name("imported-plugin"), true).unwrap();
        assert_eq!(outcome, ImportOutcome::Written);

        // Re-read the updated plugins.lua through eval_config and assert UNION.
        let content = fs::read_to_string(f.config_dir.join("plugins.lua")).unwrap();
        let spec = mote_lua::eval_config(&content, "plugins.lua")
            .expect("plugins.lua must parse after import --write");

        let keys: Vec<&str> = spec.plugins.iter().map(|p| p.key.as_str()).collect();
        assert!(
            keys.contains(&"existing-plugin"),
            "existing-plugin must survive import --write; got keys: {keys:?}"
        );
        assert!(
            keys.contains(&"imported-plugin"),
            "imported-plugin must be present after import --write; got keys: {keys:?}"
        );
        assert_eq!(
            spec.plugins.len(),
            2,
            "both plugins must be in the union; got: {keys:?}"
        );
    }
}
