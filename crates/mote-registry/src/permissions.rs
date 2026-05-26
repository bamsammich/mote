//! The typed permission registry, parsed from `permissions/vN.toml`.

use std::collections::BTreeMap;

use mote_types::SchemaVersion;
use serde::Deserialize;

use crate::error::RegistryLoadError;

/// The shape of a permission's optional resource segment (DESIGN risk C4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ResourceShape {
    /// No resource segment permitted; the term is exactly `domain:action`.
    None,
    /// An optional glob resource (origin/host pattern). Absent ⇒ all-in-scope.
    Glob,
    /// A required, plugin-supplied free-form name segment the registry cannot
    /// enumerate (`mcp:client:<server-name>`, `secret:read:<name>`). Step-1
    /// validates that *a* resource is present; the concrete value is validated
    /// later, at grant/resolution time.
    Dynamic,
}

/// One permission entry in the registry: a `domain:action` term, its resource
/// shape, and human-facing metadata.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct PermissionEntry {
    /// The permission domain (e.g. `net`, `page`, `secret`).
    pub domain: String,
    /// The permission action (e.g. `intercept_request`, `read`).
    pub action: String,
    /// The shape of the resource segment this permission accepts.
    pub resource: ResourceShape,
    /// Short description of what the permission allows.
    pub description: String,
    /// Risk / scrutiny note surfaced in the approval UI and integrity panel.
    pub risk: String,
}

impl PermissionEntry {
    /// The canonical `domain:action` key for this entry.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.domain, self.action)
    }
}

/// Raw deserialization shape of `permissions/vN.toml`.
#[derive(Debug, Deserialize)]
struct PermissionsFile {
    #[serde(default, rename = "permission")]
    permissions: Vec<PermissionEntry>,
}

/// The permission registry for one schema version.
///
/// Indexed by `domain:action` for O(log n) lookup during step-1 schema
/// validation.
#[derive(Debug, Clone)]
pub struct PermissionRegistry {
    by_key: BTreeMap<String, PermissionEntry>,
}

impl PermissionRegistry {
    /// Parses a `permissions/vN.toml` source string and builds the index.
    pub(crate) fn from_toml(src: &str, version: SchemaVersion) -> Result<Self, RegistryLoadError> {
        let file: PermissionsFile =
            toml::from_str(src).map_err(|e| RegistryLoadError::toml("permissions", version, e))?;
        let mut by_key = BTreeMap::new();
        for entry in file.permissions {
            let key = entry.key();
            if by_key.insert(key.clone(), entry).is_some() {
                return Err(RegistryLoadError::inconsistent(
                    version,
                    format!("duplicate permission entry `{key}`"),
                ));
            }
        }
        Ok(Self { by_key })
    }

    /// Looks up a permission entry by its `domain:action` key.
    #[must_use]
    pub fn get(&self, domain: &str, action: &str) -> Option<&PermissionEntry> {
        self.by_key.get(&format!("{domain}:{action}"))
    }

    /// Returns every permission entry, ordered by `domain:action`.
    pub fn entries(&self) -> impl Iterator<Item = &PermissionEntry> {
        self.by_key.values()
    }

    /// The number of permission entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether the registry has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}
