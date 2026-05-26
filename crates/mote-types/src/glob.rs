//! Permission-pattern glob matching with `!` negation and deny-precedence.
//!
//! Permission resources use a small glob grammar (DESIGN §Permission
//! Primitives): `*` is a wildcard matching any run of characters (including
//! empty), and a leading `!` marks the whole pattern as a *deny*. A [`Glob`] is
//! one such pattern; a [`GlobSet`] is a collection evaluated with **deny
//! precedence** — a matching deny beats any matching allow.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The marker prefix that turns a pattern into a deny rule.
const NEGATION: char = '!';
/// The wildcard character.
const WILDCARD: char = '*';

/// A single permission-resource glob pattern.
///
/// The pattern body is split on `*` into literal segments; matching requires
/// each segment to appear in order, with arbitrary (possibly empty) runs of
/// characters between them. A leading `!` is stripped and recorded as negation
/// (see [`Glob::is_negated`]); it affects how a [`GlobSet`] resolves the
/// pattern but does not change what the pattern *matches*.
///
/// There is no escape syntax: a literal `*` in matched input is matched by a
/// literal `*` in the same position of a segment, but a pattern cannot require
/// an asterisk to appear at a wildcard position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Glob {
    negated: bool,
    /// Literal segments between wildcards, in order. A pattern of `"a*b"`
    /// yields `["a", "b"]`; `"*"` yields `["", ""]`.
    segments: Vec<String>,
}

impl Glob {
    /// Returns `true` if this pattern is a deny (was written with a leading `!`).
    #[must_use]
    pub const fn is_negated(&self) -> bool {
        self.negated
    }

    /// Returns `true` if `candidate` matches this pattern's glob body.
    ///
    /// Negation does not affect matching; it is resolved by [`GlobSet`].
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        // Anchor the first segment at the start.
        let Some((first, rest)) = self.segments.split_first() else {
            return false;
        };
        if !candidate.starts_with(first.as_str()) {
            return false;
        }
        let mut pos = first.len();

        let Some((last, middles)) = rest.split_last() else {
            // Single segment: must match the entire candidate exactly.
            return pos == candidate.len();
        };

        // Each middle segment must be found in order, after the current position.
        for seg in middles {
            if seg.is_empty() {
                continue;
            }
            match candidate[pos..].find(seg.as_str()) {
                Some(found) => pos += found + seg.len(),
                None => return false,
            }
        }

        // The final segment must match the tail (anchored at the end).
        let tail = &candidate[pos..];
        tail.len() >= last.len() && tail.ends_with(last.as_str())
    }
}

impl fmt::Display for Glob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            f.write_str("!")?;
        }
        f.write_str(&self.segments.join("*"))
    }
}

/// Error returned when a string is not a valid [`Glob`] pattern.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GlobParseError {
    /// The pattern (after any `!`) was empty.
    #[error("glob pattern must not be empty")]
    Empty,
}

impl FromStr for Glob {
    type Err = GlobParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (negated, body) = s
            .strip_prefix(NEGATION)
            .map_or((false, s), |rest| (true, rest));
        if body.is_empty() {
            return Err(GlobParseError::Empty);
        }
        let segments = body.split(WILDCARD).map(str::to_owned).collect();
        Ok(Self { negated, segments })
    }
}

/// The outcome of evaluating a candidate against a [`GlobSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Match {
    /// At least one allow pattern matched and no deny pattern matched.
    Allow,
    /// At least one deny pattern matched (deny takes precedence).
    Deny,
    /// No pattern matched at all.
    Unmatched,
}

/// A collection of [`Glob`] patterns evaluated with deny precedence.
///
/// Evaluation rule (DESIGN §Permission Primitives — "Negative / deny (takes
/// precedence)"): if any negated pattern matches the candidate, the result is
/// [`Match::Deny`], regardless of allow matches and regardless of pattern
/// order. Otherwise, if any allow pattern matches, the result is
/// [`Match::Allow`]. If nothing matches, the result is [`Match::Unmatched`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobSet {
    globs: Vec<Glob>,
}

impl GlobSet {
    /// Builds a [`GlobSet`] from already-parsed globs.
    #[must_use]
    pub const fn new(globs: Vec<Glob>) -> Self {
        Self { globs }
    }

    /// Parses each pattern string into a [`Glob`] and collects them into a set.
    ///
    /// # Errors
    ///
    /// Returns the first [`GlobParseError`] encountered.
    pub fn parse<I, S>(patterns: I) -> Result<Self, GlobParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let globs = patterns
            .into_iter()
            .map(|p| p.as_ref().parse::<Glob>())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { globs })
    }

    /// Evaluates `candidate` against the set with deny precedence.
    #[must_use]
    pub fn evaluate(&self, candidate: &str) -> Match {
        let mut allowed = false;
        for glob in &self.globs {
            if glob.matches(candidate) {
                if glob.is_negated() {
                    return Match::Deny;
                }
                allowed = true;
            }
        }
        if allowed {
            Match::Allow
        } else {
            Match::Unmatched
        }
    }

    /// Convenience: returns `true` only when [`Self::evaluate`] is [`Match::Allow`].
    ///
    /// An unmatched candidate is *not* allowed.
    #[must_use]
    pub fn is_allowed(&self, candidate: &str) -> bool {
        self.evaluate(candidate) == Match::Allow
    }

    /// Returns the globs in the set.
    #[must_use]
    pub fn globs(&self) -> &[Glob] {
        &self.globs
    }
}
