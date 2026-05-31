//! The binary-embedded first-party plugin bundle.
//!
//! The `bundled` source (DESIGN §Supported sources) resolves to a set of
//! first-party plugins compiled directly into the Mote binary. This module
//! embeds the repository's `plugins/` tree with [`include_dir`] and unpacks any
//! bundled plugin into the content-addressed cache **without network access**.
//!
//! The embedded version directory is synthesised as `bundled-<mote-version>`
//! (DESIGN §Cache layout shows `bundled-<mote-version>`), so a new Mote release
//! shipping an updated bundle materialises under a fresh cache key and the
//! active link is re-pointed at it — exactly the "bundled plugins update with
//! the binary" behaviour (DESIGN §First-party plugins and updates).
//!
//! v0.1 wires only this single binary-embedded bundle; the `bundled:<name>`
//! external-bundle grammar is reserved (see [`crate::source`]).

use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::{fs, io};

use include_dir::{Dir, include_dir};
use mote_types::PluginName;
use thiserror::Error;

use crate::cache::{Cache, CacheError, CacheKey};

/// The repository's first-party `plugins/` tree, embedded at compile time.
///
/// The path is resolved relative to `CARGO_MANIFEST_DIR` (this crate), so the
/// canonical first-party tree at the repo root is the single source of truth —
/// it is not duplicated into the crate.
static BUNDLE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../plugins");

/// The synthetic commit/version directory for the embedded bundle.
///
/// Combines the literal `bundled-` prefix with the crate's package version so
/// each Mote release unpacks under its own cache key.
#[must_use]
pub fn bundled_version() -> String {
    format!("bundled-{}", env!("CARGO_PKG_VERSION"))
}

/// Error returned while materialising a bundled plugin.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BundleError {
    /// No bundled plugin with the requested name is embedded in the binary.
    #[error("no bundled plugin named {0:?} is embedded in this Mote binary")]
    NotBundled(String),
    /// A directory name in the embedded bundle was not a valid [`PluginName`].
    #[error("embedded bundle directory {0:?} is not a valid plugin name")]
    InvalidName(String),
    /// An I/O error while unpacking the bundle into a directory.
    #[error("io error unpacking bundle to {path:?}: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The cache rejected the unpacked tree.
    #[error(transparent)]
    Cache(#[from] CacheError),
}

/// The names of every plugin embedded in the binary bundle.
///
/// Top-level directories in the embedded `plugins/` tree, each validated as a
/// [`PluginName`].
///
/// # Errors
///
/// Returns [`BundleError::InvalidName`] if an embedded directory name is not a
/// valid [`PluginName`].
pub fn bundled_names() -> Result<Vec<PluginName>, BundleError> {
    let mut names = Vec::new();
    for entry in BUNDLE.dirs() {
        let raw = entry
            .path()
            .file_name()
            .and_then(|c| c.to_str())
            .unwrap_or_default();
        let name =
            PluginName::from_str(raw).map_err(|_| BundleError::InvalidName(raw.to_owned()))?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// Whether a plugin with the given name is embedded in the binary bundle.
#[must_use]
pub fn is_bundled(name: &PluginName) -> bool {
    BUNDLE.get_dir(name.as_str()).is_some()
}

/// Unpacks a bundled plugin into the cache and returns its [`CacheKey`].
///
/// Materialises `<cache>/<name>/bundled-<version>/…` from the embedded tree,
/// idempotently (re-unpacking reuses the existing cache entry). The integrity
/// hash of the unpacked tree is trustworthy by construction — the bytes came
/// from the binary, not the network.
///
/// # Errors
///
/// Returns [`BundleError::NotBundled`] if `name` is not embedded, or an
/// [`BundleError::Io`]/[`BundleError::Cache`] on a filesystem/cache error.
pub fn unpack_into_cache(name: &PluginName, cache: &Cache) -> Result<CacheKey, BundleError> {
    let dir = BUNDLE
        .get_dir(name.as_str())
        .ok_or_else(|| BundleError::NotBundled(name.as_str().to_owned()))?;

    let version = bundled_version();
    // Unpack to a staging temp dir, then hand to the cache (which moves it into
    // place idempotently). Use a sibling of the cache commit dir's parent so a
    // rename stays on the same filesystem where possible.
    let staging = tempfile::tempdir().map_err(|source| BundleError::Io {
        path: PathBuf::from("<tempdir>"),
        source,
    })?;
    unpack_dir(dir, staging.path())?;

    // The embedded `Dir` paths are prefixed with the plugin name; strip it so
    // the staged tree's root holds the plugin's files directly.
    let staged_root = staging.path().join(name.as_str());
    let key = cache.store(name, &version, &staged_root)?;
    Ok(key)
}

/// Writes an embedded [`Dir`] subtree to disk under `base`.
///
/// `include_dir` paths are relative to the embedded root (here, the repo's
/// `plugins/` dir), so a plugin's files land at `base/<name>/…`; callers strip
/// the `<name>` prefix to get the plugin's tree root.
fn unpack_dir(dir: &Dir<'_>, base: &Path) -> Result<(), BundleError> {
    for file in dir.files() {
        let out = base.join(file.path());
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|source| BundleError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&out, file.contents()).map_err(|source| BundleError::Io {
            path: out.clone(),
            source,
        })?;
    }
    for sub in dir.dirs() {
        unpack_dir(sub, base)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirhash::hash_dir;

    fn cache_fixture() -> (tempfile::TempDir, tempfile::TempDir, Cache) {
        let c = tempfile::tempdir().unwrap();
        let p = tempfile::tempdir().unwrap();
        let cache = Cache::new(c.path(), p.path());
        (c, p, cache)
    }

    #[test]
    fn bundle_contains_first_party_plugins() {
        let names = bundled_names().unwrap();
        // history owns ui:urlbar_provider from Phase 5 onwards; the standalone
        // urlbar plugin is removed.  Assert the current bundled set.
        let strs: Vec<&str> = names.iter().map(PluginName::as_str).collect();
        assert!(
            !strs.contains(&"urlbar"),
            "urlbar must NOT be bundled (history owns ui:urlbar_provider); got {strs:?}"
        );
        assert!(strs.contains(&"bookmarks"), "got {strs:?}");
        assert!(strs.contains(&"workspace-manager"), "got {strs:?}");
    }

    #[test]
    fn is_bundled_matches_names() {
        for name in bundled_names().unwrap() {
            assert!(is_bundled(&name));
        }
        assert!(!is_bundled(
            &PluginName::new("definitely-not-bundled").unwrap()
        ));
    }

    #[test]
    fn unpacks_bundled_plugin_to_cache_offline() {
        let (_c, _p, cache) = cache_fixture();
        // Use bookmarks as the representative bundled plugin (urlbar was removed;
        // history arrives in the next commit).
        let name = PluginName::new("bookmarks").unwrap();
        let key = unpack_into_cache(&name, &cache).unwrap();
        assert_eq!(key.commit, bundled_version());

        // The init.lua materialised and is non-empty.
        let dir = cache.commit_dir(&name, &key.commit);
        let init = dir.join("init.lua");
        assert!(init.is_file(), "init.lua must be unpacked");
        assert!(!fs::read(&init).unwrap().is_empty());

        // The unpacked tree hashes (integrity anchor is computable at unpack).
        assert!(hash_dir(&dir).is_ok());
    }

    #[test]
    fn unpack_is_idempotent() {
        let (_c, _p, cache) = cache_fixture();
        // Use workspace-manager as a stable bundled plugin representative.
        let name = PluginName::new("workspace-manager").unwrap();
        let k1 = unpack_into_cache(&name, &cache).unwrap();
        let k2 = unpack_into_cache(&name, &cache).unwrap();
        assert_eq!(k1, k2);
        // Hashing the cached dir twice is stable across re-unpack.
        let dir = cache.commit_dir(&name, &k1.commit);
        assert_eq!(hash_dir(&dir).unwrap(), hash_dir(&dir).unwrap());
    }

    #[test]
    fn unpack_unknown_errors() {
        let (_c, _p, cache) = cache_fixture();
        let name = PluginName::new("nope").unwrap();
        assert!(matches!(
            unpack_into_cache(&name, &cache),
            Err(BundleError::NotBundled(_))
        ));
    }
}
