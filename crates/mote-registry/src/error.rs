//! Error types for registry loading and the two enforcement steps.

use mote_permissions::PermissionParseError;
use mote_types::SchemaVersion;
use thiserror::Error;

/// Error raised while loading or validating an embedded registry file.
///
/// A registry that fails to load is a *build-time bug in Mote*, not a plugin
/// fault — these surface in [`crate::Registry::load`] and the internal-consistency
/// checks, never in step-1 / step-3 validation of plugin input.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RegistryLoadError {
    /// A TOML registry file failed to parse.
    #[error("failed to parse {file} registry (schema {version}): {source}")]
    Toml {
        /// Which registry file (e.g. `"permissions"`).
        file: &'static str,
        /// The schema version being loaded.
        version: SchemaVersion,
        /// The underlying TOML parse error.
        #[source]
        source: toml::de::Error,
    },

    /// The registry is internally inconsistent (e.g. a duplicate term, a
    /// non-exclusive capability missing its dispatch shape, or a combination
    /// referencing an unknown permission).
    #[error("registry self-consistency check failed ({version}): {reason}")]
    Inconsistent {
        /// The schema version being loaded.
        version: SchemaVersion,
        /// Human-readable description of the inconsistency.
        reason: String,
    },
}

impl RegistryLoadError {
    /// Builds a [`RegistryLoadError::Toml`] tagged with the originating file.
    pub(crate) const fn toml(
        file: &'static str,
        version: SchemaVersion,
        source: toml::de::Error,
    ) -> Self {
        Self::Toml {
            file,
            version,
            source,
        }
    }

    /// Builds a [`RegistryLoadError::Inconsistent`] for an internal-consistency
    /// failure.
    pub(crate) fn inconsistent(version: SchemaVersion, reason: impl Into<String>) -> Self {
        Self::Inconsistent {
            version,
            reason: reason.into(),
        }
    }
}

/// Error raised by **enforcement step 1 — schema validation** when a plugin's
/// declared term does not reference a known registry entry with correct grammar.
///
/// Every variant names the offending term and why it failed, so the loader can
/// produce a precise, actionable message.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaValidationError {
    /// A permission string did not parse as `domain:action[:resource]`.
    #[error("permission {term:?} is not valid grammar: {source}")]
    PermissionGrammar {
        /// The offending term, verbatim.
        term: String,
        /// The underlying parse error.
        #[source]
        source: PermissionParseError,
    },

    /// The `domain:action` pair is not a known permission in this registry.
    #[error(
        "unknown permission `{domain}:{action}` (term {term:?}): not in the {version} registry"
    )]
    UnknownPermission {
        /// The offending term, verbatim.
        term: String,
        /// The parsed domain.
        domain: String,
        /// The parsed action.
        action: String,
        /// The registry version validated against.
        version: SchemaVersion,
    },

    /// The permission is known but carries a resource segment when the registry
    /// entry forbids one (`resource = "none"`).
    #[error("permission `{domain}:{action}` takes no resource, but {term:?} supplies one")]
    UnexpectedResource {
        /// The offending term, verbatim.
        term: String,
        /// The parsed domain.
        domain: String,
        /// The parsed action.
        action: String,
    },

    /// The permission requires a dynamic resource segment but none was supplied
    /// (`resource = "dynamic"`, e.g. `secret:read` without a `<name>`).
    #[error("permission `{domain}:{action}` requires a resource name, but {term:?} supplies none")]
    MissingResource {
        /// The offending term, verbatim.
        term: String,
        /// The parsed domain.
        domain: String,
        /// The parsed action.
        action: String,
    },

    /// The resource segment of a `dynamic`-shaped permission contains glob
    /// metacharacters or characters outside the allowed literal-name charset
    /// (`[A-Za-z0-9_.:-]`).
    ///
    /// Dynamic permissions (`mcp:client:<name>`, `secret:read:<name>`) grant
    /// access to ONE named resource.  Glob metacharacters (`*`, `!`, `[`, `?`,
    /// `{`) would silently widen the grant to multiple resources, which is
    /// forbidden.  Use an exact, literal name only.
    #[error(
        "permission `{domain}:{action}` takes a literal resource name, but {term:?} contains \
         glob metacharacters or invalid characters (allowed: [A-Za-z0-9_.:-])"
    )]
    InvalidDynamicResource {
        /// The offending term, verbatim.
        term: String,
        /// The parsed domain.
        domain: String,
        /// The parsed action.
        action: String,
    },

    /// A capability term (in `capabilities` or `consumes`) is not a known
    /// capability in this registry.
    #[error("unknown capability {term:?}: not in the {version} registry")]
    UnknownCapability {
        /// The offending term, verbatim.
        term: String,
        /// The registry version validated against.
        version: SchemaVersion,
    },
}

/// Error raised by **enforcement step 3 — contract conformance** when a plugin
/// claiming a capability is missing required API or event surface.
///
/// Contracts are loose: extra surface is fine; missing required surface fails
/// here, naming exactly what is absent.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConformanceError {
    /// The plugin claims a capability not present in this registry. (Normally
    /// step 1 catches this first; checked again here for a standalone step-3 call.)
    #[error("plugin claims capability {capability:?}, which is not in the {version} registry")]
    UnknownCapability {
        /// The claimed capability.
        capability: String,
        /// The registry version checked against.
        version: SchemaVersion,
    },

    /// The plugin claims a capability but does not declare a required API function.
    #[error(
        "plugin claims capability {capability:?} but is missing required API function {missing:?} in M.api (declared: {declared:?})"
    )]
    MissingApi {
        /// The claimed capability.
        capability: String,
        /// The required API function name that is absent.
        missing: String,
        /// The API function names the plugin actually declared.
        declared: Vec<String>,
    },

    /// The plugin claims a capability but does not declare a required event handler.
    #[error(
        "plugin claims capability {capability:?} but is missing required event handler {missing:?} (declared events: {declared_events:?}, hooks: {declared_hooks:?})"
    )]
    MissingEvent {
        /// The claimed capability.
        capability: String,
        /// The required event-handler key that is absent.
        missing: String,
        /// The event keys the plugin declared (`M.events`).
        declared_events: Vec<String>,
        /// The hook keys the plugin declared (`M.hooks`).
        declared_hooks: Vec<String>,
    },
}
