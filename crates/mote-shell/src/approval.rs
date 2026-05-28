//! Pure, headless install→approval flow brain (ADR-0007, Phase 3 Task 2).
//!
//! This module contains **no CEF, no async, no shell event loop** — just
//! well-tested logic that the shell wires up:
//!
//! - [`DecidedPolicy`] — replays a decision already made by the coordinator or
//!   the approval dialog.
//! - [`classify`] — inspects a manifest + provenance + approval store to decide
//!   whether a plugin can be auto-granted or requires a dialog.
//! - [`approval_from_dialog`] — maps a [`DialogResult`] from the approval dialog
//!   op payload back to an [`Approval`].
//!
//! The production shell drives the dialog and then calls back into
//! [`approval_from_dialog`] and [`DecidedPolicy`] once the user has responded;
//! tests wire these pieces directly without any event loop.
//!
//! Both paths are now wired into the shell: the install/auto-grant path
//! (`classify`, `DecidedPolicy`) through the plugin host's load pass, and the
//! dialog-result path (`approval_from_dialog`, `DialogResult`, the op-boundary
//! [`validate_origin_glob`]) through the `approve_plugin` op handler.

use std::collections::BTreeSet;

use mote_lua::Manifest;
use mote_permissions::Permission;
use mote_pluginmgr::{ApprovalStore, DeltaKind, DiffReport, Provenance, diff};
use mote_registry::CombinationRegistry;
use mote_runtime::{Approval, ApprovalHash, ApprovalPolicy, Narrowing};
use mote_ui::{ApprovalRequest, NarrowMode, NarrowablePermission};
use thiserror::Error;

// ── DecidedPolicy ─────────────────────────────────────────────────────────────

/// An [`ApprovalPolicy`] that replays a decision already made (by the
/// coordinator or the approval dialog).
///
/// `decide()` never renders or blocks — per ADR-0007 the dialog/await happens
/// in the shell, not inside synchronous `decide()`.
pub(crate) struct DecidedPolicy {
    decision: Approval,
}

impl std::fmt::Debug for DecidedPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecidedPolicy").finish_non_exhaustive()
    }
}

impl DecidedPolicy {
    /// Wraps a pre-made [`Approval`] decision.
    pub(crate) const fn new(decision: Approval) -> Self {
        Self { decision }
    }

    /// Convenience: an auto-grant policy.
    ///
    /// Used on the auto-grant path where no dialog is shown (e.g. bundled or
    /// dev-mode plugins, or a non-expanding update).
    pub(crate) const fn grant() -> Self {
        Self::new(Approval::GrantAsRequested)
    }
}

impl ApprovalPolicy for DecidedPolicy {
    fn decide(&self, _plugin: &str, _requested: &[Permission]) -> Approval {
        self.decision.clone()
    }
}

// ── classify ──────────────────────────────────────────────────────────────────

/// Errors that can occur during [`classify`].
#[derive(Debug, Error)]
pub(crate) enum ClassifyError {
    /// The approval store returned an error.
    #[error("approval store error: {0}")]
    Store(#[from] mote_pluginmgr::ApprovalStoreError),
}

/// Whether a plugin can be auto-granted or needs the approval dialog.
pub(crate) enum ApprovalOutcome {
    /// Auto-grant: the plugin is trusted by construction (bundled / dev-mode),
    /// or its manifest did not expand the previously approved permission set.
    AutoGrant,
    /// The dialog must be shown before the plugin can load.
    NeedsDialog(ApprovalRequest),
}

impl std::fmt::Debug for ApprovalOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AutoGrant => write!(f, "AutoGrant"),
            Self::NeedsDialog(_) => write!(f, "NeedsDialog(..)"),
        }
    }
}

/// Classifies a plugin install/reload: can it be auto-granted, or does it need
/// a user-facing approval dialog?
///
/// # Auto-grant rules (no dialog shown)
///
/// - [`Provenance::Bundled`] — first-party, trusted by construction.
/// - [`Provenance::DevMode`] — developer's own code; explicit opt-in gesture.
/// - Prior approval present and candidate does NOT expand it (unchanged or
///   contracting manifest).
///
/// # Dialog rules
///
/// - First install with no prior approval → `NeedsDialog`, `is_update=false`.
/// - Prior approval present but candidate expands it → `NeedsDialog`,
///   `is_update=true`, `new_permissions` contains the added terms.
///
/// # Errors
///
/// Returns [`ClassifyError::Store`] if the approval store cannot be read.
pub(crate) fn classify(
    manifest: &Manifest,
    provenance: Provenance,
    store: &ApprovalStore,
    combos: &CombinationRegistry,
) -> Result<ApprovalOutcome, ClassifyError> {
    // Bundled and dev-mode are auto-granted without touching the store.
    if matches!(provenance, Provenance::Bundled | Provenance::DevMode) {
        return Ok(ApprovalOutcome::AutoGrant);
    }

    let candidate = ApprovalHash::of(manifest);
    let prior = store.get(&manifest.name)?;

    match prior {
        None => {
            // First install — no prior record.
            let req = build_request(manifest, provenance, combos, false, vec![]);
            Ok(ApprovalOutcome::NeedsDialog(req))
        }
        Some(p) if !candidate.is_expansion_of(&p) => {
            // Unchanged or contracting manifest — silent re-approval.
            Ok(ApprovalOutcome::AutoGrant)
        }
        Some(p) => {
            // Expanding manifest — collect everything `is_expansion_of` fires on
            // so the dialog's "what's new" list is never silently empty: added
            // permissions, capabilities, consumes, and any identity_scope change.
            let report = diff(&p, &candidate);
            let new_permissions = collect_new_surface(&report);
            let req = build_request(manifest, provenance, combos, true, new_permissions);
            Ok(ApprovalOutcome::NeedsDialog(req))
        }
    }
}

/// Collects every newly-added approval-surface term from a [`DiffReport`] into
/// the flat `new_permissions` list the dialog renders as "what's new".
///
/// `is_expansion_of` fires on added permissions, capabilities, consumes, or an
/// `identity_scope` change — so all four must be surfaced or the dialog would
/// show `is_update=true` with an empty list for, e.g., a capability-only
/// expansion. Non-permission entries are label-prefixed for legibility:
/// `capability:<term>`, `consumes:<term>`, and `identity_scope: <old> → <new>`.
/// Only `Added` deltas are included for the three list fields (removals do not
/// expand the surface).
fn collect_new_surface(report: &DiffReport) -> Vec<String> {
    let mut new_surface = Vec::new();
    let added = |deltas: &[mote_pluginmgr::Delta]| -> Vec<String> {
        deltas
            .iter()
            .filter(|d| matches!(d.kind, DeltaKind::Added))
            .map(|d| d.term.clone())
            .collect()
    };

    new_surface.extend(added(&report.permission_changes));
    new_surface.extend(
        added(&report.capability_changes)
            .into_iter()
            .map(|t| format!("capability:{t}")),
    );
    new_surface.extend(
        added(&report.consumes_changes)
            .into_iter()
            .map(|t| format!("consumes:{t}")),
    );
    if let Some((old, new)) = report.identity_scope_change {
        new_surface.push(format!("identity_scope: {old:?} → {new:?}"));
    }

    new_surface
}

/// Returns the short source label shown in the approval dialog.
const fn provenance_label(provenance: Provenance) -> &'static str {
    match provenance {
        Provenance::Bundled => "bundled",
        Provenance::DevMode => "dev-mode",
        Provenance::DeclaredGit => "git",
        Provenance::Path => "path",
        Provenance::ImplicitLocal => "local",
    }
}

/// A deliberately-modest hardcoded high-risk set.
///
/// These `domain:action` strings are inherently high-impact regardless of the
/// dangerous-combination registry:
///
/// - `page:inject_script` — arbitrary script injection into any page.
/// - `page:inject_unsafe_script` — unsafe variant of the above.
/// - `page:read_dom` — read the full DOM of any page (exfil risk).
/// - `mcp:server` — opens a local network port reachable from any process.
///
/// The list is kept short by design: marking too many permissions as high-risk
/// dilutes the signal. Additions require a documented reason.
const HARDCODED_HIGH_RISK: &[&str] = &[
    "page:inject_script",
    "page:inject_unsafe_script",
    "page:read_dom",
    "mcp:server",
];

/// Builds an `is_update=true` [`ApprovalRequest`] for a re-approval / adjust-
/// scope dialog (panel actions, Task 5 §6.5).
///
/// Unlike [`classify`]'s first-install path, this is used when the plugin is
/// already loaded and the user is re-narrowing or re-approving an expanded
/// manifest: `is_update` is set so the dialog reads "approve update", and
/// `new_permissions` carries the (possibly empty) "what's new" surface the
/// caller computed.
pub(crate) fn build_update_request(
    manifest: &Manifest,
    provenance: Provenance,
    combos: &CombinationRegistry,
    new_permissions: Vec<String>,
) -> ApprovalRequest {
    build_request(manifest, provenance, combos, true, new_permissions)
}

/// Builds the [`ApprovalRequest`] view-model for the approval dialog.
fn build_request(
    manifest: &Manifest,
    provenance: Provenance,
    combos: &CombinationRegistry,
    is_update: bool,
    new_permissions: Vec<String>,
) -> ApprovalRequest {
    // Build the domain:action key set for dangerous-combination matching.
    // The combination registry matches resource-independently, so we strip
    // the resource segment and deduplicate.
    let da_keys: BTreeSet<String> = manifest
        .permissions
        .iter()
        .map(|p| {
            // Parse to extract domain + action reliably; fall back to the raw
            // string on parse failure (registry validation catches the error
            // upstream; we don't panic here).
            p.parse::<Permission>().map_or_else(
                |_| p.clone(),
                |parsed| format!("{}:{}", parsed.domain(), parsed.action()),
            )
        })
        .collect();

    // Map each permission string to a NarrowablePermission.
    let permissions = manifest
        .permissions
        .iter()
        .map(|p| permission_to_narrowable(p))
        .collect();

    // Collect dangerous combination warnings (human-readable sentences).
    let dangerous_combinations: Vec<String> = combos
        .triggered_by(&da_keys)
        .map(|entry| entry.warning.clone())
        .collect();

    ApprovalRequest {
        plugin: manifest.name.as_str().to_owned(),
        version: manifest.version.clone(),
        source: provenance_label(provenance).to_owned(),
        permissions,
        dangerous_combinations,
        is_update,
        new_permissions,
    }
}

/// Converts one raw permission string from the manifest to a
/// [`NarrowablePermission`].
///
/// ## Narrowable heuristic (deliberately modest)
///
/// A permission is **narrowable** iff it has a resource segment AND that
/// resource is **origin-shaped**. Concretely, with `resource` = the parsed
/// `Permission`'s resource string:
///
/// ```text
/// narrowable = resource == "*" || resource.contains("://") || resource.contains('/')
/// ```
///
/// This makes `page:inject_script:*` and `page:inject_script:https://*.example.com/*`
/// narrowable (the dialog can offer origin-narrowing), while bare-name
/// `dynamic`-shaped resources like `secret:read:anthropic_api_key` and
/// `mcp:client:my-server` are **not** narrowable — origin-narrowing a secret
/// name is nonsensical. Permissions without any resource (e.g.
/// `storage:persistent`, `tabs:list`) are likewise not narrowable.
///
/// `requested_scope` is set to the resource string regardless of narrowability,
/// so the dialog can display the requested scope even for non-narrowable perms.
///
/// ## High-risk heuristic (deliberately modest)
///
/// A permission is **high-risk** if its `domain:action` key is in
/// [`HARDCODED_HIGH_RISK`]. The hardcoded list catches well-known
/// individually-risky actions. Combination-based per-permission `high_risk`
/// (where a permission is high-risk because it participates in a triggered
/// combination) would require passing the triggered combination set through
/// here; that is a future refinement.
///
/// ## Description
///
/// For now, the description is the raw permission string itself. Rich
/// human-readable descriptions for each permission are a future task (they
/// require registry annotations not yet present in v1.toml).
fn permission_to_narrowable(raw: &str) -> NarrowablePermission {
    let (da_key, requested_scope, narrowable) = raw.parse::<Permission>().map_or_else(
        // Unparsable permission string — treat as non-narrowable with no scope.
        |_| (raw.to_owned(), String::new(), false),
        |parsed| {
            let da = format!("{}:{}", parsed.domain(), parsed.action());
            if let Some(resource) = parsed.resource() {
                let scope = resource.to_string();
                // Origin-shaped resources can be narrowed to specific origins;
                // bare-name dynamic resources (secret names, mcp server names)
                // cannot. `requested_scope` keeps the resource string for display
                // either way.
                let narrowable = scope == "*" || scope.contains("://") || scope.contains('/');
                (da, scope, narrowable)
            } else {
                (da, String::new(), false)
            }
        },
    );

    // High-risk: use the hardcoded list as the deliberate-modest signal.
    // Combination-based per-permission high_risk would require passing the
    // triggered combination set through here; that's a future refinement.
    let high_risk = HARDCODED_HIGH_RISK.contains(&da_key.as_str());

    // Description: raw permission string as a modest placeholder.
    let description = format!("grants {raw}");

    NarrowablePermission {
        domain: da_key,
        requested_scope,
        mode: NarrowMode::GrantFull,
        description,
        narrowable,
        high_risk,
    }
}

// ── Op-boundary structural validation ───────────────────────────────────────

/// The maximum byte length of a single origin glob (ADR-0005 op-boundary cap).
///
/// Origin globs are short URL/host patterns (`https://*.example.com/*`); 2048
/// is generous for any legitimate pattern while bounding the resource string a
/// hostile chrome payload could push into a [`Narrowing`].
pub(crate) const MAX_ORIGIN_GLOB_LEN: usize = 2048;

/// The maximum number of origin globs a single dialog permission may carry
/// (ADR-0005 op-boundary cap). Caps the fan-out a hostile payload can create.
pub(crate) const MAX_ORIGINS_PER_PERMISSION: usize = 64;

/// Structurally validates one origin-glob string at the `approve_plugin` op
/// boundary (ADR-0005 "closed structured operations").
///
/// This is the **security-critical** gate that keeps an arbitrary chrome-supplied
/// string from becoming a [`Narrowing`] resource. It enforces, in order:
///
/// - **Non-empty.** A zero-length glob is not a meaningful origin pattern.
/// - **Bounded length.** At most [`MAX_ORIGIN_GLOB_LEN`] bytes.
/// - **Bounded character set.** Only printable ASCII drawn from the set a
///   URL/host glob needs: ASCII letters and digits plus `. - _ : / * ? @ ~`.
///   Everything else — control characters, whitespace, quotes, backslash, `<`,
///   `>`, `%`, and any non-ASCII byte — is rejected. This neutralises both
///   markup-injection shapes (`<script>`) and ambiguous/encoded inputs.
///
/// It deliberately does **not** assert a full URL grammar (that the glob is a
/// well-formed origin); narrowing semantics are matched downstream. Its job is
/// to bound what can cross the trust boundary, not to parse it.
pub(crate) fn validate_origin_glob(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_ORIGIN_GLOB_LEN {
        return false;
    }
    s.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'.' | b'-' | b'_' | b':' | b'/' | b'*' | b'?' | b'@' | b'~'
            )
    })
}

// ── Dialog result → Approval mapping ─────────────────────────────────────────

/// Per-permission decision coming back from the approval dialog op payload.
///
/// Fields are `pub(crate)` so the shell's `approve_plugin` op handler can run
/// the op-boundary structural validation ([`validate_origin_glob`]) and the
/// semantic cross-check before pushing the decision to the pump thread.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct DialogPermission {
    pub(crate) domain: String,
    pub(crate) action: String,
    /// `"full"` | `"origins"` | `"deny"`
    pub(crate) mode: String,
    pub(crate) origins: Option<Vec<String>>,
}

/// The approval dialog's op result payload.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct DialogResult {
    pub(crate) plugin: String,
    /// `"grant"` | `"deny"`
    pub(crate) decision: String,
    pub(crate) permissions: Vec<DialogPermission>,
}

impl DialogResult {
    /// The plugin name for cross-checking against the expected plugin.
    pub(crate) fn plugin(&self) -> &str {
        &self.plugin
    }
}

/// Maps a [`DialogResult`] from the approval dialog back to an [`Approval`].
///
/// # Mapping rules
///
/// - Overall `decision == "deny"` → `Approval::Deny`.
/// - Any per-permission `mode == "deny"` → forces `Approval::Deny` (v1 policy:
///   the dialog denies the whole plugin when any required permission is denied).
/// - Per-permission `mode == "origins"` with non-empty `origins` → one
///   [`Narrowing`] per such permission.
/// - Per-permission `mode == "origins"` with `None`/empty `origins` → MALFORMED.
///   A narrowing to zero origins is not a grant; treating it as `GrantAsRequested`
///   would silently *widen* the user's intent. We return `Approval::Deny`.
/// - Per-permission `mode == "full"` → no narrowing entry (granted as requested).
/// - If there are no narrowings → `Approval::GrantAsRequested`.
/// - If there are narrowings → `Approval::Narrow { narrowings }`.
///
/// Note: cross-checking each `DialogPermission`'s `(domain, action)` against the
/// pending [`ApprovalRequest`] (i.e. verifying the user only answered for
/// permissions that were actually requested) happens **upstream** in the op
/// handler, not here. This function trusts that membership and only translates
/// the per-permission verdicts into an [`Approval`].
pub(crate) fn approval_from_dialog(result: &DialogResult) -> Approval {
    // Overall decision: "deny" → immediate Deny.
    if result.decision != "grant" {
        return Approval::Deny {
            reason: format!(
                "plugin '{}' was denied by the user in the approval dialog",
                result.plugin
            ),
        };
    }

    // Per-permission scan: any "deny" mode forces whole-plugin Deny.
    if result.permissions.iter().any(|p| p.mode == "deny") {
        return Approval::Deny {
            reason: format!(
                "plugin '{}' was denied: at least one required permission was denied",
                result.plugin
            ),
        };
    }

    // Collect narrowings from permissions with mode "origins". An "origins"
    // verdict with no origins is malformed — a narrowing to zero origins is not
    // a grant, and silently dropping it would widen the user's intent to a full
    // grant. Reject the whole plugin in that case.
    let mut narrowings: Vec<Narrowing> = Vec::new();
    for p in &result.permissions {
        if p.mode != "origins" {
            continue;
        }
        let origins = p.origins.clone().unwrap_or_default();
        if origins.is_empty() {
            return Approval::Deny {
                reason: format!(
                    "plugin '{}' was denied: permission '{}:{}' was narrowed to origins \
                     but no origins were provided (malformed dialog result)",
                    result.plugin, p.domain, p.action
                ),
            };
        }
        narrowings.push(Narrowing {
            domain: p.domain.clone(),
            action: p.action.clone(),
            resources: origins,
        });
    }

    if narrowings.is_empty() {
        Approval::GrantAsRequested
    } else {
        Approval::Narrow { narrowings }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use mote_lua::load_plugin;
    use mote_registry::Registry;
    use mote_runtime::ApprovalHash;
    use mote_storage::Store;
    use mote_types::SchemaVersion;

    use super::*;

    // ── Helpers ────────────────────────────────────────────────────────────

    fn manifest_from_src(src: &str) -> Manifest {
        load_plugin(src, "test").unwrap().manifest().clone()
    }

    fn combos() -> CombinationRegistry {
        Registry::load(SchemaVersion::V1)
            .expect("v1 registry loads")
            .combinations()
            .clone()
    }

    fn open_store() -> (Store, ApprovalStore) {
        let store = Store::open_in_memory().unwrap();
        let approval = ApprovalStore::new(&store);
        (store, approval)
    }

    // ── Lua manifest sources ───────────────────────────────────────────────

    const BASE_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "test-plugin",
            version = "1.0",
            permissions = { "storage:persistent", "tabs:list" },
        }
        return M
    "#;

    // Adds history:read — expansion of BASE_SRC.
    const EXPANDED_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "test-plugin",
            version = "2.0",
            permissions = { "storage:persistent", "tabs:list", "history:read" },
        }
        return M
    "#;

    // Only storage:persistent — contraction of BASE_SRC.
    const CONTRACTED_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "test-plugin",
            version = "2.0",
            permissions = { "storage:persistent" },
        }
        return M
    "#;

    // Same permissions as BASE_SRC but ADDS a capability — an expansion that
    // shows up only in `capability_changes`, not `permission_changes`.
    const CAP_ADDED_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "test-plugin",
            version = "2.0",
            permissions = { "storage:persistent", "tabs:list" },
            capabilities = { "theme:provider" },
        }
        return M
    "#;

    // Triggers the page:read_dom + mcp:server dangerous combination (v1 registry).
    const COMBO_SRC: &str = r#"
        local M = {}
        M.manifest = {
            schema = "v1",
            name = "spy-plugin",
            version = "1.0",
            permissions = { "page:read_dom", "mcp:server" },
        }
        return M
    "#;

    // ── Task 2a: DecidedPolicy ─────────────────────────────────────────────

    #[test]
    fn decided_policy_grant_as_requested() {
        let policy = DecidedPolicy::new(Approval::GrantAsRequested);
        let result = policy.decide("p", &[]);
        assert!(
            matches!(result, Approval::GrantAsRequested),
            "expected GrantAsRequested"
        );
    }

    #[test]
    fn decided_policy_grant_convenience() {
        let policy = DecidedPolicy::grant();
        let result = policy.decide("p", &[]);
        assert!(
            matches!(result, Approval::GrantAsRequested),
            "grant() shorthand must return GrantAsRequested"
        );
    }

    #[test]
    fn decided_policy_narrow() {
        let narrowings = vec![Narrowing {
            domain: "page".into(),
            action: "inject_script".into(),
            resources: vec!["https://example.com/*".into()],
        }];
        let policy = DecidedPolicy::new(Approval::Narrow { narrowings });
        let result = policy.decide("p", &[]);
        match result {
            Approval::Narrow { narrowings: got } => {
                assert_eq!(got.len(), 1);
                assert_eq!(got[0].domain, "page");
                assert_eq!(got[0].action, "inject_script");
                assert_eq!(got[0].resources, vec!["https://example.com/*"]);
            }
            other => panic!("expected Narrow, got {other:?}"),
        }
    }

    #[test]
    fn decided_policy_deny() {
        let policy = DecidedPolicy::new(Approval::Deny {
            reason: "test denial".into(),
        });
        let result = policy.decide("p", &[]);
        match result {
            Approval::Deny { reason } => assert_eq!(reason, "test denial"),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    // ── Task 2b: classify ──────────────────────────────────────────────────

    #[test]
    fn bundled_provenance_is_auto_grant() {
        let (_store, approval) = open_store();
        let manifest = manifest_from_src(BASE_SRC);
        let result = classify(&manifest, Provenance::Bundled, &approval, &combos()).unwrap();
        assert!(
            matches!(result, ApprovalOutcome::AutoGrant),
            "bundled must auto-grant"
        );
    }

    #[test]
    fn devmode_provenance_is_auto_grant() {
        let (_store, approval) = open_store();
        let manifest = manifest_from_src(BASE_SRC);
        let result = classify(&manifest, Provenance::DevMode, &approval, &combos()).unwrap();
        assert!(
            matches!(result, ApprovalOutcome::AutoGrant),
            "dev-mode must auto-grant"
        );
    }

    #[test]
    fn first_install_needs_dialog() {
        let (_store, approval) = open_store();
        let manifest = manifest_from_src(BASE_SRC);
        let result = classify(&manifest, Provenance::Path, &approval, &combos()).unwrap();
        match result {
            ApprovalOutcome::NeedsDialog(req) => {
                assert!(!req.is_update, "first install must not be is_update");
                assert!(
                    req.new_permissions.is_empty(),
                    "first install must have empty new_permissions"
                );
                assert_eq!(req.plugin, "test-plugin");
            }
            ApprovalOutcome::AutoGrant => panic!("first install must need dialog"),
        }
    }

    #[test]
    fn first_install_needs_dialog_git_provenance() {
        let (_store, approval) = open_store();
        let manifest = manifest_from_src(BASE_SRC);
        let result = classify(&manifest, Provenance::DeclaredGit, &approval, &combos()).unwrap();
        assert!(
            matches!(result, ApprovalOutcome::NeedsDialog(_)),
            "git first install must need dialog"
        );
    }

    #[test]
    fn prior_equal_hash_is_auto_grant() {
        let (_store, approval) = open_store();
        let manifest = manifest_from_src(BASE_SRC);
        // Pre-approve with the same hash.
        let hash = ApprovalHash::of(&manifest);
        approval.put(&manifest.name, &hash).unwrap();

        let result = classify(&manifest, Provenance::Path, &approval, &combos()).unwrap();
        assert!(
            matches!(result, ApprovalOutcome::AutoGrant),
            "equal hash must auto-grant"
        );
    }

    #[test]
    fn contracting_manifest_is_auto_grant() {
        let (_store, approval) = open_store();
        let base = manifest_from_src(BASE_SRC);
        let contracted = manifest_from_src(CONTRACTED_SRC);

        // Prior was approved at BASE (2 permissions).
        let prior_hash = ApprovalHash::of(&base);
        approval.put(&base.name, &prior_hash).unwrap();

        // Candidate contracts to 1 permission.
        let result = classify(&contracted, Provenance::DeclaredGit, &approval, &combos()).unwrap();
        assert!(
            matches!(result, ApprovalOutcome::AutoGrant),
            "contracting manifest must auto-grant"
        );
    }

    #[test]
    fn expanding_manifest_needs_dialog_with_new_permissions() {
        let (_store, approval) = open_store();
        let base = manifest_from_src(BASE_SRC);
        let expanded = manifest_from_src(EXPANDED_SRC);

        // Prior was approved at BASE (2 permissions).
        let prior_hash = ApprovalHash::of(&base);
        approval.put(&base.name, &prior_hash).unwrap();

        // Candidate adds history:read.
        let result = classify(&expanded, Provenance::DeclaredGit, &approval, &combos()).unwrap();
        match result {
            ApprovalOutcome::NeedsDialog(req) => {
                assert!(req.is_update, "expanding update must be is_update=true");
                assert!(
                    req.new_permissions.contains(&"history:read".to_owned()),
                    "history:read must appear in new_permissions; got: {:?}",
                    req.new_permissions
                );
            }
            ApprovalOutcome::AutoGrant => panic!("expanding manifest must need dialog"),
        }
    }

    #[test]
    fn capability_only_expansion_surfaces_in_new_permissions() {
        // Regression: an update that adds only a CAPABILITY (no new permission)
        // is still an expansion per is_expansion_of, and must not render an empty
        // "what's new" list.
        let (_store, approval) = open_store();
        let base = manifest_from_src(BASE_SRC);
        let cap_added = manifest_from_src(CAP_ADDED_SRC);

        // Prior was approved at BASE (no capabilities).
        let prior_hash = ApprovalHash::of(&base);
        approval.put(&base.name, &prior_hash).unwrap();

        // Candidate adds the theme:provider capability (same permissions).
        let result = classify(&cap_added, Provenance::DeclaredGit, &approval, &combos()).unwrap();
        match result {
            ApprovalOutcome::NeedsDialog(req) => {
                assert!(req.is_update, "capability expansion must be is_update=true");
                assert!(
                    req.new_permissions
                        .contains(&"capability:theme:provider".to_owned()),
                    "capability:theme:provider must appear in new_permissions; got: {:?}",
                    req.new_permissions
                );
            }
            ApprovalOutcome::AutoGrant => panic!("capability expansion must need dialog"),
        }
    }

    #[test]
    fn combo_triggers_dangerous_combinations_in_request() {
        // The v1 registry has one combination: page:read_dom + mcp:server.
        let (_store, approval) = open_store();
        let manifest = manifest_from_src(COMBO_SRC);

        let result = classify(&manifest, Provenance::DeclaredGit, &approval, &combos()).unwrap();
        match result {
            ApprovalOutcome::NeedsDialog(req) => {
                assert!(
                    !req.dangerous_combinations.is_empty(),
                    "page:read_dom + mcp:server must trigger dangerous combinations; got: {:?}",
                    req.dangerous_combinations
                );
            }
            ApprovalOutcome::AutoGrant => panic!("first install must need dialog"),
        }
    }

    #[test]
    fn narrowable_only_for_origin_shaped_resources() {
        // Bare-name dynamic resource → NOT narrowable (origin-narrowing a secret
        // name is nonsensical), but the scope is still surfaced for display.
        let secret = permission_to_narrowable("secret:read:anthropic_api_key");
        assert!(
            !secret.narrowable,
            "bare-name dynamic resource must not be narrowable"
        );
        assert_eq!(secret.requested_scope, "anthropic_api_key");

        // Bare-name mcp client → NOT narrowable.
        let mcp = permission_to_narrowable("mcp:client:my-server");
        assert!(
            !mcp.narrowable,
            "bare-name mcp server resource must not be narrowable"
        );

        // `*` resource → narrowable.
        let inject_star = permission_to_narrowable("page:inject_script:*");
        assert!(inject_star.narrowable, "`*` resource must be narrowable");
        assert_eq!(inject_star.requested_scope, "*");

        // Origin glob → narrowable.
        let inject_origin = permission_to_narrowable("page:inject_script:https://*.example.com/*");
        assert!(
            inject_origin.narrowable,
            "origin-glob resource must be narrowable"
        );

        // No resource → NOT narrowable.
        let storage = permission_to_narrowable("storage:persistent");
        assert!(
            !storage.narrowable,
            "no-resource permission must not be narrowable"
        );
        assert_eq!(storage.requested_scope, "");
    }

    // ── Task 2c: approval_from_dialog ──────────────────────────────────────

    fn make_result(
        decision: &str,
        perms: Vec<(&str, &str, &str, Option<Vec<&str>>)>,
    ) -> DialogResult {
        DialogResult {
            plugin: "test-plugin".into(),
            decision: decision.into(),
            permissions: perms
                .into_iter()
                .map(|(domain, action, mode, origins)| DialogPermission {
                    domain: domain.into(),
                    action: action.into(),
                    mode: mode.into(),
                    origins: origins.map(|v| v.into_iter().map(String::from).collect()),
                })
                .collect(),
        }
    }

    #[test]
    fn all_full_permissions_yields_grant_as_requested() {
        let result = make_result(
            "grant",
            vec![
                ("storage", "persistent", "full", None),
                ("tabs", "list", "full", None),
            ],
        );
        let approval = approval_from_dialog(&result);
        assert!(
            matches!(approval, Approval::GrantAsRequested),
            "all-full must yield GrantAsRequested"
        );
    }

    #[test]
    fn origins_narrowed_permission_yields_narrow() {
        let result = make_result(
            "grant",
            vec![
                (
                    "page",
                    "inject_script",
                    "origins",
                    Some(vec!["https://example.com/*", "https://github.com/*"]),
                ),
                ("storage", "persistent", "full", None),
            ],
        );
        let approval = approval_from_dialog(&result);
        match approval {
            Approval::Narrow { narrowings } => {
                assert_eq!(narrowings.len(), 1, "expected exactly 1 narrowing");
                let n = &narrowings[0];
                assert_eq!(n.domain, "page");
                assert_eq!(n.action, "inject_script");
                assert_eq!(
                    n.resources,
                    vec!["https://example.com/*", "https://github.com/*"]
                );
            }
            other => panic!("expected Narrow, got {other:?}"),
        }
    }

    #[test]
    fn overall_deny_yields_approval_deny() {
        let result = make_result("deny", vec![("storage", "persistent", "full", None)]);
        let approval = approval_from_dialog(&result);
        assert!(
            matches!(approval, Approval::Deny { .. }),
            "overall deny must yield Approval::Deny"
        );
    }

    #[test]
    fn per_permission_deny_forces_overall_deny() {
        // One permission denied → whole plugin denied (v1 policy).
        let result = make_result(
            "grant",
            vec![
                ("storage", "persistent", "full", None),
                ("page", "read_dom", "deny", None),
            ],
        );
        let approval = approval_from_dialog(&result);
        assert!(
            matches!(approval, Approval::Deny { .. }),
            "per-permission deny must force Approval::Deny"
        );
    }

    #[test]
    fn multiple_origins_permissions_yields_multiple_narrowings() {
        let result = make_result(
            "grant",
            vec![
                (
                    "page",
                    "inject_script",
                    "origins",
                    Some(vec!["https://a.com/*"]),
                ),
                (
                    "net",
                    "intercept_request",
                    "origins",
                    Some(vec!["https://b.com/*"]),
                ),
            ],
        );
        let approval = approval_from_dialog(&result);
        match approval {
            Approval::Narrow { narrowings } => {
                assert_eq!(narrowings.len(), 2, "expected 2 narrowings");
            }
            other => panic!("expected Narrow, got {other:?}"),
        }
    }

    #[test]
    fn origins_mode_with_none_origins_is_deny() {
        // "origins" with no origins is malformed — must NOT widen to a full grant.
        let result = make_result("grant", vec![("page", "inject_script", "origins", None)]);
        let approval = approval_from_dialog(&result);
        assert!(
            matches!(approval, Approval::Deny { .. }),
            "origins mode with None origins must yield Approval::Deny, got {approval:?}"
        );
    }

    #[test]
    fn origins_mode_with_empty_origins_is_deny() {
        // "origins" with an empty list is equally malformed.
        let result = make_result(
            "grant",
            vec![("page", "inject_script", "origins", Some(vec![]))],
        );
        let approval = approval_from_dialog(&result);
        assert!(
            matches!(approval, Approval::Deny { .. }),
            "origins mode with empty origins must yield Approval::Deny, got {approval:?}"
        );
    }

    #[test]
    fn dialog_result_plugin_accessor() {
        let result = DialogResult {
            plugin: "my-plugin".into(),
            decision: "grant".into(),
            permissions: vec![],
        };
        assert_eq!(result.plugin(), "my-plugin");
    }

    // ── Op-boundary structural validation (ADR-0005, security-critical) ──────

    #[test]
    fn valid_origin_globs_pass() {
        for s in [
            "*",
            "https://example.com/*",
            "https://*.example.com/*",
            "http://localhost:3000/*",
            "*.github.com",
            "https://example.com/path?query",
            "user@host:1234/*",
            "~/local-ish",
            "a", // single printable char
        ] {
            assert!(validate_origin_glob(s), "must accept origin glob: {s:?}");
        }
    }

    #[test]
    fn empty_origin_glob_is_rejected() {
        assert!(
            !validate_origin_glob(""),
            "empty glob is not a valid origin"
        );
    }

    #[test]
    fn over_length_origin_glob_is_rejected() {
        let ok = "a".repeat(MAX_ORIGIN_GLOB_LEN);
        assert!(validate_origin_glob(&ok), "at the cap is allowed");
        let too_long = "a".repeat(MAX_ORIGIN_GLOB_LEN + 1);
        assert!(
            !validate_origin_glob(&too_long),
            "over the cap must be rejected"
        );
    }

    #[test]
    fn control_chars_and_whitespace_are_rejected() {
        for s in [
            "https://e.com/\n",
            "https://e.com/ ",
            "\thttps://e.com",
            "https://e\0.com",
            "https://e.com/\r\n",
        ] {
            assert!(
                !validate_origin_glob(s),
                "control/whitespace must be rejected: {s:?}"
            );
        }
    }

    #[test]
    fn quotes_backslash_and_markup_are_rejected() {
        for s in [
            "https://e.com/\"",
            "https://e.com/'",
            "https://e.com\\admin",
            "<script>alert(1)</script>",
            "https://e.com/<img>",
            "https://e.com/\">x",
            "java%73cript:alert(1)", // `%` is not in the allowed set
        ] {
            assert!(
                !validate_origin_glob(s),
                "injection-shaped input must be rejected: {s:?}"
            );
        }
    }

    #[test]
    fn non_ascii_is_rejected() {
        // Homoglyph / unicode payloads must not slip through.
        assert!(!validate_origin_glob("https://exa\u{0430}mple.com/*"));
        assert!(!validate_origin_glob("https://例え.com/*"));
    }
}
