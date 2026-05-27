//! Permission-set diff between an approved [`ApprovalHash`] and a candidate.
//!
//! When a plugin is updated or an installed plugin's manifest is re-checked,
//! the plugin manager computes a [`DiffReport`] capturing exactly what changed
//! across the four approval-relevant fields (`permissions`, `capabilities`,
//! `consumes`, `identity_scope`). The diff is what the approval dialog renders
//! and what `mote plugin diff` prints headlessly (DISCIPLINES §9; plan §6.3).
//!
//! ## Expansion semantics
//!
//! A report [`DiffReport::is_expansion`] when any permission, capability, or
//! consumed capability was **added** relative to the prior approved hash, or
//! when `identity_scope` changed. This must agree with
//! [`ApprovalHash::is_expansion_of`] — a property asserted by the tests in
//! this module.

use mote_lua::IdentityScope;
use mote_runtime::ApprovalHash;

// ---------------------------------------------------------------------------
// Delta types
// ---------------------------------------------------------------------------

/// The direction of a single-term change in a field list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaKind {
    /// The term was added in the candidate (not present in the prior hash).
    Added,
    /// The term was removed from the candidate (present in the prior hash but
    /// not in the candidate).
    Removed,
}

/// A single added or removed term in a field list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    /// The string value of the changed term (e.g. `"storage:persistent"`).
    pub term: String,
    /// Whether the term was added or removed.
    pub kind: DeltaKind,
}

impl Delta {
    fn added(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            kind: DeltaKind::Added,
        }
    }

    fn removed(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            kind: DeltaKind::Removed,
        }
    }
}

// ---------------------------------------------------------------------------
// DiffReport
// ---------------------------------------------------------------------------

/// The full delta between a prior approved [`ApprovalHash`] and a candidate.
///
/// Produced by [`diff`]. Contains one [`Delta`] list per approval-relevant
/// field, plus an optional `identity_scope_change`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    /// Permissions that were added or removed.
    pub permission_changes: Vec<Delta>,
    /// Capabilities that were added or removed.
    pub capability_changes: Vec<Delta>,
    /// Consumed capabilities that were added or removed.
    pub consumes_changes: Vec<Delta>,
    /// `Some((old, new))` when `identity_scope` changed; `None` when it did
    /// not change.
    pub identity_scope_change: Option<(Option<IdentityScope>, Option<IdentityScope>)>,
}

impl DiffReport {
    /// Returns `true` when the candidate **expands** the approval surface
    /// beyond what was previously approved.
    ///
    /// An expansion is any added permission, capability, or consumed
    /// capability, or any change to `identity_scope`. This must agree with
    /// [`ApprovalHash::is_expansion_of`] for the same pair of hashes —
    /// the tests assert this invariant.
    #[must_use]
    pub fn is_expansion(&self) -> bool {
        let has_addition =
            |deltas: &[Delta]| deltas.iter().any(|d| matches!(d.kind, DeltaKind::Added));
        has_addition(&self.permission_changes)
            || has_addition(&self.capability_changes)
            || has_addition(&self.consumes_changes)
            || self.identity_scope_change.is_some()
    }
}

// ---------------------------------------------------------------------------
// diff function
// ---------------------------------------------------------------------------

/// Computes the [`DiffReport`] from `prior` (last-approved) to `candidate`.
///
/// All four approval-relevant fields are compared:
/// - `permissions`, `capabilities`, `consumes`: terms added in `candidate`
///   but absent in `prior` appear as [`DeltaKind::Added`]; terms present in
///   `prior` but absent in `candidate` appear as [`DeltaKind::Removed`].
/// - `identity_scope`: emitted as `Some((old, new))` when the value changed.
///
/// The returned deltas are in the natural order they appear in the
/// canonicalized (sorted) field lists.
#[must_use]
pub fn diff(prior: &ApprovalHash, candidate: &ApprovalHash) -> DiffReport {
    DiffReport {
        permission_changes: field_diff(prior.permissions(), candidate.permissions()),
        capability_changes: field_diff(prior.capabilities(), candidate.capabilities()),
        consumes_changes: field_diff(prior.consumes(), candidate.consumes()),
        identity_scope_change: scope_diff(prior.identity_scope(), candidate.identity_scope()),
    }
}

/// Computes the per-field delta between two sorted, de-duplicated term slices.
fn field_diff(prior: &[String], candidate: &[String]) -> Vec<Delta> {
    let mut out = Vec::new();
    for t in candidate {
        if !prior.contains(t) {
            out.push(Delta::added(t.clone()));
        }
    }
    for t in prior {
        if !candidate.contains(t) {
            out.push(Delta::removed(t.clone()));
        }
    }
    // Sort for deterministic output: Added terms first, then Removed, each
    // group sorted lexicographically by term.
    out.sort_by(|a, b| {
        let kind_order = |d: &Delta| match d.kind {
            DeltaKind::Added => 0_u8,
            DeltaKind::Removed => 1_u8,
        };
        kind_order(a)
            .cmp(&kind_order(b))
            .then_with(|| a.term.cmp(&b.term))
    });
    out
}

/// Returns `Some((old, new))` when the identity scope changed.
fn scope_diff(
    prior: Option<IdentityScope>,
    candidate: Option<IdentityScope>,
) -> Option<(Option<IdentityScope>, Option<IdentityScope>)> {
    if prior == candidate {
        None
    } else {
        Some((prior, candidate))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use mote_lua::{IdentityScope as LuaScope, load_plugin};
    use mote_runtime::ApprovalHash;

    use super::*;

    fn hash_from_src(src: &str) -> ApprovalHash {
        let manifest = load_plugin(src, "test").unwrap().manifest().clone();
        ApprovalHash::of(&manifest)
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

    const PERM_ADDED: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "p",
            version = "2",
            permissions = { "storage:persistent", "tabs:list", "history:read" },
            identity_scope = "global",
        }
        return M
    "#;

    const PERM_REMOVED: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "p",
            version = "2",
            permissions = { "storage:persistent" },
            identity_scope = "global",
        }
        return M
    "#;

    const CAP_ADDED: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "p",
            version = "2",
            permissions = { "storage:persistent", "tabs:list" },
            capabilities = { "password-manager:fill" },
            identity_scope = "global",
        }
        return M
    "#;

    const CONSUMES_ADDED: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "p",
            version = "2",
            permissions = { "storage:persistent", "tabs:list" },
            consumes = { "password-manager:fill" },
            identity_scope = "global",
        }
        return M
    "#;

    const SCOPE_CHANGED: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "p",
            version = "2",
            permissions = { "storage:persistent", "tabs:list" },
            identity_scope = "per_identity",
        }
        return M
    "#;

    const NO_CHANGE: &str = BASE; // same four fields

    // -----------------------------------------------------------------------
    // Expansion cases
    // -----------------------------------------------------------------------

    #[test]
    fn added_permission_is_expansion() {
        let prior = hash_from_src(BASE);
        let candidate = hash_from_src(PERM_ADDED);
        let report = diff(&prior, &candidate);

        assert!(
            report.is_expansion(),
            "added permission must be an expansion"
        );
        assert!(
            report
                .permission_changes
                .iter()
                .any(|d| d.term == "history:read" && d.kind == DeltaKind::Added),
            "history:read must appear as Added"
        );
        // is_expansion must agree with ApprovalHash::is_expansion_of.
        assert_eq!(
            report.is_expansion(),
            candidate.is_expansion_of(&prior),
            "is_expansion must agree with ApprovalHash::is_expansion_of"
        );
    }

    #[test]
    fn added_capability_is_expansion() {
        let prior = hash_from_src(BASE);
        let candidate = hash_from_src(CAP_ADDED);
        let report = diff(&prior, &candidate);

        assert!(
            report.is_expansion(),
            "added capability must be an expansion"
        );
        assert!(
            report
                .capability_changes
                .iter()
                .any(|d| d.term == "password-manager:fill" && d.kind == DeltaKind::Added),
            "password-manager:fill must appear as Added in capability_changes"
        );
        assert_eq!(
            report.is_expansion(),
            candidate.is_expansion_of(&prior),
            "is_expansion must agree with ApprovalHash::is_expansion_of"
        );
    }

    #[test]
    fn added_consumes_is_expansion() {
        let prior = hash_from_src(BASE);
        let candidate = hash_from_src(CONSUMES_ADDED);
        let report = diff(&prior, &candidate);

        assert!(
            report.is_expansion(),
            "added consumes entry must be an expansion"
        );
        assert!(
            report
                .consumes_changes
                .iter()
                .any(|d| d.term == "password-manager:fill" && d.kind == DeltaKind::Added),
            "password-manager:fill must appear as Added in consumes_changes"
        );
        assert_eq!(
            report.is_expansion(),
            candidate.is_expansion_of(&prior),
            "is_expansion must agree with ApprovalHash::is_expansion_of"
        );
    }

    // -----------------------------------------------------------------------
    // Contraction case
    // -----------------------------------------------------------------------

    #[test]
    fn removed_permission_is_not_expansion() {
        let prior = hash_from_src(BASE);
        let candidate = hash_from_src(PERM_REMOVED);
        let report = diff(&prior, &candidate);

        assert!(
            !report.is_expansion(),
            "removed permission must NOT be an expansion"
        );
        assert!(
            report
                .permission_changes
                .iter()
                .any(|d| d.term == "tabs:list" && d.kind == DeltaKind::Removed),
            "tabs:list must appear as Removed"
        );
        assert_eq!(
            report.is_expansion(),
            candidate.is_expansion_of(&prior),
            "is_expansion must agree with ApprovalHash::is_expansion_of"
        );
    }

    // -----------------------------------------------------------------------
    // No-change case
    // -----------------------------------------------------------------------

    #[test]
    fn no_change_is_not_expansion_and_empty_diff() {
        let prior = hash_from_src(BASE);
        let candidate = hash_from_src(NO_CHANGE);
        let report = diff(&prior, &candidate);

        assert!(
            !report.is_expansion(),
            "identical hashes must not be classified as expansion"
        );
        assert!(
            report.permission_changes.is_empty(),
            "no permission changes expected"
        );
        assert!(
            report.capability_changes.is_empty(),
            "no capability changes expected"
        );
        assert!(
            report.consumes_changes.is_empty(),
            "no consumes changes expected"
        );
        assert!(
            report.identity_scope_change.is_none(),
            "no scope change expected"
        );
        assert_eq!(
            report.is_expansion(),
            candidate.is_expansion_of(&prior),
            "is_expansion must agree with ApprovalHash::is_expansion_of"
        );
    }

    // -----------------------------------------------------------------------
    // Identity scope change
    // -----------------------------------------------------------------------

    #[test]
    fn scope_change_is_expansion() {
        let prior = hash_from_src(BASE);
        let candidate = hash_from_src(SCOPE_CHANGED);
        let report = diff(&prior, &candidate);

        assert!(
            report.is_expansion(),
            "identity_scope change must be an expansion"
        );
        assert_eq!(
            report.identity_scope_change,
            Some((Some(LuaScope::Global), Some(LuaScope::PerIdentity))),
            "identity_scope_change must capture old and new scope"
        );
        // No permission or capability changes — only the scope changed.
        assert!(
            report.permission_changes.is_empty(),
            "no permission changes expected for scope-only change"
        );
        assert_eq!(
            report.is_expansion(),
            candidate.is_expansion_of(&prior),
            "is_expansion must agree with ApprovalHash::is_expansion_of"
        );
    }

    // -----------------------------------------------------------------------
    // Symmetry invariant: is_expansion must always agree with is_expansion_of
    // -----------------------------------------------------------------------

    #[test]
    fn is_expansion_agrees_with_approval_hash_for_all_cases() {
        let base = hash_from_src(BASE);

        let cases = [
            ("expansion: added perm", hash_from_src(PERM_ADDED)),
            ("contraction: removed perm", hash_from_src(PERM_REMOVED)),
            ("no change", hash_from_src(NO_CHANGE)),
            ("capability added", hash_from_src(CAP_ADDED)),
            ("consumes added", hash_from_src(CONSUMES_ADDED)),
            ("scope changed", hash_from_src(SCOPE_CHANGED)),
        ];

        for (label, candidate) in &cases {
            let report = diff(&base, candidate);
            assert_eq!(
                report.is_expansion(),
                candidate.is_expansion_of(&base),
                "is_expansion disagreement for case: {label}"
            );
        }
    }
}
