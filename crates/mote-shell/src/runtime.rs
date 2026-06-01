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
//!    bundled first-party defaults) in [`PluginHost::run_initial_load_pass`],
//!    which the shell runs *after* the window is live (so a slow/offline git
//!    fetch never blocks startup and a fatal resolution error never prevents
//!    the window opening). Each [`ResolvedPlugin`] is run through the approval
//!    coordinator ([`classify`]): auto-grant plugins (bundled, dev-mode, or
//!    unchanged/contracting since a prior approval) load immediately through
//!    [`mote_runtime::Runtime::load`]'s four-step pipeline with a
//!    [`DecidedPolicy`], and their approval is recorded; plugins that need the
//!    install/update dialog are parked in [`PluginHost::pending_approvals`] and
//!    the shell renders the dialog (Task 5).
//! 3. **Builds the integrity-panel view-model from LIVE data** ([`build_panel`]):
//!    the loaded plugins (name / version / provenance-derived kind /
//!    requested→effective permissions / capabilities / integrity status), any
//!    awaiting-approval plugins, the audit query (recent activity → denials),
//!    and per-plugin `mote-storage` sizes.
//!
//! The rendered HTML is produced by [`render_panel_html`] and served as the
//! `mote://overlay/integrity.html` overlay surface; the shell composites it
//! full-window on the `Ctrl+Shift+I` keybind.

use std::rc::Rc;
use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_pluginmgr::{
    IntegrityStatus as MgrIntegrity, PluginManager, Provenance, ResolvedPlugin, UpdateOutcome,
    build_secret_resolver,
};
use mote_registry::{CombinationRegistry, Registry};
use mote_runtime::{Approval, ApprovalHash, IdentityContext, Narrowing, Runtime};
use mote_secrets::SecretResolver;
use mote_storage::{IdentityScope, Store};
use mote_types::{IdentityId, PluginName, SchemaVersion};
use mote_ui::{
    ApprovalRequest, AuditDecision, AuditRow, DenialRow, IntegrityPanel, IntegrityStatus,
    PermissionRow, PluginAction, PluginKind, PluginRow, SecretAccessRow, StorageRow,
};

use crate::approval::{
    ApprovalOutcome, DecidedPolicy, DialogResult, approval_from_dialog, build_update_request,
    classify,
};

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
    /// The dangerous-combination registry, retained from boot so the deferred
    /// load pass and the re-approval (update) path can re-classify manifests
    /// without reloading the registry.
    combos: CombinationRegistry,
    /// The plugins that loaded successfully, in dependency (load) order.
    pub(crate) loaded: Vec<ResolvedPlugin>,
    /// Plugins that resolved but require a user-facing approval dialog before
    /// they can load. Task 5 drives the dialog and finishes the load; Task 3
    /// only records them (and renders them as "awaiting approval" panel rows).
    pub(crate) pending_approvals: Vec<(ResolvedPlugin, ApprovalRequest)>,
    /// Secret resolver loaded from `secrets.lua` at boot, used by
    /// `build_panel` to look up backend labels for the integrity panel's
    /// per-plugin secret rows (Task 9).
    secret_resolver: Rc<SecretResolver>,
}

/// The result of resolving a dialog approval in [`PluginHost::approve_pending`].
///
/// The shell maps each variant to the chrome update it performs (hide the
/// dialog, re-render the panel, log the failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApproveOutcome {
    /// The plugin was approved and loaded; its approval was recorded and it
    /// moved from `pending_approvals` into `loaded`.
    Loaded,
    /// The user denied the plugin (overall or per-permission); the pending
    /// entry was dropped and the denial audited.
    Denied,
    /// The user approved but the runtime load failed; the entry stays pending.
    LoadFailed,
    /// No pending entry matched the result's plugin name; the result was dropped
    /// (an approve for a non-pending plugin — stale or spoofed).
    NotPending,
}

/// The result of a panel `plugin_update` action in [`PluginHost::update_plugin`].
#[derive(Debug)]
pub(crate) enum UpdateAction {
    /// The update expanded the approval surface; a re-approval dialog must be
    /// shown. Carries the request the shell renders.
    ReApproval(ApprovalRequest),
    /// A non-expanding update was applied and the plugin reloaded.
    Applied,
    /// The update or the subsequent reload failed (logged + audited).
    Failed,
}

impl PluginHost {
    /// Stand up the runtime + manager + audit over `store`. **Does not load any
    /// plugins** — the resolve/sync/load pass runs later in
    /// [`run_initial_load_pass`](Self::run_initial_load_pass), once the window
    /// is live (so a slow/offline git fetch never blocks startup and a fatal
    /// resolution error never prevents the window from opening).
    ///
    /// Reuses the shell's shared `mote-storage` [`Store`] so plugin storage,
    /// the audit sink, the session, and the approval store all live in one
    /// database. The manager's config/cache dirs are the canonical
    /// [`PluginManager::default_dirs`] so an approval recorded by the `mote`
    /// CLI is honored here and vice versa.
    ///
    /// # Errors
    /// Returns a boxed error only if the registry, audit log, or the canonical
    /// state directories cannot be resolved.
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
    /// resolve → classify → load pass with tempdirs and an in-memory store.
    ///
    /// **No plugin loading happens here.** Construction only stands up the
    /// runtime + manager + audit; the resolve/sync/load pass runs later in
    /// [`run_initial_load_pass`](Self::run_initial_load_pass), which the shell
    /// calls *after* the window is live. This keeps a slow or offline git fetch
    /// (`resolved_set` → `sync`) off the startup path so the window always
    /// opens, and keeps a fatal resolution error from aborting the app.
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
        let mut runtime = Runtime::new(registry, store.clone(), audit.producer());

        let manager = PluginManager::new(config_dir, cache_dir, &store);

        // Wire the per-identity secret resolver BEFORE any plugin loads.
        // Plugins capture the resolver Rc at load time, so it must be set here,
        // in boot_in, which runs before run_initial_load_pass.
        let session_identity = IdentityId::new(super::SESSION_IDENTITY);
        let router = runtime.make_secret_router();
        let secret_resolver =
            match build_secret_resolver(config_dir, Some(&session_identity), Some(router)) {
                Ok((resolver, errors)) => {
                    for e in &errors {
                        eprintln!("mote-shell: secret skipped: {e}");
                    }
                    let resolver = Rc::new(resolver);
                    runtime.set_secret_resolver(Rc::clone(&resolver));
                    resolver
                }
                Err(e) => {
                    // A malformed secrets.lua must NOT abort boot; the window always opens.
                    eprintln!("mote-shell: secrets config failed to load; secrets disabled: {e}");
                    Rc::new(SecretResolver::empty())
                }
            };

        Ok(Self {
            runtime,
            audit,
            store,
            manager,
            combos,
            loaded: Vec::new(),
            pending_approvals: Vec::new(),
            secret_resolver,
        })
    }

    /// Resolve, classify, and load every plugin the composed spec set declares
    /// (plus the bundled first-party defaults).
    ///
    /// This is the deferred load pass: the shell calls it once on the first
    /// post-paint tick, *after* the window and chrome are live. It runs the
    /// reconciling [`PluginManager::resolved_set`] (which fetches/links/hashes
    /// git plugins over the network), then walks the resolved set through
    /// [`load_resolved`](Self::load_resolved).
    ///
    /// **This method never panics and never aborts the app.** A fatal
    /// `resolved_set` error (e.g. a `plugins.lua` that does not parse) is logged
    /// and swallowed, leaving [`loaded`](Self::loaded) and
    /// [`pending_approvals`](Self::pending_approvals) empty — the window stays
    /// alive. Per-plugin failures are logged and skipped inside `load_resolved`.
    /// Calling it more than once is a no-op after the first pass (the load loop
    /// short-circuits on already-loaded plugins via the runtime), but the shell
    /// guards it with a `did_initial_load` flag so it runs exactly once.
    pub(crate) fn run_initial_load_pass(&mut self) {
        let session_identity = IdentityId::new(super::SESSION_IDENTITY);
        let resolved = match self.manager.resolved_set(Some(&session_identity)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "mote-shell: plugin resolution failed; no plugins loaded \
                     (window stays alive): {e}"
                );
                return;
            }
        };
        let identity = IdentityContext::new(session_identity);
        let combos = self.combos.clone();
        self.load_resolved(resolved, identity, &combos);
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
                            // sees this manifest as approved (bundled + dev-mode
                            // plugins are skipped — see `record_approval`).
                            self.record_approval(&rp);
                            self.loaded.push(rp);
                        }
                        Err(e) => {
                            eprintln!("mote-shell: plugin `{}` failed to load: {e}", rp.name);
                        }
                    }
                }
                ApprovalOutcome::NeedsDialog(req) => {
                    eprintln!(
                        "mote-shell: plugin `{}` awaiting approval (dialog shown)",
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

    /// Record an approved manifest's hash in the approval store (skipping
    /// bundled AND dev-mode plugins, which `classify` auto-grants WITHOUT
    /// consulting the store — recording one would be dead data that pollutes
    /// `mote plugin` CLI enumeration, never read on a later launch). Logs on
    /// failure; the load already succeeded so a store hiccup is non-fatal.
    fn record_approval(&self, rp: &ResolvedPlugin) {
        if matches!(rp.provenance, Provenance::Bundled | Provenance::DevMode) {
            return;
        }
        if let Err(e) = self
            .manager
            .approval_store()
            .put(&rp.name, &ApprovalHash::of(&rp.manifest))
        {
            eprintln!(
                "mote-shell: failed to record approval for `{}`: {e}",
                rp.name
            );
        }
    }

    /// Finish a dialog approval (ADR-0007 async approval), on the pump thread.
    ///
    /// The op handler already ran the op-boundary structural validation; this
    /// does the semantic work:
    ///
    /// 1. Find the matching `pending_approvals` entry by plugin name. None →
    ///    [`ApproveOutcome::NotPending`] (drop a stale/spoofed result).
    /// 2. Cross-check that every answered permission corresponds to one the
    ///    plugin actually requested (the `domain` keys must be a subset of the
    ///    pending [`ApprovalRequest`]'s permission domains). A mismatch → drop
    ///    (treated as not-pending; never load on an unexpected permission set).
    /// 3. Map the verdict to an [`Approval`]. A deny removes the entry and
    ///    audits the denial → [`ApproveOutcome::Denied`].
    /// 4. Otherwise load the plugin under the decided policy. On success record
    ///    the approval and move the entry into `loaded` →
    ///    [`ApproveOutcome::Loaded`]; on failure leave it pending →
    ///    [`ApproveOutcome::LoadFailed`].
    pub(crate) fn approve_pending(&mut self, result: &DialogResult) -> ApproveOutcome {
        let Some(idx) = self
            .pending_approvals
            .iter()
            .position(|(rp, _)| rp.name.as_str() == result.plugin())
        else {
            return ApproveOutcome::NotPending;
        };

        // Semantic cross-check: every answered permission must correspond to a
        // (domain, action) PAIR the plugin actually requested. Matching on the
        // pair (not the bare domain) prevents a dialog result from smuggling a
        // narrowing for an action the plugin never requested when it has two
        // actions in the same domain. Both sides now carry bare domain+action.
        let requested_pairs: std::collections::BTreeSet<(&str, &str)> = self.pending_approvals[idx]
            .1
            .permissions
            .iter()
            .map(|p| (p.domain.as_str(), p.action.as_str()))
            .collect();
        if !result
            .permissions
            .iter()
            .all(|p| requested_pairs.contains(&(p.domain.as_str(), p.action.as_str())))
        {
            eprintln!(
                "mote-shell: approve for `{}` answered an unrequested permission; dropping",
                result.plugin()
            );
            return ApproveOutcome::NotPending;
        }

        let approval = approval_from_dialog(result);
        let identity = IdentityContext::new(IdentityId::new(super::SESSION_IDENTITY));

        if let Approval::Deny { reason } = &approval {
            eprintln!("mote-shell: plugin `{}` denied: {reason}", result.plugin());
            // Drop the pending entry; the audit log records the denial via the
            // runtime's gatekept path on the next launch attempt — here we only
            // surface the user's decision in the shell log.
            self.pending_approvals.remove(idx);
            return ApproveOutcome::Denied;
        }

        let rp = self.pending_approvals[idx].0.clone();
        let policy = DecidedPolicy::new(approval);
        // adjust-scope parks an already-running plugin: re-narrow via `reload`
        // (require_reapproval=true so a narrowing/re-grant always proceeds).
        // A first install / expanding update is not yet running → `load`.
        let result_load = if self.runtime.running(&rp.name).is_some() {
            self.runtime
                .reload(&rp.name, &rp.init_source, identity, &policy, true)
                .map(|r| r.name)
                .map_err(|e| e.to_string())
        } else {
            self.runtime
                .load(&rp.init_source, identity, &policy)
                .map(|r| r.name)
                .map_err(|e| e.to_string())
        };
        match result_load {
            Ok(loaded_name) => {
                eprintln!("mote-shell: approved + (re)loaded `{loaded_name}`");
                self.record_approval(&rp);
                self.pending_approvals.remove(idx);
                self.loaded.push(rp);
                ApproveOutcome::Loaded
            }
            Err(e) => {
                eprintln!(
                    "mote-shell: approved `{}` but (re)load failed: {e} (left pending)",
                    rp.name
                );
                ApproveOutcome::LoadFailed
            }
        }
    }

    /// Look up a loaded plugin's [`ResolvedPlugin`] by name (panel actions key
    /// off the name string the chrome sends).
    fn loaded_resolved(&self, name: &str) -> Option<&ResolvedPlugin> {
        self.loaded.iter().find(|rp| rp.name.as_str() == name)
    }

    /// Panel action — update the named plugin (`plugin_update`, §6.5).
    ///
    /// Runs [`PluginManager::update`]. An expanding update returns
    /// [`UpdateAction::ReApproval`] carrying a re-approval request the shell
    /// renders (the user re-approves via `approve_plugin`, which reloads with
    /// the new grant); a non-expanding update relinks the lock and the plugin is
    /// reloaded in-place ([`UpdateAction::Applied`]). Any failure is logged and
    /// returns [`UpdateAction::Failed`].
    pub(crate) fn update_plugin(&mut self, name: &str) -> UpdateAction {
        let Ok(plugin) = PluginName::new(name) else {
            eprintln!("mote-shell: plugin_update: invalid plugin name `{name}`");
            return UpdateAction::Failed;
        };
        match self.manager.update(&plugin) {
            Ok(UpdateOutcome::Applied { commit }) => {
                eprintln!("mote-shell: updated `{name}` -> {commit}; reloading");
                if self.reload_from_disk(&plugin).is_ok() {
                    UpdateAction::Applied
                } else {
                    UpdateAction::Failed
                }
            }
            Ok(UpdateOutcome::NeedsReapproval { .. }) => {
                eprintln!("mote-shell: update of `{name}` needs re-approval");
                // Park a re-approval request from the just-updated on-disk
                // manifest so `approve_plugin` reloads with the new grant.
                self.park_reapproval(&plugin)
                    .map_or(UpdateAction::Failed, UpdateAction::ReApproval)
            }
            Err(e) => {
                eprintln!("mote-shell: update of `{name}` failed: {e}");
                UpdateAction::Failed
            }
        }
    }

    /// Re-resolve a single plugin and park a re-approval request for it in
    /// `pending_approvals` so the subsequent `approve_plugin` reloads it with
    /// the new grant. Moves the plugin out of `loaded` (it is re-added on a
    /// successful approve). Returns `None` (logging) if it cannot be resolved.
    fn park_reapproval(&mut self, plugin: &PluginName) -> Option<ApprovalRequest> {
        let rp = self.reresolve(plugin)?;
        // Classify the freshly-resolved manifest to get the precise "what's new"
        // surface; fall back to a bare update request if classification fails.
        let req = match classify(
            &rp.manifest,
            rp.provenance,
            self.manager.approval_store(),
            &self.combos,
        ) {
            Ok(ApprovalOutcome::NeedsDialog(req)) => req,
            _ => build_update_request(&rp.manifest, rp.provenance, &self.combos, Vec::new()),
        };
        self.park_pending(rp, req.clone());
        Some(req)
    }

    /// Re-resolve a single plugin from the manager's composed spec set so its
    /// `dir` / `init_source` / `manifest` reflect the current on-disk state
    /// (after an update/rollback relink). Returns `None` (logging) on failure.
    fn reresolve(&self, plugin: &PluginName) -> Option<ResolvedPlugin> {
        let session_identity = IdentityId::new(super::SESSION_IDENTITY);
        match self.manager.resolved_set(Some(&session_identity)) {
            Ok(set) => set.into_iter().find(|rp| &rp.name == plugin),
            Err(e) => {
                eprintln!("mote-shell: re-resolve of `{plugin}` failed: {e}");
                None
            }
        }
    }

    /// Move `rp` into `pending_approvals` under `req`, removing any stale
    /// `loaded`/`pending` entry for the same name first (so the approve path
    /// operates on exactly one fresh entry).
    fn park_pending(&mut self, rp: ResolvedPlugin, req: ApprovalRequest) {
        let name = rp.name.clone();
        self.loaded.retain(|existing| existing.name != name);
        self.pending_approvals
            .retain(|(existing, _)| existing.name != name);
        self.pending_approvals.push((rp, req));
    }

    /// Panel action — roll the named plugin back to its prior commit
    /// (`plugin_rollback`, §6.5), then reload it. Logs on failure.
    pub(crate) fn rollback_plugin(&mut self, name: &str) {
        let Ok(plugin) = PluginName::new(name) else {
            eprintln!("mote-shell: plugin_rollback: invalid plugin name `{name}`");
            return;
        };
        if let Err(e) = self.manager.rollback(&plugin) {
            eprintln!("mote-shell: rollback of `{name}` failed: {e}");
            return;
        }
        eprintln!("mote-shell: rolled back `{name}`; reloading");
        let _ = self.reresolve_and_reload(&plugin);
    }

    /// Panel action — reload the named plugin (`plugin_reload`, §6.5;
    /// path/dev re-run from its on-disk source). Logs on failure.
    pub(crate) fn reload_plugin(&mut self, name: &str) {
        let Ok(plugin) = PluginName::new(name) else {
            eprintln!("mote-shell: plugin_reload: invalid plugin name `{name}`");
            return;
        };
        if self.reload_from_disk(&plugin).is_ok() {
            eprintln!("mote-shell: reloaded `{name}`");
        }
    }

    /// Reload a loaded plugin from its resolved on-disk `init.lua` directory,
    /// preserving its prior grant (`require_reapproval=false`; an expanding
    /// manifest is refused by [`Runtime::reload`] and the panel update path
    /// routes that case through the re-approval dialog instead). Picks up a
    /// `path:`/dev edit. Returns `Err(())` (logging) on any failure.
    fn reload_from_disk(&mut self, plugin: &PluginName) -> Result<(), ()> {
        let Some(rp) = self.loaded_resolved(plugin.as_str()) else {
            eprintln!("mote-shell: reload: `{plugin}` is not loaded");
            return Err(());
        };
        let source = std::fs::read_to_string(rp.dir.join("init.lua"))
            .unwrap_or_else(|_| rp.init_source.clone());
        self.reload_with_source(plugin, source)
    }

    /// Re-resolve a plugin (to pick up a relink) and reload it in place. Used by
    /// rollback, where the active link moved to a different commit dir.
    fn reresolve_and_reload(&mut self, plugin: &PluginName) -> Result<(), ()> {
        let Some(rp) = self.reresolve(plugin) else {
            return Err(());
        };
        let source = rp.init_source.clone();
        // Refresh the cached resolved entry so later renders see the new dir.
        if let Some(existing) = self.loaded.iter_mut().find(|e| &e.name == plugin) {
            *existing = rp;
        }
        self.reload_with_source(plugin, source)
    }

    /// Run [`Runtime::reload`] with `source` under the prior grant and refresh
    /// the cached source on success. Returns `Err(())` (logging) on failure.
    fn reload_with_source(&mut self, plugin: &PluginName, source: String) -> Result<(), ()> {
        let identity = IdentityContext::new(IdentityId::new(super::SESSION_IDENTITY));
        match self
            .runtime
            .reload(plugin, &source, identity, &DecidedPolicy::grant(), false)
        {
            Ok(_) => {
                if let Some(rp) = self.loaded.iter_mut().find(|rp| &rp.name == plugin) {
                    rp.init_source = source;
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("mote-shell: reload of `{plugin}` failed: {e}");
                Err(())
            }
        }
    }

    /// Panel action — revoke the named plugin (`plugin_revoke`, §6.5): unload it
    /// from the runtime and drop its stored approval so it does not auto-load on
    /// the next launch. The plugin leaves the loaded set.
    ///
    /// If `unload` fails the runtime still holds the plugin, so we leave it in
    /// `self.loaded` and keep its approval — panel and runtime stay consistent
    /// (no phantom "revoked" row for a plugin that is still running). The
    /// approval-store removal is logged independently of the unload.
    pub(crate) fn revoke_plugin(&mut self, name: &str) {
        let Ok(plugin) = PluginName::new(name) else {
            eprintln!("mote-shell: plugin_revoke: invalid plugin name `{name}`");
            return;
        };
        if let Err(e) = self.runtime.unload(&plugin) {
            // Unload failed → the plugin is still running. Do NOT drop it from
            // `loaded` or remove its approval; keep state consistent.
            eprintln!("mote-shell: revoke of `{name}` — unload failed, leaving loaded: {e}");
            return;
        }
        if let Err(e) = self.manager.approval_store().remove(&plugin) {
            eprintln!("mote-shell: revoke of `{name}` — drop approval failed: {e}");
        }
        self.loaded.retain(|rp| rp.name != plugin);
        eprintln!("mote-shell: revoked `{name}`");
    }

    /// Panel action — revoke a specific secret grant from a loaded plugin
    /// (`plugin_revoke_secret`, Task 9).
    ///
    /// Narrows the plugin's `(secret, read)` grant to exclude `secret_name`,
    /// keeping all other secret grants, then reloads in place (the new
    /// effective-permission set no longer includes `secret:read:<secret_name>`).
    ///
    /// This is **session-scoped** — consistent with all other narrowings in
    /// Mote. The `ApprovalStore` persists only the manifest hash, never
    /// narrowings; a relaunch re-grants the full manifest set.
    ///
    /// Edge case: revoking the last secret → `remaining` is empty →
    /// `GrantSet::narrow` with an empty resource set produces a [`GlobSet`]
    /// that matches nothing (deny-all for that pair), which is the correct
    /// revoke-everything outcome.
    pub(crate) fn revoke_secret(&mut self, plugin_name: &str, secret_name: &str) {
        let Ok(plugin) = PluginName::new(plugin_name) else {
            eprintln!("mote-shell: plugin_revoke_secret: invalid plugin name `{plugin_name}`");
            return;
        };
        let Some(running) = self.runtime.running(&plugin) else {
            eprintln!("mote-shell: plugin_revoke_secret: plugin `{plugin_name}` is not running");
            return;
        };
        // Collect the remaining secret-read resources (all but the revoked one).
        let remaining: Vec<String> = running
            .effective_permissions
            .iter()
            .filter_map(|p| p.strip_prefix("secret:read:").map(str::to_owned))
            .filter(|n| n != secret_name)
            .collect();

        let approval = Approval::Narrow {
            narrowings: vec![Narrowing {
                domain: "secret".to_owned(),
                action: "read".to_owned(),
                resources: remaining,
            }],
        };

        let Some(rp) = self.loaded_resolved(plugin_name) else {
            eprintln!("mote-shell: plugin_revoke_secret: plugin `{plugin_name}` not in loaded set");
            return;
        };
        let source = rp.init_source.clone();
        let identity = IdentityContext::new(IdentityId::new(super::SESSION_IDENTITY));
        let policy = DecidedPolicy::new(approval);
        match self
            .runtime
            .reload(&plugin, &source, identity, &policy, false)
        {
            Ok(_) => {
                eprintln!("mote-shell: revoked secret `{secret_name}` from `{plugin_name}`");
            }
            Err(e) => {
                eprintln!(
                    "mote-shell: plugin_revoke_secret: reload of `{plugin_name}` failed: {e}"
                );
            }
        }
    }

    /// Panel action — build a re-approval request for `plugin_adjust_scope`
    /// (§6.5): re-open the install dialog for a loaded plugin seeded with its
    /// current manifest so the user can re-narrow its grant. The subsequent
    /// `approve_plugin` reloads with the new narrowing (re-grant-via-reload):
    /// the plugin is parked in `pending_approvals`, so `approve_pending` finds
    /// it and reloads. Returns `None` (logging) if the plugin is not loaded.
    pub(crate) fn adjust_scope_request(&mut self, name: &str) -> Option<ApprovalRequest> {
        let rp = self.loaded_resolved(name)?.clone();
        // Seed the dialog as an update (is_update=true) with no "what's new"
        // list: nothing expanded, the user is re-narrowing an existing grant.
        let req = build_update_request(&rp.manifest, rp.provenance, &self.combos, Vec::new());
        // Park it as pending so the eventual `approve_plugin` re-loads with the
        // new grant. The plugin remains loaded in the runtime; `approve_pending`
        // detects that and uses `reload` rather than `load`.
        self.park_pending(rp, req.clone());
        Some(req)
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
        let running = self.runtime.running(&rp.name);
        let permissions = running
            .as_ref()
            .map(|r| {
                r.effective_permissions
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

        // Build per-secret rows from the plugin's effective `secret:read:<name>`
        // permissions.  The audit query is O(N) over the ring; acceptable for
        // the panel render (noted as a v0.2 follow-up).
        let secrets = running
            .as_ref()
            .map(|r| {
                let query = self.audit.query();
                r.effective_permissions
                    .iter()
                    .filter_map(|p| p.strip_prefix("secret:read:").map(str::to_owned))
                    .map(|name| {
                        let backend = self
                            .secret_resolver
                            .backend_label(&name)
                            .unwrap_or("unknown")
                            .to_owned();
                        // Find the most-recent audit event whose operation matches
                        // `secret:read:<name>` for this plugin.
                        let op = format!("secret:read:{name}");
                        let last_read = query
                            .recent_for_plugin(&rp.name, usize::MAX)
                            .into_iter()
                            .rfind(|ev| ev.operation == op)
                            .map(|ev| relative_time(ev.timestamp));
                        SecretAccessRow {
                            name,
                            backend,
                            last_read,
                        }
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
            secrets,
            last_used: self.last_used(&rp.name),
            integrity: ui_integrity(rp.provenance, &rp.integrity),
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
/// commit is recovered from the cache-dir name. `DevMode` and `ImplicitLocal`
/// are both produced by [`PluginManager::resolved_set`] (Task 6): `DevMode` via
/// the dev-mode override (name in `dev_mode.plugins` or dir under a
/// `dev_mode.directories` entry), `ImplicitLocal` via the
/// `<config>/plugins/<name>` real-dir scan. Each maps to its respective mote-ui
/// kind.
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

/// The integrity-panel status to render for a resolved plugin (Task 6c).
///
/// A `DevMode`-provenance plugin always shows [`IntegrityStatus::DevMode`]
/// ("dev mode") regardless of the sync-computed hash status: it is the
/// developer's working copy, auto-approved on every change, so "verified" /
/// "mismatch" would be misleading. Every other provenance maps its
/// [`MgrIntegrity`] through [`mgr_integrity_to_ui`].
const fn ui_integrity(provenance: Provenance, status: &MgrIntegrity) -> IntegrityStatus {
    match provenance {
        Provenance::DevMode => IntegrityStatus::DevMode,
        _ => mgr_integrity_to_ui(status),
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
        secrets: Vec::new(),
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
        // Using bookmarks as a representative bundled plugin (urlbar was removed
        // in Phase 5a; history owns ui:urlbar_provider from this point on).
        let rp = resolved("bookmarks", Provenance::Bundled, MgrIntegrity::Bundled);
        let kind = provenance_to_kind(rp.provenance, &rp.dir);
        assert!(matches!(kind, PluginKind::Bundled));
        assert_eq!(mgr_integrity_to_ui(&rp.integrity), IntegrityStatus::Bundled);
        assert_eq!(
            actions_for_kind(&kind),
            vec![PluginAction::Update, PluginAction::Revoke]
        );
    }

    #[test]
    fn dev_mode_loaded_row_kind_integrity_actions() {
        // Task 6c: a DevMode-provenance resolved plugin renders with the DevMode
        // kind (⊙ glyph), DevMode integrity *regardless of the sync hash status*,
        // and the dev/path action set (reload / revoke / adjust-scope).
        let rp = resolved("my-dev-plugin", Provenance::DevMode, MgrIntegrity::Unknown);

        let kind = provenance_to_kind(rp.provenance, &rp.dir);
        assert!(
            matches!(kind, PluginKind::DevMode { .. }),
            "DevMode provenance -> PluginKind::DevMode"
        );
        assert_eq!(kind.glyph(), "⊙", "dev-mode glyph renders via PluginKind");

        // The sync integrity is Unknown, but a dev-mode plugin always shows
        // "dev mode" — not "unknown" / "verified".
        assert_eq!(
            ui_integrity(rp.provenance, &rp.integrity),
            IntegrityStatus::DevMode,
            "DevMode provenance overrides sync integrity"
        );

        assert_eq!(
            actions_for_kind(&kind),
            vec![
                PluginAction::Reload,
                PluginAction::Revoke,
                PluginAction::AdjustScope,
            ],
            "dev-mode shares the path action set"
        );
    }

    #[test]
    fn implicit_local_loaded_row_kind_and_actions() {
        // Task 6a: an ImplicitLocal-provenance resolved plugin renders with the
        // ImplicitLocal kind (◇ glyph) and the revoke / adjust-scope action set
        // (no update/rollback source). The shell needs no extra wiring — the
        // existing provenance->kind / actions mapping covers it.
        let rp = resolved(
            "dropped-in",
            Provenance::ImplicitLocal,
            MgrIntegrity::Unknown,
        );
        let kind = provenance_to_kind(rp.provenance, &rp.dir);
        assert!(
            matches!(kind, PluginKind::ImplicitLocal { .. }),
            "ImplicitLocal provenance -> PluginKind::ImplicitLocal"
        );
        assert_eq!(
            kind.glyph(),
            "◇",
            "implicit-local glyph renders via PluginKind"
        );
        assert_eq!(
            actions_for_kind(&kind),
            vec![PluginAction::Revoke, PluginAction::AdjustScope]
        );
    }

    #[test]
    fn dev_mode_build_panel_row_shows_dev_mode() {
        // End-to-end through the live panel builder: a DevMode plugin pushed
        // into `loaded` renders a row with DevMode kind + DevMode integrity.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();

        // A DevMode plugin whose sync integrity is PathLocal (would map to
        // "verified") — the panel must still show "dev mode".
        host.loaded.push(resolved(
            "dev-widget",
            Provenance::DevMode,
            MgrIntegrity::PathLocal,
        ));

        let panel = host.build_panel();
        let row = panel
            .plugins
            .iter()
            .find(|p| p.name == "dev-widget")
            .expect("dev-widget row present");
        assert!(matches!(row.kind, PluginKind::DevMode { .. }));
        assert_eq!(row.integrity, IntegrityStatus::DevMode);
        assert_eq!(
            row.actions,
            vec![
                PluginAction::Reload,
                PluginAction::Revoke,
                PluginAction::AdjustScope,
            ]
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
        // Using bookmarks as a representative bundled plugin (urlbar was removed
        // in Phase 5a; history owns ui:urlbar_provider from this point on).
        let panel = IntegrityPanel {
            plugins: vec![PluginRow {
                name: "bookmarks".into(),
                version: "0.1.0".into(),
                fulfills: vec!["ui:bookmarks_provider".into()],
                consumes: vec![],
                permissions: vec![PermissionRow {
                    requested: "storage:persistent".into(),
                    effective: "storage:persistent".into(),
                    narrowed: false,
                    denied: false,
                }],
                secrets: vec![],
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
        assert!(html.contains("bookmarks"));
        assert!(html.contains("ui:bookmarks_provider"));
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

    /// Writes a path: plugin dir whose manifest requests exactly `permissions`
    /// (each a full `domain:action[:resource]` string).
    fn write_path_plugin_with_perms(dir: &std::path::Path, name: &str, permissions: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        let perms = permissions
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let lua = format!(
            r#"
local M = {{}}
M.manifest = {{
    schema = "v1",
    name = "{name}",
    version = "0.1.0",
    permissions = {{ {perms} }},
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

        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();

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
            loaded.contains(&"history"),
            "bundled history loaded: {loaded:?}"
        );
        assert!(
            loaded.contains(&"bookmarks"),
            "bundled bookmarks loaded: {loaded:?}"
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

        let mut host = PluginHost::boot_in(store.clone(), config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();
        drop(host);

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
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();

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
            loaded.contains(&"history"),
            "bundled history still loads: {loaded:?}"
        );
        assert!(
            loaded.contains(&"bookmarks"),
            "bundled bookmarks still loads: {loaded:?}"
        );
        assert!(
            loaded.contains(&"workspace-manager"),
            "bundled workspace-manager still loads: {loaded:?}"
        );
    }

    #[test]
    fn run_initial_load_pass_with_broken_config_is_non_fatal() {
        // Critical #2 regression guard: a fatal `resolved_set` error (here, a
        // `plugins.lua` that does not parse — `composed_spec_set` returns Err)
        // must NOT panic and must NOT abort. The host stays usable with empty
        // loaded/pending so the window keeps running.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // A `plugins.lua` that is not valid config Lua → ManagerError::Config.
        std::fs::write(
            config.path().join("plugins.lua"),
            "this is not ){ valid lua at all (((",
        )
        .unwrap();

        let store = Store::open_in_memory().unwrap();
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        // The load pass must swallow the resolution error.
        host.run_initial_load_pass();

        assert!(
            host.loaded.is_empty(),
            "a fatal resolution error must leave no plugins loaded: {:?}",
            host.loaded
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            host.pending_approvals.is_empty(),
            "a fatal resolution error must leave nothing pending"
        );
        // The host is still usable: building the panel does not panic.
        let _ = host.build_panel();
    }

    // -----------------------------------------------------------------------
    // Task 5b/5c: approve_pending / revoke / reload (headless host logic)
    // -----------------------------------------------------------------------

    use crate::approval::{DialogPermission, DialogResult};

    /// A grant `DialogResult` for `plugin`, granting the bare `(domain, action)`
    /// pair fully (no narrowing).
    fn grant_full(plugin: &str, domain: &str, action: &str) -> DialogResult {
        DialogResult {
            plugin: plugin.to_owned(),
            decision: "grant".to_owned(),
            permissions: vec![DialogPermission {
                domain: domain.to_owned(),
                action: action.to_owned(),
                mode: "full".to_owned(),
                origins: None,
            }],
        }
    }

    /// A grant `DialogResult` for `plugin` that narrows the bare `(domain,
    /// action)` pair to the given `origins`.
    fn grant_origins(plugin: &str, domain: &str, action: &str, origins: &[&str]) -> DialogResult {
        DialogResult {
            plugin: plugin.to_owned(),
            decision: "grant".to_owned(),
            permissions: vec![DialogPermission {
                domain: domain.to_owned(),
                action: action.to_owned(),
                mode: "origins".to_owned(),
                origins: Some(origins.iter().map(|s| (*s).to_owned()).collect()),
            }],
        }
    }

    /// Boots a host against a single never-approved path plugin so it lands in
    /// `pending_approvals` after the load pass.
    fn host_with_pending_plugin(name: &str) -> (PluginHost, tempfile::TempDir) {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin(plugin_dir.path(), name);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[(name, &src)]);

        let store = Store::open_in_memory().unwrap();
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();
        assert!(
            host.pending_approvals
                .iter()
                .any(|(rp, _)| rp.name.as_str() == name),
            "plugin `{name}` must be pending after the load pass"
        );
        // Keep plugin_dir alive for the host's lifetime by leaking it into the
        // returned tuple (the path: source points into it).
        (host, plugin_dir)
    }

    #[test]
    fn approve_pending_grant_loads_and_records_approval() {
        let (mut host, _dir) = host_with_pending_plugin("grant-me");
        let result = grant_full("grant-me", "storage", "persistent");

        let outcome = host.approve_pending(&result);
        assert_eq!(
            outcome,
            ApproveOutcome::Loaded,
            "grant must load the plugin"
        );
        assert!(
            host.loaded.iter().any(|rp| rp.name.as_str() == "grant-me"),
            "approved plugin moves into loaded"
        );
        assert!(
            !host
                .pending_approvals
                .iter()
                .any(|(rp, _)| rp.name.as_str() == "grant-me"),
            "approved plugin leaves pending"
        );
        // The approval is recorded so a later launch auto-grants it.
        let recorded = host
            .manager
            .approval_store()
            .list()
            .unwrap()
            .iter()
            .any(|n| n.as_str() == "grant-me");
        assert!(
            recorded,
            "an approved path plugin records its approval hash"
        );
        assert!(
            host.runtime
                .running(&PluginName::new("grant-me").unwrap())
                .is_some(),
            "the runtime now has the plugin loaded"
        );
    }

    #[test]
    fn approve_pending_deny_drops_without_loading() {
        let (mut host, _dir) = host_with_pending_plugin("deny-me");
        let mut result = grant_full("deny-me", "storage", "persistent");
        result.decision = "deny".to_owned();

        let outcome = host.approve_pending(&result);
        assert_eq!(outcome, ApproveOutcome::Denied, "deny must drop the plugin");
        assert!(
            !host.loaded.iter().any(|rp| rp.name.as_str() == "deny-me"),
            "denied plugin must NOT load"
        );
        assert!(
            !host
                .pending_approvals
                .iter()
                .any(|(rp, _)| rp.name.as_str() == "deny-me"),
            "denied plugin leaves pending"
        );
        assert!(
            host.runtime
                .running(&PluginName::new("deny-me").unwrap())
                .is_none(),
            "the runtime must not hold a denied plugin"
        );
    }

    #[test]
    fn approve_pending_for_non_pending_plugin_is_dropped() {
        let (mut host, _dir) = host_with_pending_plugin("real-plugin");
        // A result naming a plugin that is not pending.
        let result = grant_full("ghost-plugin", "storage", "persistent");
        let outcome = host.approve_pending(&result);
        assert_eq!(
            outcome,
            ApproveOutcome::NotPending,
            "an approve for a non-pending plugin is dropped"
        );
        // The genuinely-pending plugin is untouched.
        assert!(
            host.pending_approvals
                .iter()
                .any(|(rp, _)| rp.name.as_str() == "real-plugin"),
            "the real pending plugin is left intact"
        );
    }

    #[test]
    fn approve_pending_with_unrequested_permission_is_dropped() {
        // The plugin only requests storage:persistent; a result that answers an
        // unrequested (domain, action) pair must be dropped (never loaded).
        let (mut host, _dir) = host_with_pending_plugin("strict");
        let result = grant_full("strict", "secret", "read"); // not requested
        let outcome = host.approve_pending(&result);
        assert_eq!(
            outcome,
            ApproveOutcome::NotPending,
            "answering an unrequested permission must be dropped"
        );
        assert!(
            !host.loaded.iter().any(|rp| rp.name.as_str() == "strict"),
            "a mismatched result must not load the plugin"
        );
    }

    #[test]
    fn approve_pending_with_unrequested_action_in_same_domain_is_dropped() {
        // The plugin requests storage:persistent. A result answering a DIFFERENT
        // action in the SAME domain (storage:memory) must be dropped — the
        // strengthened (domain, action)-pair cross-check, not domain-alone.
        let (mut host, _dir) = host_with_pending_plugin("same-domain");
        let result = grant_full("same-domain", "storage", "memory"); // wrong action
        assert_eq!(
            host.approve_pending(&result),
            ApproveOutcome::NotPending,
            "an unrequested action in a requested domain must be dropped"
        );
        assert!(
            !host
                .loaded
                .iter()
                .any(|rp| rp.name.as_str() == "same-domain"),
            "a mismatched action must not load the plugin"
        );
    }

    #[test]
    fn revoke_unloads_and_drops_approval() {
        let (mut host, _dir) = host_with_pending_plugin("revoke-me");
        // Approve + load first.
        let _ = host.approve_pending(&grant_full("revoke-me", "storage", "persistent"));
        let name = PluginName::new("revoke-me").unwrap();
        assert!(
            host.runtime.running(&name).is_some(),
            "loaded before revoke"
        );
        assert!(
            host.manager
                .approval_store()
                .list()
                .unwrap()
                .iter()
                .any(|n| n == &name),
            "approval recorded before revoke"
        );

        host.revoke_plugin("revoke-me");
        assert!(
            host.runtime.running(&name).is_none(),
            "revoke unloads the plugin from the runtime"
        );
        assert!(
            !host.loaded.iter().any(|rp| rp.name.as_str() == "revoke-me"),
            "revoke removes the plugin from the loaded set"
        );
        assert!(
            !host
                .manager
                .approval_store()
                .list()
                .unwrap()
                .iter()
                .any(|n| n == &name),
            "revoke drops the stored approval so it does not auto-load next launch"
        );
    }

    #[test]
    fn reload_keeps_plugin_loaded() {
        let (mut host, _dir) = host_with_pending_plugin("reload-me");
        let _ = host.approve_pending(&grant_full("reload-me", "storage", "persistent"));
        let name = PluginName::new("reload-me").unwrap();
        assert!(
            host.runtime.running(&name).is_some(),
            "loaded before reload"
        );

        host.reload_plugin("reload-me");
        assert!(
            host.runtime.running(&name).is_some(),
            "the plugin stays loaded across a reload"
        );
        assert!(
            host.loaded.iter().any(|rp| rp.name.as_str() == "reload-me"),
            "the plugin stays in the loaded set across a reload"
        );
    }

    #[test]
    fn adjust_scope_parks_loaded_plugin_for_reapproval() {
        let (mut host, _dir) = host_with_pending_plugin("scope-me");
        let _ = host.approve_pending(&grant_full("scope-me", "storage", "persistent"));
        assert!(host.loaded.iter().any(|rp| rp.name.as_str() == "scope-me"));

        let req = host.adjust_scope_request("scope-me");
        assert!(req.is_some(), "adjust-scope yields a re-approval request");
        let req = req.unwrap();
        assert!(req.is_update, "adjust-scope dialog is an update dialog");
        assert!(
            host.pending_approvals
                .iter()
                .any(|(rp, _)| rp.name.as_str() == "scope-me"),
            "the plugin is parked as pending for re-approval"
        );
        // The plugin is still running (re-grant happens on the eventual approve).
        assert!(
            host.runtime
                .running(&PluginName::new("scope-me").unwrap())
                .is_some(),
            "the plugin keeps running while awaiting re-narrowing"
        );

        // Re-approving with a grant reloads it (re-grant-via-reload) and moves it
        // back into loaded.
        let outcome = host.approve_pending(&grant_full("scope-me", "storage", "persistent"));
        assert_eq!(
            outcome,
            ApproveOutcome::Loaded,
            "re-approve reloads via reload"
        );
        assert!(
            host.loaded.iter().any(|rp| rp.name.as_str() == "scope-me"),
            "re-approved plugin returns to the loaded set"
        );
    }

    /// Boots a host against a never-approved path plugin requesting exactly
    /// `permissions`, so it lands in `pending_approvals`.
    fn host_with_pending_perms(
        name: &str,
        permissions: &[&str],
    ) -> (PluginHost, tempfile::TempDir) {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin_with_perms(plugin_dir.path(), name, permissions);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[(name, &src)]);

        let store = Store::open_in_memory().unwrap();
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();
        assert!(
            host.pending_approvals
                .iter()
                .any(|(rp, _)| rp.name.as_str() == name),
            "plugin `{name}` must be pending after the load pass"
        );
        (host, plugin_dir)
    }

    /// THE PROOF TEST (regression guard for the narrowing-contract bug).
    ///
    /// A plugin requests `page:inject_script:*`. The user approves it narrowed
    /// to `https://example.com/*`. The resulting `RunningPlugin`'s effective
    /// permission for `page:inject_script` must be restricted to that origin and
    /// must NOT retain the full `*`. Before the bare-field fix the narrowing
    /// no-op'd (composite domain + resource-in-action never matched
    /// `GrantSet::narrow`) and the effective grant stayed `*` — this test fails
    /// in that state.
    #[test]
    fn approve_pending_origins_narrowing_restricts_effective_grant() {
        let (mut host, _dir) = host_with_pending_perms("narrow-me", &["page:inject_script:*"]);

        let result = grant_origins(
            "narrow-me",
            "page",
            "inject_script",
            &["https://example.com/*"],
        );
        let outcome = host.approve_pending(&result);
        assert_eq!(
            outcome,
            ApproveOutcome::Loaded,
            "a narrowed grant must load the plugin"
        );

        let running = host
            .runtime
            .running(&PluginName::new("narrow-me").unwrap())
            .expect("plugin must be running after a narrowed approve");
        let effective = &running.effective_permissions;

        // The narrowing took effect: the effective grant is the chosen origin…
        assert!(
            effective
                .iter()
                .any(|p| p == "page:inject_script:https://example.com/*"),
            "effective grant must be narrowed to the chosen origin; got: {effective:?}"
        );
        // …and the full `*` is GONE (this is the regression guard).
        assert!(
            !effective.iter().any(|p| p == "page:inject_script:*"),
            "the full `*` grant must NOT survive narrowing; got: {effective:?}"
        );
    }

    /// Two pending plugins: the initial-load pass shows only the first; once it
    /// resolves, the shell advances to the next (the minimal pending queue).
    /// Headless coverage of the queue advance on `approve_pending`.
    #[test]
    fn approve_pending_advances_through_multiple_pending() {
        // Two never-approved path plugins → two pending entries.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        write_path_plugin(dir_a.path(), "pending-a");
        write_path_plugin(dir_b.path(), "pending-b");
        write_plugins_lua(
            config.path(),
            &[
                ("pending-a", &format!("path:{}", dir_a.path().display())),
                ("pending-b", &format!("path:{}", dir_b.path().display())),
            ],
        );
        let store = Store::open_in_memory().unwrap();
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();
        assert_eq!(
            host.pending_approvals.len(),
            2,
            "both unapproved plugins are pending"
        );

        // Resolve the first; the second remains pending (the shell would now
        // show it). approve_pending only resolves the named entry.
        let outcome = host.approve_pending(&grant_full("pending-a", "storage", "persistent"));
        assert_eq!(outcome, ApproveOutcome::Loaded);
        assert_eq!(
            host.pending_approvals.len(),
            1,
            "exactly one pending entry remains after resolving the first"
        );
        assert_eq!(
            host.pending_approvals[0].0.name.as_str(),
            "pending-b",
            "the remaining pending entry is the second plugin"
        );
    }

    // -----------------------------------------------------------------------
    // Task 9: revoke_secret + loaded_row secret rows
    // -----------------------------------------------------------------------

    /// Write a `secrets.lua` that defines one env-backed secret named `name`
    /// reading from the env var `var`.
    fn write_secrets_lua(config_dir: &std::path::Path, name: &str, var: &str) {
        std::fs::write(
            config_dir.join("secrets.lua"),
            format!(
                "mote.secrets.define({{ [\"{name}\"] = {{ backend = \"env\", var = \"{var}\" }} }})\n"
            ),
        )
        .unwrap();
    }

    /// Boot a host with a path-plugin that requests the given permissions and
    /// a `secrets.lua` defining one env-backed secret.  Returns the host and the
    /// tempdir that owns the plugin source so the path: source stays alive.
    fn host_with_secret_plugin(
        plugin_name: &str,
        permissions: &[&str],
        secret_name: &str,
        env_var: &str,
    ) -> (PluginHost, tempfile::TempDir) {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin_with_perms(plugin_dir.path(), plugin_name, permissions);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[(plugin_name, &src)]);
        write_secrets_lua(config.path(), secret_name, env_var);

        // Pre-approve so the plugin auto-grants.
        let store = Store::open_in_memory().unwrap();
        let manifest = mote_lua::load_plugin(
            &std::fs::read_to_string(plugin_dir.path().join("init.lua")).unwrap(),
            plugin_name,
        )
        .unwrap()
        .manifest()
        .clone();
        let approval_store = mote_pluginmgr::ApprovalStore::new(&store);
        approval_store
            .put(&manifest.name, &ApprovalHash::of(&manifest))
            .unwrap();

        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();
        assert!(
            host.loaded.iter().any(|rp| rp.name.as_str() == plugin_name),
            "plugin `{plugin_name}` must be loaded"
        );
        (host, plugin_dir)
    }

    /// Test (1): `revoke_secret` narrows exactly the target secret and leaves
    /// the other intact.
    #[test]
    fn revoke_secret_removes_target_keeps_other() {
        let (mut host, _dir) = host_with_secret_plugin(
            "multi-secret",
            &["secret:read:A", "secret:read:B"],
            // Only define A for the resolver; B will be "unknown" — that's fine.
            "A",
            "HOME",
        );

        let plugin = PluginName::new("multi-secret").unwrap();

        // Before: plugin can read both A and B.
        let running = host.runtime.running(&plugin).unwrap();
        assert!(
            running
                .effective_permissions
                .iter()
                .any(|p| p == "secret:read:A"),
            "before revoke: A must be in effective permissions"
        );
        assert!(
            running
                .effective_permissions
                .iter()
                .any(|p| p == "secret:read:B"),
            "before revoke: B must be in effective permissions"
        );

        host.revoke_secret("multi-secret", "A");

        // After: A is gone, B remains.
        let running = host.runtime.running(&plugin).unwrap();
        assert!(
            !running
                .effective_permissions
                .iter()
                .any(|p| p == "secret:read:A"),
            "after revoke: A must NOT be in effective permissions"
        );
        assert!(
            running
                .effective_permissions
                .iter()
                .any(|p| p == "secret:read:B"),
            "after revoke: B must still be in effective permissions"
        );
    }

    /// Test (2): revoking the last secret leaves the plugin loaded but with no
    /// secret-read grants.
    #[test]
    fn revoke_last_secret_leaves_plugin_loaded_with_no_secret_grants() {
        let (mut host, _dir) =
            host_with_secret_plugin("single-secret", &["secret:read:ONLY"], "ONLY", "HOME");

        let plugin = PluginName::new("single-secret").unwrap();

        // Before: can read ONLY.
        let running = host.runtime.running(&plugin).unwrap();
        assert!(
            running
                .effective_permissions
                .iter()
                .any(|p| p == "secret:read:ONLY"),
            "before: ONLY must be in effective permissions"
        );

        host.revoke_secret("single-secret", "ONLY");

        // After: plugin still running, but no secret:read grants.
        let running = host
            .runtime
            .running(&plugin)
            .expect("plugin must remain loaded after revoking its last secret");
        assert!(
            !running
                .effective_permissions
                .iter()
                .any(|p| p.starts_with("secret:read:")),
            "after revoking last secret: no secret:read grants must remain; got: {:?}",
            running.effective_permissions
        );
    }

    /// Test (3): `build_panel` / `loaded_row` populates `secrets` with the
    /// correct backend label for a plugin granted a defined secret.
    #[test]
    fn loaded_row_populates_secret_rows_with_backend() {
        // Set HOME so the env backend can be looked up (it always exists).
        let (host, _dir) =
            host_with_secret_plugin("env-secret-user", &["secret:read:MY_KEY"], "MY_KEY", "HOME");

        let panel = host.build_panel();
        let row = panel
            .plugins
            .iter()
            .find(|p| p.name == "env-secret-user")
            .expect("plugin row must be present in the panel");

        assert_eq!(
            row.secrets.len(),
            1,
            "exactly one secret row expected; got {:?}",
            row.secrets
        );
        assert_eq!(row.secrets[0].name, "MY_KEY");
        assert_eq!(
            row.secrets[0].backend, "env",
            "secret defined with env backend must have backend label `env`"
        );
        // last_read is None because no secrets.get call has been audited.
        assert_eq!(row.secrets[0].last_read, None);
    }

    /// Test (4): a plugin granted a secret not defined in `secrets.lua` shows
    /// backend `"unknown"` in the panel row.
    #[test]
    fn loaded_row_unknown_backend_for_undefined_secret() {
        // The plugin requests secret:read:UNDEF but no secrets.lua defines it.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_path_plugin_with_perms(
            plugin_dir.path(),
            "undef-secret-user",
            &["secret:read:UNDEF"],
        );
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(config.path(), &[("undef-secret-user", &src)]);
        // No write_secrets_lua call — resolver has no defs.

        let store = Store::open_in_memory().unwrap();
        let manifest = mote_lua::load_plugin(
            &std::fs::read_to_string(plugin_dir.path().join("init.lua")).unwrap(),
            "undef-secret-user",
        )
        .unwrap()
        .manifest()
        .clone();
        let approval_store = mote_pluginmgr::ApprovalStore::new(&store);
        approval_store
            .put(&manifest.name, &ApprovalHash::of(&manifest))
            .unwrap();

        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();

        let panel = host.build_panel();
        let row = panel
            .plugins
            .iter()
            .find(|p| p.name == "undef-secret-user")
            .expect("plugin row must be present");

        assert_eq!(row.secrets.len(), 1);
        assert_eq!(row.secrets[0].name, "UNDEF");
        assert_eq!(
            row.secrets[0].backend, "unknown",
            "undefined secret must have backend label `unknown`"
        );
    }

    // -----------------------------------------------------------------------
    // Task C2: urlbar_query — invoke_capability path (shell-side handler logic)
    //
    // The eval_js → chrome push cannot be unit-tested without a live CEF engine
    // (real CEF dependency; the integration seam is live-verified as part of D1
    // + ydotool). These tests cover the assertable intermediate state:
    // invoke_capability correctly resolves the ui:urlbar_provider, and the
    // HostValue result serialises to valid JSON — the exact bytes the shell
    // passes to eval_js.
    // -----------------------------------------------------------------------

    /// Boot a host with the bundled plugins loaded (no path plugins; rely on
    /// the auto-grant bundled pass). Returns the host and two tempdirs whose
    /// lifetimes cover the test.
    fn boot_host_with_bundled_plugins() -> (PluginHost, tempfile::TempDir, tempfile::TempDir) {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let mut host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        host.run_initial_load_pass();
        let loaded: Vec<&str> = host.loaded.iter().map(|r| r.name.as_str()).collect();
        assert!(
            loaded.contains(&"history"),
            "bundled history must load for these tests: {loaded:?}"
        );
        (host, config, cache)
    }

    /// Seed a visit via `ui:history_provider` → `record_visit`.
    ///
    /// Passes a wall-clock `time` in milliseconds.  Callers that do not care
    /// about a specific timestamp can pass any positive value; callers that seed
    /// many visits should use distinct timestamps to avoid event-key collisions.
    fn seed_visit(host: &PluginHost, url: &str, time_ms: f64) {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        let mut m = BTreeMap::new();
        m.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        m.insert("time".to_owned(), HostValue::Number(time_ms));
        host.runtime
            .invoke_capability("ui:history_provider", "record_visit", &HostValue::Map(m))
            .expect("record_visit must succeed when history is loaded");
    }

    /// After seeding a history visit, the shell's `invoke_capability` path
    /// (Task C2) returns a non-empty list and the JSON string is a valid JSON
    /// array of suggestion objects — this is the exact payload pushed to chrome
    /// via `eval_js`.
    ///
    /// The history plugin's `query(text)` function accepts a plain string
    /// argument; the shell passes `HostValue::Str(text)` directly (matching the
    /// pattern established by `host_invoke_capability.rs` Task C1 tests).
    ///
    /// NOTE: The `eval_js` → chrome render is the live-verification gap flagged
    /// for D1; this test covers the assertable intermediate state.
    #[test]
    fn urlbar_query_op_produces_suggestions() {
        use mote_runtime::{HostValue, host_to_json};

        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        seed_visit(&host, "https://example.com/foo", 1_700_000_001_000.0);
        seed_visit(&host, "https://example.com/foo", 1_700_000_002_000.0);
        seed_visit(&host, "https://other.example/bar", 1_700_000_003_000.0);

        // The shell passes text as a plain Str (the Lua function signature is
        // `query(text)` — a string, not a map).
        let arg = HostValue::Str("foo".to_owned());

        let suggestions = host
            .runtime
            .invoke_capability("ui:urlbar_provider", "query", &arg)
            .unwrap_or_else(|| HostValue::List(vec![]));

        // Must be a non-empty list.
        match &suggestions {
            HostValue::List(items) => assert!(
                !items.is_empty(),
                "query('foo') must return at least one suggestion"
            ),
            other => panic!("expected HostValue::List; got {other:?}"),
        }

        // Serialise exactly as the shell handler does.
        let payload_json = serde_json::to_string(&host_to_json(&suggestions))
            .expect("HostValue must serialise to JSON");

        // The payload must be a valid JSON array.
        let parsed: serde_json::Value =
            serde_json::from_str(&payload_json).expect("payload must be valid JSON");
        assert!(
            parsed.is_array(),
            "urlbar_suggestions payload must be a JSON array; got {payload_json}"
        );
        assert!(
            !parsed.as_array().unwrap().is_empty(),
            "urlbar_suggestions payload must not be empty for 'foo' query"
        );
    }

    /// Empty text → history plugin's `query("")` early-exits returning `{}` (an
    /// empty Lua table). The runtime marshals that as `HostValue::Map({})` rather
    /// than `HostValue::List([])` (Lua cannot distinguish the two at the empty
    /// boundary). The shell normalises any non-List result to an empty List, so
    /// the final payload pushed to chrome is always `[]`.
    ///
    /// NOTE: The `eval_js` → chrome render is the live-verification gap flagged
    /// for D1; this test covers the assertable intermediate state.
    #[test]
    fn urlbar_query_empty_text_pushes_empty_or_valid_list() {
        use mote_runtime::{HostValue, host_to_json};

        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        // No visits seeded — empty text hits the fast-path in the Lua function.
        let arg = HostValue::Str(String::new());

        // Mirror the shell's normalisation: only a List passes through; anything
        // else (including the Map({}) the Lua empty-table becomes) → empty list.
        let raw = host
            .runtime
            .invoke_capability("ui:urlbar_provider", "query", &arg);
        let suggestions = match raw {
            Some(HostValue::List(items)) => HostValue::List(items),
            _ => HostValue::List(vec![]),
        };

        let payload_json = serde_json::to_string(&host_to_json(&suggestions))
            .expect("HostValue must serialise to JSON");

        // After normalisation the payload must be the JSON empty array.
        assert_eq!(
            payload_json, "[]",
            "empty-text query must produce `[]` after List normalisation; got {payload_json}"
        );
    }

    /// When no provider is loaded, `invoke_capability` returns `None`; the shell
    /// falls back to `HostValue::List(vec![])` and serialises it as `[]`. The
    /// chrome receives `applyOp('urlbar_suggestions', [])` so the dropdown clears.
    ///
    /// NOTE: The `eval_js` → chrome render is the live-verification gap flagged
    /// for D1; this test covers the assertable intermediate state.
    #[test]
    fn urlbar_query_with_no_provider_pushes_empty_list() {
        use mote_runtime::{HostValue, host_to_json};

        // Boot WITHOUT running the load pass → no plugins loaded, no fulfiller.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        // Deliberately NOT calling run_initial_load_pass.

        let arg = HostValue::Str("foo".to_owned());

        // None → falls back to empty list (the handler's unwrap_or_else).
        let suggestions = host
            .runtime
            .invoke_capability("ui:urlbar_provider", "query", &arg)
            .unwrap_or_else(|| HostValue::List(vec![]));

        let payload_json = serde_json::to_string(&host_to_json(&suggestions))
            .expect("HostValue::List(vec![]) must serialise to JSON");

        assert_eq!(
            payload_json, "[]",
            "no-provider fallback must serialise to the JSON empty array `[]`"
        );
    }

    // -----------------------------------------------------------------------
    // Task F2: navigate_records_visit — verify the record_visit invocation
    // path that navigate_active wires.
    //
    // ShellApp is not constructible headlessly (requires a live CEF engine +
    // window), so these tests exercise PluginHost::runtime.invoke_capability
    // directly — the exact call that navigate_active adds.  The integration
    // seam (navigate_active *actually* calling record_visit) is verified by the
    // live in-app smoke test flagged in the plan as "live verification required"
    // for F2.
    // -----------------------------------------------------------------------

    /// After `navigate_active` fires `record_visit` for a URL, querying
    /// `query_history` returns that URL in the result set and a `u:<url>` record
    /// exists (via `sort=relevance`) plus at least one `e:<...>` event (via
    /// `sort=recent`).
    ///
    /// The call shape mirrors the exact code in `navigate_active`:
    ///   `HostValue::Map({"url" → Str, "time" → Number})`
    #[test]
    fn navigate_records_visit() {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        let (host, _config, _cache) = boot_host_with_bundled_plugins();

        // Simulate what navigate_active calls.
        let mut arg_map = BTreeMap::new();
        arg_map.insert(
            "url".to_owned(),
            HostValue::Str("https://example.test".to_owned()),
        );
        arg_map.insert("time".to_owned(), HostValue::Number(1_700_000_001_000.0));
        let arg = HostValue::Map(arg_map);
        let result = host
            .runtime
            .invoke_capability("ui:history_provider", "record_visit", &arg);
        assert!(
            result.is_some(),
            "record_visit must return Some when history is loaded"
        );

        // URL record exists (sort=relevance returns deduped URL-level records).
        let mut qmap = BTreeMap::new();
        qmap.insert(
            "filter".to_owned(),
            HostValue::Str("example.test".to_owned()),
        );
        qmap.insert("sort".to_owned(), HostValue::Str("relevance".to_owned()));
        let raw = host.runtime.invoke_capability(
            "ui:history_provider",
            "query_history",
            &HostValue::Map(qmap),
        );
        let items = match raw {
            Some(HostValue::List(v)) => v,
            other => panic!("query_history(relevance) must return List; got {other:?}"),
        };
        assert_eq!(
            items.len(),
            1,
            "must have exactly one URL record; got {items:?}"
        );
        let record = match &items[0] {
            HostValue::Map(m) => m,
            other => panic!("record must be a Map; got {other:?}"),
        };
        assert_eq!(
            record.get("url"),
            Some(&HostValue::Str("https://example.test".to_owned())),
            "URL must match"
        );
        // total_count must be 1 after one visit.
        match record.get("total_count") {
            Some(HostValue::Number(f)) => assert!(
                (*f - 1.0_f64).abs() < f64::EPSILON,
                "total_count must be 1.0; got {f}"
            ),
            other => panic!("total_count must be Number; got {other:?}"),
        }

        // Event also exists (sort=recent returns one row per event).
        let mut eqmap = BTreeMap::new();
        eqmap.insert(
            "filter".to_owned(),
            HostValue::Str("example.test".to_owned()),
        );
        eqmap.insert("sort".to_owned(), HostValue::Str("recent".to_owned()));
        let eraw = host.runtime.invoke_capability(
            "ui:history_provider",
            "query_history",
            &HostValue::Map(eqmap),
        );
        let eitems = match eraw {
            Some(HostValue::List(v)) => v,
            other => panic!("query_history(recent) must return List; got {other:?}"),
        };
        assert!(
            !eitems.is_empty(),
            "at least one event must exist after record_visit"
        );
    }

    /// Navigating to the same URL twice produces 2 event rows (one per visit)
    /// and 1 deduped URL record (via sort=relevance) with `total_count` = 2.
    #[test]
    fn navigate_increments_visit_count_on_repeat() {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        let (host, _config, _cache) = boot_host_with_bundled_plugins();

        let url = "https://example.test/page";

        // First navigation — distinct timestamp.
        let mut arg1 = BTreeMap::new();
        arg1.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        arg1.insert("time".to_owned(), HostValue::Number(1_700_000_001_000.0));
        host.runtime
            .invoke_capability("ui:history_provider", "record_visit", &HostValue::Map(arg1))
            .expect("first record_visit must succeed");

        // Second navigation — distinct timestamp.
        let mut arg2 = BTreeMap::new();
        arg2.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        arg2.insert("time".to_owned(), HostValue::Number(1_700_000_002_000.0));
        host.runtime
            .invoke_capability("ui:history_provider", "record_visit", &HostValue::Map(arg2))
            .expect("second record_visit must succeed");

        // URL record (sort=relevance) — exactly 1 deduped record, total_count=2.
        let mut qmap = BTreeMap::new();
        qmap.insert(
            "filter".to_owned(),
            HostValue::Str("example.test".to_owned()),
        );
        qmap.insert("sort".to_owned(), HostValue::Str("relevance".to_owned()));
        let raw = host
            .runtime
            .invoke_capability(
                "ui:history_provider",
                "query_history",
                &HostValue::Map(qmap),
            )
            .expect("query_history must return Some");
        let items = match raw {
            HostValue::List(v) => v,
            other => panic!("query_history(relevance) must return List; got {other:?}"),
        };
        assert_eq!(
            items.len(),
            1,
            "repeated navigate must deduplicate to one URL record; got {items:?}"
        );
        let record = match &items[0] {
            HostValue::Map(m) => m,
            other => panic!("record must be a Map; got {other:?}"),
        };
        match record.get("total_count") {
            Some(HostValue::Number(f)) => assert!(
                (*f - 2.0_f64).abs() < f64::EPSILON,
                "total_count must be 2.0 after two navigations; got {f}"
            ),
            other => panic!("total_count must be Number; got {other:?}"),
        }

        // Events (sort=recent) — exactly 2 rows.
        let mut eqmap = BTreeMap::new();
        eqmap.insert(
            "filter".to_owned(),
            HostValue::Str("example.test".to_owned()),
        );
        eqmap.insert("sort".to_owned(), HostValue::Str("recent".to_owned()));
        let eraw = host
            .runtime
            .invoke_capability(
                "ui:history_provider",
                "query_history",
                &HostValue::Map(eqmap),
            )
            .expect("query_history(recent) must return Some");
        let eitems = match eraw {
            HostValue::List(v) => v,
            other => panic!("query_history(recent) must return List; got {other:?}"),
        };
        assert_eq!(
            eitems.len(),
            2,
            "two navigations must produce 2 event rows; got {eitems:?}"
        );
    }

    /// When no history plugin is loaded, `invoke_capability` returns `None` and
    /// `navigate_active` absorbs the result with `let _ = ...` — no panic.
    ///
    /// The shell must continue normally when the history provider is absent.
    #[test]
    fn navigate_without_history_plugin_does_not_panic() {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        // Boot WITHOUT running the load pass → no plugins, no history fulfiller.
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let host = PluginHost::boot_in(store, config.path(), cache.path()).unwrap();
        // Deliberately NOT calling run_initial_load_pass.

        // Simulate the navigate_active call: must not panic, result is None.
        let mut arg_map = BTreeMap::new();
        arg_map.insert(
            "url".to_owned(),
            HostValue::Str("https://example.test".to_owned()),
        );
        let arg = HostValue::Map(arg_map);
        let result = host
            .runtime
            .invoke_capability("ui:history_provider", "record_visit", &arg);

        // The shell absorbs this with `let _ = ...`; no panic is the assertion.
        assert!(
            result.is_none(),
            "invoke_capability must return None when history provider is not loaded"
        );
    }

    // Task F2 follow-up: title-on-load — verify that `update_title` updates the
    // cosmetic title field without re-counting the visit.
    //
    // The shell calls `record_visit({url})` at navigate time (F2), then calls
    // `update_title({url, title})` when `sync_active_title` fires with the
    // resolved title.  The invariants:
    //   • title is populated after `update_title`.
    //   • `visit_count` remains 1 — `update_title` is not a re-visit.
    //   • `last_visited` is unchanged — only the title field is overwritten.
    //   • Calling `update_title` for a URL with no prior record is a no-op.

    /// Query history (sort=relevance, URL-level records) and return the single
    /// deduped record for `filter`. Panics if the result is not exactly one `Map`.
    fn query_one_record(
        host: &PluginHost,
        filter: &str,
    ) -> std::collections::BTreeMap<String, mote_runtime::HostValue> {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;
        let mut qmap = BTreeMap::new();
        qmap.insert("filter".to_owned(), HostValue::Str(filter.to_owned()));
        qmap.insert("sort".to_owned(), HostValue::Str("relevance".to_owned()));
        let raw = host
            .runtime
            .invoke_capability(
                "ui:history_provider",
                "query_history",
                &HostValue::Map(qmap),
            )
            .expect("query_history must return Some");
        let items = match raw {
            HostValue::List(v) => v,
            other => panic!("query_history must return List; got {other:?}"),
        };
        assert_eq!(
            items.len(),
            1,
            "expected exactly one record for filter {filter:?}; got {items:?}"
        );
        match items.into_iter().next().unwrap() {
            HostValue::Map(m) => m,
            other => panic!("history record must be a Map; got {other:?}"),
        }
    }

    /// `update_title` resolves the async page title without incrementing
    /// `total_count` or touching `last_seen_ms`.
    ///
    /// Flow mirrors the production path:
    ///   1. `record_visit({url, time})` — F2 navigate (URL + timestamp, no title).
    ///   2. `update_title({url, title})` — title-on-load (`sync_active_title`).
    ///
    /// Asserts: title populated; `total_count` stays 1.0; `last_seen_ms`
    /// unchanged; `update_title` on a never-visited URL creates no phantom record.
    #[test]
    fn update_title_resolves_title_without_recounting_visit() {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        let url = "https://example.test/async-title";

        // Step 1: F2 navigate path — record_visit with URL + timestamp.
        let mut arg = BTreeMap::new();
        arg.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        arg.insert("time".to_owned(), HostValue::Number(1_700_000_001_000.0));
        host.runtime
            .invoke_capability("ui:history_provider", "record_visit", &HostValue::Map(arg))
            .expect("record_visit must succeed when history is loaded");

        // Capture baseline: title empty, total_count == 1, record last_seen_ms.
        let rec = query_one_record(&host, "async-title");
        let last_seen_ms_before = match rec.get("last_seen_ms").expect("must have last_seen_ms") {
            HostValue::Number(f) => *f,
            other => panic!("last_seen_ms must be Number; got {other:?}"),
        };
        match rec.get("total_count").expect("must have total_count") {
            HostValue::Number(f) => assert!(
                (*f - 1.0_f64).abs() < f64::EPSILON,
                "total_count must be 1.0 after record_visit; got {f}"
            ),
            other => panic!("total_count must be Number; got {other:?}"),
        }
        match rec.get("title").expect("must have title") {
            HostValue::Str(s) => assert!(
                s.is_empty(),
                "title must be empty after record_visit (title is set by update_title)"
            ),
            other => panic!("title must be Str; got {other:?}"),
        }

        // Step 2: title-on-load path — update_title with url + title.
        let mut arg = BTreeMap::new();
        arg.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        arg.insert(
            "title".to_owned(),
            HostValue::Str("Example Title".to_owned()),
        );
        assert!(
            host.runtime
                .invoke_capability("ui:history_provider", "update_title", &HostValue::Map(arg))
                .is_some(),
            "update_title must return Some when history is loaded and record exists"
        );

        // Assert all invariants after update_title.
        let rec = query_one_record(&host, "async-title");
        match rec.get("title").expect("must have title") {
            HostValue::Str(s) => assert_eq!(s, "Example Title", "title must be populated"),
            other => panic!("title must be Str; got {other:?}"),
        }
        match rec.get("total_count").expect("must have total_count") {
            HostValue::Number(f) => assert!(
                (*f - 1.0_f64).abs() < f64::EPSILON,
                "total_count must remain 1.0 after update_title; got {f}"
            ),
            other => panic!("total_count must be Number; got {other:?}"),
        }
        match rec.get("last_seen_ms").expect("must have last_seen_ms") {
            HostValue::Number(f) => assert!(
                (*f - last_seen_ms_before).abs() < f64::EPSILON,
                "last_seen_ms must be unchanged after update_title; got {f}, was {last_seen_ms_before}"
            ),
            other => panic!("last_seen_ms must be Number; got {other:?}"),
        }

        // Step 3: update_title on a never-visited URL must be a no-op (no phantom record).
        let mut arg = BTreeMap::new();
        arg.insert(
            "url".to_owned(),
            HostValue::Str("https://never-visited.test".to_owned()),
        );
        arg.insert("title".to_owned(), HostValue::Str("Ghost".to_owned()));
        let _ = host.runtime.invoke_capability(
            "ui:history_provider",
            "update_title",
            &HostValue::Map(arg),
        );

        // An empty Lua `{}` decodes as Map({}) or List([]); both mean no records.
        let mut qmap = BTreeMap::new();
        qmap.insert(
            "filter".to_owned(),
            HostValue::Str("never-visited".to_owned()),
        );
        qmap.insert("sort".to_owned(), HostValue::Str("relevance".to_owned()));
        let raw = host
            .runtime
            .invoke_capability(
                "ui:history_provider",
                "query_history",
                &HostValue::Map(qmap),
            )
            .expect("query_history must return Some");
        let is_empty = match &raw {
            HostValue::List(v) => v.is_empty(),
            HostValue::Map(m) => m.is_empty(),
            other => panic!("expected empty result; got {other:?}"),
        };
        assert!(
            is_empty,
            "update_title on a never-visited URL must not create a phantom record"
        );
    }

    // ── bookmarks + history panel data tests (Phase 5a Tasks D3 / D4) ────
    //
    // These tests exercise the shell-side data-assembly logic for the two new
    // sidebar panels.  The assertable seam is the JSON payload that would be
    // pushed to chrome via eval_js.  The actual eval_js call is NOT exercised
    // here because the chrome page is not available in headless tests.
    //
    // LIVE-VERIFICATION GAP: the orchestrator must drive the following in a
    // running Mote instance to confirm end-to-end wiring:
    //   1. Click the bookmarks button → panel switches, header shows [bookmarks],
    //      bookmark rows appear (add a bookmark first if none exist).
    //   2. Click a bookmark row → navigates to that URL.
    //   3. Click × on a bookmark row → entry removed, list re-renders without it.
    //   4. Click the history button → panel switches, header shows [history],
    //      history rows appear sorted by recency.
    //   5. Click a history row → navigates to that URL.
    //   6. Switch back to tabs → [tabs] header restored.

    /// Helper: seed a bookmark via `ui:bookmarks_provider` → `add_bookmark`.
    fn seed_bookmark(host: &PluginHost, url: &str, title: &str) {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        let mut m = BTreeMap::new();
        m.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        m.insert("title".to_owned(), HostValue::Str(title.to_owned()));
        host.runtime
            .invoke_capability("ui:bookmarks_provider", "add_bookmark", &HostValue::Map(m))
            .expect("add_bookmark must succeed when bookmarks is loaded");
    }

    /// Helper: remove a bookmark via `ui:bookmarks_provider` → `remove_bookmark`.
    fn remove_bookmark(host: &PluginHost, url: &str) {
        use std::collections::BTreeMap;

        use mote_runtime::HostValue;

        let mut m = BTreeMap::new();
        m.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        host.runtime
            .invoke_capability(
                "ui:bookmarks_provider",
                "remove_bookmark",
                &HostValue::Map(m),
            )
            .expect("remove_bookmark must succeed when bookmarks is loaded");
    }

    /// `set_active_panel("bookmarks")` causes the shell to build a `bookmark_list`
    /// payload that includes previously-added bookmarks.
    ///
    /// Assertion technique: call `crate::build_bookmark_list_json` (the
    /// test-accessible JSON-build seam) directly rather than wiring a full
    /// `ShellApp` (which requires a live window bridge not available headlessly).
    /// The `eval_js` push is the live-verification gap documented above.
    #[test]
    fn set_active_panel_bookmarks_invokes_list() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        seed_bookmark(&host, "https://example.test/page", "Example Page");

        let json = crate::build_bookmark_list_json(&host)
            .expect("bookmark_list payload must be built when bookmarks plugin is loaded");

        // Parse and assert the seeded bookmark is in the payload.
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("payload must be valid JSON");

        let rows = parsed["rows"].as_array().expect("rows must be an array");
        assert!(!rows.is_empty(), "rows must contain the seeded bookmark");

        let found = rows
            .iter()
            .any(|r| r["url"].as_str() == Some("https://example.test/page"));
        assert!(
            found,
            "seeded url must appear in the bookmark_list payload; got {json}"
        );

        let count = parsed["count"].as_u64().expect("count must be a number");
        assert_eq!(count, rows.len() as u64, "count must match the rows length");
    }

    /// `set_active_panel("history")` builds a `history_list` payload that caps at
    /// 200 rows and sets `truncated: true` when the provider returns more than 200.
    ///
    /// Now that `query_history` accepts `{limit=200}`, the plugin returns up to 200
    /// rows. This test seeds 250 distinct URLs and asserts that exactly 200 rows are
    /// returned with `truncated=true`. The plugin caps at 200 (limit param) and the
    /// shell's defensive cap fires at the same threshold — `truncated` is produced
    /// by the shell's `original_count > HISTORY_CAP` guard.
    ///
    /// Assertion technique: `crate::build_history_list_json` (same seam as above).
    #[test]
    fn set_active_panel_history_caps_at_200_and_flags_truncation() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();

        // Seed 250 distinct URLs with distinct timestamps.  The shell requests
        // limit=HISTORY_CAP+1 (201) to overfetch and detect truncation; the
        // plugin returns up to 201; the shell truncates to HISTORY_CAP (200)
        // and flags truncated=true.
        for i in 0..250_u32 {
            seed_visit(
                &host,
                &format!("https://example.test/page-{i:03}"),
                1_700_000_000_000.0 + f64::from(i),
            );
        }

        let json = crate::build_history_list_json(&host)
            .expect("history_list payload must be built when history plugin is loaded");

        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("payload must be valid JSON");

        let rows = parsed["rows"].as_array().expect("rows must be an array");
        assert_eq!(
            rows.len(),
            200,
            "displayed rows must be capped at HISTORY_CAP (200) when 250 visits exist"
        );

        let count = parsed["count"].as_u64().expect("count must be a number");
        assert_eq!(
            count, 200,
            "count must be 200 (the truncated display length)"
        );

        let truncated = parsed["truncated"]
            .as_bool()
            .expect("truncated must be a bool");
        assert!(
            truncated,
            "truncated must be true: 250 > 200, so the overfetch (limit=201) \
             returns 201 rows and the shell flags truncation"
        );
    }

    /// Negative companion to the truncation test: when stored visits <= `HISTORY_CAP`,
    /// the overfetch trick correctly returns truncated=false.
    #[test]
    fn set_active_panel_history_truncated_false_below_cap() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        for i in 0..150_u32 {
            seed_visit(
                &host,
                &format!("https://below-cap.test/p-{i:03}"),
                1_700_000_000_000.0 + f64::from(i),
            );
        }
        let json = crate::build_history_list_json(&host).expect("payload built");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let rows = parsed["rows"].as_array().expect("rows array");
        assert_eq!(rows.len(), 150, "exactly the seeded count of rows");
        assert!(
            !parsed["truncated"].as_bool().unwrap(),
            "truncated must be false when fewer than HISTORY_CAP visits exist"
        );
    }

    /// `bookmark_remove` drops the targeted entry and the re-pushed list no longer
    /// contains it; count decreases accordingly.
    ///
    /// Assertion technique: `crate::build_bookmark_list_json` (same seam as above).
    #[test]
    fn bookmark_remove_op_drops_entry_and_repushes_list() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        seed_bookmark(&host, "https://keep.test/a", "Keep");
        seed_bookmark(&host, "https://remove.test/b", "Remove");

        // Confirm both entries are present.
        let before = crate::build_bookmark_list_json(&host)
            .expect("bookmark_list before remove must be buildable");
        let parsed_before: serde_json::Value =
            serde_json::from_str(&before).expect("before payload must be valid JSON");
        assert_eq!(
            parsed_before["count"].as_u64().unwrap(),
            2,
            "must have 2 bookmarks before remove"
        );

        // Remove the second bookmark.
        remove_bookmark(&host, "https://remove.test/b");

        let after = crate::build_bookmark_list_json(&host)
            .expect("bookmark_list after remove must be buildable");
        let parsed_after: serde_json::Value =
            serde_json::from_str(&after).expect("after payload must be valid JSON");

        let rows_after = parsed_after["rows"].as_array().expect("rows must be array");
        assert_eq!(
            parsed_after["count"].as_u64().unwrap(),
            1,
            "count must decrease to 1 after remove"
        );
        let still_present = rows_after
            .iter()
            .any(|r| r["url"].as_str() == Some("https://remove.test/b"));
        assert!(
            !still_present,
            "removed url must not appear in the list after remove_bookmark"
        );
        let kept = rows_after
            .iter()
            .any(|r| r["url"].as_str() == Some("https://keep.test/a"));
        assert!(kept, "non-removed bookmark must still appear in the list");
    }

    /// `bookmark_toggle` on an un-bookmarked URL adds it; count goes from 0 → 1.
    ///
    /// Assertion technique: `crate::is_url_bookmarked_in_host` (same headless
    /// seam as `build_bookmark_list_json`).
    #[test]
    fn bookmark_toggle_adds_when_not_bookmarked() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        let url = "https://toggle-add.test/page";

        // Confirm the URL is not yet bookmarked.
        assert!(
            !crate::is_url_bookmarked_in_host(&host, url),
            "url must not be bookmarked before toggle"
        );

        // Simulate the toggle: add it (the add-path of bookmark_toggle).
        seed_bookmark(&host, url, "Toggle Add Test");

        assert!(
            crate::is_url_bookmarked_in_host(&host, url),
            "url must be bookmarked after toggle-add"
        );

        let json = crate::build_bookmark_list_json(&host)
            .expect("bookmark_list must be buildable after add");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("payload must be valid JSON");
        assert_eq!(
            parsed["count"].as_u64().unwrap(),
            1,
            "count must be 1 after adding one bookmark"
        );
    }

    /// `bookmark_toggle` on an already-bookmarked URL removes it; count goes
    /// from 1 → 0.
    #[test]
    fn bookmark_toggle_removes_when_bookmarked() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        let url = "https://toggle-remove.test/page";

        // Pre-add so it is bookmarked.
        seed_bookmark(&host, url, "Toggle Remove Test");
        assert!(
            crate::is_url_bookmarked_in_host(&host, url),
            "url must be bookmarked before the remove toggle"
        );

        // Simulate the remove-path of bookmark_toggle.
        remove_bookmark(&host, url);

        assert!(
            !crate::is_url_bookmarked_in_host(&host, url),
            "url must no longer be bookmarked after toggle-remove"
        );

        let json = crate::build_bookmark_list_json(&host)
            .expect("bookmark_list must be buildable after remove");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("payload must be valid JSON");
        assert_eq!(
            parsed["count"].as_u64().unwrap(),
            0,
            "count must be 0 after removing the only bookmark"
        );
    }

    /// Two consecutive `bookmark_toggle` operations restore the original state
    /// (add then remove → not bookmarked again).
    #[test]
    fn bookmark_toggle_is_idempotent_pair() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();
        let url = "https://toggle-pair.test/page";

        // Confirm starts empty.
        assert!(
            !crate::is_url_bookmarked_in_host(&host, url),
            "must start un-bookmarked"
        );

        // First toggle: add.
        seed_bookmark(&host, url, "Idempotent Pair");
        assert!(
            crate::is_url_bookmarked_in_host(&host, url),
            "must be bookmarked after first toggle"
        );

        // Second toggle: remove.
        remove_bookmark(&host, url);
        assert!(
            !crate::is_url_bookmarked_in_host(&host, url),
            "must be un-bookmarked after second toggle (back to original state)"
        );
    }

    // -----------------------------------------------------------------------
    // Task E3: workspace switch — shell mechanism tests
    //
    // `ShellApp` cannot be constructed headlessly (requires a live CEF engine
    // + bridge).  The assertable intermediate state is:
    //
    //  1. `workspace_id_for_slug` maps string ids to stable numeric ids.
    //  2. `invoke_switch_workspace` (the plugin path) returns true for valid ids
    //     and false for unknown ids.
    //  3. `list_workspaces` after a switch shows the new active entry, proving
    //     the plugin actually persisted the change (E3 test 3 — persistence).
    //  4. `Session::tab_picker_ranked` returns the expected set after seeding
    //     tabs, proving the session's workspace-keyed tab list works (the
    //     rebuild-tabs logic in `ShellApp::rebuild_tabs_for_workspace` delegates
    //     to this exact call).
    //
    // The `eval_js` → chrome push half of `switch_workspace` is the live-
    // verification gap (same pattern as C2/D1/D3) — noted in the report.
    // -----------------------------------------------------------------------

    /// `workspace_id_for_slug` returns the stable numeric ids for the built-in
    /// workspace slugs and `None` for unknown slugs.
    #[test]
    fn workspace_id_for_slug_maps_builtin_slugs() {
        use mote_types::WorkspaceId;

        assert_eq!(
            crate::workspace_id_for_slug("default"),
            Some(WorkspaceId::new(0)),
            "\"default\" must map to WorkspaceId(0) — the existing WORKSPACE const"
        );
        assert_eq!(
            crate::workspace_id_for_slug("work"),
            Some(WorkspaceId::new(1)),
            "\"work\" must map to WorkspaceId(1)"
        );
        assert!(
            crate::workspace_id_for_slug("does-not-exist").is_none(),
            "unknown slug must return None"
        );
    }

    /// After a successful `switch_workspace("work")`, `list_workspaces` shows
    /// `work` as the active entry.  This proves the plugin persisted the change
    /// (E3 test 3 — persistence via plugin).
    #[test]
    fn switch_workspace_persists_via_plugin() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();

        // Verify the workspace-manager loaded.
        assert!(
            host.loaded
                .iter()
                .any(|r| r.name.as_str() == "workspace-manager"),
            "workspace-manager must be loaded for this test"
        );

        // Switch to "work".
        let accepted = crate::invoke_switch_workspace(&host, "work");
        assert!(
            accepted,
            "switch_workspace(\"work\") must be accepted by the plugin"
        );

        // Query the plugin: exactly one workspace must be active, and it must be "work".
        let result = host.runtime.invoke_capability(
            "workspace:provider",
            "list_workspaces",
            &mote_runtime::HostValue::Nil,
        );
        let Some(mote_runtime::HostValue::List(workspaces)) = result else {
            panic!("list_workspaces must return a List");
        };

        let active_ids: Vec<String> = workspaces
            .iter()
            .filter_map(|ws| {
                let mote_runtime::HostValue::Map(m) = ws else {
                    return None;
                };
                if matches!(m.get("active"), Some(mote_runtime::HostValue::Bool(true)))
                    && let Some(mote_runtime::HostValue::Str(id)) = m.get("id")
                {
                    return Some(id.clone());
                }
                None
            })
            .collect();

        assert_eq!(
            active_ids,
            vec!["work"],
            "after switch_workspace(\"work\"), list_workspaces must show \"work\" as active; \
             got active_ids={active_ids:?}"
        );
    }

    /// `switch_workspace` rejects an unknown workspace id — the plugin returns
    /// `false` and the seam returns `false`.
    #[test]
    fn switch_workspace_rejects_unknown_id() {
        let (host, _config, _cache) = boot_host_with_bundled_plugins();

        // The "default" workspace must remain active before the test.
        let accepted = crate::invoke_switch_workspace(&host, "does-not-exist");
        assert!(
            !accepted,
            "switch_workspace with an unknown id must return false"
        );

        // "default" must still be the active workspace (no state change).
        let result = host.runtime.invoke_capability(
            "workspace:provider",
            "list_workspaces",
            &mote_runtime::HostValue::Nil,
        );
        let Some(mote_runtime::HostValue::List(workspaces)) = result else {
            panic!("list_workspaces must return a List");
        };
        let active_ids: Vec<String> = workspaces
            .iter()
            .filter_map(|ws| {
                let mote_runtime::HostValue::Map(m) = ws else {
                    return None;
                };
                if matches!(m.get("active"), Some(mote_runtime::HostValue::Bool(true)))
                    && let Some(mote_runtime::HostValue::Str(id)) = m.get("id")
                {
                    return Some(id.clone());
                }
                None
            })
            .collect();

        assert_eq!(
            active_ids,
            vec!["default"],
            "active workspace must remain \"default\" after a rejected switch; \
             got {active_ids:?}"
        );
    }

    /// `Session::tab_picker_ranked` returns only the tabs belonging to the given
    /// workspace — the mechanism the shell's `rebuild_tabs_for_workspace` relies on.
    ///
    /// Seeds two tabs in "default" (WorkspaceId(0)) and two in "work" (WorkspaceId(1)),
    /// then asserts that each workspace's ranked list contains exactly those tabs.
    /// This is a black-box test of the session contract; `ShellApp` delegates to
    /// this exact call.
    #[test]
    fn session_tab_picker_ranked_is_workspace_keyed() {
        use mote_session::Session;
        use mote_types::{IdentityId, WorkspaceId};

        let ws_default = WorkspaceId::new(0);
        let ws_work = WorkspaceId::new(1);
        let mut session = Session::new(IdentityId::new(0), ws_default);

        // Seed two tabs per workspace.
        let _d1 = session.add_tab("https://default.test/a".to_owned(), ws_default);
        let _d2 = session.add_tab("https://default.test/b".to_owned(), ws_default);
        let _w1 = session.add_tab("https://work.test/a".to_owned(), ws_work);
        let _w2 = session.add_tab("https://work.test/b".to_owned(), ws_work);

        // Default workspace: must have exactly the two default-namespace tabs.
        let default_tabs = session.tab_picker_ranked(ws_default);
        assert_eq!(
            default_tabs.len(),
            2,
            "default workspace must have 2 tabs; got {}",
            default_tabs.len()
        );
        assert!(
            default_tabs
                .iter()
                .all(|t| t.url.starts_with("https://default.test/")),
            "all default-workspace tabs must be from default.test; got {:?}",
            default_tabs.iter().map(|t| &t.url).collect::<Vec<_>>()
        );

        // Work workspace: must have exactly the two work-namespace tabs.
        let work_tabs = session.tab_picker_ranked(ws_work);
        assert_eq!(
            work_tabs.len(),
            2,
            "work workspace must have 2 tabs; got {}",
            work_tabs.len()
        );
        assert!(
            work_tabs
                .iter()
                .all(|t| t.url.starts_with("https://work.test/")),
            "all work-workspace tabs must be from work.test; got {:?}",
            work_tabs.iter().map(|t| &t.url).collect::<Vec<_>>()
        );
    }
}
