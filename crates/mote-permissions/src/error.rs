//! Parse-error type for the `domain:action[:resource]` permission grammar.

use thiserror::Error;

use mote_types::GlobParseError;

/// Error returned when a string is not a valid permission.
///
/// The grammar is `domain:action[:resource]`:
///
/// - `domain` — a non-empty ASCII identifier (`[a-z][a-z0-9_]*`).
/// - `action` — same grammar as `domain`.
/// - `resource` — an optional [`mote_types::Glob`] (including `!`-negated
///   deny patterns).  When absent, the permission is treated as if the
///   resource were `*`.
///
/// Syntactically invalid strings (wrong segment count, bad identifiers, bad
/// glob) are rejected here with a precise error variant. Whether a parsed
/// `(domain, action)` pair is a *known* registry term is `mote-registry`'s
/// job, not this crate's.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PermissionParseError {
    /// The string had no `:` separators at all.
    #[error("permission {input:?} must be in the form domain:action[:resource]")]
    MissingSeparator {
        /// The original input.
        input: String,
    },
    /// The `domain` segment was empty.
    #[error("permission {input:?} has an empty domain segment")]
    EmptyDomain {
        /// The original input.
        input: String,
    },
    /// The `action` segment was empty.
    #[error("permission {input:?} has an empty action segment")]
    EmptyAction {
        /// The original input.
        input: String,
    },
    /// A `domain` or `action` identifier contained a character outside
    /// `[a-z0-9_]`, or started with a digit or underscore.
    #[error("permission {input:?} segment {segment:?} is not a valid identifier: {reason}")]
    InvalidIdentifier {
        /// The original input.
        input: String,
        /// Which segment was invalid (`"domain"` or `"action"`).
        segment: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// The `resource` glob was syntactically invalid.
    #[error("permission {input:?} resource pattern is invalid: {source}")]
    InvalidResourceGlob {
        /// The original input.
        input: String,
        /// Underlying glob parse failure.
        #[source]
        source: GlobParseError,
    },
}
