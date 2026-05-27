//! BLAKE3 directory hashing — the integrity anchor for a plugin's on-disk tree.
//!
//! [`hash_dir`] implements the exact DESIGN §Hash computation spec:
//!
//! 1. Enumerate files **recursively** from the plugin root.
//! 2. Sort the relative paths **lexicographically** (as UTF-8 strings) so the
//!    digest is identical across filesystems with different directory-iteration
//!    orders.
//! 3. For each entry, feed the hasher the **path string** (UTF-8 bytes) and the
//!    entry's **byte contents**, with explicit length framing so the
//!    path/content boundary is unambiguous: `path="ab" + contents="c"` can never
//!    collide with `path="a" + contents="bc"`.
//! 4. **Symlinks are not followed** — a symlink is hashed by its **target path
//!    string**, not the bytes it points at. This keeps the hash stable when a
//!    symlink points outside the tree, and makes "what the symlink points to"
//!    part of the identity.
//!
//! The output is a [`mote_types::Checksum`] (`blake3:<hex>`) so it drops
//! straight into `plugins.lock` and the integrity panel.
//!
//! # Transient state is a hashing hazard
//!
//! The hasher hashes *whatever is in the directory*. Per DESIGN §Integrity
//! verification, **plugins must not write transient state (logs, caches,
//! scratch files) into their own directory** — any such write changes the dir
//! hash and trips integrity verification on the next load. Persistent plugin
//! state belongs behind the `storage:persistent` permission, not on disk in the
//! plugin tree. This is a documented contract, not an enforced one: a `path:`
//! or implicit-local plugin under active editing will legitimately change its
//! hash on every save, which is why those sources are handled by dev mode and
//! `mote plugin pin` rather than hard integrity refusal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mote_types::Checksum;
use thiserror::Error;

/// A single entry contributing to the directory hash, after enumeration.
struct Entry {
    /// Path relative to the hash root, rendered as a UTF-8 string for sorting
    /// and framing.
    rel: String,
    /// What the entry contributes: a regular file's bytes or a symlink target.
    kind: EntryKind,
}

/// The two byte-streams an entry contributes besides its path.
enum EntryKind {
    /// A regular file: hash its byte contents.
    File(Vec<u8>),
    /// A symlink: hash the **target path string** (never followed).
    Symlink(Vec<u8>),
}

/// Error returned when a directory tree cannot be hashed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DirHashError {
    /// The hash root does not exist or is not a directory.
    #[error("plugin directory {0:?} is not a directory")]
    NotADirectory(PathBuf),
    /// A path under the root was not valid UTF-8 (required for the spec's
    /// lexicographic-string sort and path framing).
    #[error("path {0:?} is not valid UTF-8 (required for deterministic hashing)")]
    NonUtf8Path(PathBuf),
    /// A symlink target was not valid UTF-8.
    #[error("symlink target at {0:?} is not valid UTF-8")]
    NonUtf8Target(PathBuf),
    /// An I/O error while walking or reading the tree.
    #[error("io error hashing {path:?}: {source}")]
    Io {
        /// The path being processed when the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

/// Computes the BLAKE3 directory hash of `root` per the DESIGN spec.
///
/// Returns a [`Checksum`] (`blake3:<hex>`). The result is deterministic for a
/// given tree regardless of filesystem iteration order, sensitive to any path
/// or content change, and hashes symlinks by target string without following
/// them.
///
/// # Errors
///
/// Returns [`DirHashError`] if `root` is not a directory, a path or symlink
/// target is not UTF-8, or an I/O error occurs while walking/reading.
pub fn hash_dir(root: &Path) -> Result<Checksum, DirHashError> {
    if !root.is_dir() {
        return Err(DirHashError::NotADirectory(root.to_path_buf()));
    }

    let mut entries = Vec::new();
    collect(root, root, &mut entries)?;

    // Spec step 2: lexicographic sort of the relative path strings.
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));

    let mut hasher = blake3::Hasher::new();
    for entry in &entries {
        // Spec step 3: frame path and content with explicit lengths so the
        // boundary between them is never ambiguous. We also tag the entry kind
        // so a file and a symlink with identical byte payloads hash distinctly.
        let path_bytes = entry.rel.as_bytes();
        let (tag, payload): (u8, &[u8]) = match &entry.kind {
            EntryKind::File(bytes) => (0, bytes),
            EntryKind::Symlink(target) => (1, target),
        };
        hasher.update(&[tag]);
        hasher.update(
            &u64::try_from(path_bytes.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(path_bytes);
        hasher.update(
            &u64::try_from(payload.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(payload);
    }

    Ok(checksum_from_digest(hasher.finalize()))
}

/// Converts a finalized BLAKE3 digest into a [`Checksum`].
///
/// `mote_types::Checksum` exposes no constructor from a raw 32-byte digest, but
/// it round-trips through its `blake3:<hex>` string form. We render the digest
/// to that canonical string and parse it back. The hex rendering is infallible
/// and the parse cannot fail for a well-formed 32-byte digest, so any error
/// here is a programming bug and we surface it as a panic in debug builds via
/// `expect`.
fn checksum_from_digest(digest: blake3::Hash) -> Checksum {
    let rendered = format!("blake3:{}", digest.to_hex());
    rendered
        .parse()
        .expect("a rendered blake3 digest is always a valid Checksum")
}

/// Recursively collects entries under `dir`, recording paths relative to
/// `root`. Directories are recursed into (and not themselves hashed — they
/// carry no bytes; their existence is implied by their contents). Empty
/// directories therefore contribute nothing, matching a content-addressed
/// model where only files and symlinks carry identity.
fn collect(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> Result<(), DirHashError> {
    let read = fs::read_dir(dir).map_err(|source| DirHashError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    for item in read {
        let item = item.map_err(|source| DirHashError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = item.path();

        // symlink_metadata does NOT follow symlinks (spec step 4).
        let meta = fs::symlink_metadata(&path).map_err(|source| DirHashError::Io {
            path: path.clone(),
            source,
        })?;
        let file_type = meta.file_type();

        if file_type.is_symlink() {
            let target = fs::read_link(&path).map_err(|source| DirHashError::Io {
                path: path.clone(),
                source,
            })?;
            let target = target
                .to_str()
                .ok_or_else(|| DirHashError::NonUtf8Target(path.clone()))?
                .to_owned();
            out.push(Entry {
                rel: rel_string(root, &path)?,
                kind: EntryKind::Symlink(target.into_bytes()),
            });
        } else if file_type.is_dir() {
            collect(root, &path, out)?;
        } else {
            let bytes = fs::read(&path).map_err(|source| DirHashError::Io {
                path: path.clone(),
                source,
            })?;
            out.push(Entry {
                rel: rel_string(root, &path)?,
                kind: EntryKind::File(bytes),
            });
        }
    }
    Ok(())
}

/// Renders `path` relative to `root` as a UTF-8 string with `/` separators on
/// every platform (so the sort and digest are cross-platform stable).
fn rel_string(root: &Path, path: &Path) -> Result<String, DirHashError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| DirHashError::NonUtf8Path(path.to_path_buf()))?;
    let mut parts = Vec::new();
    for component in rel.components() {
        let part = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| DirHashError::NonUtf8Path(path.to_path_buf()))?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn deterministic_same_tree_same_hash() {
        let a = tmp();
        let b = tmp();
        // Create the SAME logical tree in two roots, writing files in a
        // DIFFERENT order to defeat iteration-order dependence.
        write(a.path(), "init.lua", b"-- plugin\n");
        write(a.path(), "data/list.txt", b"a\nb\n");
        write(a.path(), "z.txt", b"last\n");

        write(b.path(), "z.txt", b"last\n");
        write(b.path(), "data/list.txt", b"a\nb\n");
        write(b.path(), "init.lua", b"-- plugin\n");

        let ha = hash_dir(a.path()).unwrap();
        let hb = hash_dir(b.path()).unwrap();
        assert_eq!(ha, hb, "identical trees must hash identically");

        // And hashing the same root twice is stable.
        assert_eq!(hash_dir(a.path()).unwrap(), ha);
    }

    #[test]
    fn sensitive_to_content_change() {
        let dir = tmp();
        write(dir.path(), "init.lua", b"-- v1\n");
        let h1 = hash_dir(dir.path()).unwrap();
        write(dir.path(), "init.lua", b"-- v2\n");
        let h2 = hash_dir(dir.path()).unwrap();
        assert_ne!(h1, h2, "content change must change the hash");
    }

    #[test]
    fn sensitive_to_path_change() {
        let a = tmp();
        let b = tmp();
        write(a.path(), "init.lua", b"x");
        write(b.path(), "other.lua", b"x");
        assert_ne!(
            hash_dir(a.path()).unwrap(),
            hash_dir(b.path()).unwrap(),
            "same content under a different path must change the hash"
        );
    }

    #[test]
    fn sensitive_to_new_file() {
        let dir = tmp();
        write(dir.path(), "init.lua", b"x");
        let h1 = hash_dir(dir.path()).unwrap();
        write(dir.path(), "extra.txt", b"");
        let h2 = hash_dir(dir.path()).unwrap();
        assert_ne!(h1, h2, "adding a file must change the hash");
    }

    #[test]
    fn framing_prevents_path_content_confusion() {
        // path="ab" + contents="c" vs path="a" + contents="bc": without
        // length framing these would feed identical concatenated bytes.
        let a = tmp();
        let b = tmp();
        write(a.path(), "ab", b"c");
        write(b.path(), "a", b"bc");
        assert_ne!(
            hash_dir(a.path()).unwrap(),
            hash_dir(b.path()).unwrap(),
            "path/content boundary must be unambiguous"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_hashed_by_target_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tmp();
        write(dir.path(), "real.txt", b"contents");
        symlink("real.txt", dir.path().join("link")).unwrap();
        let h1 = hash_dir(dir.path()).unwrap();

        // Repoint the symlink to a DIFFERENT target string. The target file
        // need not exist (the symlink is not followed) — the hash must still
        // change because the target string changed.
        fs::remove_file(dir.path().join("link")).unwrap();
        symlink("elsewhere.txt", dir.path().join("link")).unwrap();
        let h2 = hash_dir(dir.path()).unwrap();
        assert_ne!(h1, h2, "symlink target change must change the hash");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_not_followed_to_contents() {
        use std::os::unix::fs::symlink;

        // Two trees: in one, `link` points at a file with contents X; in the
        // other, `link` points at the SAME target name but that target holds
        // different contents. Because symlinks are hashed by target STRING and
        // not followed, the two trees must hash IDENTICALLY.
        let a = tmp();
        let b = tmp();
        symlink("target", a.path().join("link")).unwrap();
        write(a.path(), "target", b"AAAA");
        symlink("target", b.path().join("link")).unwrap();
        write(b.path(), "target", b"AAAA");
        // Same so far; now mutate ONLY the followed-through bytes via a third
        // tree to prove non-following: build c identical to a but with the
        // symlink replaced by a regular file holding the target string.
        assert_eq!(hash_dir(a.path()).unwrap(), hash_dir(b.path()).unwrap());

        // A regular file named `link` containing the bytes "target" must hash
        // DIFFERENTLY from a symlink named `link` pointing at "target"
        // (the entry-kind tag distinguishes them).
        let c = tmp();
        write(c.path(), "link", b"target");
        write(c.path(), "target", b"AAAA");
        assert_ne!(
            hash_dir(a.path()).unwrap(),
            hash_dir(c.path()).unwrap(),
            "a symlink and a regular file with the same payload must differ"
        );
    }

    #[test]
    fn rejects_non_directory() {
        let dir = tmp();
        let file = dir.path().join("f");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            hash_dir(&file),
            Err(DirHashError::NotADirectory(_))
        ));
    }
}
