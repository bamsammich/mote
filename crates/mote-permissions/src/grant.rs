//! Effective grant sets — the narrowed permission scope a plugin actually holds.
//!
//! A plugin's *requested* permissions are what its manifest declares.  The user
//! may narrow any resource pattern at install time; the resulting *effective*
//! grants are what the runtime enforces.
//!
//! # Narrowing
//!
//! Per DESIGN §User narrowing at install time:
//!
//! > A plugin manifest may declare `page:inject_script:*` but the user can
//! > grant something narrower.  Narrowing is not denial — the plugin **loads**
//! > with the narrower grant.
//!
//! Narrowing replaces the requested resource pattern with a union of narrower
//! patterns.  For example, a request for `page:inject_script:*` narrowed to
//! `["github.com/*", "gitlab.com/*"]` gives the plugin exactly those two
//! effective scopes.
//!
//! # Data model
//!
//! [`GrantSet`] is indexed by `(domain, action)` pairs.  Each pair maps to a
//! [`GlobSet`] of effective resource patterns (which may include deny globs).
//! [`EffectiveGrants`] wraps a [`GrantSet`] and exposes the high-level query
//! API used by the [`crate::Gatekeeper`].

use std::collections::HashMap;

use mote_types::{GlobParseError, GlobSet, Match};

use crate::permission::Permission;

/// The key into a [`GrantSet`]: a `(domain, action)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GrantKey {
    domain: String,
    action: String,
}

impl GrantKey {
    fn new(domain: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            action: action.into(),
        }
    }
}

/// A plugin's effective permission grants, indexed by `(domain, action)`.
///
/// Each `(domain, action)` pair maps to a [`GlobSet`] of resource patterns.
/// Evaluating a resource string against that set uses **deny precedence**:
/// a matching `!`-negated pattern beats any allow match.
///
/// Build with [`GrantSet::from_permissions`] or [`GrantSet::builder`].
#[derive(Debug, Clone)]
pub struct GrantSet {
    /// Maps `(domain, action)` → set of resource globs.
    grants: HashMap<GrantKey, GlobSet>,
}

impl GrantSet {
    /// Constructs an empty [`GrantSet`] (denies everything).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            grants: HashMap::new(),
        }
    }

    /// Constructs a [`GrantSet`] from a slice of [`Permission`]s.
    ///
    /// Permissions with the same `(domain, action)` are merged into a single
    /// [`GlobSet`]; their resource globs are combined.  A permission without an
    /// explicit resource is treated as `*` (allow all resources for that pair).
    ///
    /// ## Exact-match guarantee for dynamic resources
    ///
    /// `dynamic`-shaped permissions (`mcp:client:<name>`, `secret:read:<name>`)
    /// are validated by `mote-registry` step 1 to contain **no glob
    /// metacharacters** before they can reach this function.  A resource string
    /// containing only `[A-Za-z0-9_.:-]` characters (the enforced charset) is
    /// treated by the underlying [`GlobSet`] as a literal — it matches exactly
    /// one string (itself).  No additional exact-match storage path is needed;
    /// the glob semantics degenerate to equality for metacharacter-free patterns.
    ///
    /// # Errors
    ///
    /// Returns [`GlobParseError`] if the implicit `*` resource cannot be parsed
    /// (this is a programming error; `*` is always valid).
    pub fn from_permissions(perms: &[Permission]) -> Result<Self, GlobParseError> {
        let mut builder = GrantSetBuilder::default();
        for perm in perms {
            let resource = perm
                .resource()
                .map_or_else(|| "*".to_owned(), ToString::to_string);
            builder.add(perm.domain(), perm.action(), &resource)?;
        }
        Ok(builder.build())
    }

    /// Returns a [`GrantSetBuilder`] for incremental construction.
    #[must_use]
    pub fn builder() -> GrantSetBuilder {
        GrantSetBuilder::default()
    }

    /// Evaluates whether `resource` is permitted under `(domain, action)`.
    ///
    /// Returns [`Match::Unmatched`] when no grant exists for the pair.
    #[must_use]
    pub fn evaluate(&self, domain: &str, action: &str, resource: &str) -> Match {
        let key = GrantKey::new(domain, action);
        self.grants
            .get(&key)
            .map_or(Match::Unmatched, |glob_set| glob_set.evaluate(resource))
    }

    /// Returns all `(domain, action)` pairs that have at least one grant.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.grants
            .keys()
            .map(|k| (k.domain.as_str(), k.action.as_str()))
    }

    /// Returns the [`GlobSet`] for `(domain, action)`, if any.
    #[must_use]
    pub fn get(&self, domain: &str, action: &str) -> Option<&GlobSet> {
        let key = GrantKey::new(domain, action);
        self.grants.get(&key)
    }

    /// Produces a new [`GrantSet`] with `(domain, action)` narrowed to
    /// `narrowed_resources`.
    ///
    /// Narrowing replaces the existing resource globs for that pair with the
    /// union of the provided patterns.  Other pairs are unchanged.
    ///
    /// This is the operation the install dialog performs when the user selects
    /// "grant on specific origins" instead of the wildcard default.
    ///
    /// # Errors
    ///
    /// Returns [`GlobParseError`] if any pattern in `narrowed_resources` is
    /// syntactically invalid.
    pub fn narrow<I, S>(
        &self,
        domain: &str,
        action: &str,
        narrowed_resources: I,
    ) -> Result<Self, GlobParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut new_grants = self.grants.clone();
        let key = GrantKey::new(domain, action);
        let glob_set = GlobSet::parse(narrowed_resources)?;
        new_grants.insert(key, glob_set);
        Ok(Self { grants: new_grants })
    }
}

/// Incremental builder for [`GrantSet`].
///
/// Accumulated patterns for the same `(domain, action)` pair are merged.
#[derive(Debug, Default)]
pub struct GrantSetBuilder {
    /// Raw patterns per key, accumulated before building.
    raw: HashMap<GrantKey, Vec<String>>,
}

impl GrantSetBuilder {
    /// Adds a resource pattern for `(domain, action)`.
    ///
    /// # Errors
    ///
    /// Returns [`GlobParseError`] eagerly if the pattern is invalid.
    pub fn add(
        &mut self,
        domain: &str,
        action: &str,
        resource_pattern: &str,
    ) -> Result<&mut Self, GlobParseError> {
        // Validate by parsing; discard the result — we re-parse at build time
        // to consolidate into a GlobSet.
        resource_pattern.parse::<mote_types::Glob>()?;
        self.raw
            .entry(GrantKey::new(domain, action))
            .or_default()
            .push(resource_pattern.to_owned());
        Ok(self)
    }

    /// Consumes the builder and produces a [`GrantSet`].
    ///
    /// # Panics
    ///
    /// Panics if any previously-validated pattern fails to re-parse.  This
    /// cannot happen in practice because [`Self::add`] validates eagerly.
    #[must_use]
    pub fn build(self) -> GrantSet {
        let grants = self
            .raw
            .into_iter()
            .map(|(key, patterns)| {
                let glob_set = GlobSet::parse(&patterns)
                    .expect("pattern was validated at add time; re-parse cannot fail");
                (key, glob_set)
            })
            .collect();
        GrantSet { grants }
    }
}

/// A plugin's complete effective permission state, as returned by
/// `permissions.effective()` (DESIGN §User narrowing at install time).
///
/// This is the data structure the runtime hands to a plugin at `setup()` time
/// and that the integrity panel reads for display.  It carries the full
/// [`GrantSet`] plus a flat list of effective permission strings for plugins
/// that call `permissions.effective()`.
#[derive(Debug, Clone)]
pub struct EffectiveGrants {
    grant_set: GrantSet,
    /// Flat list in `domain:action:resource` form, for the Lua API surface.
    strings: Vec<String>,
}

impl EffectiveGrants {
    /// Constructs [`EffectiveGrants`] from a set of effective [`Permission`]s.
    ///
    /// The `strings` list mirrors the permissions in their canonical display
    /// form (what `permissions.effective()` returns to a plugin).
    ///
    /// # Errors
    ///
    /// Returns [`GlobParseError`] if building the underlying [`GrantSet`]
    /// fails.
    pub fn from_permissions(perms: &[Permission]) -> Result<Self, GlobParseError> {
        let grant_set = GrantSet::from_permissions(perms)?;
        let strings = perms.iter().map(ToString::to_string).collect();
        Ok(Self { grant_set, strings })
    }

    /// Returns the underlying [`GrantSet`].
    #[must_use]
    pub const fn grant_set(&self) -> &GrantSet {
        &self.grant_set
    }

    /// Returns the flat string list of effective permissions.
    ///
    /// This is the value exposed to Lua plugins via `permissions.effective()`.
    #[must_use]
    pub fn as_strings(&self) -> &[String] {
        &self.strings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mote_types::Match;

    // Helper: parse a permission string, panic on error.
    fn perm(s: &str) -> Permission {
        s.parse().unwrap()
    }

    // --- GrantSet::from_permissions -----------------------------------------

    #[test]
    fn grant_set_no_resource_means_wildcard() {
        let grants = GrantSet::from_permissions(&[perm("net:intercept_request")]).unwrap();
        // Implicit wildcard — any resource matches
        assert_eq!(
            grants.evaluate("net", "intercept_request", "https://example.com/"),
            Match::Allow
        );
        assert_eq!(
            grants.evaluate("net", "intercept_request", "anything"),
            Match::Allow
        );
    }

    #[test]
    fn grant_set_explicit_wildcard() {
        let grants = GrantSet::from_permissions(&[perm("page:inject_script:*")]).unwrap();
        assert_eq!(
            grants.evaluate("page", "inject_script", "https://example.com/"),
            Match::Allow
        );
    }

    #[test]
    fn grant_set_specific_resource() {
        let grants =
            GrantSet::from_permissions(&[perm("page:inject_script:https://*.1password.com/*")])
                .unwrap();
        assert_eq!(
            grants.evaluate(
                "page",
                "inject_script",
                "https://subdomain.1password.com/path"
            ),
            Match::Allow
        );
        assert_eq!(
            grants.evaluate("page", "inject_script", "https://evil.com/"),
            Match::Unmatched
        );
    }

    #[test]
    fn grant_set_deny_beats_allow() {
        // A set that allows everything BUT *.banking.com hosts.
        // The pattern `*.banking.com` matches host strings like
        // `secure.banking.com`; the runtime passes the host (not the full URL)
        // to the gatekeeper for `net:intercept_request`.
        let grants = GrantSet::from_permissions(&[
            perm("net:intercept_request:*"),
            perm("net:intercept_request:!*.banking.com"),
        ])
        .unwrap();
        // Wildcard allow applies to non-banking hosts
        assert_eq!(
            grants.evaluate("net", "intercept_request", "example.com"),
            Match::Allow
        );
        // Deny beats the wildcard allow for a banking host
        assert_eq!(
            grants.evaluate("net", "intercept_request", "secure.banking.com"),
            Match::Deny
        );
    }

    #[test]
    fn grant_set_unknown_pair_is_unmatched() {
        let grants = GrantSet::from_permissions(&[perm("net:intercept_request:*")]).unwrap();
        assert_eq!(
            grants.evaluate("page", "inject_script", "https://example.com/"),
            Match::Unmatched
        );
    }

    #[test]
    fn grant_set_multiple_pairs() {
        let grants = GrantSet::from_permissions(&[
            perm("net:intercept_request"),
            // Pattern for *.github.com — matches subdomains like gist.github.com
            perm("page:inject_script:https://*.github.com/*"),
        ])
        .unwrap();
        assert_eq!(
            grants.evaluate("net", "intercept_request", "anything"),
            Match::Allow
        );
        // The pattern `https://*.github.com/*` matches `https://gist.github.com/user/repo`
        // because `*.github.com` matches `gist.github.com`.
        assert_eq!(
            grants.evaluate("page", "inject_script", "https://gist.github.com/user/repo"),
            Match::Allow
        );
        assert_eq!(
            grants.evaluate("page", "inject_script", "https://evil.com/"),
            Match::Unmatched
        );
    }

    // --- narrowing ----------------------------------------------------------

    #[test]
    fn narrow_replaces_wildcard_with_specific_origins() {
        let requested = GrantSet::from_permissions(&[perm("page:inject_script:*")]).unwrap();

        // User narrows to two specific origins
        let effective = requested
            .narrow("page", "inject_script", ["github.com/*", "gitlab.com/*"])
            .unwrap();

        assert_eq!(
            effective.evaluate("page", "inject_script", "github.com/foo"),
            Match::Allow
        );
        assert_eq!(
            effective.evaluate("page", "inject_script", "gitlab.com/bar"),
            Match::Allow
        );
        // Wildcard no longer applies
        assert_eq!(
            effective.evaluate("page", "inject_script", "evil.com/pwned"),
            Match::Unmatched
        );
    }

    #[test]
    fn narrow_leaves_other_pairs_unchanged() {
        let requested = GrantSet::from_permissions(&[
            perm("page:inject_script:*"),
            perm("net:intercept_request"),
        ])
        .unwrap();

        let effective = requested
            .narrow("page", "inject_script", ["github.com/*"])
            .unwrap();

        // Narrowed pair is updated
        assert_eq!(
            effective.evaluate("page", "inject_script", "github.com/foo"),
            Match::Allow
        );
        assert_eq!(
            effective.evaluate("page", "inject_script", "evil.com/"),
            Match::Unmatched
        );

        // Untouched pair is unchanged
        assert_eq!(
            effective.evaluate("net", "intercept_request", "anything"),
            Match::Allow
        );
    }

    #[test]
    fn narrow_invalid_pattern_returns_error() {
        let set = GrantSet::from_permissions(&[perm("page:inject_script:*")]).unwrap();
        // Empty pattern is invalid
        assert!(set.narrow("page", "inject_script", [""]).is_err());
    }

    // --- EffectiveGrants ---------------------------------------------------

    #[test]
    fn effective_grants_strings_match_permissions() {
        let perms = vec![
            perm("net:intercept_request:*"),
            perm("page:inject_script:https://*.1password.com/*"),
        ];
        let eff = EffectiveGrants::from_permissions(&perms).unwrap();
        let strings = eff.as_strings();
        assert!(strings.contains(&"net:intercept_request:*".to_owned()));
        assert!(strings.contains(&"page:inject_script:https://*.1password.com/*".to_owned()));
    }

    #[test]
    fn effective_grants_grant_set_evaluates_correctly() {
        let perms = vec![perm("http:fetch:https://api.bitwarden.com/*")];
        let eff = EffectiveGrants::from_permissions(&perms).unwrap();
        assert_eq!(
            eff.grant_set()
                .evaluate("http", "fetch", "https://api.bitwarden.com/v1/sync"),
            Match::Allow
        );
        assert_eq!(
            eff.grant_set()
                .evaluate("http", "fetch", "https://attacker.com/exfil"),
            Match::Unmatched
        );
    }

    // --- GrantSetBuilder ----------------------------------------------------

    #[test]
    fn builder_accumulates_same_pair() {
        let mut b = GrantSet::builder();
        b.add("page", "inject_script", "github.com/*").unwrap();
        b.add("page", "inject_script", "gitlab.com/*").unwrap();
        let set = b.build();

        assert_eq!(
            set.evaluate("page", "inject_script", "github.com/foo"),
            Match::Allow
        );
        assert_eq!(
            set.evaluate("page", "inject_script", "gitlab.com/bar"),
            Match::Allow
        );
        assert_eq!(
            set.evaluate("page", "inject_script", "evil.com/"),
            Match::Unmatched
        );
    }

    #[test]
    fn builder_invalid_pattern_returns_error() {
        let mut b = GrantSet::builder();
        assert!(b.add("page", "inject_script", "").is_err());
    }
}
