//! The `managed.lua` writer — Mote's machine-managed plugin declaration layer.
//!
//! `managed.lua` (`~/.config/mote/managed.lua`) is a **Mote-owned, generated**
//! Lua file that records plugins added via `mote plugin add`. It is **never**
//! hand-edited by users; Mote regenerates it wholesale on every mutation via an
//! atomic temp-write + `rename()`. It is loaded by the config loader alongside
//! the user's `plugins.lua` and parsed by `mote_lua::eval_config`.
//!
//! ## Contract
//!
//! - Wholesale-generated from a [`ManagedFile`] on every mutation.
//! - Carries a "DO NOT EDIT" header (see [`HEADER`]).
//! - Human edits are not preserved — Mote overwrites.
//! - Loaded *last* by the config loader (after the human's `plugins.lua`),
//!   so the managed layer additively overrides user config via last-writer-wins.
//! - Sorted by [`PluginName`] for byte-stable, deterministic output.
//!
//! ## Round-trip guarantee
//!
//! Everything written by [`ManagedFile::render`] must parse back cleanly
//! through `mote_lua::eval_config`. Tests enforce this property.
//!
//! ## ADR
//!
//! See `docs/adr/0006-user-config-read-only-to-mote.md`.

use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use mote_types::PluginName;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The header prepended to every generated `managed.lua`.
///
/// Quoted to avoid typos picking up the literal phrase in the source.
const HEADER: &str = concat!(
    "-- ",
    "DO NOT EDIT",
    " \u{2014} managed by Mote (mote plugin add/remove/source).\n",
    "-- Your hand-authored config lives in plugins.lua; this file is regenerated.\n",
);

// ---------------------------------------------------------------------------
// Entry type
// ---------------------------------------------------------------------------

/// A single plugin declaration in the managed layer.
///
/// Entries are stored sorted by [`PluginName`] so that [`ManagedFile::render`]
/// is deterministic (same logical set → byte-identical output) regardless of
/// insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedEntry {
    /// The canonical, validated plugin name (hyphenated lowercase).
    pub name: PluginName,
    /// The raw source string (e.g. `"github:mote-browser/adblock"`,
    /// `"path:~/code/x"`, `"bundled"`).
    pub source: String,
    /// An optional version/tag/branch constraint.  `None` means the `version`
    /// field is omitted from the rendered Lua.
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by [`ManagedFile`] operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ManagedError {
    /// The file path has no parent directory.
    #[error("managed.lua path has no parent directory: {path:?}")]
    NoParent {
        /// The path that was missing a parent.
        path: PathBuf,
    },
    /// An I/O error during reading, writing, or renaming.
    #[error("I/O error on {path:?}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file does not parse as valid config Lua.
    #[error("managed.lua does not parse as valid config Lua: {0}")]
    Parse(#[from] mote_lua::ConfigError),
    /// A plugin key in the file is not a valid [`PluginName`].
    #[error("managed.lua contains invalid plugin name {key:?}: {source}")]
    InvalidName {
        /// The raw key string that failed validation.
        key: String,
        /// The underlying name-parse error.
        #[source]
        source: mote_types::PluginNameError,
    },
    /// A plugin entry is missing the required `source` field after parsing.
    ///
    /// This should not occur in practice because `eval_config` already
    /// rejects entries without `source`, but is preserved as a belt-and-
    /// suspenders guard.
    #[error("managed.lua entry {key:?} is missing a source")]
    MissingSource {
        /// The key whose source was missing.
        key: String,
    },
    /// A source or version string contains a character that cannot be
    /// safely escaped into a Lua double-quoted string.
    #[error(
        "plugin {name:?} source/version contains an unescapable control character (U+{cp:04X})"
    )]
    UnescapableChar {
        /// The plugin name.
        name: String,
        /// The Unicode code point.
        cp: u32,
    },
}

// ---------------------------------------------------------------------------
// Lua string escaping
// ---------------------------------------------------------------------------

/// Escapes a string for embedding inside a Lua double-quoted string literal.
///
/// Escapes `\` (→ `\\`), `"` (→ `\"`), and ASCII control characters
/// (→ `\<decimal>`). Returns an error if the string contains a character whose
/// code point exceeds `U+007F` and is a Lua-unescapable control category
/// (non-printable, non-ASCII control). In practice source strings are ASCII,
/// but the function guards against accidental non-ASCII control injection.
fn lua_escape(s: &str, name: &str) -> Result<String, ManagedError> {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // ASCII control characters (U+0000–U+001F, U+007F)
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                write!(out, "\\{}", c as u32).expect("write to String is infallible");
            }
            // Non-ASCII C1 controls (U+0080–U+009F) — reject
            c if (c as u32) >= 0x80 && (c as u32) < 0xA0 => {
                return Err(ManagedError::UnescapableChar {
                    name: name.to_owned(),
                    cp: c as u32,
                });
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// ManagedFile
// ---------------------------------------------------------------------------

/// The in-memory model for `managed.lua`.
///
/// Entries are stored in a [`BTreeMap`] keyed by [`PluginName`] so that
/// iteration order is deterministic (sorted). All mutation methods (`upsert`,
/// `remove`) preserve this invariant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedFile {
    entries: BTreeMap<PluginName, ManagedEntry>,
}

impl ManagedFile {
    /// Creates an empty [`ManagedFile`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Inserts or replaces the entry for `name`.
    ///
    /// If an entry with the same name already exists it is replaced wholesale
    /// (upsert semantics). Insertion order does not affect output; entries are
    /// always iterated in [`PluginName`] sort order.
    pub fn upsert(&mut self, name: PluginName, source: String, version: Option<String>) {
        self.entries.insert(
            name.clone(),
            ManagedEntry {
                name,
                source,
                version,
            },
        );
    }

    /// Removes the entry for `name`.
    ///
    /// Returns `true` if an entry was present and removed, `false` if no entry
    /// existed for `name`.
    pub fn remove(&mut self, name: &PluginName) -> bool {
        self.entries.remove(name).is_some()
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Returns an iterator over the entries in [`PluginName`] sort order.
    pub fn entries(&self) -> impl Iterator<Item = &ManagedEntry> {
        self.entries.values()
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Renders this [`ManagedFile`] to a Lua string.
    ///
    /// The output is deterministic: the same logical set of entries, inserted
    /// in any order, produces byte-identical output. Keys are quoted
    /// (`["name"]`) because hyphens are illegal in bare Lua identifiers.
    /// The `version` field is omitted entirely when [`None`].
    ///
    /// The rendered string is valid input to `mote_lua::eval_config`
    /// (round-trip guarantee enforced by tests).
    ///
    /// # Errors
    ///
    /// Returns [`ManagedError::UnescapableChar`] if any source or version
    /// string contains a non-ASCII control character that cannot be safely
    /// expressed in a Lua double-quoted string literal.
    pub fn render(&self) -> Result<String, ManagedError> {
        let mut out = String::new();
        out.push_str(HEADER);
        out.push_str("mote.plugins({\n");

        // Compute the maximum key length for alignment (cosmetic, not required
        // for correctness — but it matches the spec's aligned example).
        let max_key_len = self
            .entries
            .keys()
            .map(|n| n.as_str().len())
            .max()
            .unwrap_or(0);

        for entry in self.entries.values() {
            let key = entry.name.as_str();
            // `["key"]` — quoted because hyphens are illegal in bare Lua identifiers.
            let quoted_key = format!("[\"{key}\"]");
            // Pad the key column for visual alignment.
            let padding = " ".repeat(max_key_len.saturating_sub(key.len()));

            let source_escaped = lua_escape(&entry.source, key)?;

            let fields = match &entry.version {
                None => format!("{{ source = \"{source_escaped}\" }}"),
                Some(v) => {
                    let version_escaped = lua_escape(v, key)?;
                    format!("{{ source = \"{source_escaped}\", version = \"{version_escaped}\" }}")
                }
            };

            writeln!(out, "  {quoted_key}{padding} = {fields},")
                .expect("write to String is infallible");
        }

        out.push_str("})\n");
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Atomic I/O
    // -----------------------------------------------------------------------

    /// Writes this [`ManagedFile`] to `path` atomically.
    ///
    /// Writes first to a temporary file in the **same directory** as `path`
    /// (ensuring the temp file and target are on the same filesystem), then
    /// calls `std::fs::rename` to replace the target. Parent directories are
    /// created if they do not exist. On failure the target is not modified.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedError`] if:
    /// - `path` has no parent directory ([`ManagedError::NoParent`]).
    /// - Any I/O operation fails ([`ManagedError::Io`]).
    /// - `render()` fails ([`ManagedError::UnescapableChar`]).
    pub fn write_atomic(&self, path: &Path) -> Result<(), ManagedError> {
        let parent = path.parent().ok_or_else(|| ManagedError::NoParent {
            path: path.to_owned(),
        })?;

        // Create parent dirs if missing.
        fs::create_dir_all(parent).map_err(|e| ManagedError::Io {
            path: parent.to_owned(),
            source: e,
        })?;

        let content = self.render()?;

        // Write to a temp file in the same directory so rename is same-fs.
        let mut tmp = tempfile::Builder::new()
            .prefix(".managed_tmp")
            .tempfile_in(parent)
            .map_err(|e| ManagedError::Io {
                path: parent.to_owned(),
                source: e,
            })?;

        tmp.write_all(content.as_bytes())
            .map_err(|e| ManagedError::Io {
                path: tmp.path().to_owned(),
                source: e,
            })?;

        // Persist (keep) the file before rename so the OS doesn't delete it.
        let tmp_path = tmp.into_temp_path();
        tmp_path.persist(path).map_err(|e| ManagedError::Io {
            path: path.to_owned(),
            source: e.error,
        })?;

        Ok(())
    }

    /// Loads a `managed.lua` from `path` into a [`ManagedFile`].
    ///
    /// Reads the file, evaluates it through `mote_lua::eval_config`, then maps
    /// each [`PluginEntry`](mote_lua::config::PluginEntry) back to a
    /// [`ManagedEntry`], validating each key as a [`PluginName`].
    ///
    /// # Errors
    ///
    /// Returns [`ManagedError`] if the file cannot be read, does not parse as
    /// valid config Lua, or contains an invalid plugin name.
    pub fn load(path: &Path) -> Result<Self, ManagedError> {
        let source = fs::read_to_string(path).map_err(|e| ManagedError::Io {
            path: path.to_owned(),
            source: e,
        })?;

        let spec = mote_lua::eval_config(&source, "managed.lua")?;

        let mut file = Self::new();
        for entry in spec.plugins {
            let name = PluginName::new(&entry.key).map_err(|e| ManagedError::InvalidName {
                key: entry.key.clone(),
                source: e,
            })?;
            if entry.source.is_empty() {
                return Err(ManagedError::MissingSource { key: entry.key });
            }
            file.upsert(name, entry.source, entry.version);
        }
        Ok(file)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> PluginName {
        PluginName::new(s).unwrap()
    }

    // -----------------------------------------------------------------------
    // round-trip: render → eval_config → entries match
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_render_to_eval_config() {
        let mut mf = ManagedFile::new();
        mf.upsert(
            name("adblock"),
            "github:mote-browser/adblock".to_owned(),
            None,
        );
        mf.upsert(
            name("cool-plugin"),
            "github:them/cool-plugin".to_owned(),
            Some("v1.2.3".to_owned()),
        );
        mf.upsert(name("my-local"), "path:~/code/x".to_owned(), None);

        let lua = mf.render().unwrap();

        let spec = mote_lua::eval_config(&lua, "managed.lua").unwrap();
        assert_eq!(spec.plugins.len(), 3);

        // Collect by key for order-independent comparison.
        let by_key: std::collections::HashMap<_, _> =
            spec.plugins.iter().map(|e| (e.key.as_str(), e)).collect();

        assert_eq!(by_key["adblock"].source, "github:mote-browser/adblock");
        assert_eq!(by_key["adblock"].version, None);

        assert_eq!(by_key["cool-plugin"].source, "github:them/cool-plugin");
        assert_eq!(by_key["cool-plugin"].version.as_deref(), Some("v1.2.3"));

        assert_eq!(by_key["my-local"].source, "path:~/code/x");
        assert_eq!(by_key["my-local"].version, None);
    }

    // -----------------------------------------------------------------------
    // round-trip: write_atomic → load → equal
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_write_then_load() {
        let mut mf = ManagedFile::new();
        mf.upsert(
            name("adblock"),
            "github:mote-browser/adblock".to_owned(),
            None,
        );
        mf.upsert(
            name("vim-mode"),
            "github:mote-browser/vim-mode".to_owned(),
            Some("v2.0.0".to_owned()),
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.lua");

        mf.write_atomic(&path).unwrap();
        let loaded = ManagedFile::load(&path).unwrap();

        assert_eq!(loaded, mf, "load(write_atomic(x)) must equal x");
    }

    // -----------------------------------------------------------------------
    // determinism: same set inserted in different orders → identical bytes
    // -----------------------------------------------------------------------

    #[test]
    fn deterministic_output_regardless_of_insertion_order() {
        let entries = [
            (name("zeta"), "github:them/zeta".to_owned(), None::<String>),
            (
                name("alpha"),
                "github:them/alpha".to_owned(),
                Some("v1.0".to_owned()),
            ),
            (name("mid"), "path:~/mid".to_owned(), None),
        ];

        let mut mf1 = ManagedFile::new();
        for (n, s, v) in &entries {
            mf1.upsert(n.clone(), s.clone(), v.clone());
        }

        let mut mf2 = ManagedFile::new();
        for (n, s, v) in entries.iter().rev() {
            mf2.upsert(n.clone(), s.clone(), v.clone());
        }

        assert_eq!(
            mf1.render().unwrap(),
            mf2.render().unwrap(),
            "render must be byte-identical regardless of insertion order"
        );
    }

    // -----------------------------------------------------------------------
    // escaping: source containing " and \ round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn escape_quotes_and_backslashes_round_trip() {
        let tricky_source = r#"path:~/my "plugin"\dir"#;
        let mut mf = ManagedFile::new();
        mf.upsert(name("tricky"), tricky_source.to_owned(), None);

        let lua = mf.render().unwrap();
        let spec = mote_lua::eval_config(&lua, "managed.lua").unwrap();

        assert_eq!(spec.plugins.len(), 1);
        assert_eq!(spec.plugins[0].source, tricky_source);
    }

    // -----------------------------------------------------------------------
    // empty: empty ManagedFile renders valid Lua with empty plugin set
    // -----------------------------------------------------------------------

    #[test]
    fn empty_renders_valid_lua() {
        let mf = ManagedFile::new();
        let lua = mf.render().unwrap();

        // Must contain the mote.plugins({}) call.
        assert!(lua.contains("mote.plugins({"), "must call mote.plugins");
        assert!(lua.contains("})"), "must close the call");

        // Must parse without error.
        let spec = mote_lua::eval_config(&lua, "managed.lua").unwrap();
        assert!(
            spec.plugins.is_empty(),
            "empty file must parse to empty set"
        );
    }

    // -----------------------------------------------------------------------
    // atomicity: no leftover temp file; target contains rendered bytes
    // -----------------------------------------------------------------------

    #[test]
    fn write_atomic_no_leftover_temp() {
        let mut mf = ManagedFile::new();
        mf.upsert(name("myplugin"), "bundled".to_owned(), None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.lua");

        mf.write_atomic(&path).unwrap();

        // Target must exist and contain the rendered content.
        let on_disk = fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, mf.render().unwrap());

        // No `.managed_tmp*` leftover files in the directory.
        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".managed_tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "no temp files should remain after write_atomic: {leftover:?}"
        );
    }

    // -----------------------------------------------------------------------
    // write_atomic creates parent dirs if missing
    // -----------------------------------------------------------------------

    #[test]
    fn write_atomic_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path that doesn't exist yet.
        let path = dir.path().join("a").join("b").join("managed.lua");

        let mf = ManagedFile::new();
        mf.write_atomic(&path).unwrap();

        assert!(path.exists(), "target must exist after write_atomic");
    }

    // -----------------------------------------------------------------------
    // upsert semantics: replaces an existing entry
    // -----------------------------------------------------------------------

    #[test]
    fn upsert_replaces_existing_entry() {
        let mut mf = ManagedFile::new();
        mf.upsert(name("adblock"), "github:old/adblock".to_owned(), None);
        // Upsert with a new source and version.
        mf.upsert(
            name("adblock"),
            "github:new/adblock".to_owned(),
            Some("v2.0".to_owned()),
        );

        let entries: Vec<_> = mf.entries().collect();
        assert_eq!(entries.len(), 1, "upsert must not create a duplicate entry");
        assert_eq!(entries[0].source, "github:new/adblock");
        assert_eq!(entries[0].version.as_deref(), Some("v2.0"));
    }

    // -----------------------------------------------------------------------
    // remove semantics: returns false for absent; true for present
    // -----------------------------------------------------------------------

    #[test]
    fn remove_returns_correct_bool() {
        let mut mf = ManagedFile::new();
        mf.upsert(name("present"), "bundled".to_owned(), None);

        assert!(
            !mf.remove(&name("absent")),
            "remove of absent entry must return false"
        );
        assert!(
            mf.remove(&name("present")),
            "remove of present entry must return true"
        );
        assert!(
            !mf.remove(&name("present")),
            "second remove of same entry must return false"
        );
        assert_eq!(mf.entries().count(), 0);
    }

    // -----------------------------------------------------------------------
    // sorted output: alpha before zeta in rendered Lua
    // -----------------------------------------------------------------------

    #[test]
    fn output_sorted_by_plugin_name() {
        let mut mf = ManagedFile::new();
        mf.upsert(name("zeta"), "bundled".to_owned(), None);
        mf.upsert(name("alpha"), "bundled".to_owned(), None);
        mf.upsert(name("mid"), "bundled".to_owned(), None);

        let lua = mf.render().unwrap();
        let alpha_pos = lua.find("\"alpha\"").unwrap();
        let mid_pos = lua.find("\"mid\"").unwrap();
        let zeta_pos = lua.find("\"zeta\"").unwrap();

        assert!(
            alpha_pos < mid_pos && mid_pos < zeta_pos,
            "entries must be sorted lexicographically:\n{lua}"
        );
    }
}
