//! The typed capability registry, parsed from `capabilities/vN.toml`.

use std::collections::BTreeMap;

use mote_types::SchemaVersion;
use serde::Deserialize;

use crate::error::RegistryLoadError;

/// Whether a capability may be fulfilled by one plugin or many (DESIGN
/// §Capability Roles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Composability {
    /// Only one plugin may fulfill at a time; a second claimant fails to load.
    Exclusive,
    /// Multiple plugins may fulfill simultaneously; see [`Dispatch`].
    NonExclusive,
}

/// How the runtime treats multiple fulfillers of a non-exclusive capability
/// (DESIGN §Capability Roles — dispatch shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Dispatch {
    /// Call each fulfiller in priority order and stack their results (themes).
    Stack,
    /// Aggregate contributions into a unified surface (mcp:server tools,
    /// adblock rule sets).
    Aggregate,
    /// Fan events out to all fulfillers (form-services, secret providers).
    FanOut,
}

/// The loose conformance contract for a capability (DESIGN §Enforcement step 3).
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct Contract {
    /// API function names the fulfiller MUST expose in `M.api`.
    #[serde(default)]
    pub required_api: Vec<String>,
    /// Event-handler keys the fulfiller MUST declare in `M.events` / `M.hooks`.
    #[serde(default)]
    pub required_events: Vec<String>,
}

/// One capability entry: its name, composability, dispatch shape, criticality,
/// and conformance contract.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct CapabilityEntry {
    /// The capability term (also the `capabilities` / `consumes` token).
    pub name: String,
    /// Whether one or many plugins may fulfill it.
    pub composability: Composability,
    /// For non-exclusive capabilities, the dispatch shape. `None` for exclusive.
    #[serde(default)]
    pub dispatch: Option<Dispatch>,
    /// Whether this is a browser-critical-path capability (extended schema
    /// deprecation window).
    #[serde(default)]
    pub critical: bool,
    /// Short role summary.
    pub description: String,
    /// The conformance contract.
    #[serde(default)]
    pub contract: Contract,
}

/// Raw deserialization shape of `capabilities/vN.toml`.
#[derive(Debug, Deserialize)]
struct CapabilitiesFile {
    #[serde(default, rename = "capability")]
    capabilities: Vec<CapabilityEntry>,
}

/// The capability registry for one schema version, indexed by name.
#[derive(Debug, Clone)]
pub struct CapabilityRegistry {
    by_name: BTreeMap<String, CapabilityEntry>,
}

impl CapabilityRegistry {
    /// Parses a `capabilities/vN.toml` source string and builds the index,
    /// enforcing internal consistency: no duplicate names, and every
    /// non-exclusive capability declares a dispatch shape while no exclusive one
    /// does.
    pub(crate) fn from_toml(src: &str, version: SchemaVersion) -> Result<Self, RegistryLoadError> {
        let file: CapabilitiesFile =
            toml::from_str(src).map_err(|e| RegistryLoadError::toml("capabilities", version, e))?;
        let mut by_name = BTreeMap::new();
        for entry in file.capabilities {
            match entry.composability {
                Composability::NonExclusive if entry.dispatch.is_none() => {
                    return Err(RegistryLoadError::inconsistent(
                        version,
                        format!(
                            "non-exclusive capability `{}` must declare a dispatch shape",
                            entry.name
                        ),
                    ));
                }
                Composability::Exclusive if entry.dispatch.is_some() => {
                    return Err(RegistryLoadError::inconsistent(
                        version,
                        format!(
                            "exclusive capability `{}` must not declare a dispatch shape",
                            entry.name
                        ),
                    ));
                }
                _ => {}
            }
            let name = entry.name.clone();
            if by_name.insert(name.clone(), entry).is_some() {
                return Err(RegistryLoadError::inconsistent(
                    version,
                    format!("duplicate capability entry `{name}`"),
                ));
            }
        }
        Ok(Self { by_name })
    }

    /// Looks up a capability entry by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CapabilityEntry> {
        self.by_name.get(name)
    }

    /// Returns every capability entry, ordered by name.
    pub fn entries(&self) -> impl Iterator<Item = &CapabilityEntry> {
        self.by_name.values()
    }

    /// The number of capability entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether the registry has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}
