//! View-model types for the integrity panel and permission-approval dialog.
//!
//! These are **pure Rust data shapes** the shell populates. The chrome surfaces
//! render from them; no GPU/CEF/shell types appear here.
//!
//! ## Integrity panel (`chrome/integrity-panel.html`)
//!
//! Declared `About → Browser Integrity`, this is a runtime-owned surface (not a
//! plugin). It lists active plugins (each rendered as a card via [`PluginRow`]),
//! a network audit summary ([`AuditRow`]), storage accounting ([`StorageRow`]),
//! and permission denials ([`DenialRow`]). Actions on each plugin card
//! ([`PluginAction`]) are wired back to the shell.
//!
//! ## Permission-approval dialog (`chrome/approval-dialog.html`)
//!
//! A floating dialog surfaced at install time (and on permission-expanding
//! updates). Renders one [`ApprovalRequest`]: the list of permissions requested
//! with per-permission narrowing UI ([`NarrowablePermission`]) and dangerous
//! combination warnings above the list (DISCIPLINES §4).

use std::fmt;

// ──────────────────────────────────────────────────────────────────────────────
// Plugin integrity
// ──────────────────────────────────────────────────────────────────────────────

/// Integrity status of a plugin's on-disk files vs. the lock-file checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IntegrityStatus {
    /// Files match the recorded checksum (BLAKE3).
    Verified,
    /// Files do not match — checksum mismatch.
    Mismatch,
    /// Plugin is in dev mode (auto-approved; visually marked `[dev]`).
    DevMode,
    /// Plugin was sourced from the Mote binary bundle (first-party).
    Bundled,
    /// Verification has not yet been run (e.g., newly added).
    Unknown,
}

impl IntegrityStatus {
    /// Short label shown in the integrity panel badge.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Mismatch => "mismatch",
            Self::DevMode => "dev mode",
            Self::Bundled => "bundled",
            Self::Unknown => "unknown",
        }
    }

    /// Badge variant class (maps to `.badge.*` in badge.css).
    #[must_use]
    pub const fn badge_variant(self) -> &'static str {
        match self {
            Self::Verified => "success",
            Self::Mismatch => "danger",
            Self::DevMode => "accent",
            Self::Bundled => "info",
            Self::Unknown => "",
        }
    }
}

/// How a plugin's code reached the user's machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PluginKind {
    /// Declared in `plugins.lua` with a `github:` or `git+https:` source.
    DeclaredGit {
        /// Short source string, e.g. `github:mote-browser/adblock`.
        source: String,
        /// The resolved commit hash (abbrev 12 chars is conventional).
        commit: String,
    },
    /// Declared in `plugins.lua` with a `path:` source.
    PathLocal {
        /// Absolute or `~`-prefixed path.
        path: String,
    },
    /// Present in `~/.config/mote/plugins/` but not in `plugins.lua`.
    ImplicitLocal {
        /// Absolute path to the plugin directory.
        path: String,
    },
    /// Plugin is in dev mode.
    DevMode {
        /// Absolute path under `mote.dev_mode { directories = … }`.
        path: String,
    },
    /// Plugin comes from the Mote binary bundle (first-party).
    Bundled,
}

impl PluginKind {
    /// Source label for the provenance row in the plugin card.
    #[must_use]
    pub fn source_label(&self) -> String {
        match self {
            Self::DeclaredGit { source, commit } => {
                format!("{source} @ {}", &commit[..commit.len().min(12)])
            }
            Self::PathLocal { path } => format!("path:{path}"),
            Self::ImplicitLocal { path } => format!("implicit  {path}"),
            Self::DevMode { path } => format!("dev  {path}"),
            Self::Bundled => "bundled".to_owned(),
        }
    }

    /// The small glyph prefix used in compact plugin lists (mirrors DESIGN §UI).
    /// `○` declared-git, `◐` path-local, `◇` implicit-local, `⊙` dev, `·` bundled.
    #[must_use]
    pub const fn glyph(&self) -> &'static str {
        match self {
            Self::DeclaredGit { .. } => "○",
            Self::PathLocal { .. } => "◐",
            Self::ImplicitLocal { .. } => "◇",
            Self::DevMode { .. } => "⊙",
            Self::Bundled => "·",
        }
    }
}

/// A single permission row shown inside a plugin card.
///
/// Reflects the requested permission vs. the effective (possibly narrowed)
/// scope the user approved (DESIGN §Permission Primitives).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PermissionRow {
    /// The permission as declared in the plugin manifest
    /// (e.g. `page:inject_script:*`).
    pub requested: String,
    /// The effective permission after user narrowing. Equals `requested` when
    /// no narrowing was applied.
    pub effective: String,
    /// `true` when the effective scope is narrower than requested.
    pub narrowed: bool,
    /// `true` when the permission was explicitly denied by the user.
    pub denied: bool,
}

/// Actions available on a plugin card in the integrity panel.
///
/// These correspond to the keycap buttons rendered at the bottom of each card.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PluginAction {
    /// Open the permission-scope editor for this plugin.
    AdjustScope,
    /// Revoke all permissions and disable the plugin.
    Revoke,
    /// Fetch the latest version (Git-sourced only).
    Update,
    /// Relink to the previous cached commit (Git-sourced only).
    Rollback,
    /// Open the plugin-specific settings pane (if registered).
    Settings,
    /// Reload the plugin from disk (dev-mode / path-local).
    Reload,
}

impl PluginAction {
    /// Human-readable label for the keycap button.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::AdjustScope => "adjust scope",
            Self::Revoke => "revoke",
            Self::Update => "update",
            Self::Rollback => "rollback",
            Self::Settings => "settings",
            Self::Reload => "reload",
        }
    }

    /// Whether this action is destructive (renders as `btn-danger`).
    #[must_use]
    pub const fn is_destructive(&self) -> bool {
        matches!(self, Self::Revoke)
    }
}

/// A single plugin entry in the integrity panel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginRow {
    /// Plugin name as declared in the manifest.
    pub name: String,
    /// Plugin version string.
    pub version: String,
    /// Capabilities this plugin fulfills (e.g. `password-manager:provider`).
    pub fulfills: Vec<String>,
    /// Capabilities this plugin consumes.
    pub consumes: Vec<String>,
    /// Permissions declared in the manifest.
    pub permissions: Vec<PermissionRow>,
    /// Last time the plugin exercised a permission (human-readable).
    pub last_used: Option<String>,
    /// Integrity status of the plugin's files vs. the lock-file checksum.
    pub integrity: IntegrityStatus,
    /// Where the plugin came from (source provenance).
    pub kind: PluginKind,
    /// Actions available on this row (varies by `kind`).
    pub actions: Vec<PluginAction>,
}

impl PluginRow {
    /// Returns `true` if this plugin is running in dev mode.
    #[must_use]
    pub const fn is_dev(&self) -> bool {
        matches!(self.kind, PluginKind::DevMode { .. })
    }

    /// The count of permissions that were narrowed by the user.
    #[must_use]
    pub fn narrowed_count(&self) -> usize {
        self.permissions.iter().filter(|p| p.narrowed).count()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Network audit log
// ──────────────────────────────────────────────────────────────────────────────

/// Decision made by the dispatch chain for a network request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AuditDecision {
    /// Request was blocked by a plugin.
    Blocked,
    /// Request was allowed (no plugin blocked it).
    Allowed,
    /// Request was modified by at least one plugin.
    Modified,
}

impl fmt::Display for AuditDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Blocked => "blocked",
            Self::Allowed => "allowed",
            Self::Modified => "modified",
        })
    }
}

/// A single entry in the network audit summary shown in the integrity panel.
///
/// Each row is a summary by plugin, not per-request. The panel shows totals;
/// the shell holds the full ring-buffer if the user wants to drill down.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditRow {
    /// Who acted — plugin name, or `"browser"` for browser-own requests.
    pub actor: String,
    /// How many requests were acted on in the audit window.
    pub count: u64,
    /// The decision that was applied.
    pub decision: AuditDecision,
    /// Optional context (e.g. the rule-list name for an ad blocker).
    pub detail: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Storage audit
// ──────────────────────────────────────────────────────────────────────────────

/// A single entry in the storage audit section of the integrity panel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageRow {
    /// Plugin name.
    pub plugin: String,
    /// Human-readable size string, e.g. `"2.3 MB"`.
    pub size_human: String,
    /// Bytes used (for sorting / progress bars).
    pub size_bytes: u64,
    /// What the storage is used for (from plugin metadata, optional).
    pub label: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Permission denials
// ──────────────────────────────────────────────────────────────────────────────

/// A permission call that was denied (logged in the ring buffer).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DenialRow {
    /// Plugin that attempted the call.
    pub plugin: String,
    /// The permission that was denied.
    pub permission: String,
    /// Human-readable timestamp (relative, e.g. `"3 days ago"`).
    pub when: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// Full integrity panel view-model
// ──────────────────────────────────────────────────────────────────────────────

/// The complete view-model for the integrity panel.
///
/// The shell constructs this and passes it to the renderer. The renderer
/// calls [`IntegrityPanel::to_html`] (or drives the chrome surface directly).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntegrityPanel {
    /// Active plugins, in display order (declared first, implicit local last).
    pub plugins: Vec<PluginRow>,
    /// Network audit summary (last 24 h).
    pub network_audit: Vec<AuditRow>,
    /// Storage accounting per plugin.
    pub storage: Vec<StorageRow>,
    /// Permission denials in the last 7 days.
    pub denials: Vec<DenialRow>,
}

impl IntegrityPanel {
    /// An empty panel (no plugins loaded yet).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            plugins: Vec::new(),
            network_audit: Vec::new(),
            storage: Vec::new(),
            denials: Vec::new(),
        }
    }

    /// Sample panel populated with realistic data for visual review.
    #[must_use]
    #[allow(clippy::too_many_lines)] // sample fixture — intentionally long
    pub fn sample() -> Self {
        Self {
            plugins: vec![
                PluginRow {
                    name: "password-manager-1password".into(),
                    version: "1.0.0".into(),
                    fulfills: vec!["password-manager:provider".into(), "secret:provider".into()],
                    consumes: vec!["password-manager-form-services".into()],
                    permissions: vec![
                        PermissionRow {
                            requested: "http:fetch:https://*.1password.com/*".into(),
                            effective: "http:fetch:https://*.1password.com/*".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "storage:persistent".into(),
                            effective: "storage:persistent".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "page:inject_script:*".into(),
                            effective: "page:inject_script:https://github.com/*".into(),
                            narrowed: true,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "crypto:seal_to_plugin".into(),
                            effective: "crypto:seal_to_plugin".into(),
                            narrowed: false,
                            denied: false,
                        },
                    ],
                    last_used: Some("2 minutes ago".into()),
                    integrity: IntegrityStatus::Verified,
                    kind: PluginKind::DeclaredGit {
                        source: "github:1password/mote-plugin".into(),
                        commit: "abc123def456789".into(),
                    },
                    actions: vec![
                        PluginAction::AdjustScope,
                        PluginAction::Revoke,
                        PluginAction::Update,
                        PluginAction::Rollback,
                        PluginAction::Settings,
                    ],
                },
                PluginRow {
                    name: "vim-mode".into(),
                    version: "0.5.0".into(),
                    fulfills: vec![],
                    consumes: vec![],
                    permissions: vec![
                        PermissionRow {
                            requested: "keys:bind".into(),
                            effective: "keys:bind".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "keys:intercept_input".into(),
                            effective: "keys:intercept_input".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "page:inject_script:*".into(),
                            effective: "page:inject_script:*".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "storage:memory".into(),
                            effective: "storage:memory".into(),
                            narrowed: false,
                            denied: false,
                        },
                    ],
                    last_used: Some("now".into()),
                    integrity: IntegrityStatus::Verified,
                    kind: PluginKind::DeclaredGit {
                        source: "github:mote-browser/vim-mode".into(),
                        commit: "def456abc789012".into(),
                    },
                    actions: vec![
                        PluginAction::Revoke,
                        PluginAction::Update,
                        PluginAction::Rollback,
                        PluginAction::Settings,
                    ],
                },
                PluginRow {
                    name: "my-experiment".into(),
                    version: "local".into(),
                    fulfills: vec![],
                    consumes: vec![],
                    permissions: vec![
                        PermissionRow {
                            requested: "page:read_dom".into(),
                            effective: "page:read_dom".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "storage:memory".into(),
                            effective: "storage:memory".into(),
                            narrowed: false,
                            denied: false,
                        },
                        PermissionRow {
                            requested: "events:emit".into(),
                            effective: "events:emit".into(),
                            narrowed: false,
                            denied: false,
                        },
                    ],
                    last_used: Some("just now".into()),
                    integrity: IntegrityStatus::DevMode,
                    kind: PluginKind::DevMode {
                        path: "~/code/experiment".into(),
                    },
                    actions: vec![
                        PluginAction::Revoke,
                        PluginAction::Reload,
                        PluginAction::Settings,
                    ],
                },
            ],
            network_audit: vec![
                AuditRow {
                    actor: "adblock".into(),
                    count: 3247,
                    decision: AuditDecision::Blocked,
                    detail: Some("easylist + uBlock origin filters".into()),
                },
                AuditRow {
                    actor: "browser".into(),
                    count: 142,
                    decision: AuditDecision::Allowed,
                    detail: Some("telemetry.example.com (explain | block)".into()),
                },
            ],
            storage: vec![
                StorageRow {
                    plugin: "adblock".into(),
                    size_human: "2.3 MB".into(),
                    size_bytes: 2_411_724,
                    label: Some("filter lists".into()),
                },
                StorageRow {
                    plugin: "vim-mode".into(),
                    size_human: "12 KB".into(),
                    size_bytes: 12_288,
                    label: Some("config".into()),
                },
            ],
            denials: vec![],
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Permission-approval dialog
// ──────────────────────────────────────────────────────────────────────────────

/// The three narrowing modes for a single narrowable permission.
///
/// Rendered as a radio group in the approval dialog. Mirrors the design
/// specification in DESIGN.md §Permission Primitives — "Grant fully / Grant on
/// specific origins / Deny".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NarrowMode {
    /// Grant exactly as declared in the manifest.
    GrantFull,
    /// Grant only on the user-specified origin patterns (glob syntax).
    GrantOrigins(Vec<String>),
    /// Deny this permission entirely.
    Deny,
}

impl NarrowMode {
    /// Whether the permission is fully or partially granted (not denied).
    #[must_use]
    pub const fn is_granted(&self) -> bool {
        !matches!(self, Self::Deny)
    }
}

/// A single permission entry in the approval dialog, with its narrowing state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NarrowablePermission {
    /// The BARE permission domain, e.g. `page` (the first `domain:action`
    /// segment). The dialog renders `domain:action` for display; the narrowing
    /// op carries `domain` and `action` separately so the runtime's
    /// `GrantSet::narrow` matches the correct `(domain, action)` pair.
    pub domain: String,
    /// The BARE permission action, e.g. `inject_script` (the second
    /// `domain:action` segment). Paired with [`Self::domain`] for narrowing.
    pub action: String,
    /// The `[:resource]` part of the requested scope, e.g. `*` or
    /// `https://*.1password.com/*`. Empty string when there is no resource.
    pub requested_scope: String,
    /// The narrowing mode the user has chosen (or the default `GrantFull`).
    pub mode: NarrowMode,
    /// Human-readable explanation of what this permission allows.
    pub description: String,
    /// Whether this permission can be narrowed (i.e. has a resource component
    /// that can be constrained to specific origins).
    pub narrowable: bool,
    /// Whether this permission carries elevated risk (shown with a danger tint).
    pub high_risk: bool,
}

impl NarrowablePermission {
    /// The bare `domain:action` key (the display base for the permission row
    /// and the base of [`Self::effective_string`]).
    #[must_use]
    pub fn domain_action(&self) -> String {
        format!("{}:{}", self.domain, self.action)
    }

    /// Returns the effective permission string (`domain:action[:scope]`).
    ///
    /// `GrantFull` → `domain:action[:requested_scope]`; `GrantOrigins` →
    /// `domain:action:<origins joined by ,>`; `Deny` → empty (callers should
    /// check `mode` first). This is a derived display helper — it is never
    /// serialized across the bridge (the wire carries `domain`, `action`,
    /// `requested_scope`, and `mode` as separate fields).
    #[must_use]
    pub fn effective_string(&self) -> String {
        let base = self.domain_action();
        match &self.mode {
            NarrowMode::GrantFull => {
                if self.requested_scope.is_empty() {
                    base
                } else {
                    format!("{base}:{}", self.requested_scope)
                }
            }
            NarrowMode::GrantOrigins(origins) => {
                format!("{base}:{}", origins.join(","))
            }
            NarrowMode::Deny => String::new(),
        }
    }
}

/// The full view-model for the permission-approval dialog.
///
/// Rendered as a floating dialog when a new plugin is installed or an existing
/// plugin updates its manifest in a permission-expanding way.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApprovalRequest {
    /// Plugin name.
    pub plugin: String,
    /// Plugin version.
    pub version: String,
    /// Source provenance (from [`PluginKind::source_label`]).
    pub source: String,
    /// Permissions to show in the dialog, in display order.
    pub permissions: Vec<NarrowablePermission>,
    /// Dangerous combinations detected in this permission set (DISCIPLINES §4).
    /// Shown ABOVE the per-permission list. Empty = no warnings.
    pub dangerous_combinations: Vec<String>,
    /// Whether this is an update approval (vs. first install).
    pub is_update: bool,
    /// For updates: which permissions are new (not in the previously approved set).
    pub new_permissions: Vec<String>,
}

impl ApprovalRequest {
    /// Sample approval request for visual review.
    #[must_use]
    pub fn sample() -> Self {
        Self {
            plugin: "frontend-introspection-mcp".into(),
            version: "0.3.0".into(),
            source: "github:mote-browser/frontend-introspection-mcp @ a1b2c3d4e5f6".into(),
            permissions: vec![
                NarrowablePermission {
                    domain: "mcp".into(),
                    action: "server".into(),
                    requested_scope: "bind_loopback".into(),
                    mode: NarrowMode::GrantFull,
                    description: "exposes an mcp endpoint on localhost only".into(),
                    narrowable: false,
                    high_risk: false,
                },
                NarrowablePermission {
                    domain: "tabs".into(),
                    action: "list".into(),
                    requested_scope: String::new(),
                    mode: NarrowMode::GrantFull,
                    description: "read the list of open tabs in the current workspace".into(),
                    narrowable: false,
                    high_risk: false,
                },
                NarrowablePermission {
                    domain: "page".into(),
                    action: "read_dom".into(),
                    requested_scope: String::new(),
                    mode: NarrowMode::GrantFull,
                    description: "read the dom tree of any open page".into(),
                    narrowable: false,
                    high_risk: true,
                },
                NarrowablePermission {
                    domain: "page".into(),
                    action: "inject_script".into(),
                    requested_scope: "*".into(),
                    mode: NarrowMode::GrantOrigins(vec![
                        "https://localhost/*".into(),
                        "https://staging.example.com/*".into(),
                    ]),
                    description: "run scripts in web pages — currently narrowed to specific origins"
                        .into(),
                    narrowable: true,
                    high_risk: true,
                },
                NarrowablePermission {
                    domain: "introspect".into(),
                    action: "accessibility_tree".into(),
                    requested_scope: String::new(),
                    mode: NarrowMode::GrantFull,
                    description: "read the full accessibility tree of any page".into(),
                    narrowable: false,
                    high_risk: true,
                },
                NarrowablePermission {
                    domain: "introspect".into(),
                    action: "network_history".into(),
                    requested_scope: String::new(),
                    mode: NarrowMode::GrantFull,
                    description: "read full network request/response history including bodies"
                        .into(),
                    narrowable: false,
                    high_risk: true,
                },
            ],
            dangerous_combinations: vec![
                "page:read_dom + mcp:server — external agents can read page content from any tab via mcp".into(),
                "introspect:network_history + mcp:server — full network history (including auth tokens in headers) is reachable from external mcp clients".into(),
            ],
            is_update: false,
            new_permissions: vec![],
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_status_labels_are_lowercase() {
        for status in [
            IntegrityStatus::Verified,
            IntegrityStatus::Mismatch,
            IntegrityStatus::DevMode,
            IntegrityStatus::Bundled,
            IntegrityStatus::Unknown,
        ] {
            let label = status.label();
            assert_eq!(
                label,
                label.to_lowercase(),
                "label must be lowercase: {label}"
            );
        }
    }

    #[test]
    fn plugin_kind_source_labels_are_non_empty() {
        let kinds = [
            PluginKind::DeclaredGit {
                source: "github:mote-browser/adblock".into(),
                commit: "abc123def456".into(),
            },
            PluginKind::PathLocal {
                path: "~/plugins/x".into(),
            },
            PluginKind::ImplicitLocal {
                path: "~/.config/mote/plugins/x".into(),
            },
            PluginKind::DevMode {
                path: "~/code/x".into(),
            },
            PluginKind::Bundled,
        ];
        for kind in &kinds {
            assert!(!kind.source_label().is_empty());
        }
    }

    #[test]
    fn plugin_kind_git_commit_truncates_to_12() {
        let kind = PluginKind::DeclaredGit {
            source: "github:x/y".into(),
            commit: "abc123def456789extra".into(),
        };
        let label = kind.source_label();
        // Only the first 12 chars of the commit appear.
        assert!(label.contains("abc123def456"), "got: {label}");
        assert!(!label.contains("789extra"), "should be truncated: {label}");
    }

    #[test]
    fn plugin_row_is_dev_matches_kind() {
        let row_dev = PluginRow {
            name: "x".into(),
            version: "0.1".into(),
            fulfills: vec![],
            consumes: vec![],
            permissions: vec![],
            last_used: None,
            integrity: IntegrityStatus::DevMode,
            kind: PluginKind::DevMode {
                path: "~/code/x".into(),
            },
            actions: vec![],
        };
        assert!(row_dev.is_dev());

        let row_git = PluginRow {
            kind: PluginKind::DeclaredGit {
                source: "github:x/y".into(),
                commit: "abc123".into(),
            },
            integrity: IntegrityStatus::Verified,
            ..row_dev
        };
        assert!(!row_git.is_dev());
    }

    #[test]
    fn narrowed_count_counts_correctly() {
        let row = PluginRow {
            name: "p".into(),
            version: "1".into(),
            fulfills: vec![],
            consumes: vec![],
            permissions: vec![
                PermissionRow {
                    requested: "page:inject_script:*".into(),
                    effective: "page:inject_script:https://github.com/*".into(),
                    narrowed: true,
                    denied: false,
                },
                PermissionRow {
                    requested: "storage:persistent".into(),
                    effective: "storage:persistent".into(),
                    narrowed: false,
                    denied: false,
                },
            ],
            last_used: None,
            integrity: IntegrityStatus::Verified,
            kind: PluginKind::Bundled,
            actions: vec![],
        };
        assert_eq!(row.narrowed_count(), 1);
    }

    #[test]
    fn narrow_mode_grant_full_effective_string() {
        let perm = NarrowablePermission {
            domain: "page".into(),
            action: "inject_script".into(),
            requested_scope: "*".into(),
            mode: NarrowMode::GrantFull,
            description: String::new(),
            narrowable: true,
            high_risk: true,
        };
        assert_eq!(perm.domain_action(), "page:inject_script");
        assert_eq!(perm.effective_string(), "page:inject_script:*");
    }

    #[test]
    fn narrow_mode_grant_origins_effective_string() {
        let perm = NarrowablePermission {
            domain: "page".into(),
            action: "inject_script".into(),
            requested_scope: "*".into(),
            mode: NarrowMode::GrantOrigins(vec![
                "https://github.com/*".into(),
                "https://linear.app/*".into(),
            ]),
            description: String::new(),
            narrowable: true,
            high_risk: false,
        };
        let s = perm.effective_string();
        assert!(s.starts_with("page:inject_script:"), "got: {s}");
        assert!(s.contains("github.com"), "got: {s}");
        assert!(s.contains("linear.app"), "got: {s}");
    }

    #[test]
    fn narrow_mode_deny_effective_string_is_empty() {
        let perm = NarrowablePermission {
            domain: "tabs".into(),
            action: "list".into(),
            requested_scope: String::new(),
            mode: NarrowMode::Deny,
            description: String::new(),
            narrowable: false,
            high_risk: false,
        };
        assert_eq!(perm.effective_string(), "");
        assert!(!perm.mode.is_granted());
    }

    #[test]
    fn approval_request_round_trips_through_json() {
        let original = ApprovalRequest::sample();
        let json = serde_json::to_string(&original).expect("serialization must not fail");
        let deserialized: ApprovalRequest =
            serde_json::from_str(&json).expect("deserialization must not fail");
        assert_eq!(original, deserialized);
    }

    #[test]
    fn approval_request_json_contains_expected_content() {
        let original = ApprovalRequest::sample();
        let json = serde_json::to_string(&original).expect("serialization must not fail");

        // Plugin name is present.
        assert!(
            json.contains("frontend-introspection-mcp"),
            "plugin name missing from json"
        );

        // The wire carries `domain`, `action`, and `requested_scope` as separate
        // serialized fields (NOT the derived `effective_string`, which is a
        // display-only helper never sent across the bridge). Assert each is
        // present as its own serialized value, plus each GrantOrigins origin.
        for perm in &original.permissions {
            assert!(
                json.contains(&format!("\"domain\":\"{}\"", perm.domain)),
                "permission domain '{}' missing from json",
                perm.domain
            );
            assert!(
                json.contains(&format!("\"action\":\"{}\"", perm.action)),
                "permission action '{}' missing from json",
                perm.action
            );
            assert!(
                json.contains(&format!("\"requested_scope\":\"{}\"", perm.requested_scope)),
                "requested_scope '{}' missing from json",
                perm.requested_scope
            );
            if let NarrowMode::GrantOrigins(origins) = &perm.mode {
                for origin in origins {
                    assert!(
                        json.contains(origin.as_str()),
                        "narrowed origin '{origin}' missing from json"
                    );
                }
            }
        }

        // Every dangerous_combinations entry is present verbatim (plain ASCII,
        // never JSON-escaped).
        for combo in &original.dangerous_combinations {
            assert!(
                json.contains(combo.as_str()),
                "dangerous combination '{combo}' missing from json"
            );
        }
    }

    #[test]
    fn approval_request_json_contains_new_permissions_when_update() {
        // Construct a minimal is_update=true request so new_permissions appears.
        let req = ApprovalRequest {
            plugin: "my-plugin".into(),
            version: "1.0.0".into(),
            source: "github:example/my-plugin @ abc123".into(),
            permissions: vec![NarrowablePermission {
                domain: "tabs".into(),
                action: "list".into(),
                requested_scope: String::new(),
                mode: NarrowMode::GrantFull,
                description: "list tabs".into(),
                narrowable: false,
                high_risk: false,
            }],
            dangerous_combinations: vec![],
            is_update: true,
            new_permissions: vec!["tabs:list".into()],
        };
        let json = serde_json::to_string(&req).expect("serialization must not fail");
        assert!(
            json.contains("\"is_update\":true"),
            "is_update flag missing"
        );
        assert!(json.contains("tabs:list"), "new_permissions entry missing");
    }

    #[test]
    fn approval_request_sample_has_danger_combinations() {
        let req = ApprovalRequest::sample();
        assert!(
            !req.dangerous_combinations.is_empty(),
            "sample must include dangerous combinations"
        );
        // Combinations must appear above the permission list (content check).
        assert!(req.dangerous_combinations.iter().all(|c| !c.is_empty()));
    }

    #[test]
    fn integrity_panel_sample_has_plugins() {
        let panel = IntegrityPanel::sample();
        assert!(!panel.plugins.is_empty());
        // At least one dev-mode plugin in the sample.
        assert!(panel.plugins.iter().any(PluginRow::is_dev));
        // At least one verified plugin.
        assert!(
            panel
                .plugins
                .iter()
                .any(|p| p.integrity == IntegrityStatus::Verified)
        );
    }

    #[test]
    fn audit_row_decision_display_is_lowercase() {
        assert_eq!(AuditDecision::Blocked.to_string(), "blocked");
        assert_eq!(AuditDecision::Allowed.to_string(), "allowed");
        assert_eq!(AuditDecision::Modified.to_string(), "modified");
    }

    #[test]
    fn plugin_action_revoke_is_destructive() {
        assert!(PluginAction::Revoke.is_destructive());
        assert!(!PluginAction::Update.is_destructive());
        assert!(!PluginAction::AdjustScope.is_destructive());
    }
}
