//! The dangerous-permission-combination registry, parsed from
//! `combinations/vN.toml` (DISCIPLINES §4).

use mote_types::SchemaVersion;
use serde::Deserialize;

use crate::error::RegistryLoadError;

/// How serious a dangerous combination is, for approval-UI emphasis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Severity {
    /// Worth flagging, not necessarily blocking.
    Warn,
    /// High-risk; surfaced prominently above the per-permission list.
    Danger,
}

/// One dangerous combination: the set of `domain:action` terms that together
/// create a capability none has alone, plus the warning text.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct CombinationEntry {
    /// The `domain:action` terms that together are dangerous. Matched
    /// resource-independently.
    pub permissions: Vec<String>,
    /// Severity for UI emphasis.
    pub severity: Severity,
    /// Warning text shown in the approval UI.
    pub warning: String,
}

/// Raw deserialization shape of `combinations/vN.toml`.
#[derive(Debug, Deserialize)]
struct CombinationsFile {
    #[serde(default, rename = "combination")]
    combinations: Vec<CombinationEntry>,
}

/// The dangerous-combination registry for one schema version.
#[derive(Debug, Clone)]
pub struct CombinationRegistry {
    entries: Vec<CombinationEntry>,
}

impl CombinationRegistry {
    /// Parses a `combinations/vN.toml` source string.
    pub(crate) fn from_toml(src: &str, version: SchemaVersion) -> Result<Self, RegistryLoadError> {
        let file: CombinationsFile =
            toml::from_str(src).map_err(|e| RegistryLoadError::toml("combinations", version, e))?;
        Ok(Self {
            entries: file.combinations,
        })
    }

    /// Returns the combinations whose every term is present in
    /// `requested_keys` (the set of `domain:action` keys a plugin requested).
    ///
    /// Matching is resource-independent: only the `domain:action` key matters,
    /// per DISCIPLINES §4.
    pub fn triggered_by<'a>(
        &'a self,
        requested_keys: &'a std::collections::BTreeSet<String>,
    ) -> impl Iterator<Item = &'a CombinationEntry> {
        self.entries
            .iter()
            .filter(move |c| c.permissions.iter().all(|p| requested_keys.contains(p)))
    }

    /// Returns every combination entry.
    #[must_use]
    pub fn entries(&self) -> &[CombinationEntry] {
        &self.entries
    }
}
