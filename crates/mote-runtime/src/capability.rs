//! The capability fulfillment map and consumes resolution (ADR-0002; DESIGN
//! §Resolution at load time).
//!
//! Tracks which loaded plugin fulfills which capability, enforcing:
//!
//! - **Exclusive double-claim** — an exclusive capability claimed by a second
//!   plugin fails to load (the user enables exactly one fulfiller).
//! - **Non-exclusive composition** — multiple plugins may fulfill a
//!   non-exclusive capability; the map records all of them. For synchronous
//!   `capabilities.invoke`, the runtime routes according to the registry's
//!   declared dispatch shape (see [`CapabilityMap::fulfillers_for`]).
//! - **Dangling consumer** — a plugin that consumes a capability no loaded
//!   plugin fulfills fails to load with the dangling-consumer error.

use std::collections::BTreeMap;

use mote_registry::{CapabilityRegistry, Composability};
use mote_types::PluginName;

/// Maps each capability name to the ordered list of plugins fulfilling it.
///
/// Ownership lives in the runtime; the host API reads it (through the shared
/// core) to route `capabilities.invoke`.
#[derive(Debug, Default, Clone)]
pub struct CapabilityMap {
    /// capability name → fulfillers, in registration order.
    fulfillers: BTreeMap<String, Vec<PluginName>>,
}

/// Why a capability could not be claimed.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaimError {
    /// The capability is exclusive and already claimed by another plugin.
    Exclusive {
        /// The plugin already fulfilling the capability.
        existing: PluginName,
    },
    /// The capability name is unknown to the registry.
    Unknown,
}

impl CapabilityMap {
    /// A new, empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims `capability` for `plugin`, honoring exclusivity.
    ///
    /// # Errors
    ///
    /// Returns [`ClaimError::Exclusive`] if the capability is exclusive and
    /// already held by a different plugin, or [`ClaimError::Unknown`] if the
    /// registry does not know the capability.
    pub fn claim(
        &mut self,
        registry: &CapabilityRegistry,
        capability: &str,
        plugin: &PluginName,
    ) -> Result<(), ClaimError> {
        let entry = registry.get(capability).ok_or(ClaimError::Unknown)?;
        let fulfillers = self.fulfillers.entry(capability.to_owned()).or_default();

        if entry.composability == Composability::Exclusive
            && let Some(existing) = fulfillers.iter().find(|p| *p != plugin)
        {
            return Err(ClaimError::Exclusive {
                existing: existing.clone(),
            });
        }
        if !fulfillers.contains(plugin) {
            fulfillers.push(plugin.clone());
        }
        Ok(())
    }

    /// Whether any loaded plugin fulfills `capability`.
    #[must_use]
    pub fn is_fulfilled(&self, capability: &str) -> bool {
        self.fulfillers
            .get(capability)
            .is_some_and(|v| !v.is_empty())
    }

    /// The single fulfiller for an **exclusive** capability invocation, or
    /// `None` if the capability is unfulfilled.
    ///
    /// For non-exclusive capabilities, use [`fulfillers_for`](Self::fulfillers_for)
    /// instead — calling this on a non-exclusive capability silently ignores all
    /// fulfillers beyond the first, which is the silent-first-wins anti-pattern
    /// the security review flagged.
    #[must_use]
    pub fn exclusive_fulfiller(&self, capability: &str) -> Option<&PluginName> {
        self.fulfillers.get(capability).and_then(|v| v.first())
    }

    /// Returns the ordered list of fulfillers for `capability` (in registration
    /// order, which reflects priority for `stack` dispatch).
    ///
    /// - An **empty** slice means the capability is unfulfilled.
    /// - A **single-element** slice means one fulfiller (covers both exclusive
    ///   and single-fulfiller non-exclusive cases).
    /// - A **multi-element** slice means multiple fulfillers for a non-exclusive
    ///   capability; the runtime dispatches according to the registry's declared
    ///   [`Dispatch`](mote_registry::Dispatch) shape.
    #[must_use]
    pub fn fulfillers_for(&self, capability: &str) -> &[PluginName] {
        self.fulfillers.get(capability).map_or(&[], Vec::as_slice)
    }

    /// Removes `plugin` from every capability it fulfilled (on unload/reload).
    pub fn remove_plugin(&mut self, plugin: &PluginName) {
        for fulfillers in self.fulfillers.values_mut() {
            fulfillers.retain(|p| p != plugin);
        }
        self.fulfillers.retain(|_, v| !v.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mote_registry::Registry;
    use mote_types::SchemaVersion;

    fn registry() -> Registry {
        Registry::load(SchemaVersion::V1).unwrap()
    }

    fn name(s: &str) -> PluginName {
        PluginName::new(s).unwrap()
    }

    #[test]
    fn exclusive_double_claim_fails() {
        let r = registry();
        let mut map = CapabilityMap::new();
        // ui:urlbar_provider is exclusive.
        map.claim(r.capabilities(), "ui:urlbar_provider", &name("a"))
            .unwrap();
        let err = map
            .claim(r.capabilities(), "ui:urlbar_provider", &name("b"))
            .unwrap_err();
        assert_eq!(
            err,
            ClaimError::Exclusive {
                existing: name("a")
            }
        );
    }

    #[test]
    fn reclaiming_same_plugin_is_idempotent() {
        let r = registry();
        let mut map = CapabilityMap::new();
        map.claim(r.capabilities(), "ui:urlbar_provider", &name("a"))
            .unwrap();
        // Same plugin re-claiming (e.g. reload) is fine.
        map.claim(r.capabilities(), "ui:urlbar_provider", &name("a"))
            .unwrap();
    }

    #[test]
    fn non_exclusive_allows_multiple() {
        let r = registry();
        let mut map = CapabilityMap::new();
        // theme:provider is non-exclusive (stack dispatch).
        map.claim(r.capabilities(), "theme:provider", &name("a"))
            .unwrap();
        map.claim(r.capabilities(), "theme:provider", &name("b"))
            .unwrap();
        // Both fulfillers are recorded; the dispatch shape decides what to do
        // with them (not a silent first-wins pick).
        assert_eq!(
            map.fulfillers_for("theme:provider"),
            &[name("a"), name("b")]
        );
    }

    #[test]
    fn unknown_capability_errors() {
        let r = registry();
        let mut map = CapabilityMap::new();
        assert_eq!(
            map.claim(r.capabilities(), "no-such-cap", &name("a")),
            Err(ClaimError::Unknown)
        );
    }

    #[test]
    fn remove_plugin_frees_exclusive_claim() {
        let r = registry();
        let mut map = CapabilityMap::new();
        map.claim(r.capabilities(), "ui:urlbar_provider", &name("a"))
            .unwrap();
        map.remove_plugin(&name("a"));
        assert!(!map.is_fulfilled("ui:urlbar_provider"));
        // Now b can claim it.
        map.claim(r.capabilities(), "ui:urlbar_provider", &name("b"))
            .unwrap();
    }
}
