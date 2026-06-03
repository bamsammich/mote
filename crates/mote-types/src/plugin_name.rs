//! The validated plugin-name identifier.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// A validated plugin name.
///
/// Plugin names form storage namespaces, cache paths
/// (`~/.cache/mote/plugins/<name>/<commit>/`), and capability-tool prefixes
/// (`<plugin-name>.<tool>`), so they must be filesystem- and URL-safe and
/// stable. The accepted grammar is a lowercase DNS-label-style identifier:
///
/// - non-empty;
/// - characters are ASCII lowercase letters, ASCII digits, or `-`;
/// - must start and end with an alphanumeric character;
/// - no two consecutive hyphens.
///
/// Examples: `history`, `dark-mode`, `password-manager-1password`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginName(String);

impl PluginName {
    /// Validates `name` and constructs a [`PluginName`].
    ///
    /// User-supplied / third-party plugin names go through this path. The
    /// `mote` and `mote-*` namespace is reserved for built-in identifiers
    /// (ADR-0016 status-line built-ins use `mote.<id>`); a user plugin
    /// shipped with one of those names is rejected so it cannot create
    /// id collisions with built-in elements. Internal Mote pseudo-plugins
    /// (e.g. the per-identity approval store, the session-storage namespace)
    /// that legitimately need the `mote-*` prefix use
    /// [`PluginName::new_internal`] instead — that constructor bypasses
    /// the reservation but is `pub(crate)`-discipline at the call sites
    /// (it's `pub` to allow cross-crate use but conventionally only Mote's
    /// own crates call it).
    ///
    /// # Errors
    ///
    /// Returns [`PluginNameError`] if `name` violates the identifier grammar
    /// described on the type, or if `name` is in the reserved `mote` /
    /// `mote-*` namespace.
    pub fn new(name: impl Into<String>) -> Result<Self, PluginNameError> {
        let validated = Self::validate_grammar(name)?;
        // Reserve the `mote` prefix for built-in identifiers (ADR-0016
        // status-line built-ins use `mote.<id>`; future built-in namespaces
        // may follow the same convention). A plugin named "mote" or
        // "mote-<anything>" could create id collisions with built-ins
        // (e.g. status-line element `mote.hoverurl`). The collision is not
        // a privilege escalation (any plugin can lie in surfaces it can
        // publish to), but it would let a malicious plugin overwrite
        // built-in element state — closed here by reserving the prefix at
        // the name-validation boundary.
        if validated.0 == "mote" || validated.0.starts_with("mote-") {
            return Err(PluginNameError::ReservedName(validated.0));
        }
        Ok(validated)
    }

    /// Validates `name` for use as an INTERNAL Mote identifier — bypasses the
    /// `mote-*` reservation. Use this from Mote's own crates for pseudo-plugin
    /// namespaces (e.g. `mote-session` for the session-storage namespace,
    /// `mote-approval-store` for the per-identity approval store). External /
    /// user-supplied plugin names MUST go through [`PluginName::new`] which
    /// rejects the reserved prefix.
    ///
    /// # Errors
    ///
    /// Returns [`PluginNameError`] if `name` violates the identifier grammar.
    /// Does NOT return `PluginNameError::ReservedName` — that check is the
    /// whole point of the carve-out.
    pub fn new_internal(name: impl Into<String>) -> Result<Self, PluginNameError> {
        Self::validate_grammar(name)
    }

    /// Shared grammar check used by both [`new`](Self::new) and
    /// [`new_internal`](Self::new_internal). Does NOT enforce the reserved-
    /// namespace rule — callers layer that on top as needed.
    fn validate_grammar(name: impl Into<String>) -> Result<Self, PluginNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(PluginNameError::Empty);
        }

        let mut prev_hyphen = false;
        for (i, ch) in name.char_indices() {
            match ch {
                'a'..='z' | '0'..='9' => prev_hyphen = false,
                '-' => {
                    if i == 0 {
                        return Err(PluginNameError::LeadingHyphen);
                    }
                    if prev_hyphen {
                        return Err(PluginNameError::ConsecutiveHyphens);
                    }
                    prev_hyphen = true;
                }
                other => return Err(PluginNameError::InvalidChar(other)),
            }
        }

        if prev_hyphen {
            return Err(PluginNameError::TrailingHyphen);
        }

        Ok(Self(name))
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PluginName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for PluginName {
    type Err = PluginNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Error returned when a string is not a valid [`PluginName`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PluginNameError {
    /// The name was empty.
    #[error("plugin name must not be empty")]
    Empty,
    /// The name started with a hyphen.
    #[error("plugin name must not start with a hyphen")]
    LeadingHyphen,
    /// The name ended with a hyphen.
    #[error("plugin name must not end with a hyphen")]
    TrailingHyphen,
    /// The name contained two consecutive hyphens.
    #[error("plugin name must not contain consecutive hyphens")]
    ConsecutiveHyphens,
    /// The name contained a character outside `[a-z0-9-]`.
    #[error("plugin name contains invalid character {0:?}: only [a-z0-9-] allowed")]
    InvalidChar(char),
    /// The name is reserved for built-in identifiers — `mote` or `mote-*`.
    /// Reserved to prevent collisions with the `mote.<id>` built-in namespace
    /// used by ADR-0016 status-line elements and future built-in surfaces.
    #[error("plugin name {0:?} is reserved (mote, mote-* are reserved for built-ins)")]
    ReservedName(String),
}
