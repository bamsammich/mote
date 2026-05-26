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
    /// # Errors
    ///
    /// Returns [`PluginNameError`] if `name` violates the identifier grammar
    /// described on the type.
    pub fn new(name: impl Into<String>) -> Result<Self, PluginNameError> {
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
}
