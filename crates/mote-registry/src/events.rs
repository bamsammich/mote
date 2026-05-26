//! The typed hook/event-name registry, parsed from `events/vN.toml`.
//!
//! Risk C1/G6: hook/event names are a separate namespace from permission names.
//! This registry lets conformance and future hook-key validation check
//! `M.hooks` / `M.events` keys against a versioned vocabulary.

use std::collections::BTreeMap;

use mote_types::SchemaVersion;
use serde::Deserialize;

use crate::error::RegistryLoadError;

/// The dispatch pattern the runtime applies to a hook/event (DESIGN §Plugin
/// Dispatch and Composition).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EventDispatch {
    /// Ordered chain; first `block` wins, `modify` cascades.
    FilterChain,
    /// All handlers receive the event independently; no return semantics.
    Broadcast,
    /// Subscribers contribute results an exclusive provider merges.
    Collector,
    /// Each plugin runs independently in its own isolated world.
    FanOutPerOrigin,
}

/// Whether a name is a runtime-originated hook (`M.hooks`) or an inter-plugin
/// bus event (`M.events`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EventKind {
    /// Declared in `M.hooks`; the runtime originates it.
    Hook,
    /// Declared in `M.events`; carried on the inter-plugin event bus.
    Event,
}

/// One hook/event entry: its key, dispatch pattern, kind, and description.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct EventEntry {
    /// The hook/event key (the `M.hooks` / `M.events` table key).
    pub name: String,
    /// The dispatch pattern.
    pub dispatch: EventDispatch,
    /// Whether it is a hook or a bus event.
    pub kind: EventKind,
    /// Short description.
    pub description: String,
}

/// Raw deserialization shape of `events/vN.toml`.
#[derive(Debug, Deserialize)]
struct EventsFile {
    #[serde(default, rename = "event")]
    events: Vec<EventEntry>,
}

/// The hook/event-name registry for one schema version, indexed by name.
#[derive(Debug, Clone)]
pub struct EventRegistry {
    by_name: BTreeMap<String, EventEntry>,
}

impl EventRegistry {
    /// Parses an `events/vN.toml` source string and builds the index, rejecting
    /// duplicate names.
    pub(crate) fn from_toml(src: &str, version: SchemaVersion) -> Result<Self, RegistryLoadError> {
        let file: EventsFile =
            toml::from_str(src).map_err(|e| RegistryLoadError::toml("events", version, e))?;
        let mut by_name = BTreeMap::new();
        for entry in file.events {
            let name = entry.name.clone();
            if by_name.insert(name.clone(), entry).is_some() {
                return Err(RegistryLoadError::inconsistent(
                    version,
                    format!("duplicate event entry `{name}`"),
                ));
            }
        }
        Ok(Self { by_name })
    }

    /// Looks up an event entry by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EventEntry> {
        self.by_name.get(name)
    }

    /// Whether a given hook/event name is known to this registry.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Returns every event entry, ordered by name.
    pub fn entries(&self) -> impl Iterator<Item = &EventEntry> {
        self.by_name.values()
    }

    /// The number of event entries.
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
