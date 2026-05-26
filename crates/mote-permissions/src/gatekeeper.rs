//! The [`Gatekeeper`] trait — the enforcement seam between the runtime and the
//! permission grant set.
//!
//! The runtime (and eventually `mote-dispatch`) calls [`Gatekeeper::check`]
//! before every privileged plugin operation.  The trait is the abstraction
//! boundary; [`GrantSetGatekeeper`] is the in-memory implementation backed by
//! a [`GrantSet`].

use mote_types::Match;

use crate::grant::GrantSet;

/// The outcome of a permission check.
///
/// The runtime treats [`Decision::Deny`] and [`Decision::Unmatched`] both as
/// "not permitted," but distinguishes them so the audit log can record the
/// precise reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    /// The plugin holds an effective grant that allows this `(domain, action,
    /// resource)` triple and no deny pattern matched.
    Allow,
    /// A deny (`!`-negated) pattern matched, overriding any allow grant.
    Deny,
    /// No grant exists for this `(domain, action)` pair at all, or no resource
    /// pattern matched.
    Unmatched,
}

impl Decision {
    /// Returns `true` only for [`Decision::Allow`].
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl From<Match> for Decision {
    fn from(m: Match) -> Self {
        match m {
            Match::Allow => Self::Allow,
            Match::Deny => Self::Deny,
            Match::Unmatched => Self::Unmatched,
        }
    }
}

/// The enforcement seam the runtime queries for every privileged plugin action.
///
/// Implementors are expected to be cheap to clone (or `Arc`-wrapped) so they
/// can be handed to the dispatch layer without blocking the plugin runtime.
///
/// # Contract
///
/// - `Deny` beats `Allow` (deny-precedence from the [`GlobSet`] evaluation).
/// - `Unmatched` is not allowed; the runtime must treat it as a denial.
/// - The check is **synchronous and allocation-free** on the hot path; no
///   async, no locking in the default implementation.
pub trait Gatekeeper: Send + Sync + std::fmt::Debug {
    /// Checks whether the operation `domain:action:resource` is permitted.
    fn check(&self, domain: &str, action: &str, resource: &str) -> Decision;
}

/// A [`Gatekeeper`] backed by an in-memory [`GrantSet`].
///
/// This is the production implementation used at plugin `setup()` time and
/// throughout the plugin's lifetime.  It holds the effective grant set for
/// **one plugin**; each plugin gets its own [`GrantSetGatekeeper`].
#[derive(Debug, Clone)]
pub struct GrantSetGatekeeper {
    grants: GrantSet,
}

impl GrantSetGatekeeper {
    /// Constructs a gatekeeper over the given effective grant set.
    #[must_use]
    pub const fn new(grants: GrantSet) -> Self {
        Self { grants }
    }

    /// Returns a reference to the underlying [`GrantSet`].
    #[must_use]
    pub const fn grants(&self) -> &GrantSet {
        &self.grants
    }
}

impl Gatekeeper for GrantSetGatekeeper {
    fn check(&self, domain: &str, action: &str, resource: &str) -> Decision {
        Decision::from(self.grants.evaluate(domain, action, resource))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::Permission;

    fn perm(s: &str) -> Permission {
        s.parse().unwrap()
    }

    fn gk(perms: &[Permission]) -> GrantSetGatekeeper {
        let gs = GrantSet::from_permissions(perms).unwrap();
        GrantSetGatekeeper::new(gs)
    }

    // --- basic allow / unmatched --------------------------------------------

    #[test]
    fn allow_when_wildcard_grant_exists() {
        let g = gk(&[perm("net:intercept_request")]);
        assert_eq!(
            g.check("net", "intercept_request", "https://example.com/"),
            Decision::Allow
        );
    }

    #[test]
    fn allow_when_specific_resource_matches() {
        let g = gk(&[perm("page:inject_script:https://*.github.com/*")]);
        assert_eq!(
            g.check("page", "inject_script", "https://gist.github.com/user/abc"),
            Decision::Allow
        );
    }

    #[test]
    fn unmatched_when_no_grant_for_domain_action() {
        let g = gk(&[perm("net:intercept_request")]);
        assert_eq!(
            g.check("page", "inject_script", "https://example.com/"),
            Decision::Unmatched
        );
    }

    #[test]
    fn unmatched_when_resource_doesnt_match_specific_pattern() {
        let g = gk(&[perm("page:inject_script:https://*.github.com/*")]);
        assert_eq!(
            g.check("page", "inject_script", "https://evil.com/"),
            Decision::Unmatched
        );
    }

    // --- deny-precedence ----------------------------------------------------

    #[test]
    fn deny_beats_wildcard_allow() {
        // net:intercept_request:* plus deny for *.banking.com hosts.
        // The runtime passes the *host* string to the gatekeeper for
        // `net:intercept_request`; `*.banking.com` matches `secure.banking.com`
        // but NOT a full URL-with-path like `https://secure.banking.com/login`.
        let g = gk(&[
            perm("net:intercept_request:*"),
            perm("net:intercept_request:!*.banking.com"),
        ]);
        // Wildcard allow applies to non-banking hosts
        assert_eq!(
            g.check("net", "intercept_request", "example.com"),
            Decision::Allow
        );
        // Deny beats the wildcard allow for banking subdomains
        assert_eq!(
            g.check("net", "intercept_request", "secure.banking.com"),
            Decision::Deny
        );
    }

    #[test]
    fn deny_beats_specific_allow_when_both_match() {
        // Give explicit allow for an API URL pattern, then also deny that same pattern.
        // The deny takes precedence even though the allow pattern also matches.
        let g = gk(&[
            perm("http:fetch:https://api.bank.com/*"),
            perm("http:fetch:!https://api.bank.com/*"),
        ]);
        assert_eq!(
            g.check("http", "fetch", "https://api.bank.com/v1/accounts"),
            Decision::Deny
        );
    }

    #[test]
    fn deny_only_grant_is_deny_not_unmatched() {
        // A lone deny with no matching allow should still be Deny (not Unmatched)
        // when the resource matches the deny pattern.
        // The pattern `!*.banking.com` matches hosts like `secure.banking.com`.
        let g = gk(&[perm("net:intercept_request:!*.banking.com")]);
        assert_eq!(
            g.check("net", "intercept_request", "secure.banking.com"),
            Decision::Deny
        );
        // A resource that does not match the deny pattern → Unmatched (no allow)
        assert_eq!(
            g.check("net", "intercept_request", "example.com"),
            Match::Unmatched.into()
        );
    }

    // --- is_allowed convenience ---------------------------------------------

    #[test]
    fn is_allowed_returns_true_only_for_allow() {
        let g = gk(&[perm("net:intercept_request:*")]);
        assert!(
            g.check("net", "intercept_request", "https://example.com/")
                .is_allowed()
        );
        assert!(!g.check("page", "inject_script", "anything").is_allowed());
    }

    // --- trait object -------------------------------------------------------

    #[test]
    fn gatekeeper_usable_as_trait_object() {
        let g: Box<dyn Gatekeeper> = Box::new(gk(&[perm("storage:persistent")]));
        // resource is ignored for a no-resource permission (uses implicit `*`)
        assert_eq!(
            g.check("storage", "persistent", "anything"),
            Decision::Allow
        );
        assert_eq!(
            g.check("net", "intercept_request", "anything"),
            Decision::Unmatched
        );
    }

    // --- narrowed gatekeeper -----------------------------------------------

    #[test]
    fn gatekeeper_after_narrowing() {
        let gs = GrantSet::from_permissions(&[perm("page:inject_script:*")]).unwrap();
        let narrowed = gs
            .narrow("page", "inject_script", ["github.com/*", "linear.app/*"])
            .unwrap();
        let g = GrantSetGatekeeper::new(narrowed);

        assert_eq!(
            g.check("page", "inject_script", "github.com/user/repo"),
            Decision::Allow
        );
        assert_eq!(
            g.check("page", "inject_script", "linear.app/workspace"),
            Decision::Allow
        );
        assert_eq!(
            g.check("page", "inject_script", "evil.com/pwned"),
            Decision::Unmatched
        );
    }

    // --- Decision helpers ---------------------------------------------------

    #[test]
    fn decision_is_allowed() {
        assert!(Decision::Allow.is_allowed());
        assert!(!Decision::Deny.is_allowed());
        assert!(!Decision::Unmatched.is_allowed());
    }

    #[test]
    fn decision_from_match() {
        assert_eq!(Decision::from(Match::Allow), Decision::Allow);
        assert_eq!(Decision::from(Match::Deny), Decision::Deny);
        assert_eq!(Decision::from(Match::Unmatched), Decision::Unmatched);
    }
}
