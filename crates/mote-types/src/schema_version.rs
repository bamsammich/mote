//! The plugin-manifest schema version selector.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The schema version a plugin manifest targets.
///
/// A manifest declares `schema = "v1"`; the runtime resolves that to the
/// matching registry version. Only `v1` exists today. Adding a variant is a
/// release event governed by the schema-versioning discipline (DISCIPLINES §2),
/// not a routine change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SchemaVersion {
    /// Schema version 1 — the initial, locked manifest schema.
    V1,
}

impl SchemaVersion {
    /// Returns the canonical wire string for this version (e.g. `"v1"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
        }
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string is not a recognized [`SchemaVersion`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown schema version {input:?}: expected one of [\"v1\"]")]
pub struct SchemaVersionParseError {
    input: String,
}

impl FromStr for SchemaVersion {
    type Err = SchemaVersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "v1" => Ok(Self::V1),
            other => Err(SchemaVersionParseError {
                input: other.to_owned(),
            }),
        }
    }
}
