//! The web-origin newtype used in permission resources.

use std::fmt;

/// A web origin, as it appears in permission resources and network hooks.
///
/// This is a thin, validated-on-use wrapper around the origin string (e.g.
/// `https://github.com`). `mote-types` deliberately keeps no URL-parsing logic;
/// origins flow through permission patterns ([`crate::Glob`]) as strings, and
/// the engine layer (`mote-cef`) is the authority on canonicalization. Keeping
/// it a distinct newtype prevents accidentally passing an arbitrary string
/// where an origin is expected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Origin(String);

impl Origin {
    /// Wraps `origin` as an [`Origin`].
    pub fn new(origin: impl Into<String>) -> Self {
        Self(origin.into())
    }

    /// Returns the origin as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Origin {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
