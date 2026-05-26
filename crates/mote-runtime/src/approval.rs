//! Permission approval (load-step 4) and the re-approval hash (hot reload).
//!
//! Step 4 of the load pipeline shows the user the requested permission set and
//! lets them approve, narrow, or deny (DESIGN §Enforcement Rules step 4;
//! §User narrowing at install time). The real UI approval is Phase 2; the
//! runtime takes an injected [`ApprovalPolicy`] so the pipeline is exercisable
//! end-to-end now and the production UI plugs in later.
//!
//! Hot reload recomputes a **re-approval hash** over exactly
//! `{permissions, capabilities, consumes, identity_scope}` (ADR-0001,
//! ADR-0002; DESIGN §Hot Reload). A code-only change leaves the hash unchanged
//! → no re-approval. Any change to those four fields changes the hash →
//! re-approval is required.

use mote_lua::{IdentityScope, Manifest};
use mote_permissions::Permission;
use mote_types::Checksum;

/// The outcome of an [`ApprovalPolicy`] decision for one plugin's requested
/// permissions.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Approval {
    /// Approve the requested permissions exactly as declared.
    GrantAsRequested,
    /// Approve, but narrow specific `(domain, action)` pairs to the given
    /// resource patterns (DESIGN §User narrowing at install time). Pairs not
    /// listed are granted as requested.
    Narrow {
        /// Each entry replaces the effective resource patterns for one
        /// `(domain, action)` pair with the listed globs.
        narrowings: Vec<Narrowing>,
    },
    /// Deny the plugin; the load fails at step 4.
    Deny {
        /// Human-readable reason surfaced to the user / audit log.
        reason: String,
    },
}

/// One narrowing: restrict a `(domain, action)` pair to a set of resource
/// patterns.
#[derive(Debug, Clone)]
pub struct Narrowing {
    /// The permission domain (e.g. `page`).
    pub domain: String,
    /// The permission action (e.g. `inject_script`).
    pub action: String,
    /// The user-chosen narrower resource patterns (globs).
    pub resources: Vec<String>,
}

/// The decision seam for load-step 4 — permission approval.
///
/// The runtime calls [`ApprovalPolicy::decide`] with the plugin's requested
/// permissions. Implementors return an [`Approval`]. The production
/// implementation (Phase 2) drives the install dialog; tests use
/// [`GrantAsRequested`] or a narrowing/denying policy.
pub trait ApprovalPolicy {
    /// Decides how to approve `requested` for the plugin named `plugin`.
    fn decide(&self, plugin: &str, requested: &[Permission]) -> Approval;
}

/// An [`ApprovalPolicy`] that always grants exactly what was requested.
///
/// The simplest policy and the default for tests proving the happy path: the
/// effective set equals the requested set, with no narrowing or denial.
#[derive(Debug, Clone, Copy, Default)]
pub struct GrantAsRequested;

impl ApprovalPolicy for GrantAsRequested {
    fn decide(&self, _plugin: &str, _requested: &[Permission]) -> Approval {
        Approval::GrantAsRequested
    }
}

/// The four manifest fields whose change triggers re-approval on hot reload
/// (ADR-0001/0002; DESIGN §Hot Reload).
///
/// Captured as a canonicalized, order-independent fingerprint so that
/// reordering a manifest list does not spuriously force re-approval, while any
/// genuine expansion (or contraction) of the four fields does change it.
///
/// Note that **contraction** also changes the hash. DESIGN says a *non-expanding*
/// manifest change is approved silently by intersecting the prior grant with the
/// new request; only an *expansion* forces a prompt. The runtime therefore does
/// not gate on hash-equality alone — it compares the field sets directionally
/// (see [`ApprovalHash::is_expansion_of`]). The hash is the cheap equality
/// fast-path for the common code-only reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalHash {
    permissions: Vec<String>,
    capabilities: Vec<String>,
    consumes: Vec<String>,
    identity_scope: Option<IdentityScope>,
}

impl ApprovalHash {
    /// Computes the re-approval fingerprint from a manifest.
    ///
    /// Lists are sorted and de-duplicated so the fingerprint is independent of
    /// declaration order and of accidental repeats.
    #[must_use]
    pub fn of(manifest: &Manifest) -> Self {
        Self {
            permissions: canonical(&manifest.permissions),
            capabilities: canonical(&manifest.capabilities),
            consumes: canonical(&manifest.consumes),
            identity_scope: manifest.identity_scope,
        }
    }

    /// A content checksum of the fingerprint (BLAKE3), for compact storage and
    /// for the integrity panel's "approved configuration" display.
    #[must_use]
    pub fn checksum(&self) -> Checksum {
        let mut bytes = Vec::new();
        for section in [&self.permissions, &self.capabilities, &self.consumes] {
            for term in section {
                bytes.extend_from_slice(term.as_bytes());
                bytes.push(0);
            }
            bytes.push(1);
        }
        if let Some(scope) = self.identity_scope {
            bytes.extend_from_slice(format!("{scope:?}").as_bytes());
        }
        Checksum::hash(&bytes)
    }

    /// Whether `self` expands the approval surface beyond `prior`.
    ///
    /// Returns `true` if `self` requests any permission, capability, or consumed
    /// capability that `prior` did not, or changes `identity_scope`. This is the
    /// exact re-approval trigger from DESIGN §Hot Reload: expansion of
    /// `permissions`, `capabilities`, `consumes`, or `identity_scope`. A pure
    /// contraction (or no change) returns `false` — the reload proceeds without
    /// a prompt, intersecting the prior grant with the new request.
    #[must_use]
    pub fn is_expansion_of(&self, prior: &Self) -> bool {
        let added = |new: &[String], old: &[String]| new.iter().any(|t| !old.contains(t));
        if added(&self.permissions, &prior.permissions)
            || added(&self.capabilities, &prior.capabilities)
            || added(&self.consumes, &prior.consumes)
        {
            return true;
        }
        // Any identity_scope change is treated as approval-relevant per DESIGN.
        self.identity_scope != prior.identity_scope
    }
}

/// Sorts and de-duplicates a term list for an order-independent fingerprint.
fn canonical(terms: &[String]) -> Vec<String> {
    let mut out = terms.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mote_lua::load_plugin;

    fn manifest(src: &str) -> Manifest {
        load_plugin(src, "test").unwrap().manifest().clone()
    }

    const BASE: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "p",
            version = "1",
            permissions = { "storage:persistent", "tabs:list" },
            identity_scope = "global",
        }
        return M
    "#;

    #[test]
    fn code_only_change_keeps_hash() {
        let a = ApprovalHash::of(&manifest(BASE));
        let b = ApprovalHash::of(&manifest(BASE));
        assert_eq!(a, b);
        assert!(!b.is_expansion_of(&a));
    }

    #[test]
    fn reordering_permissions_keeps_hash() {
        let reordered = r#"
            local M = {}
            M.manifest = {
                schema = "v1", name = "p", version = "1",
                permissions = { "tabs:list", "storage:persistent" },
                identity_scope = "global",
            }
            return M
        "#;
        assert_eq!(
            ApprovalHash::of(&manifest(BASE)),
            ApprovalHash::of(&manifest(reordered))
        );
    }

    #[test]
    fn adding_a_permission_is_expansion() {
        let expanded = r#"
            local M = {}
            M.manifest = {
                schema = "v1", name = "p", version = "1",
                permissions = { "storage:persistent", "tabs:list", "history:read" },
                identity_scope = "global",
            }
            return M
        "#;
        let prior = ApprovalHash::of(&manifest(BASE));
        let new = ApprovalHash::of(&manifest(expanded));
        assert_ne!(prior, new);
        assert!(new.is_expansion_of(&prior));
    }

    #[test]
    fn removing_a_permission_is_not_expansion() {
        let contracted = r#"
            local M = {}
            M.manifest = {
                schema = "v1", name = "p", version = "1",
                permissions = { "storage:persistent" },
                identity_scope = "global",
            }
            return M
        "#;
        let prior = ApprovalHash::of(&manifest(BASE));
        let new = ApprovalHash::of(&manifest(contracted));
        assert_ne!(prior, new);
        assert!(!new.is_expansion_of(&prior));
    }

    #[test]
    fn changing_identity_scope_is_expansion() {
        let scoped = r#"
            local M = {}
            M.manifest = {
                schema = "v1", name = "p", version = "1",
                permissions = { "storage:persistent", "tabs:list" },
                identity_scope = "per_identity",
            }
            return M
        "#;
        let prior = ApprovalHash::of(&manifest(BASE));
        let new = ApprovalHash::of(&manifest(scoped));
        assert!(new.is_expansion_of(&prior));
    }
}
