//! The content-addressed plugin cache and the plugins-directory link scheme.
//!
//! Two roots (DESIGN §Cache layout):
//!
//! ```text
//! ~/.cache/mote/plugins/
//!   cool-plugin/abc123def456/   init.lua …   # one dir per fetched commit
//!   cool-plugin/def456abc789/   init.lua …   # previous version → instant rollback
//! ~/.config/mote/plugins/
//!   cool-plugin/   -> ~/.cache/mote/plugins/cool-plugin/def456abc789/   (symlink, git source)
//!   my-local-plugin/   (real dir, path: source)
//!   pasted-plugin/     (real dir, implicit local)
//! ```
//!
//! - Git-backed sources are **stored** under `<cache>/<name>/<commit>/` and the
//!   active version is selected by a **symlink** at
//!   `<config>/plugins/<name>` pointing into the cache. Rollback is a relink —
//!   no file copies.
//! - `path:` and implicit-local plugins are **real directories** living
//!   directly at `<config>/plugins/<name>`; the cache never touches them and
//!   their integrity hash is computed in place.
//!
//! [`Cache::store`] is idempotent: re-storing an already-present commit reuses
//! it, which makes re-`sync` cheap and lets identities share the cache.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mote_types::PluginName;
use thiserror::Error;

/// Identifies a stored commit in the cache: `<name>/<commit>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// The plugin's canonical name.
    pub name: PluginName,
    /// The commit (or synthetic version, e.g. `bundled-<version>`) directory.
    pub commit: String,
}

/// The content-addressed cache plus the active-version link scheme.
///
/// Owns two roots: the cache root (`~/.cache/mote/plugins`) and the plugins
/// directory (`~/.config/mote/plugins`). Construct with [`Cache::new`]; tests
/// point both at temp dirs.
#[derive(Debug, Clone)]
pub struct Cache {
    cache_root: PathBuf,
    plugins_dir: PathBuf,
}

/// Error returned by cache operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    /// An I/O error during a cache operation.
    #[error("io error on {path:?}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A `link`/`resolve_active` operation referenced a commit not in the cache.
    #[error("commit {commit:?} for plugin {name} is not in the cache")]
    NotCached {
        /// The plugin name.
        name: PluginName,
        /// The commit that was missing.
        commit: String,
    },
}

impl Cache {
    /// Creates a cache rooted at the given cache + plugins directories.
    ///
    /// Neither directory needs to exist yet; they are created lazily on first
    /// write. In production these are `~/.cache/mote/plugins` and
    /// `~/.config/mote/plugins`; in tests they are temp dirs.
    #[must_use]
    pub fn new(cache_root: impl Into<PathBuf>, plugins_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: cache_root.into(),
            plugins_dir: plugins_dir.into(),
        }
    }

    /// The cache directory for a `<name>/<commit>` pair (may not exist).
    #[must_use]
    pub fn commit_dir(&self, name: &PluginName, commit: &str) -> PathBuf {
        self.cache_root.join(name.as_str()).join(commit)
    }

    /// The active-version link path `<plugins_dir>/<name>` (symlink or real dir).
    #[must_use]
    pub fn active_link(&self, name: &PluginName) -> PathBuf {
        self.plugins_dir.join(name.as_str())
    }

    /// Stores a fetched tree at `<cache>/<name>/<commit>/`, moving from `tree`.
    ///
    /// Idempotent: if the commit directory already exists, the existing copy is
    /// kept and `tree` is removed (so a re-fetch is cheap and identities share
    /// the cache). Returns the [`CacheKey`] for the stored commit.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] if directories cannot be created or the tree
    /// cannot be moved/copied into place.
    pub fn store(
        &self,
        name: &PluginName,
        commit: &str,
        tree: &Path,
    ) -> Result<CacheKey, CacheError> {
        let dest = self.commit_dir(name, commit);
        if dest.exists() {
            // Already cached — drop the incoming tree, reuse the stored one.
            remove_dir_all_if_exists(tree)?;
            return Ok(CacheKey {
                name: name.clone(),
                commit: commit.to_owned(),
            });
        }
        if let Some(parent) = dest.parent() {
            create_dir_all(parent)?;
        }
        // Prefer an atomic rename within the same filesystem; fall back to a
        // recursive copy across filesystems (temp dir on a different mount).
        if fs::rename(tree, &dest).is_err() {
            copy_tree(tree, &dest)?;
            remove_dir_all_if_exists(tree)?;
        }
        Ok(CacheKey {
            name: name.clone(),
            commit: commit.to_owned(),
        })
    }

    /// Points `<plugins_dir>/<name>` at the cached `<name>/<commit>` directory.
    ///
    /// Used for Git-backed and bundled sources (which live in the cache). Any
    /// existing link or real directory at the target is removed first, so this
    /// doubles as the rollback primitive (relink to a previous commit).
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::NotCached`] if the commit is not stored, or
    /// [`CacheError::Io`] on a filesystem error.
    pub fn link(&self, name: &PluginName, commit: &str) -> Result<(), CacheError> {
        let target = self.commit_dir(name, commit);
        if !target.is_dir() {
            return Err(CacheError::NotCached {
                name: name.clone(),
                commit: commit.to_owned(),
            });
        }
        let link = self.active_link(name);
        create_dir_all(&self.plugins_dir)?;
        remove_link_or_dir(&link)?;
        symlink_dir(&target, &link)?;
        Ok(())
    }

    /// Resolves the commit the active link currently points at, if it is a
    /// cache symlink for `name`.
    ///
    /// Returns `None` if the link is absent or is a real directory (a `path:`
    /// or implicit-local plugin, which carries no cache commit).
    #[must_use]
    pub fn resolve_active(&self, name: &PluginName) -> Option<String> {
        let link = self.active_link(name);
        let meta = fs::symlink_metadata(&link).ok()?;
        if !meta.file_type().is_symlink() {
            return None;
        }
        let target = fs::read_link(&link).ok()?;
        // The commit is the final path component of the cache target.
        target
            .file_name()
            .and_then(|c| c.to_str())
            .map(ToOwned::to_owned)
    }

    /// Lists the commits cached for a plugin (directory names under
    /// `<cache>/<name>/`), in lexicographic order. Empty if none.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] on a filesystem error other than a missing
    /// plugin directory (which yields an empty list).
    pub fn list_commits(&self, name: &PluginName) -> Result<Vec<String>, CacheError> {
        let dir = self.cache_root.join(name.as_str());
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut commits = Vec::new();
        for item in read_dir(&dir)? {
            let item = item.map_err(|source| CacheError::Io {
                path: dir.clone(),
                source,
            })?;
            if item.path().is_dir()
                && let Some(name) = item.file_name().to_str()
            {
                commits.push(name.to_owned());
            }
        }
        commits.sort();
        Ok(commits)
    }

    /// Removes a cached commit directory (garbage collection).
    ///
    /// Does not touch the active link; the caller is responsible for not
    /// reaping the live commit. Removing an already-absent commit is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] on a filesystem error.
    pub fn gc_commit(&self, name: &PluginName, commit: &str) -> Result<(), CacheError> {
        remove_dir_all_if_exists(&self.commit_dir(name, commit))
    }
}

// ---- filesystem helpers (each maps io::Error to a path-tagged CacheError) ---

fn create_dir_all(path: &Path) -> Result<(), CacheError> {
    fs::create_dir_all(path).map_err(|source| CacheError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_dir(path: &Path) -> Result<fs::ReadDir, CacheError> {
    fs::read_dir(path).map_err(|source| CacheError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_dir_all_if_exists(path: &Path) -> Result<(), CacheError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CacheError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Removes whatever is at `path`: a symlink, a real directory, or nothing.
fn remove_link_or_dir(path: &Path) -> Result<(), CacheError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            // A symlink (even to a dir) is removed with remove_file on Unix;
            // a real directory needs remove_dir_all.
            if meta.file_type().is_symlink() || meta.is_file() {
                fs::remove_file(path).map_err(|source| CacheError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            } else {
                remove_dir_all_if_exists(path)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CacheError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Creates a directory symlink at `link` pointing to `target`.
///
/// Platform detail isolated here (DESIGN: abstract link-vs-junction-vs-copy):
/// Unix uses `symlink`; Windows uses `symlink_dir` (requires privilege — the
/// non-v0.1 path, kept compiling for a clean seam).
fn symlink_dir(target: &Path, link: &Path) -> Result<(), CacheError> {
    let io = |source| CacheError::Io {
        path: link.to_path_buf(),
        source,
    };
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(io)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).map_err(io)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = target;
        Err(io(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks unsupported on this platform",
        )))
    }
}

/// Recursively copies `src` into `dst` (cross-filesystem fallback for `store`).
fn copy_tree(src: &Path, dst: &Path) -> Result<(), CacheError> {
    create_dir_all(dst)?;
    for item in read_dir(src)? {
        let item = item.map_err(|source| CacheError::Io {
            path: src.to_path_buf(),
            source,
        })?;
        let from = item.path();
        let to = dst.join(item.file_name());
        let meta = fs::symlink_metadata(&from).map_err(|source| CacheError::Io {
            path: from.clone(),
            source,
        })?;
        let ft = meta.file_type();
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_symlink() {
            let target = fs::read_link(&from).map_err(|source| CacheError::Io {
                path: from.clone(),
                source,
            })?;
            symlink_dir(&target, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|source| CacheError::Io {
                path: from.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _cache: tempfile::TempDir,
        _plugins: tempfile::TempDir,
        cache: Cache,
    }

    fn fixture() -> Fixture {
        let cache_dir = tempfile::tempdir().unwrap();
        let plugins_dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(cache_dir.path(), plugins_dir.path());
        Fixture {
            _cache: cache_dir,
            _plugins: plugins_dir,
            cache,
        }
    }

    fn name(s: &str) -> PluginName {
        PluginName::new(s).unwrap()
    }

    /// Builds a throwaway "fetched tree" temp dir with one file.
    fn tree(contents: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("init.lua"), contents).unwrap();
        dir
    }

    #[test]
    fn store_then_link_then_resolve() {
        let f = fixture();
        let n = name("cool-plugin");
        let t = tree(b"-- v1\n");
        let key = f.cache.store(&n, "abc123", t.path()).unwrap();
        assert_eq!(key.commit, "abc123");

        f.cache.link(&n, "abc123").unwrap();
        assert_eq!(f.cache.resolve_active(&n).as_deref(), Some("abc123"));

        // The link resolves to the cached file.
        let link = f.cache.active_link(&n);
        assert_eq!(
            fs::read_to_string(link.join("init.lua")).unwrap(),
            "-- v1\n"
        );
    }

    #[test]
    fn store_is_idempotent() {
        let f = fixture();
        let n = name("p");
        f.cache.store(&n, "c1", tree(b"x").path()).unwrap();
        // Second store of the same commit with different incoming bytes keeps
        // the original cached copy.
        let t2 = tree(b"DIFFERENT");
        f.cache.store(&n, "c1", t2.path()).unwrap();
        let cached = f.cache.commit_dir(&n, "c1").join("init.lua");
        assert_eq!(fs::read_to_string(cached).unwrap(), "x");
    }

    #[test]
    fn rollback_relinks_to_previous_commit() {
        let f = fixture();
        let n = name("p");
        f.cache.store(&n, "old", tree(b"-- old\n").path()).unwrap();
        f.cache.store(&n, "new", tree(b"-- new\n").path()).unwrap();

        f.cache.link(&n, "new").unwrap();
        assert_eq!(f.cache.resolve_active(&n).as_deref(), Some("new"));

        // Rollback == relink to the previous commit. No copies.
        f.cache.link(&n, "old").unwrap();
        assert_eq!(f.cache.resolve_active(&n).as_deref(), Some("old"));
        let link = f.cache.active_link(&n);
        assert_eq!(
            fs::read_to_string(link.join("init.lua")).unwrap(),
            "-- old\n"
        );
    }

    #[test]
    fn link_to_uncached_commit_errors() {
        let f = fixture();
        let n = name("p");
        assert!(matches!(
            f.cache.link(&n, "nope"),
            Err(CacheError::NotCached { .. })
        ));
    }

    #[test]
    fn list_and_gc_commits() {
        let f = fixture();
        let n = name("p");
        f.cache.store(&n, "c2", tree(b"2").path()).unwrap();
        f.cache.store(&n, "c1", tree(b"1").path()).unwrap();
        assert_eq!(f.cache.list_commits(&n).unwrap(), vec!["c1", "c2"]);

        f.cache.gc_commit(&n, "c1").unwrap();
        assert_eq!(f.cache.list_commits(&n).unwrap(), vec!["c2"]);
        // GC of an absent commit is a no-op.
        f.cache.gc_commit(&n, "c1").unwrap();
    }

    #[test]
    fn resolve_active_on_real_dir_is_none() {
        let f = fixture();
        let n = name("local");
        // Simulate a path:/implicit-local real directory at the active link.
        let link = f.cache.active_link(&n);
        fs::create_dir_all(&link).unwrap();
        fs::write(link.join("init.lua"), b"x").unwrap();
        assert_eq!(f.cache.resolve_active(&n), None);
    }

    #[test]
    fn list_commits_absent_plugin_is_empty() {
        let f = fixture();
        assert!(f.cache.list_commits(&name("never")).unwrap().is_empty());
    }
}
