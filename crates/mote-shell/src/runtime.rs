//! Plugin runtime wiring (driven by [`PluginManager`]) + the live
//! integrity-panel view-model.
//!
//! This module is the shell's bridge to the plugin subsystem. It:
//!
//! 1. **Instantiates the runtime** over the shared `mote-storage` [`Store`], a
//!    `mote-audit` [`AuditLog`] (whose `EventProducer` is fed into the runtime
//!    so every gatekept `mote.*` call is audited), and the v1 [`Registry`].
//! 2. **Resolves and loads the composed plugin set** via
//!    [`PluginManager::resolved_set`] (`plugins.lua` + `managed.lua` + the
//!    bundled first-party defaults). Each [`ResolvedPlugin`] is run through the
//!    approval coordinator ([`classify`]): auto-grant plugins (bundled,
//!    dev-mode, or unchanged/contracting since a prior approval) load
//!    immediately through [`mote_runtime::Runtime::load`]'s four-step pipeline
//!    with a [`DecidedPolicy`], and their approval is recorded; plugins that
//!    need the install/update dialog are parked in
//!    [`PluginHost::pending_approvals`] for Task 5 to resolve.
//! 3. **Builds the integrity-panel view-model from LIVE data** ([`build_panel`]):
//!    the loaded plugins (name / version / provenance-derived kind /
//!    requested→effective permissions / capabilities / integrity status), any
//!    awaiting-approval plugins, the audit query (recent activity → denials),
//!    and per-plugin `mote-storage` sizes.
//!
//! The rendered HTML is produced by [`render_panel_html`] and served as the
//! `mote://overlay/integrity.html` overlay surface; the shell composites it
//! full-window on the `Ctrl+Shift+I` keybind.

use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_pluginmgr::{IntegrityStatus as MgrIntegrity, PluginManager, Provenance, ResolvedPlugin};
use mote_registry::{CombinationRegistry, Registry};
use mote_runtime::{ApprovalHash, IdentityContext, Runtime};
use mote_storage::{IdentityScope, Store};
use mote_types::{IdentityId, PluginName, SchemaVersion};
use mote_ui::{
    AuditDecision, AuditRow, DenialRow, IntegrityPanel, IntegrityStatus, PermissionRow,
    PluginAction, PluginKind, PluginRow, StorageRow,
};

use crate::approval::{ApprovalOutcome, DecidedPolicy, classify};

/// The audit-log handle bundled with the runtime. The shell holds it so the
/// background audit thread stays alive and so the integrity panel can query it.
#[derive(Debug)]
pub(crate) struct PluginHost {
    pub(crate) runtime: Runtime,
    pub(crate) audit: AuditLog,
    pub(crate) store: Store,
    /// The plugin-management façade (cache, lock, approval store) resolving the
    /// composed spec set the shell loads from.
    pub(crate) manager: PluginManager,
    /// The plugins that loaded successfully, in dependency (load) order.
    pub(crate) loaded: Vec<ResolvedPlugin>,
    /// Plugins that resolved but require a user-facing approval dialog before
    /// they can load. Task 5 drives the dialog and finishes the load; Task 3
    /// only records them (and renders them as "awaiting approval" panel rows).
    pub(crate) pending_approvals: Vec<(ResolvedPlugin, mote_ui::ApprovalRequest)>,
}

impl PluginHost {
    /// Stand up the runtime over `store`, then resolve and load every plugin
    /// the composed spec set declares (plus the bundled first-party defaults).
    ///
    /// Loading is driven through the approval coordinator: each resolved plugin
    /// is [`classify`]d against its provenance + the approval store. Auto-grant
    /// plugins load immediately (and their approval is recorded); plugins that
    /// need the dialog are parked in [`Self::pending_approvals`] for Task 5.
    ///
    /// Reuses the shell's shared `mote-storage` [`Store`] so plugin storage,
    /// the audit sink, the session, and the approval store all live in one
    /// database. The manager's config/cache dirs are the canonical
    /// [`PluginManager::default_dirs`] so an approval recorded by the `mote`
    /// CLI is honored here and vice versa.
    ///
    /// # Errors
    /// Returns a boxed error only if the registry, audit log, or the canonical
    /// state directories cannot be resolved. A plugin that fails to load or
    /// classify is logged and skipped (the window keeps running); it does not
    /// abort startup.
    pub(crate) fn boot(store: Store) -> Result<Self, Box<dyn std::error::Error>> {
        // Canonical state dirs — shared with the CLI so approvals are mutual.
        let (config_dir, cache_dir) = PluginManager::default_dirs()
            .ok_or("cannot resolve Mote state directories ($XDG_*/$HOME unset)")?;
        Self::boot_in(store, &config_dir, &cache_dir)
    }

    /// Boots the plugin host against explicit config/cache directories.
    ///
    /// [`boot`](Self::boot) is the production entry-point (canonical dirs);
    /// this variant takes the dirs explicitly so headless tests can drive the
    /// full resolve → classify → load pass with tempdirs and an in-memory store.
    ///
    /// # Errors
    /// See [`boot`](Self::boot).
    pub(crate) fn boot_in(
        store: Store,
        config_dir: &std::path::Path,
        cache_dir: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let registry = Registry::load(SchemaVersion::V1)?;
        let combos = registry.combinations().clone();
        let audit = AuditLog::new(
            &store,
            Config {
                // The integrity panel reads recent activity from the ring; keep a
                // generous window. Flush promptly so history survives a crash.
                ring_capacity: 512,
                flush_threshold: 16,
                flush_interval: Duration::from_millis(250),
            },
        )?;
        let runtime = Runtime::new(registry, store.clone(), audit.producer());

        let manager = PluginManager::new(config_dir, cache_dir, &store);

        // Resolve the composed spec set (declared + seeded bundled defaults),
        // reconciled (fetched/linked/hashed) by the manager.
        let resolved = manager.resolved_set()?;

        let identity = IdentityContext::new(IdentityId::new(super::SESSION_IDENTITY));
        let mut host = Self {
            runtime,
            audit,
            store,
            manager,
            loaded: Vec::new(),
            pending_approvals: Vec::new(),
        };
        host.load_resolved(resolved, identity, &combos);
        Ok(host)
    }

    /// Classifies and loads each resolved plugin (auto-grant path only here;
    /// dialog-gated plugins are parked in [`Self::pending_approvals`]).
    fn load_resolved(
        &mut self,
        resolved: Vec<ResolvedPlugin>,
        identity: IdentityContext,
        combos: &CombinationRegistry,
    ) {
        let total = resolved.len();
        for rp in resolved {
            let outcome = match classify(
                &rp.manifest,
                rp.provenance,
                self.manager.approval_store(),
                combos,
            ) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!(
                        "mote-shell: plugin `{}` could not be classified: {e}",
                        rp.name
                    );
                    continue;
                }
            };

            match outcome {
                ApprovalOutcome::AutoGrant => {
                    match self
                        .runtime
                        .load(&rp.init_source, identity, &DecidedPolicy::grant())
                    {
                        Ok(running) => {
                            eprintln!(
                                "mote-shell: loaded plugin `{}` (caps: {:?}, perms: {})",
                                running.name,
                                running.capabilities,
                                running.effective_permissions.len()
                            );
                            // Record the approval so a later launch (or the CLI)
                            // sees this manifest as approved — but NOT for
                            // bundled plugins. `classify` short-circuits on
                            // `Provenance::Bundled` without ever reading the
                            // store, so a stored entry for a bundled plugin
                            // would be dead data that also pollutes what the
                            // `mote plugin` CLI enumerates from the store.
                            if !matches!(rp.provenance, Provenance::Bundled)
                                && let Err(e) = self
                                    .manager
                                    .approval_store()
                                    .put(&rp.name, &ApprovalHash::of(&rp.manifest))
                            {
                                eprintln!(
                                    "mote-shell: failed to record approval for `{}`: {e}",
                                    rp.name
                                );
                            }
                            self.loaded.push(rp);
                        }
                        Err(e) => {
                            eprintln!("mote-shell: plugin `{}` failed to load: {e}", rp.name);
                        }
                    }
                }
                ApprovalOutcome::NeedsDialog(req) => {
                    eprintln!(
                        "mote-shell: plugin `{}` awaiting approval (dialog deferred to Task 5)",
                        rp.name
                    );
                    self.pending_approvals.push((rp, req));
                }
            }
        }
        eprintln!(
            "mote-shell: plugin runtime up; {}/{} plugins loaded, {} awaiting approval",
            self.loaded.len(),
            total,
            self.pending_approvals.len()
        );
    }

    /// Build the integrity-panel view-model from the host's LIVE state.
    ///
    /// Each loaded plugin renders with its real provenance-derived
    /// [`PluginKind`], the integrity status the manager computed, and the action
    /// set appropriate to its kind. Plugins awaiting approval (parked in
    /// [`Self::pending_approvals`]) render as rows marked with
    /// [`IntegrityStatus::Unknown`] — see the note on `pending_panel_row` for
    /// the mote-ui variant gap.
    pub(crate) fn build_panel(&self) -> IntegrityPanel {
        let mut plugins: Vec<PluginRow> =
            self.loaded.iter().map(|rp| self.loaded_row(rp)).collect();
        // Plugins blocked on the approval dialog render as awaiting-approval rows.
        plugins.extend(
            self.pending_approvals
                .iter()
                .map(|(rp, _req)| pending_row(rp)),
        );

        let query = self.audit.query();

        // Network/activity summary: collapse recent events into per-plugin counts.
        let mut counts: Vec<(String, usize)> = query
            .counts_per_plugin()
            .into_iter()
            .map(|(p, c)| (p.as_str().to_owned(), c))
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let network_audit = counts
            .into_iter()
            .map(|(actor, count)| AuditRow {
                actor,
                count: count as u64,
                decision: AuditDecision::Allowed,
                detail: None,
            })
            .collect();

        let denials = query
            .recent_denials(20)
            .into_iter()
            .map(|ev| DenialRow {
                plugin: ev.plugin.as_str().to_owned(),
                permission: ev.operation,
                when: relative_time(ev.timestamp),
            })
            .collect();

        let storage = self
            .loaded
            .iter()
            .filter_map(|rp| {
                let bytes = self.storage_bytes(&rp.name);
                (bytes > 0).then(|| StorageRow {
                    plugin: rp.name.as_str().to_owned(),
                    size_human: human_bytes(bytes),
                    size_bytes: bytes,
                    label: None,
                })
            })
            .collect();

        IntegrityPanel {
            plugins,
            network_audit,
            storage,
            denials,
        }
    }

    /// Builds the panel row for a loaded plugin from its live runtime state.
    ///
    /// Effective permissions come from the running instance; for an
    /// auto-granted plugin the effective set IS the requested set, so each
    /// renders as its own (un-narrowed, un-denied) form.
    fn loaded_row(&self, rp: &ResolvedPlugin) -> PluginRow {
        let permissions = self
            .runtime
            .running(&rp.name)
            .map(|running| {
                running
                    .effective_permissions
                    .iter()
                    .map(|p| PermissionRow {
                        requested: p.clone(),
                        effective: p.clone(),
                        narrowed: false,
                        denied: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let kind = provenance_to_kind(rp.provenance, &rp.dir);
        PluginRow {
            name: rp.name.as_str().to_owned(),
            version: rp.manifest.version.clone(),
            fulfills: rp.manifest.capabilities.clone(),
            consumes: rp.manifest.consumes.clone(),
            permissions,
            last_used: self.last_used(&rp.name),
            integrity: mgr_integrity_to_ui(&rp.integrity),
            actions: actions_for_kind(&kind),
            kind,
        }
    }

    /// The human-relative timestamp of this plugin's most recent audited call.
    fn last_used(&self, name: &PluginName) -> Option<String> {
        self.audit
            .query()
            .recent_for_plugin(name, usize::MAX)
            .last()
            .map(|ev| relative_time(ev.timestamp))
    }

    /// Total bytes a plugin's storage namespace occupies (summing value sizes
    /// across its keys). Bundled plugins default to the `Global` scope.
    fn storage_bytes(&self, name: &PluginName) -> u64 {
        let ns = self.store.namespace(name, IdentityScope::Global);
        let Ok(keys) = ns.list_keys() else {
            return 0;
        };
        keys.iter()
            .filter_map(|k| ns.get(k).ok().flatten())
            .map(|v| v.len() as u64)
            .sum()
    }
}

/// Maps a plugin's [`Provenance`] to the integrity-panel [`PluginKind`].
///
/// `dir` is the resolved active directory: for `path:` it is the user's real
/// directory; for git-backed plugins it is the cache commit dir
/// (`<cache>/<name>/<commit>`), so its final component is the resolved commit.
///
/// Fidelity gap: [`ResolvedPlugin`] does not carry the raw `github:`/`git+`
/// source string, so `DeclaredGit.source` falls back to the directory path. The
/// commit is recovered from the cache-dir name. `ImplicitLocal`/`DevMode`
/// provenances are Task 6 and are not produced by [`PluginManager::resolved_set`]
/// today; they map to their nearest mote-ui kind for completeness.
fn provenance_to_kind(provenance: Provenance, dir: &std::path::Path) -> PluginKind {
    let path = dir.display().to_string();
    match provenance {
        Provenance::Bundled => PluginKind::Bundled,
        Provenance::Path => PluginKind::PathLocal { path },
        Provenance::DeclaredGit => {
            let commit = dir
                .file_name()
                .and_then(|c| c.to_str())
                .unwrap_or_default()
                .to_owned();
            PluginKind::DeclaredGit {
                source: path,
                commit,
            }
        }
        Provenance::ImplicitLocal => PluginKind::ImplicitLocal { path },
        Provenance::DevMode => PluginKind::DevMode { path },
    }
}

/// Maps the pluginmgr [`MgrIntegrity`] to the integrity-panel [`IntegrityStatus`].
///
/// `PathLocal` (informational `path:`/implicit hash changes) maps to `Verified`
/// — a `path:` plugin whose lock entry matches is verified; mote-ui has no
/// distinct "path-local informational" status.
const fn mgr_integrity_to_ui(status: &MgrIntegrity) -> IntegrityStatus {
    match status {
        MgrIntegrity::Verified | MgrIntegrity::PathLocal => IntegrityStatus::Verified,
        MgrIntegrity::Mismatch { .. } => IntegrityStatus::Mismatch,
        MgrIntegrity::Bundled => IntegrityStatus::Bundled,
        MgrIntegrity::Unknown => IntegrityStatus::Unknown,
    }
}

/// The action set offered on a plugin card, by [`PluginKind`].
///
/// - git → update / rollback / revoke / adjust scope
/// - path & dev → reload / revoke / adjust scope
/// - bundled → update / revoke
/// - implicit-local → revoke / adjust scope (no update/rollback source)
fn actions_for_kind(kind: &PluginKind) -> Vec<PluginAction> {
    match kind {
        PluginKind::DeclaredGit { .. } => vec![
            PluginAction::Update,
            PluginAction::Rollback,
            PluginAction::Revoke,
            PluginAction::AdjustScope,
        ],
        PluginKind::PathLocal { .. } | PluginKind::DevMode { .. } => vec![
            PluginAction::Reload,
            PluginAction::Revoke,
            PluginAction::AdjustScope,
        ],
        PluginKind::Bundled => vec![PluginAction::Update, PluginAction::Revoke],
        PluginKind::ImplicitLocal { .. } => {
            vec![PluginAction::Revoke, PluginAction::AdjustScope]
        }
    }
}

/// Builds the panel row for a plugin awaiting the approval dialog.
///
/// The manifest's declared permissions render as requested (no effective grant
/// exists yet — the plugin is not loaded). mote-ui has no "awaiting approval"
/// [`IntegrityStatus`] variant, so [`IntegrityStatus::Unknown`] is the nearest
/// existing state (verification has not run because the plugin is not yet
/// approved/loaded). A dedicated variant is a frontend change for a later task.
fn pending_row(rp: &ResolvedPlugin) -> PluginRow {
    let kind = provenance_to_kind(rp.provenance, &rp.dir);
    let permissions = rp
        .manifest
        .permissions
        .iter()
        .map(|p| PermissionRow {
            requested: p.clone(),
            effective: p.clone(),
            narrowed: false,
            denied: false,
        })
        .collect();
    PluginRow {
        name: rp.name.as_str().to_owned(),
        version: rp.manifest.version.clone(),
        fulfills: rp.manifest.capabilities.clone(),
        consumes: rp.manifest.consumes.clone(),
        permissions,
        last_used: None,
        integrity: IntegrityStatus::Unknown,
        kind,
        actions: Vec::new(),
    }
}

/// A section heading with the `[label]` lockup and an optional count badge.
fn section_head(label: &str, count: Option<usize>) -> String {
    let count = count.map_or_else(String::new, |n| {
        format!("<span class=\"section-count\">{n}</span>")
    });
    format!(
        "<section class=\"integrity-section\"><div class=\"integrity-section-head\">\
         <span class=\"section-label\"><span class=\"br\">[</span>{label}\
         <span class=\"br\">]</span></span>{count}</div>"
    )
}

/// The active-plugins section: one card per loaded plugin (name / version /
/// provenance / fulfilled capabilities / requested→effective permissions).
fn plugins_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("active plugins", Some(panel.plugins.len()));
    if panel.plugins.is_empty() {
        s.push_str("<p class=\"empty\">no plugins loaded</p>");
    }
    for p in &panel.plugins {
        let _ = write!(
            s,
            "<article class=\"plugin-card\"><header class=\"plugin-card-head\">\
             <div class=\"plugin-card-name\"><span class=\"prov-glyph\">{}</span> {}</div>\
             <span class=\"plugin-card-version\">v{}</span>\
             <div class=\"plugin-card-badges\"><span class=\"badge {}\">\
             <span class=\"dot\"></span>{}</span></div></header>\
             <div class=\"plugin-provenance\"><span class=\"prov-label\">source</span> \
             <span class=\"prov-value\">{}</span></div>",
            esc(p.kind.glyph()),
            esc(&p.name),
            esc(&p.version),
            esc(p.integrity.badge_variant()),
            esc(p.integrity.label()),
            esc(&p.kind.source_label()),
        );
        if !p.fulfills.is_empty() {
            let caps = p
                .fulfills
                .iter()
                .map(|c| format!("<code>{}</code>", esc(c)))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                s,
                "<div class=\"plugin-caps\"><span class=\"prov-label\">fulfills</span> {caps}</div>"
            );
        }
        s.push_str("<ul class=\"perm-list\">");
        for perm in &p.permissions {
            let narrowed = if perm.narrowed {
                format!(" → <code>{}</code>", esc(&perm.effective))
            } else {
                String::new()
            };
            let _ = write!(
                s,
                "<li class=\"perm-row\"><code>{}</code>{narrowed}</li>",
                esc(&perm.requested),
            );
        }
        s.push_str("</ul>");
        if let Some(used) = &p.last_used {
            let _ = write!(
                s,
                "<div class=\"plugin-lastused\"><span class=\"prov-label\">last used</span> {}</div>",
                esc(used)
            );
        }
        s.push_str("</article>");
    }
    s.push_str("</section>");
    s
}

/// The activity-audit section: per-plugin call counts from the audit ring.
fn audit_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("activity audit", None);
    if panel.network_audit.is_empty() {
        s.push_str("<p class=\"empty\">no audited activity yet</p>");
    } else {
        s.push_str("<ul class=\"audit-list\">");
        for row in &panel.network_audit {
            let _ = write!(
                s,
                "<li class=\"audit-row\"><span class=\"audit-actor\">{}</span> \
                 <span class=\"badge\">{}</span> <span class=\"audit-count\">{} calls</span></li>",
                esc(&row.actor),
                esc(&row.decision.to_string()),
                row.count
            );
        }
        s.push_str("</ul>");
    }
    s.push_str("</section>");
    s
}

/// The storage section: per-plugin storage-namespace sizes.
fn storage_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("storage", None);
    if panel.storage.is_empty() {
        s.push_str("<p class=\"empty\">no plugin storage in use</p>");
    } else {
        s.push_str("<ul class=\"storage-list\">");
        for row in &panel.storage {
            let _ = write!(
                s,
                "<li class=\"storage-row\"><span>{}</span> <span>{}</span></li>",
                esc(&row.plugin),
                esc(&row.size_human)
            );
        }
        s.push_str("</ul>");
    }
    s.push_str("</section>");
    s
}

/// The permission-denials section: recent `Decision::Deny` audit events.
fn denials_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("permission denials", None);
    if panel.denials.is_empty() {
        s.push_str("<p class=\"empty\">no permission denials</p>");
    } else {
        s.push_str("<ul class=\"denial-list\">");
        for d in &panel.denials {
            let _ = write!(
                s,
                "<li class=\"denial-row\"><span>{}</span> <code>{}</code> \
                 <span class=\"denial-when\">{}</span></li>",
                esc(&d.plugin),
                esc(&d.permission),
                esc(&d.when)
            );
        }
        s.push_str("</ul>");
    }
    s.push_str("</section>");
    s
}

/// Render the integrity-panel view-model to a self-contained `mote://chrome`
/// HTML document, reusing `mote-ui`'s design-token + component CSS.
///
/// `mote-ui`'s `IntegrityPanel` has no `to_html` renderer and its shipped
/// `INTEGRITY_PANEL_HTML` is static sample markup with no data-injection seam,
/// so the shell renders the live document here (the doc-comment's "drive the
/// chrome surface directly" path). All plugin-derived strings are HTML-escaped.
pub(crate) fn render_panel_html(panel: &IntegrityPanel) -> String {
    let body = format!(
        "{}{}{}{}",
        plugins_section(panel),
        audit_section(panel),
        storage_section(panel),
        denials_section(panel),
    );
    format!(
        "<!doctype html><html lang=\"en\" data-theme=\"dusk\"><head>\
         <meta charset=\"utf-8\" />\
         <link rel=\"stylesheet\" href=\"tokens.css\" />\
         <link rel=\"stylesheet\" href=\"base.css\" />\
         <link rel=\"stylesheet\" href=\"components/badge.css\" />\
         <link rel=\"stylesheet\" href=\"components/card.css\" />\
         <link rel=\"stylesheet\" href=\"components/integrity-panel.css\" />\
         <style>body{{background:var(--bg);color:var(--fg);padding:var(--space-4);\
         font:14px/1.5 system-ui,sans-serif}}\
         .perm-list,.audit-list,.storage-list,.denial-list{{list-style:none;padding:0;margin:0}}\
         .perm-row,.audit-row,.storage-row,.denial-row{{padding:2px 0}}\
         .integrity-section{{margin-bottom:var(--space-4)}}\
         .plugin-card{{margin:var(--space-2) 0;padding:var(--space-3)}}\
         code{{font-family:ui-monospace,monospace}}\
         .empty{{opacity:.6}} .br{{opacity:.5}}</style>\
         <title>[integrity] — mote</title></head>\
         <body><div class=\"integrity-panel\" role=\"main\">\
         <header class=\"integrity-header\"><span class=\"lockup\">\
         <span class=\"br\">[</span><span class=\"name\">integrity</span>\
         <span class=\"br\">]</span></span>\
         <p class=\"subhead\">active plugins · activity audit · storage · permission denials \
         · press esc / ctrl+shift+i to close</p></header>{body}</div></body></html>"
    )
}

/// HTML-escape a string for safe insertion as text/attribute content. The
/// integrity panel renders plugin-derived strings (names, permissions); each is
/// escaped so a hostile manifest cannot inject markup into the privileged
/// chrome surface (bridge.rs caller discipline).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A coarse human-relative timestamp ("just now", "3m ago", "2h ago", "5d ago").
fn relative_time(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map_or(0, |d| d.as_secs());
    if secs < 5 {
        "just now".to_owned()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// A compact human byte size (B / KB / MB).
#[allow(
    clippy::cast_precision_loss,
    reason = "plugin storage sizes are far below 2^52; the f64 division is exact for display"
)]
fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(name: &str, provenance: Provenance, integrity: MgrIntegrity) -> ResolvedPlugin {
        let src = format!(
            r#"
            local M = {{}}
            M.manifest = {{
                schema = "v1",
                name = "{name}",
                version = "1.2.3",
                permissions = {{ "storage:persistent" }},
            }}
            return M
        "#
        );
        let manifest = mote_lua::load_plugin(&src, name)
            .unwrap()
            .manifest()
            .clone();
        ResolvedPlugin {
            name: PluginName::new(name).unwrap(),
            provenance,
            dir: std::path::PathBuf::from(format!("/tmp/plugins/{name}")),
            manifest,
            integrity,
            init_source: src,
        }
    }

    #[test]
    fn provenance_maps_to_plugin_kind() {
        assert!(matches!(
            provenance_to_kind(Provenance::Bundled, std::path::Path::new("/x")),
            PluginKind::Bundled
        ));
        assert!(matches!(
            provenance_to_kind(Provenance::Path, std::path::Path::new("/x")),
            PluginKind::PathLocal { .. }
        ));
        // Git dir's final component is the resolved commit.
        match provenance_to_kind(
            Provenance::DeclaredGit,
            std::path::Path::new("/cache/adblock/abc123"),
        ) {
            PluginKind::DeclaredGit { commit, .. } => assert_eq!(commit, "abc123"),
            other => panic!("expected DeclaredGit, got {other:?}"),
        }
    }

    #[test]
    fn mgr_integrity_maps_to_ui_integrity() {
        assert_eq!(
            mgr_integrity_to_ui(&MgrIntegrity::Bundled),
            IntegrityStatus::Bundled
        );
        assert_eq!(
            mgr_integrity_to_ui(&MgrIntegrity::Verified),
            IntegrityStatus::Verified
        );
        assert_eq!(
            mgr_integrity_to_ui(&MgrIntegrity::PathLocal),
            IntegrityStatus::Verified
        );
        assert_eq!(
            mgr_integrity_to_ui(&MgrIntegrity::Unknown),
            IntegrityStatus::Unknown
        );
        assert_eq!(
            mgr_integrity_to_ui(&MgrIntegrity::Mismatch {
                actual: mote_types::Checksum::hash(b"a"),
                expected: mote_types::Checksum::hash(b"b"),
            }),
            IntegrityStatus::Mismatch
        );
    }

    #[test]
    fn actions_match_plugin_kind() {
        let git = actions_for_kind(&PluginKind::DeclaredGit {
            source: "github:x/y".into(),
            commit: "abc".into(),
        });
        assert_eq!(
            git,
            vec![
                PluginAction::Update,
                PluginAction::Rollback,
                PluginAction::Revoke,
                PluginAction::AdjustScope,
            ]
        );
        let path = actions_for_kind(&PluginKind::PathLocal { path: "/x".into() });
        assert_eq!(
            path,
            vec![
                PluginAction::Reload,
                PluginAction::Revoke,
                PluginAction::AdjustScope,
            ]
        );
        let bundled = actions_for_kind(&PluginKind::Bundled);
        assert_eq!(bundled, vec![PluginAction::Update, PluginAction::Revoke]);
    }

    #[test]
    fn pending_row_is_marked_awaiting_with_no_actions() {
        let rp = resolved("needs-approval", Provenance::Path, MgrIntegrity::Unknown);
        let row = pending_row(&rp);
        assert_eq!(row.name, "needs-approval");
        assert_eq!(row.version, "1.2.3");
        // Awaiting approval is represented by Unknown integrity (no mote-ui
        // dedicated variant) and an empty action set.
        assert_eq!(row.integrity, IntegrityStatus::Unknown);
        assert!(row.actions.is_empty(), "pending row offers no actions");
        assert!(matches!(row.kind, PluginKind::PathLocal { .. }));
        // The declared permission is surfaced as requested.
        assert_eq!(row.permissions.len(), 1);
        assert_eq!(row.permissions[0].requested, "storage:persistent");
    }

    #[test]
    fn bundled_loaded_row_kind_integrity_actions() {
        // A bundled plugin maps to Bundled kind/integrity and update+revoke.
        let rp = resolved("urlbar", Provenance::Bundled, MgrIntegrity::Bundled);
        let kind = provenance_to_kind(rp.provenance, &rp.dir);
        assert!(matches!(kind, PluginKind::Bundled));
        assert_eq!(mgr_integrity_to_ui(&rp.integrity), IntegrityStatus::Bundled);
        assert_eq!(
            actions_for_kind(&kind),
            vec![PluginAction::Update, PluginAction::Revoke]
        );
    }

    #[test]
    fn esc_neutralizes_markup() {
        assert_eq!(esc("<script>"), "&lt;script&gt;");
        assert_eq!(esc("a&b\"c'"), "a&amp;b&quot;c&#39;");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert!(human_bytes(3_000_000).ends_with("MB"));
    }

    #[test]
    fn render_panel_html_includes_loaded_plugins() {
        let panel = IntegrityPanel {
            plugins: vec![PluginRow {
                name: "urlbar".into(),
                version: "0.1.0".into(),
                fulfills: vec!["ui:urlbar_provider".into()],
                consumes: vec![],
                permissions: vec![PermissionRow {
                    requested: "events:emit".into(),
                    effective: "events:emit".into(),
                    narrowed: false,
                    denied: false,
                }],
                last_used: None,
                integrity: IntegrityStatus::Bundled,
                kind: PluginKind::Bundled,
                actions: vec![],
            }],
            network_audit: vec![],
            storage: vec![],
            denials: vec![],
        };
        let html = render_panel_html(&panel);
        assert!(html.contains("urlbar"));
        assert!(html.contains("ui:urlbar_provider"));
        assert!(html.contains("v0.1.0"));
        assert!(html.contains("bundled"));
        // No sample data leaked in.
        assert!(!html.contains("1password"));
        assert!(!html.contains("vim-mode"));
    }

    #[test]
    fn empty_panel_renders_placeholders() {
        let html = render_panel_html(&IntegrityPanel::empty());
        assert!(html.contains("no plugins loaded"));
    }

    // -----------------------------------------------------------------------
    // Task 3b: PluginHost boot drives loading through the approval coordinator
    // -----------------------------------------------------------------------

    /// Writes a minimal valid path: plugin dir.
    fn write_path_plugin(dir: &std::path::Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let lua = format!(
            r#"
local M = {{}}
M.manifest = {{
    schema = "v1",
    name = "{name}",
    version = "0.1.0",
    permissions = {{ "storage:persistent" }},
    identity_scope = "global",
}}
function M.setup() end
return M
"#
        );
        std::fs::write(dir.join("init.lua"), lua).unwrap();
    }

    /// Writes a `plugins.lua` declaring the given `(name, source)` entries.
    fn write_plugins_lua(config_dir: &std::path::Path, entries: &[(&str, &str)]) {
        let body = entries
            .iter()
            .map(|(k, src)| format!(r#"  ["{k}"] = {{ source = "{src}" }},"#))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            config_dir.join("plugins.lua"),
            format!("mote.plugins({{\n{body}\n}})\n"),
        )
        .unwrap();
    }

    #[test]
    fn boot_auto_grants_bundled_and_prior_approved_path_plugin() {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin(plugin_dir.path(), "approved-plugin");

        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[("approved-plugin", &src)]);

        let store = Store::open_in_memory().unwrap();

        // Pre-seed the approval store with the path plugin's current manifest
        // hash, so classify auto-grants it (unchanged since prior approval).
        let manifest = mote_lua::load_plugin(
            &std::fs::read_to_string(plugin_dir.path().join("init.lua")).unwrap(),
            "approved-plugin",
        )
        .unwrap()
        .manifest()
        .clone();
        let approval = mote_pluginmgr::ApprovalStore::new(&store);
        approval
            .put(&manifest.name, &ApprovalHash::of(&manifest))
            .unwrap();

        let host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();

        let loaded: Vec<&str> = host.loaded.iter().map(|r| r.name.as_str()).collect();
        assert!(
            host.pending_approvals.is_empty(),
            "all plugins must auto-grant; pending: {:?}",
            host.pending_approvals
                .iter()
                .map(|(r, _)| r.name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(loaded.contains(&"approved-plugin"), "got {loaded:?}");
        assert!(
            loaded.contains(&"urlbar"),
            "bundled urlbar loaded: {loaded:?}"
        );
        assert!(
            loaded.contains(&"workspace-manager"),
            "bundled workspace-manager loaded: {loaded:?}"
        );
    }

    #[test]
    fn boot_does_not_pollute_approval_store_with_bundled_plugins() {
        // The approval store is read-mediated by classify(); bundled plugins
        // short-circuit without consulting it, so an entry for a bundled name
        // is dead data + pollutes `mote plugin` CLI enumeration. Only the
        // pre-approved path plugin should appear.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin(plugin_dir.path(), "approved-plugin");

        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[("approved-plugin", &src)]);

        let store = Store::open_in_memory().unwrap();
        let manifest = mote_lua::load_plugin(
            &std::fs::read_to_string(plugin_dir.path().join("init.lua")).unwrap(),
            "approved-plugin",
        )
        .unwrap()
        .manifest()
        .clone();
        let approval = mote_pluginmgr::ApprovalStore::new(&store);
        approval
            .put(&manifest.name, &ApprovalHash::of(&manifest))
            .unwrap();

        let _host = PluginHost::boot_in(store.clone(), config.path(), cache.path()).unwrap();

        let approval = mote_pluginmgr::ApprovalStore::new(&store);
        let names: Vec<String> = approval
            .list()
            .unwrap()
            .into_iter()
            .map(|n| n.as_str().to_owned())
            .collect();
        assert_eq!(
            names,
            vec!["approved-plugin".to_owned()],
            "approval store must contain only the path plugin's entry; \
             bundled plugins must NOT be recorded (got: {names:?})"
        );
    }

    #[test]
    fn boot_leaves_never_approved_path_plugin_pending() {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin(plugin_dir.path(), "unapproved-plugin");

        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[("unapproved-plugin", &src)]);

        // No approval pre-seeded: the path plugin is a first install → dialog.
        let store = Store::open_in_memory().unwrap();
        let host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();

        let loaded: Vec<&str> = host.loaded.iter().map(|r| r.name.as_str()).collect();
        let pending: Vec<&str> = host
            .pending_approvals
            .iter()
            .map(|(r, _)| r.name.as_str())
            .collect();

        assert!(
            !loaded.contains(&"unapproved-plugin"),
            "never-approved path plugin must NOT load: {loaded:?}"
        );
        assert_eq!(
            pending,
            vec!["unapproved-plugin"],
            "exactly the unapproved plugin is pending"
        );
        // Bundled defaults still auto-grant + load regardless.
        assert!(
            loaded.contains(&"urlbar"),
            "bundled still loads: {loaded:?}"
        );
        assert!(
            loaded.contains(&"workspace-manager"),
            "bundled still loads: {loaded:?}"
        );
    }
}
