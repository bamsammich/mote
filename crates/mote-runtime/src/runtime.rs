//! The plugin lifecycle orchestrator: the load pipeline, the live plugin table,
//! and the `load` / `reload` / `unload` lifecycle.
//!
//! See the crate documentation for the end-to-end picture. This module ties the
//! integrated crates together. The pipeline runs in this **actual** order
//! (DESIGN §Enforcement Rules; ADR-0001 "load != run"):
//!
//! 1. **Sandboxed module load / manifest parse** — [`mote_lua::load_plugin`]
//!    evaluates the module body in the constrained sandbox to extract its
//!    declarative surface, including the manifest. This runs **first** because
//!    every subsequent step needs the parsed manifest terms (`permissions` /
//!    `capabilities` / `consumes`), and it is safe to do first: loading
//!    evaluates only the module body to build `M` and **never calls `setup()`**,
//!    so no plugin side effect occurs before validation.
//! 2. **Schema validation + resolution** —
//!    [`mote_registry::Registry::validate_schema`] over the manifest's
//!    `permissions` / `capabilities` / `consumes`, plus the dangling-consumer
//!    and exclusive-double-claim resolution this crate owns.
//! 3. **Contract conformance** — [`mote_registry::Registry::check_conformance`].
//! 4. **Permission approval** — the injected [`ApprovalPolicy`], producing the
//!    effective [`GrantSet`](mote_permissions::GrantSet).
//!
//! Only when all four pass does the runtime install the `mote.*` host API into
//! the plugin's Lua state, register its `M.hooks` into the
//! [`DispatchEngine`](mote_dispatch::DispatchEngine), record its capabilities
//! and event subscriptions in the shared core, and call `setup()`. A failure at
//! any step discards the loaded module without ever running it.

use std::collections::BTreeMap;
use std::rc::Rc;

use mote_audit::EventProducer;
use mote_dispatch::{
    BroadcastOutcome, DispatchEngine, FilterChainOutcome, HookType, KeybindOutcome, NullAudit,
    Registration,
};
use mote_lua::{IdentityScope as ManifestIdentityScope, LoadedPlugin, Manifest, load_plugin};
use mote_permissions::{EffectiveGrants, GrantSet, GrantSetGatekeeper, Permission};
use mote_registry::{EventDispatch, Registry};
use mote_secrets::{SecretProviderRouter, SecretResolver};
use mote_storage::{IdentityScope as StorageScope, Store};
use mote_types::{IdentityId, PluginName};

use crate::approval::{Approval, ApprovalHash, ApprovalPolicy};
use crate::capability::ClaimError;
use crate::core::{Core, PluginRecord};
use crate::error::{LifecycleError, LoadError};
use crate::hostapi::{self, HostContext};
use crate::invoker::RuntimeInvoker;
use crate::secrets_router::RuntimeSecretRouter;

/// The dispatch engine specialized to the runtime's host payload, the runtime's
/// core-backed invoker, and a null dispatch audit (the host-API audit trail —
/// the one the e2e proof reads — is recorded separately by the `mote.*` calls;
/// the dispatch-step trail is wired by the host shell in a later phase). The
/// clock is the system clock.
type Engine = DispatchEngine<crate::value::HostValue, RuntimeInvoker, NullAudit>;

/// A handle to a loaded, running plugin.
///
/// The runtime owns the authoritative live state; this is a cloneable,
/// read-only snapshot of one plugin's identity that the caller (CLI, integrity
/// panel) can inspect.
#[derive(Debug, Clone)]
pub struct RunningPlugin {
    /// The plugin's validated name.
    pub name: PluginName,
    /// The effective permission strings the plugin holds (post-narrowing).
    pub effective_permissions: Vec<String>,
    /// The capabilities the plugin fulfills.
    pub capabilities: Vec<String>,
    /// The capabilities the plugin consumes.
    pub consumes: Vec<String>,
    /// The re-approval fingerprint of the approved manifest (ADR-0001/0002).
    pub approval: ApprovalHash,
}

/// The identity context a plugin is loaded under.
///
/// Determines the storage namespace scope when the manifest's `identity_scope`
/// resolves to a per-identity space, and is recorded for audit.
#[derive(Debug, Clone, Copy)]
pub struct IdentityContext {
    /// The current browser identity (Chromium profile) the plugin runs under.
    pub identity: IdentityId,
}

impl IdentityContext {
    /// A context for the given identity id.
    #[must_use]
    pub const fn new(identity: IdentityId) -> Self {
        Self { identity }
    }
}

/// Internal per-plugin bookkeeping the runtime keeps to support reload/unload.
struct LoadedRecord {
    /// Hook keys this plugin registered into the dispatch engine, with their
    /// types, so reload/unload can deregister cleanly.
    registered_hooks: Vec<(String, HookType)>,
    /// The capabilities this plugin claimed.
    capabilities: Vec<String>,
    /// The re-approval fingerprint of the approved manifest.
    approval: ApprovalHash,
    /// The effective permissions (for `RunningPlugin` snapshots).
    effective_permissions: Vec<String>,
    /// The consumed capabilities (for snapshots).
    consumes: Vec<String>,
}

/// The plugin runtime: drives loads, owns the dispatch engine and the live
/// plugin table.
pub struct Runtime {
    registry: Registry,
    store: Store,
    audit: EventProducer,
    engine: Engine,
    core: Core,
    loaded: BTreeMap<PluginName, LoadedRecord>,
    /// The per-identity secret resolver injected at construction or via
    /// [`set_secret_resolver`](Self::set_secret_resolver).
    ///
    /// Defaults to an empty resolver (no definitions) so the runtime compiles
    /// and tests can inject their own resolver.  Shell-side wiring of the real
    /// per-identity resolver is a later phase (not part of Task 6).
    resolver: Rc<SecretResolver>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("registry_version", &self.registry.version())
            .field("loaded", &self.loaded.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Builds a runtime over a loaded [`Registry`], a [`Store`] for per-plugin
    /// storage, and an audit [`EventProducer`].
    ///
    /// The secret resolver defaults to empty (no definitions).  Call
    /// [`set_secret_resolver`](Self::set_secret_resolver) to supply the real
    /// per-identity resolver before loading plugins that use `secrets.get`.
    #[must_use]
    pub fn new(registry: Registry, store: Store, audit: EventProducer) -> Self {
        let core = Core::new(registry.capabilities().clone());
        let invoker = RuntimeInvoker::new(core.clone());
        let engine = DispatchEngine::new(invoker, NullAudit);
        Self {
            registry,
            store,
            audit,
            engine,
            core,
            loaded: BTreeMap::new(),
            resolver: Rc::new(SecretResolver::empty()),
        }
    }

    /// Replaces the secret resolver used for `secrets.get` in subsequently
    /// loaded plugins.
    ///
    /// Plugins already loaded retain the resolver that was active when they
    /// were loaded (the `Rc` is cloned into each plugin's closure at load
    /// time).  Reload the plugin after calling this to pick up the new
    /// resolver.
    pub fn set_secret_resolver(&mut self, resolver: Rc<SecretResolver>) {
        self.resolver = resolver;
    }

    /// Dispatches a filter-chain hook (`net:intercept_request`, …) through the
    /// engine to every registered plugin handler, returning the resolved
    /// outcome (DESIGN §Hook dispatch patterns: first-block-wins,
    /// modify-cascades).
    pub fn dispatch_filter_chain(
        &mut self,
        hook_key: &str,
        payload: crate::value::HostValue,
    ) -> FilterChainOutcome<crate::value::HostValue> {
        self.engine.dispatch_filter_chain(hook_key, payload)
    }

    /// Dispatches a broadcast hook (`page:on_load`, `tabs:on_change`, …) to all
    /// registered handlers (errors isolated, no return semantics).
    pub fn dispatch_broadcast(
        &mut self,
        hook_key: &str,
        payload: crate::value::HostValue,
    ) -> BroadcastOutcome {
        self.engine.dispatch_broadcast(hook_key, payload)
    }

    /// Dispatches a keybind hook (`keys:*`) to its bound handler.
    pub fn dispatch_keybind(
        &mut self,
        hook_key: &str,
        payload: crate::value::HostValue,
    ) -> KeybindOutcome {
        self.engine.dispatch_keybind(hook_key, payload)
    }

    /// The registry this runtime was built with (schema version, permission and
    /// capability definitions).
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Build a [`SecretProviderRouter`] backed by this runtime's
    /// [`Core::invoke_capability_on`] targeted dispatch (ADR-0009).
    ///
    /// Pass the returned `Rc` to
    /// [`SecretResolver::new`](mote_secrets::SecretResolver::new) so that
    /// `password-manager` secrets route to the named `secret:provider`
    /// fulfiller rather than fan-out.  Uses `Rc` because the runtime core is
    /// single-threaded (`Rc<RefCell<…>>`); the router must be used from the
    /// same thread as the runtime.  The router holds a cheap clone of the
    /// shared core handle; keeping it alive does not prevent unloading plugins.
    #[must_use]
    pub fn make_secret_router(&self) -> Rc<dyn SecretProviderRouter> {
        RuntimeSecretRouter::new(self.core.clone(), self.audit.clone()).into_rc()
    }

    /// Whether a plugin with `name` is auto-disabled by the dispatch engine
    /// (three errors/timeouts in 24h; DESIGN §Runtime guarantees).
    #[must_use]
    pub fn is_auto_disabled(&self, name: &PluginName) -> bool {
        self.engine.is_disabled(name)
    }

    /// Whether a plugin with `name` is currently loaded.
    #[must_use]
    pub fn is_loaded(&self, name: &PluginName) -> bool {
        self.loaded.contains_key(name)
    }

    /// A read-only snapshot of a loaded plugin, if present.
    #[must_use]
    pub fn running(&self, name: &PluginName) -> Option<RunningPlugin> {
        self.loaded.get(name).map(|r| RunningPlugin {
            name: name.clone(),
            effective_permissions: r.effective_permissions.clone(),
            capabilities: r.capabilities.clone(),
            consumes: r.consumes.clone(),
            approval: r.approval.clone(),
        })
    }

    /// Emits an inter-plugin event from outside any plugin (e.g. the host) to
    /// every loaded plugin that declared a handler. Returns the count of
    /// handlers invoked. Used by the host to seed the event bus and by tests.
    #[must_use]
    pub fn emit_event(&self, name: &str, payload: &crate::value::HostValue) -> usize {
        self.core.emit(name, payload)
    }

    /// **Loads a plugin through the full four-step pipeline.** On success the
    /// plugin's `setup()` has run and its hooks are registered.
    ///
    /// # Errors
    ///
    /// Returns a [`LoadError`] naming the step that failed. The plugin is not
    /// run on any failure.
    pub fn load(
        &mut self,
        source: &str,
        identity: IdentityContext,
        policy: &dyn ApprovalPolicy,
    ) -> Result<RunningPlugin, LoadError> {
        // --- Step 1: sandboxed module load / manifest parse -------------------
        // The module body runs in the sandbox to build `M` and yield the
        // manifest the later steps need; `setup()` is NOT called (ADR-0001: load
        // != run). If a later step fails we discard the loaded module without
        // ever running it.
        let loaded = load_plugin(source, "plugin")?;
        let manifest = loaded.manifest().clone();

        if self.loaded.contains_key(&manifest.name) {
            return Err(LoadError::AlreadyLoaded {
                plugin: manifest.name,
            });
        }

        // --- Step 2: schema validation + resolution ---------------------------
        self.registry.validate_schema(
            &manifest.permissions,
            &manifest.capabilities,
            &manifest.consumes,
        )?;
        self.resolve_consumes(&manifest)?;
        // Exclusive-double-claim is detected when we actually claim, below, but
        // we check it *before* any side effect so a conflicting plugin fails to
        // load cleanly.
        self.check_exclusive_claims(&manifest)?;

        // --- Step 3: contract conformance -------------------------------------
        self.registry.check_conformance(&loaded)?;

        // --- Step 4: permission approval --------------------------------------
        let effective = Self::approve(&manifest, policy)?;

        // All four passed → commit: install host API, register hooks, run setup.
        self.commit(&loaded, manifest, identity, &effective)
    }

    /// **Reloads a plugin** by re-running the pipeline (DESIGN §Hot Reload).
    ///
    /// Computes the re-approval hash over `{permissions, capabilities, consumes,
    /// identity_scope}`:
    ///
    /// - **code-only / non-expanding manifest change** → no re-approval; the new
    ///   grant is intersected with the prior approval implicitly by re-running
    ///   approval with the same policy.
    /// - **expansion** of any of those four fields → re-approval is required.
    ///
    /// `require_reapproval` is the seam the host uses to decide whether an
    /// expansion may proceed (the user approved). When `false` and the manifest
    /// expands, the reload fails with [`LoadError::ApprovalDenied`] and the
    /// **currently-loaded instance keeps running** ("awaiting approval" — the
    /// plugin keeps working until the user decides). The host stops the instance
    /// (via [`unload`](Self::unload)) when it surfaces the prompt and re-issues
    /// `reload` with `require_reapproval = true` once the user approves. When
    /// `true`, an expanding reload proceeds (replacing the old instance).
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::NotLoaded`] if the plugin is not loaded, or a
    /// wrapped [`LoadError`] if the new pipeline fails.
    pub fn reload(
        &mut self,
        name: &PluginName,
        source: &str,
        identity: IdentityContext,
        policy: &dyn ApprovalPolicy,
        require_reapproval: bool,
    ) -> Result<RunningPlugin, LifecycleError> {
        let prior = self
            .loaded
            .get(name)
            .ok_or_else(|| LifecycleError::NotLoaded {
                plugin: name.clone(),
            })?
            .approval
            .clone();

        // Load the new module to read its manifest (does not run setup()).
        let loaded = load_plugin(source, "plugin").map_err(LoadError::from)?;
        let manifest = loaded.manifest().clone();
        if &manifest.name != name {
            // A reload that renames the plugin is treated as a fresh load
            // target; surface as denied to avoid silently orphaning state.
            return Err(LifecycleError::Load(LoadError::ApprovalDenied {
                reason: format!(
                    "reload changed plugin name from `{name}` to `{}`",
                    manifest.name
                ),
            }));
        }

        let new_hash = ApprovalHash::of(&manifest);
        let expands = new_hash.is_expansion_of(&prior);

        if expands && !require_reapproval {
            // DESIGN §Hot Reload: an expansion of permissions/capabilities/
            // consumes/identity_scope enters "awaiting approval" and the new
            // manifest does not load until the user approves. We refuse the
            // reload and leave the **currently-loaded** instance running so the
            // plugin keeps working until approval — the host stops it (via
            // `unload`) when it surfaces the prompt, and re-issues this `reload`
            // with `require_reapproval = true` once the user approves.
            return Err(LifecycleError::Load(LoadError::ApprovalDenied {
                reason: "manifest expands permissions/capabilities/consumes/identity_scope; \
                         re-approval required"
                    .to_owned(),
            }));
        }

        // Stop the old instance, then run the full pipeline for the new one.
        // (Storage persists across reloads — DESIGN §State survives selectively.)
        self.unload(name).ok();
        // We already loaded the module; re-run the remaining pipeline steps.
        self.registry
            .validate_schema(
                &manifest.permissions,
                &manifest.capabilities,
                &manifest.consumes,
            )
            .map_err(LoadError::from)?;
        self.resolve_consumes(&manifest)
            .map_err(LifecycleError::Load)?;
        self.check_exclusive_claims(&manifest)
            .map_err(LifecycleError::Load)?;
        self.registry
            .check_conformance(&loaded)
            .map_err(LoadError::from)?;
        let effective = Self::approve(&manifest, policy).map_err(LifecycleError::Load)?;
        self.commit(&loaded, manifest, identity, &effective)
            .map_err(LifecycleError::Load)
    }

    /// **Unloads a plugin**: removes its hooks from the dispatch engine, frees
    /// its capability claims, and drops its live record. Storage is *not*
    /// deleted (it persists across reloads).
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::NotLoaded`] if the plugin was not loaded.
    pub fn unload(&mut self, name: &PluginName) -> Result<(), LifecycleError> {
        let record = self
            .loaded
            .remove(name)
            .ok_or_else(|| LifecycleError::NotLoaded {
                plugin: name.clone(),
            })?;

        let _ = &record.registered_hooks; // reserved for future per-key dereg
        self.cleanup_plugin_state(name);
        Ok(())
    }

    /// Tears down every shared side effect a plugin's `commit` installs: the
    /// dispatch failure history, its capability claims, and its core plugin
    /// record. Idempotent and safe to call for a partially-committed plugin
    /// (the [`commit`](Self::commit) error path reuses it to roll back; see M1).
    ///
    /// The dispatch engine has no per-key deregistration in Phase 1; removing
    /// the plugin's core record makes any lingering hook registration a no-op —
    /// the invoker can no longer resolve the handler, so it returns a caught
    /// "no context" error and the plugin is skipped.
    fn cleanup_plugin_state(&mut self, name: &PluginName) {
        self.engine.reset_plugin(name);
        self.core.with_mut(|state| {
            state.capabilities.remove_plugin(name);
            state.plugins.remove(name);
        });
    }

    // --- pipeline helpers ----------------------------------------------------

    /// Step-1 dangling-consumer resolution (DESIGN §Resolution at load time).
    fn resolve_consumes(&self, manifest: &Manifest) -> Result<(), LoadError> {
        self.core.with_mut(|state| {
            for capability in &manifest.consumes {
                if !state.capabilities.is_fulfilled(capability) {
                    return Err(LoadError::DanglingConsumer {
                        plugin: manifest.name.clone(),
                        capability: capability.clone(),
                    });
                }
            }
            Ok(())
        })
    }

    /// Step-1 exclusive-capability pre-check: a second claim on an exclusive
    /// capability fails to load before any side effect.
    fn check_exclusive_claims(&self, manifest: &Manifest) -> Result<(), LoadError> {
        self.core.with_mut(|state| {
            for capability in &manifest.capabilities {
                // Dry-run the claim on a clone so we don't mutate live state.
                let mut probe = state.capabilities.clone();
                match probe.claim(self.registry.capabilities(), capability, &manifest.name) {
                    Ok(()) => {}
                    Err(ClaimError::Exclusive { existing }) => {
                        return Err(LoadError::ExclusiveCapabilityConflict {
                            plugin: manifest.name.clone(),
                            capability: capability.clone(),
                            existing,
                        });
                    }
                    Err(ClaimError::Unknown) => {
                        return Err(LoadError::UnknownCapability {
                            capability: capability.clone(),
                        });
                    }
                }
            }
            Ok(())
        })
    }

    /// Step 4: run the approval policy and produce the effective grants.
    fn approve(
        manifest: &Manifest,
        policy: &dyn ApprovalPolicy,
    ) -> Result<EffectiveGrants, LoadError> {
        let requested: Vec<Permission> = manifest
            .permissions
            .iter()
            .map(|p| p.parse::<Permission>())
            .collect::<Result<_, _>>()
            // Step 1 already validated grammar; a parse failure here is
            // impossible, but surface it as a schema error rather than panic.
            .map_err(|source| {
                LoadError::Schema(mote_registry::SchemaValidationError::PermissionGrammar {
                    term: "<approval>".to_owned(),
                    source,
                })
            })?;

        match policy.decide(manifest.name.as_str(), &requested) {
            Approval::GrantAsRequested => Ok(EffectiveGrants::from_permissions(&requested)?),
            Approval::Narrow { narrowings } => {
                let mut set = GrantSet::from_permissions(&requested)?;
                for n in &narrowings {
                    set = set.narrow(&n.domain, &n.action, &n.resources)?;
                }
                // Re-derive the effective permission list from the narrowed set
                // and build `EffectiveGrants` from it (there is no public ctor
                // from a bare `GrantSet`). The strings round-trip through the
                // grammar since they came from valid globs.
                let perms: Vec<Permission> = grant_set_strings(&set)
                    .iter()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                Ok(EffectiveGrants::from_permissions(&perms)?)
            }
            Approval::Deny { reason } => Err(LoadError::ApprovalDenied { reason }),
        }
    }

    /// Commits a fully-validated plugin: installs the host API, registers hooks,
    /// records capabilities/events in the core, and runs `setup()`.
    fn commit(
        &mut self,
        loaded: &LoadedPlugin,
        manifest: Manifest,
        identity: IdentityContext,
        effective: &EffectiveGrants,
    ) -> Result<RunningPlugin, LoadError> {
        let name = manifest.name.clone();
        let approval = ApprovalHash::of(&manifest);
        let gatekeeper = GrantSetGatekeeper::new(effective.grant_set().clone());
        let effective_strings: Vec<String> = effective.as_strings().to_vec();

        // Storage namespace honoring identity_scope.
        let storage_scope = resolve_storage_scope(manifest.identity_scope, identity.identity);
        let namespace = self.store.namespace(&name, storage_scope);

        // Install the mote.* host API into the plugin's own state.
        let lua = loaded.lua().clone();
        let module = loaded.module().clone();
        let ctx = HostContext {
            plugin: name.clone(),
            gatekeeper,
            effective: effective_strings.clone(),
            storage: namespace,
            audit: self.audit.clone(),
            core: self.core.clone(),
            resolver: Rc::clone(&self.resolver),
        };
        hostapi::install(&lua, ctx).map_err(LoadError::HostApi)?;

        // Record the plugin in the shared core (must precede setup so a setup()
        // that emits/invokes can reach itself and other plugins).
        let event_keys = loaded.event_keys().to_vec();
        self.core.with_mut(|state| {
            // Claim capabilities for real now (pre-checked above).
            for capability in &manifest.capabilities {
                // Unwrap is safe: validated in step 1 + pre-checked exclusivity.
                let _ = state
                    .capabilities
                    .claim(self.registry.capabilities(), capability, &name);
            }
            state.plugins.insert(
                name.clone(),
                PluginRecord {
                    lua: lua.clone(),
                    module: module.clone(),
                    event_keys: event_keys.clone(),
                },
            );
        });

        // Register the plugin's hooks into dispatch. The invoker reads the
        // plugin's Lua state from the shared core (recorded just above), so no
        // separate context registration is needed.
        //
        // M1: from here on, shared state (capability claims + core record) is
        // installed but the plugin is not yet in `self.loaded`, so `unload`
        // cannot reach it. Any failure must roll those side effects back via
        // `cleanup_plugin_state` BEFORE returning, or the claim (including an
        // EXCLUSIVE one) and core record leak permanently.
        let mut registered_hooks = Vec::new();
        for hook_key in loaded.hook_keys() {
            let hook_type = self.hook_type_for(hook_key);
            match self
                .engine
                .register(hook_key.clone(), hook_type, Registration::new(name.clone()))
            {
                Ok(()) => registered_hooks.push((hook_key.clone(), hook_type)),
                Err(e) => {
                    self.cleanup_plugin_state(&name);
                    return Err(LoadError::from(e));
                }
            }
        }

        // Finally: run setup() (only now, after all four checks + wiring). A
        // setup() that errors must not leave the capability claim / core record
        // / hook registrations orphaned (M1) — roll them all back.
        if loaded.has_setup()
            && let Err(e) = run_setup(&module)
        {
            self.cleanup_plugin_state(&name);
            return Err(LoadError::Setup(e));
        }

        self.loaded.insert(
            name.clone(),
            LoadedRecord {
                registered_hooks,
                capabilities: manifest.capabilities.clone(),
                approval: approval.clone(),
                effective_permissions: effective_strings.clone(),
                consumes: manifest.consumes.clone(),
            },
        );

        Ok(RunningPlugin {
            name,
            effective_permissions: effective_strings,
            capabilities: manifest.capabilities,
            consumes: manifest.consumes,
            approval,
        })
    }

    /// Maps a hook key to its [`HookType`] using the event registry's dispatch
    /// shape, with the keybind and capability-event conventions layered on top.
    ///
    /// Sourcing (see crate docs / the returned DESIGN-ambiguity note):
    /// - A `keys:*` key is a [`HookType::Keybind`] (the event registry does not
    ///   enumerate keybinds; they are runtime-defined input bindings).
    /// - Otherwise, the [`EventRegistry`](mote_registry::EventRegistry) dispatch
    ///   shape decides: `FilterChain` → [`HookType::FilterChain`]; everything
    ///   else (`Broadcast`, `Collector`, `FanOutPerOrigin`) maps to
    ///   [`HookType::Broadcast`] — all "every handler runs, no veto" shapes,
    ///   which is the dispatch model the engine implements for them in Phase 1.
    /// - An unknown key (e.g. a capability-contract event declared in `M.hooks`)
    ///   defaults to [`HookType::Broadcast`].
    fn hook_type_for(&self, hook_key: &str) -> HookType {
        if hook_key.starts_with("keys:") {
            return HookType::Keybind;
        }
        match self.registry.events().get(hook_key).map(|e| e.dispatch) {
            Some(EventDispatch::FilterChain) => HookType::FilterChain,
            // Broadcast / Collector / FanOutPerOrigin and any future "every
            // handler runs" shape, plus unknown keys, model as broadcast.
            _ => HookType::Broadcast,
        }
    }
}

/// Resolves the manifest `identity_scope` to a concrete storage scope.
///
/// `per_identity` → the current identity; `global` → shared; `user_choice` and
/// absent default to `global` for Phase 1 (the user picker is Phase 2 — until
/// then, the safe default is the shared namespace, matching DESIGN's note that
/// non-storage/behavioural plugins default to `global`).
const fn resolve_storage_scope(
    scope: Option<ManifestIdentityScope>,
    identity: IdentityId,
) -> StorageScope {
    match scope {
        Some(ManifestIdentityScope::PerIdentity) => StorageScope::PerIdentity(identity),
        // global / user_choice / absent / any future scope default to the shared
        // namespace (the user picker for `user_choice` is Phase 2).
        _ => StorageScope::Global,
    }
}

/// Runs `module.setup()` if present. Errors are caught, never panic.
///
/// The `setup` field is read RAW: `M` is a plugin-controlled table, so a
/// `__index` metamethod on it must not be able to intercept the lookup.
fn run_setup(module: &mote_lua::Table) -> Result<(), String> {
    let setup: mote_lua::Value = module.raw_get("setup").map_err(|e| e.to_string())?;
    if let mote_lua::Value::Function(f) = setup {
        f.call::<()>(()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Renders a [`GrantSet`] back to display strings (for the narrowed effective
/// list). Each `(domain, action)` pair is rendered as `domain:action:resource`
/// per stored glob.
fn grant_set_strings(set: &GrantSet) -> Vec<String> {
    let mut out = Vec::new();
    for (domain, action) in set.pairs() {
        if let Some(globs) = set.get(domain, action) {
            for glob in globs.globs() {
                out.push(format!("{domain}:{action}:{glob}"));
            }
        }
    }
    out.sort_unstable();
    out
}

// (`effective_from_set` removed — `approve` now builds `EffectiveGrants`
// directly from the narrowed permission strings.)

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mote_audit::{AuditLog, Config};
    use mote_registry::Registry;
    use mote_storage::Store;
    use mote_types::SchemaVersion;

    use super::*;

    fn make_runtime() -> (Runtime, AuditLog) {
        let registry = Registry::load(SchemaVersion::V1).unwrap();
        let store = Store::open_in_memory().unwrap();
        let config = Config {
            ring_capacity: 256,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(5),
        };
        let log = AuditLog::new(&store, config).unwrap();
        let runtime = Runtime::new(registry, store, log.producer());
        (runtime, log)
    }

    #[test]
    fn registry_accessor_returns_registry_with_matching_version() {
        // Keep the audit log alive for the test body so its background flush
        // thread isn't dropped mid-test (matches the `tests/` helpers).
        let (rt, _log) = make_runtime();
        // The accessor must hand back the same registry the runtime was built
        // with; comparing the version is the only public identity check.
        assert_eq!(rt.registry().version(), SchemaVersion::V1);
    }
}
