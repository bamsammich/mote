//! The `plugins.lock` model — TOML serialisation of resolved plugin versions.
//!
//! `plugins.lock` is machine-managed, checked into dotfiles, and opaque to
//! users (DESIGN §Manifest and lock file: "its TOML format is an implementation
//! detail"). One table per plugin, keyed by the canonical [`PluginName`]
//! (hyphenated — the manifest's name, *not* the `plugins.lua` Lua key, which
//! may use underscores):
//!
//! ```toml
//! [plugins.adblock]
//! source   = "github:mote-browser/adblock"
//! commit   = "abc123def456..."
//! checksum = "blake3:..."          # the DIRECTORY hash, the integrity anchor
//!
//! [plugins.my-local-plugin]
//! source   = "path:~/code/my-plugin"
//! # no commit for path/bundled sources; checksum is the dir hash at last sync
//! checksum = "blake3:..."
//! ```
//!
//! - `commit` is present for Git sources, absent for `path:`/`bundled`.
//! - `checksum` is the BLAKE3 **directory** hash ([`crate::dirhash`]) — the
//!   integrity anchor. There is no per-manifest checksum (the lock's directory
//!   hash is the mechanism).
//! - Entries are stored in a [`BTreeMap`] so the on-disk key order is
//!   deterministic and the round-trip is byte-stable.

use std::collections::BTreeMap;

use mote_types::{Checksum, PluginName};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::source::Source;

/// One plugin's pinned state in `plugins.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Where the plugin was fetched from. Recorded so `sync` on a fresh machine
    /// knows where to look. Serialised via the [`Source`] string grammar.
    #[serde(with = "source_string")]
    pub source: Source,
    /// The resolved Git commit SHA. Present for Git sources, absent for
    /// `path:`/`bundled` (which carry no commit).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub commit: Option<String>,
    /// The BLAKE3 directory hash at last sync — the integrity anchor.
    /// Serialised via the [`Checksum`] `blake3:<hex>` string form.
    #[serde(with = "checksum_string")]
    pub checksum: Checksum,
}

/// The parsed `plugins.lock`: a deterministic, name-keyed map of [`LockEntry`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockFile {
    /// Per-plugin pinned state, keyed by canonical [`PluginName`].
    ///
    /// `mote_types::PluginName` carries no `serde` impls (the shared-vocabulary
    /// crate stays dependency-light), so the map is (de)serialised through a
    /// `String`-keyed adapter that validates each key as a [`PluginName`].
    #[serde(default, with = "plugin_name_map")]
    pub plugins: BTreeMap<PluginName, LockEntry>,
}

/// Error returned when reading or writing `plugins.lock`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockError {
    /// The TOML text could not be parsed into a [`LockFile`].
    #[error("failed to parse plugins.lock: {0}")]
    Parse(#[from] toml::de::Error),
    /// The [`LockFile`] could not be serialised to TOML.
    #[error("failed to serialize plugins.lock: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl LockFile {
    /// Parses a `plugins.lock` from its TOML text.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::Parse`] if the text is not valid lock TOML.
    pub fn from_toml(text: &str) -> Result<Self, LockError> {
        Ok(toml::from_str(text)?)
    }

    /// Serialises this lock file to TOML text with deterministic key order.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::Serialize`] if serialisation fails (should not
    /// happen for a well-formed [`LockFile`]).
    pub fn to_toml(&self) -> Result<String, LockError> {
        Ok(toml::to_string_pretty(self)?)
    }
}

/// `serde` adapter for a `BTreeMap<PluginName, LockEntry>`.
///
/// `PluginName` has no `serde` impls, so we bridge through a
/// `BTreeMap<String, LockEntry>` and validate each key as a [`PluginName`] on
/// the way in. Ordering is preserved because validated `PluginName`s sort the
/// same as their underlying strings.
mod plugin_name_map {
    use std::collections::BTreeMap;
    use std::str::FromStr as _;

    use mote_types::PluginName;
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};

    use super::LockEntry;

    pub(super) fn serialize<S: Serializer>(
        map: &BTreeMap<PluginName, LockEntry>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        let stringly: BTreeMap<&str, &LockEntry> =
            map.iter().map(|(k, v)| (k.as_str(), v)).collect();
        stringly.serialize(ser)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<BTreeMap<PluginName, LockEntry>, D::Error> {
        let stringly = BTreeMap::<String, LockEntry>::deserialize(de)?;
        stringly
            .into_iter()
            .map(|(k, v)| {
                PluginName::from_str(&k)
                    .map(|name| (name, v))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

/// `serde` adapter serialising a [`Source`] as its `plugins.lua` string form.
mod source_string {
    use std::str::FromStr as _;

    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::source::Source;

    pub(super) fn serialize<S: Serializer>(src: &Source, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&src.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Source, D::Error> {
        let s = String::deserialize(de)?;
        Source::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// `serde` adapter serialising a [`Checksum`] as its `blake3:<hex>` string form.
mod checksum_string {
    use std::str::FromStr as _;

    use mote_types::Checksum;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(sum: &Checksum, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&sum.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Checksum, D::Error> {
        let s = String::deserialize(de)?;
        Checksum::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum() -> Checksum {
        Checksum::hash(b"some plugin tree")
    }

    fn name(s: &str) -> PluginName {
        PluginName::new(s).unwrap()
    }

    #[test]
    fn round_trips_git_and_path_entries() {
        let mut lock = LockFile::default();
        lock.plugins.insert(
            name("adblock"),
            LockEntry {
                source: "github:mote-browser/adblock".parse().unwrap(),
                commit: Some("abc123def456".into()),
                checksum: sum(),
            },
        );
        lock.plugins.insert(
            name("my-local-plugin"),
            LockEntry {
                source: "path:~/code/my-plugin".parse().unwrap(),
                commit: None,
                checksum: sum(),
            },
        );

        let text = lock.to_toml().unwrap();
        let parsed = LockFile::from_toml(&text).unwrap();
        assert_eq!(parsed, lock, "parse(serialize(x)) must equal x");

        // Serialise again — byte-stable (deterministic key order via BTreeMap).
        assert_eq!(parsed.to_toml().unwrap(), text);
    }

    #[test]
    fn path_entry_omits_commit_in_toml() {
        let mut lock = LockFile::default();
        lock.plugins.insert(
            name("local"),
            LockEntry {
                source: "path:/abs".parse().unwrap(),
                commit: None,
                checksum: sum(),
            },
        );
        let text = lock.to_toml().unwrap();
        assert!(
            !text.contains("commit"),
            "path source must not emit a commit key:\n{text}"
        );
    }

    #[test]
    fn key_order_is_deterministic() {
        // Insert out of lexicographic order; expect sorted output.
        let mut lock = LockFile::default();
        for n in ["zeta", "alpha", "mid"] {
            lock.plugins.insert(
                name(n),
                LockEntry {
                    source: "bundled".parse().unwrap(),
                    commit: None,
                    checksum: sum(),
                },
            );
        }
        let text = lock.to_toml().unwrap();
        let alpha = text.find("alpha").unwrap();
        let mid = text.find("mid").unwrap();
        let zeta = text.find("zeta").unwrap();
        assert!(
            alpha < mid && mid < zeta,
            "keys must be lexicographically ordered:\n{text}"
        );
    }

    #[test]
    fn empty_lock_round_trips() {
        let lock = LockFile::default();
        let text = lock.to_toml().unwrap();
        assert_eq!(LockFile::from_toml(&text).unwrap(), lock);
    }

    #[test]
    fn parses_known_good_toml() {
        let text = r#"
[plugins.adblock]
source = "github:mote-browser/adblock"
commit = "abc123def456"
checksum = "blake3:0000000000000000000000000000000000000000000000000000000000000000"
"#;
        let lock = LockFile::from_toml(text).unwrap();
        let entry = &lock.plugins[&name("adblock")];
        assert_eq!(entry.commit.as_deref(), Some("abc123def456"));
        assert!(entry.source.is_git());
    }
}
