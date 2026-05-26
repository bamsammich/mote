//! The assembled per-version [`Registry`] and the two enforcement-step entry
//! points (schema validation and contract conformance).

use std::collections::BTreeSet;

use mote_lua::LoadedPlugin;
use mote_permissions::Permission;
use mote_types::SchemaVersion;

use crate::capabilities::CapabilityRegistry;
use crate::combinations::CombinationRegistry;
use crate::error::{ConformanceError, RegistryLoadError, SchemaValidationError};
use crate::events::EventRegistry;
use crate::permissions::{PermissionRegistry, ResourceShape};

// --- Embedded registry data (ships with the source) -------------------------

const PERMISSIONS_V1: &str = include_str!("../data/permissions/v1.toml");
const CAPABILITIES_V1: &str = include_str!("../data/capabilities/v1.toml");
const EVENTS_V1: &str = include_str!("../data/events/v1.toml");
const COMBINATIONS_V1: &str = include_str!("../data/combinations/v1.toml");

/// The complete, validated registry bundle for one schema version: permissions,
/// capabilities, hook/event names, and dangerous combinations.
///
/// Construct with [`Registry::load`], which parses the embedded TOML files and
/// runs internal-consistency checks. The two enforcement steps —
/// [`Registry::validate_schema`] (step 1) and [`Registry::check_conformance`]
/// (step 3) — are methods on the loaded registry.
#[derive(Debug, Clone)]
pub struct Registry {
    version: SchemaVersion,
    permissions: PermissionRegistry,
    capabilities: CapabilityRegistry,
    events: EventRegistry,
    combinations: CombinationRegistry,
}

impl Registry {
    /// Loads and validates the registry for `version` from the embedded TOML.
    ///
    /// Runs internal-consistency checks: no duplicate terms, every non-exclusive
    /// capability declares a dispatch shape (and no exclusive one does), and
    /// every combination references known `domain:action` permissions.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryLoadError`] if a file fails to parse or the registry is
    /// internally inconsistent. A failure here is a Mote build bug, not plugin
    /// input — it cannot be triggered by a plugin.
    pub fn load(version: SchemaVersion) -> Result<Self, RegistryLoadError> {
        let (perm_src, cap_src, event_src, combo_src) = match version {
            SchemaVersion::V1 => (PERMISSIONS_V1, CAPABILITIES_V1, EVENTS_V1, COMBINATIONS_V1),
            // `SchemaVersion` is `#[non_exhaustive]`; a future version with no
            // embedded data is an explicit, recoverable load error rather than a
            // panic.
            other => {
                return Err(RegistryLoadError::Inconsistent {
                    version: other,
                    reason: format!("no embedded registry data for schema {other}"),
                });
            }
        };

        let permissions = PermissionRegistry::from_toml(perm_src, version)?;
        let capabilities = CapabilityRegistry::from_toml(cap_src, version)?;
        let events = EventRegistry::from_toml(event_src, version)?;
        let combinations = CombinationRegistry::from_toml(combo_src, version)?;

        // Cross-file consistency: every combination term must be a known permission.
        for combo in combinations.entries() {
            for term in &combo.permissions {
                let (domain, action) =
                    term.split_once(':')
                        .ok_or_else(|| RegistryLoadError::Inconsistent {
                            version,
                            reason: format!("combination term {term:?} is not `domain:action`"),
                        })?;
                if permissions.get(domain, action).is_none() {
                    return Err(RegistryLoadError::Inconsistent {
                        version,
                        reason: format!("combination references unknown permission `{term}`"),
                    });
                }
            }
        }

        Ok(Self {
            version,
            permissions,
            capabilities,
            events,
            combinations,
        })
    }

    /// The schema version this registry encodes.
    #[must_use]
    pub const fn version(&self) -> SchemaVersion {
        self.version
    }

    /// The permission registry.
    #[must_use]
    pub const fn permissions(&self) -> &PermissionRegistry {
        &self.permissions
    }

    /// The capability registry.
    #[must_use]
    pub const fn capabilities(&self) -> &CapabilityRegistry {
        &self.capabilities
    }

    /// The hook/event-name registry.
    #[must_use]
    pub const fn events(&self) -> &EventRegistry {
        &self.events
    }

    /// The dangerous-combination registry.
    #[must_use]
    pub const fn combinations(&self) -> &CombinationRegistry {
        &self.combinations
    }

    // --- Enforcement step 1 — schema validation -----------------------------

    /// **Enforcement step 1 — schema validation.** Verifies that every
    /// `permissions`, `capabilities`, and `consumes` term references a known
    /// registry entry with correct grammar.
    ///
    /// Handles dynamic-resource forms (risk C4): for a `dynamic`-shaped
    /// permission the validator checks the `domain:action` is known and a
    /// resource segment is present, but does not constrain the concrete name.
    ///
    /// All term lists are checked; the **first** offending term yields its
    /// specific error.
    ///
    /// # Errors
    ///
    /// Returns the first [`SchemaValidationError`] encountered: unknown
    /// permission/capability, a grammar failure, or a resource-shape mismatch.
    pub fn validate_schema(
        &self,
        permissions: &[String],
        capabilities: &[String],
        consumes: &[String],
    ) -> Result<(), SchemaValidationError> {
        for term in permissions {
            self.validate_permission_term(term)?;
        }
        // `capabilities` (fulfilled) and `consumes` (depended-on) both reference
        // capability registry names; the grammar is identical.
        for term in capabilities.iter().chain(consumes) {
            if self.capabilities.get(term).is_none() {
                return Err(SchemaValidationError::UnknownCapability {
                    term: term.clone(),
                    version: self.version,
                });
            }
        }
        Ok(())
    }

    /// Validates one permission term against the registry.
    fn validate_permission_term(&self, term: &str) -> Result<(), SchemaValidationError> {
        let parsed: Permission =
            term.parse()
                .map_err(|source| SchemaValidationError::PermissionGrammar {
                    term: term.to_owned(),
                    source,
                })?;

        let entry = self
            .permissions
            .get(parsed.domain(), parsed.action())
            .ok_or_else(|| SchemaValidationError::UnknownPermission {
                term: term.to_owned(),
                domain: parsed.domain().to_owned(),
                action: parsed.action().to_owned(),
                version: self.version,
            })?;

        match entry.resource {
            ResourceShape::None => {
                if parsed.resource().is_some() {
                    return Err(SchemaValidationError::UnexpectedResource {
                        term: term.to_owned(),
                        domain: parsed.domain().to_owned(),
                        action: parsed.action().to_owned(),
                    });
                }
            }
            ResourceShape::Dynamic => {
                if parsed.resource().is_none() {
                    return Err(SchemaValidationError::MissingResource {
                        term: term.to_owned(),
                        domain: parsed.domain().to_owned(),
                        action: parsed.action().to_owned(),
                    });
                }
            }
            // Glob: resource optional; absent means all-in-scope.
            ResourceShape::Glob => {}
        }
        Ok(())
    }

    // --- Enforcement step 3 — contract conformance --------------------------

    /// **Enforcement step 3 — contract conformance.** For each capability the
    /// `plugin` claims (its manifest `capabilities`), verifies the loaded module
    /// declares every required API function (in `api_keys()`) and every required
    /// event handler (in `event_keys()` or `hook_keys()`).
    ///
    /// Contracts are **loose**: a plugin may declare additional API/events;
    /// missing required surface fails with a precise error naming what's absent.
    ///
    /// # Errors
    ///
    /// Returns the first [`ConformanceError`]: an unknown claimed capability, a
    /// missing required API function, or a missing required event handler.
    pub fn check_conformance(&self, plugin: &LoadedPlugin) -> Result<(), ConformanceError> {
        let api: BTreeSet<&str> = plugin.api_keys().iter().map(String::as_str).collect();
        let events: BTreeSet<&str> = plugin.event_keys().iter().map(String::as_str).collect();
        let hooks: BTreeSet<&str> = plugin.hook_keys().iter().map(String::as_str).collect();

        for capability in &plugin.manifest().capabilities {
            let entry = self.capabilities.get(capability).ok_or_else(|| {
                ConformanceError::UnknownCapability {
                    capability: capability.clone(),
                    version: self.version,
                }
            })?;

            for required in &entry.contract.required_api {
                if !api.contains(required.as_str()) {
                    return Err(ConformanceError::MissingApi {
                        capability: capability.clone(),
                        missing: required.clone(),
                        declared: plugin.api_keys().to_vec(),
                    });
                }
            }

            // A required event handler may be declared either as a bus event
            // (M.events) or as a hook (M.hooks) — the conformance check accepts
            // the key in either declarative table.
            for required in &entry.contract.required_events {
                if !events.contains(required.as_str()) && !hooks.contains(required.as_str()) {
                    return Err(ConformanceError::MissingEvent {
                        capability: capability.clone(),
                        missing: required.clone(),
                        declared_events: plugin.event_keys().to_vec(),
                        declared_hooks: plugin.hook_keys().to_vec(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::capabilities::{Composability, Dispatch};
    use crate::permissions::ResourceShape;

    use super::*;

    fn v1() -> Registry {
        Registry::load(SchemaVersion::V1).expect("v1 registry loads and self-checks")
    }

    // --- load + internal consistency ---------------------------------------

    #[test]
    fn v1_loads_and_is_self_consistent() {
        let r = v1();
        assert_eq!(r.version(), SchemaVersion::V1);
        assert!(!r.permissions().is_empty());
        assert!(!r.capabilities().is_empty());
        assert!(!r.events().is_empty());
    }

    // --- full DESIGN permission coverage -----------------------------------
    //
    // Every domain:action from DESIGN §"Permission Primitives", minus the
    // intentional v1 omissions (introspect:* — risk C2). sys:clipboard:* is
    // encoded as single-action tokens (risk C3); mcp:client / secret:read take
    // a dynamic resource (risk C4).

    #[test]
    fn v1_enumerates_full_design_permission_set() {
        let r = v1();
        let expected: &[(&str, &str)] = &[
            ("net", "intercept_request"),
            ("net", "read_response_body"),
            ("net", "modify_response"),
            ("net", "fetch_unsigned"),
            ("page", "inject_script"),
            ("page", "inject_unsafe_script"),
            ("page", "inject_css"),
            ("page", "read_dom"),
            ("tabs", "list"),
            ("tabs", "focus"),
            ("tabs", "create"),
            ("tabs", "close"),
            ("tabs", "get_history"),
            ("tabs", "reveal"),
            ("tabs", "modify_state"),
            ("workspaces", "list"),
            ("workspaces", "switch"),
            ("identity", "read_current"),
            ("identity", "list"),
            ("identity", "create"),
            ("storage", "persistent"),
            ("storage", "memory"),
            ("session", "manage_hidden"),
            ("session", "exclude_forms"),
            ("bookmarks", "read"),
            ("bookmarks", "write"),
            ("history", "read"),
            ("history", "write"),
            ("history", "delete"),
            ("config", "read"),
            ("config", "watch"),
            ("ui", "sidebar"),
            ("ui", "panel"),
            ("ui", "action_button"),
            ("ui", "urlbar_extension"),
            ("keys", "bind"),
            ("keys", "intercept_input"),
            ("crypto", "seal_to_plugin"),
            ("sys", "native_message"),
            ("sys", "clipboard_read"),
            ("sys", "clipboard_write"),
            ("sys", "notify"),
            ("events", "emit"),
            ("events", "on"),
            ("http", "fetch"),
            ("mcp", "client"),
            ("mcp", "server"),
            ("secret", "read"),
        ];
        for (domain, action) in expected {
            assert!(
                r.permissions().get(domain, action).is_some(),
                "missing permission `{domain}:{action}`"
            );
        }
        assert_eq!(
            r.permissions().len(),
            expected.len(),
            "registry has permissions not asserted above (or vice versa)"
        );
    }

    #[test]
    fn v1_excludes_introspect_domain() {
        let r = v1();
        for action in ["accessibility_tree", "framework_state", "console"] {
            assert!(
                r.permissions().get("introspect", action).is_none(),
                "introspect:{action} must be excluded from v1 (risk C2)"
            );
        }
    }

    // --- full DESIGN capability coverage -----------------------------------

    #[test]
    fn v1_enumerates_full_design_capability_set() {
        let r = v1();
        let expected = [
            "ui:urlbar_provider",
            "ui:newtab_replacer",
            "ui:download_handler",
            "ui:bookmarks_provider",
            "ui:history_provider",
            "workspace:provider",
            "password-manager:provider",
            "theme:provider",
            "adblock:rule_source",
            "mcp:server",
            "secret:provider",
            "password-manager-form-services",
        ];
        for name in expected {
            assert!(
                r.capabilities().get(name).is_some(),
                "missing capability `{name}`"
            );
        }
        assert_eq!(r.capabilities().len(), expected.len());
    }

    #[test]
    fn critical_capabilities_match_design() {
        let r = v1();
        let critical: BTreeSet<&str> = r
            .capabilities()
            .entries()
            .filter(|c| c.critical)
            .map(|c| c.name.as_str())
            .collect();
        let expected: BTreeSet<&str> = [
            "workspace:provider",
            "ui:urlbar_provider",
            "ui:bookmarks_provider",
            "ui:history_provider",
        ]
        .into_iter()
        .collect();
        assert_eq!(critical, expected);
    }

    #[test]
    fn password_manager_provider_is_not_critical() {
        let r = v1();
        assert!(
            !r.capabilities()
                .get("password-manager:provider")
                .unwrap()
                .critical
        );
    }

    #[test]
    fn composability_and_dispatch_shapes_match_design() {
        let r = v1();
        let theme = r.capabilities().get("theme:provider").unwrap();
        assert_eq!(theme.composability, Composability::NonExclusive);
        assert_eq!(theme.dispatch, Some(Dispatch::Stack));

        let mcp = r.capabilities().get("mcp:server").unwrap();
        assert_eq!(mcp.composability, Composability::NonExclusive);
        assert_eq!(mcp.dispatch, Some(Dispatch::Aggregate));

        let urlbar = r.capabilities().get("ui:urlbar_provider").unwrap();
        assert_eq!(urlbar.composability, Composability::Exclusive);
        assert_eq!(urlbar.dispatch, None);
    }

    // --- event registry is a separate namespace (risk C1/G6) ---------------

    #[test]
    fn event_names_are_known() {
        let r = v1();
        for name in [
            "net:intercept_request",
            "page:on_load",
            "tabs:on_change",
            "workspaces:on_change",
            "urlbar:suggest",
        ] {
            assert!(r.events().contains(name), "event `{name}` should be known");
        }
    }

    // --- step 1: resource-shape handling -----------------------------------

    #[test]
    fn none_shape_rejects_a_resource() {
        let r = v1();
        // storage:persistent takes no resource.
        assert_eq!(
            r.permissions()
                .get("storage", "persistent")
                .unwrap()
                .resource,
            ResourceShape::None
        );
        let err = r
            .validate_schema(&["storage:persistent:oops".to_owned()], &[], &[])
            .unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::UnexpectedResource { .. }
        ));
    }

    #[test]
    fn glob_shape_accepts_present_or_absent_resource() {
        let r = v1();
        r.validate_schema(&["http:fetch".to_owned()], &[], &[])
            .expect("absent glob ok");
        r.validate_schema(&["http:fetch:https://*.example.com/*".to_owned()], &[], &[])
            .expect("present glob ok");
        r.validate_schema(
            &["net:intercept_request:!*.banking.com".to_owned()],
            &[],
            &[],
        )
        .expect("negated glob ok");
    }

    #[test]
    fn dynamic_shape_requires_a_resource() {
        let r = v1();
        // Present name: ok.
        r.validate_schema(&["secret:read:anthropic_api_key".to_owned()], &[], &[])
            .expect("dynamic with name ok");
        r.validate_schema(&["mcp:client:my-server".to_owned()], &[], &[])
            .expect("dynamic client name ok");
        // Absent name: missing required resource.
        let err = r
            .validate_schema(&["secret:read".to_owned()], &[], &[])
            .unwrap_err();
        assert!(matches!(err, SchemaValidationError::MissingResource { .. }));
    }

    #[test]
    fn bad_grammar_fails_step1() {
        let r = v1();
        let err = r
            .validate_schema(&["NotValid".to_owned()], &[], &[])
            .unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::PermissionGrammar { .. }
        ));
    }

    #[test]
    fn consumes_terms_are_validated_as_capabilities() {
        let r = v1();
        r.validate_schema(&[], &[], &["password-manager-form-services".to_owned()])
            .expect("known consumed capability ok");
        let err = r
            .validate_schema(&[], &[], &["nope".to_owned()])
            .unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::UnknownCapability { .. }
        ));
    }

    // --- combinations (DISCIPLINES §4) -------------------------------------

    #[test]
    fn dangerous_combination_is_detected() {
        let r = v1();
        let requested: BTreeSet<String> = ["page:read_dom".to_owned(), "mcp:server".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            r.combinations().triggered_by(&requested).count(),
            1,
            "page:read_dom + mcp:server is dangerous"
        );
    }

    #[test]
    fn partial_combination_does_not_trigger() {
        let r = v1();
        let requested: BTreeSet<String> = std::iter::once("page:read_dom".to_owned()).collect();
        assert_eq!(r.combinations().triggered_by(&requested).count(), 0);
    }
}
