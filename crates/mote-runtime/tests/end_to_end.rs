//! The Phase-1 completion proof: a real inline Lua plugin driven through the
//! full four-step load pipeline, then exercised across every integration seam
//! the runtime owns.
//!
//! This single end-to-end test asserts, against real `mlua` states and the real
//! registry/permissions/dispatch/audit/storage crates:
//!
//! 1. A valid plugin (manifest with a permission, a capability, a hook, an
//!    `M.events` handler, and a `setup()`) loads through all four steps with a
//!    grant-as-requested approval policy, and its `setup()` runs.
//! 2. The plugin's filter-chain hook is registered and dispatching through the
//!    engine actually invokes the Lua handler, returning the expected blocked
//!    `Decision`.
//! 3. `events.emit` from one plugin reaches another plugin's `M.events` handler.
//! 4. `capabilities.invoke` reaches the fulfiller and runs under the
//!    **fulfiller's** permissions (it performs a storage write the caller could
//!    not), audited with performer = fulfiller.
//! 5. A permission-denied host call returns nil/false and is recorded as a
//!    denial in the audit log under the calling plugin.
//! 6. A dangling-consumer plugin fails to load.
//! 7. An exclusive-capability double-claim fails to load.

use std::rc::Rc;
use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config, Decision as AuditDecision};
use mote_registry::Registry;
use mote_runtime::{
    ChainResolution, GrantAsRequested, HostValue, IdentityContext, LoadError, Runtime,
};
use mote_secrets::{ResolveError, SecretProviderRouter};
use mote_storage::Store;
use mote_types::{IdentityId, PluginName, SchemaVersion};
use secrecy::ExposeSecret as _;

/// The capability fulfiller: provides `password-manager-form-services`. Its
/// contract requires `show_autofill_picker` + `inject_isolated` in `M.api` and
/// a `page:on_load` handler. It holds `storage:persistent`, which the consumer
/// does NOT — so a successful storage write inside `show_autofill_picker` proves
/// the call ran under THIS plugin's permissions (D4).
const FORM_SERVICES_SRC: &str = r#"
local M = {}

M.manifest = {
  schema = "v1",
  name = "form-services",
  version = "1.0.0",
  permissions = { "page:read_dom", "storage:persistent" },
  capabilities = { "password-manager-form-services" },
  identity_scope = "global",
}

M.api = {
  show_autofill_picker = function(items)
    -- Runs under form-services' permissions: this storage write must succeed.
    storage.set("last_picker_choice", items.choice)
    return { ok = true, who = "form-services" }
  end,
  inject_isolated = function(script, world) return true end,
}

M.hooks = {
  ["page:on_load"] = function(p) end,
}

function M.setup()
  -- Marks that setup ran, within our own (granted) storage permission.
  storage.set("setup_ran", "yes")
end

return M
"#;

/// The consumer + exclusive-capability fulfiller. Consumes
/// `password-manager-form-services` (must resolve at load time), fulfills the
/// exclusive `password-manager:provider`. Listens for the form-detected event,
/// and on it invokes the form-services picker. Also declares a
/// `net:intercept_request` filter-chain hook that blocks. Lacks
/// `storage:persistent`, so its own `storage.set` is denied.
const ONEPW_SRC: &str = r#"
local M = {}

M.manifest = {
  schema = "v1",
  name = "onepw",
  version = "2.0.0",
  permissions = { "events:on", "events:emit", "net:intercept_request" },
  capabilities = { "ui:urlbar_provider" },
  consumes = { "password-manager-form-services" },
  identity_scope = "global",
}

-- Required by the ui:urlbar_provider contract (API: query).
M.api = {
  query = function(text) return {} end,
}

-- Visible side effects recorded in module-level fields the test reads back via
-- a host call would be ideal, but the test instead asserts through the audit
-- log and the fulfiller's storage. We keep a counter the events handler bumps.
M.invocations = 0

M.events = {
  ["password-manager-form-services:form-detected"] = function(form)
    M.invocations = M.invocations + 1
    -- Invoke the fulfiller's API; it runs under the fulfiller's permissions.
    local result = capabilities.invoke(
      "password-manager-form-services",
      "show_autofill_picker",
      { choice = "entry-42" }
    )
    if result and result.ok then
      M.last_who = result.who
    end
  end,
}

M.hooks = {
  -- Filter-chain handler: block the request.
  ["net:intercept_request"] = function(req)
    return { action = "block", reason = "onepw blocks tracker" }
  end,
}

function M.setup()
  -- Attempt a storage write WITHOUT storage:persistent → must be denied.
  storage.set("should_be_denied", "x")
end

return M
"#;

/// A plugin that consumes a capability no one fulfills → dangling consumer.
const DANGLING_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "dangling",
  version = "1.0.0",
  permissions = {},
  consumes = { "mcp:server" },
}
function M.setup() end
return M
"#;

/// A second plugin claiming the exclusive `ui:urlbar_provider` already held by
/// `onepw` → exclusive double-claim.
///
/// NOTE: The doc comment above previously said "exclusive `password-manager:provider`"
/// but this plugin actually claims `ui:urlbar_provider`. The PM capability is
/// non-exclusive (Task 4 of Phase 4 secrets); only UI slots are exclusive.
const SECOND_EXCLUSIVE_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "other-urlbar",
  version = "1.0.0",
  permissions = {},
  capabilities = { "ui:urlbar_provider" },
}
M.api = { query = function(text) return {} end }
function M.setup() end
return M
"#;

fn make_runtime() -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let store = Store::open_in_memory().expect("in-memory store");
    // Tight audit config so events flush quickly for assertions.
    let config = Config {
        ring_capacity: 256,
        flush_threshold: 1,
        flush_interval: Duration::from_millis(5),
    };
    let log = AuditLog::new(&store, config).expect("audit log starts");
    let runtime = Runtime::new(registry, store, log.producer());
    (runtime, log)
}

fn plugin(name: &str) -> PluginName {
    PluginName::new(name).expect("valid plugin name")
}

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(1))
}

/// Let the audit thread drain pending events.
fn drain(log: &AuditLog) {
    thread::sleep(Duration::from_millis(40));
    let _ = log;
}

#[test]
fn phase1_end_to_end() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    // --- (6) dangling consumer fails BEFORE any fulfiller is loaded --------
    let err = runtime
        .load(DANGLING_SRC, identity(), &policy)
        .expect_err("consuming an unfulfilled capability must fail to load");
    assert!(
        matches!(err, LoadError::DanglingConsumer { ref capability, .. } if capability == "mcp:server"),
        "expected dangling-consumer error, got {err:?}"
    );

    // --- (1) load the fulfiller through all four steps ---------------------
    let fs = runtime
        .load(FORM_SERVICES_SRC, identity(), &policy)
        .expect("form-services loads cleanly through the four-step pipeline");
    assert_eq!(fs.name, plugin("form-services"));
    assert!(runtime.is_loaded(&plugin("form-services")));
    assert!(
        fs.capabilities
            .contains(&"password-manager-form-services".to_owned())
    );

    // setup() ran: it wrote `setup_ran` to the fulfiller's storage.
    // We assert via the audit log (a storage write is audited under the plugin).
    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| e.plugin.as_str() == "form-services"
            && e.operation == "storage:persistent"
            && e.decision == AuditDecision::Allow),
        "form-services setup() should have performed an allowed storage write"
    );

    // --- now load the consumer (its `consumes` resolves to form-services) --
    let pw = runtime
        .load(ONEPW_SRC, identity(), &policy)
        .expect("onepw loads (consumed capability now fulfilled)");
    assert_eq!(pw.name, plugin("onepw"));
    assert!(
        pw.consumes
            .contains(&"password-manager-form-services".to_owned())
    );

    // --- (5) onepw's setup() tried a denied storage.set --------------------
    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| e.plugin.as_str() == "onepw"
            && e.operation == "storage:persistent"
            && e.decision == AuditDecision::Deny),
        "onepw lacks storage:persistent; its setup() storage.set must be denied + audited"
    );

    // --- (2) dispatch the filter-chain hook → onepw's handler blocks -------
    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Blocked { reason, .. } => {
            assert_eq!(reason, "onepw blocks tracker");
        }
        ChainResolution::Allowed { .. } => {
            panic!("expected the chain to be blocked by onepw, got an Allowed resolution")
        }
    }

    // --- (3 + 4) emit the form-detected event from the host ----------------
    // onepw's M.events handler fires, which invokes the fulfiller's API.
    let delivered = runtime.emit_event(
        "password-manager-form-services:form-detected",
        &HostValue::Map(std::collections::BTreeMap::default()),
    );
    assert_eq!(
        delivered, 1,
        "exactly onepw listens for the form-detected event"
    );

    // (4) The fulfiller's show_autofill_picker ran under form-services'
    // permissions: it wrote `last_picker_choice`, and the call was audited with
    // performer = form-services and the invocation chain in the detail.
    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| e.plugin.as_str() == "form-services"
            && e.operation == "password-manager-form-services:show_autofill_picker"
            && e.decision == AuditDecision::Allow
            && e.detail.as_deref().is_some_and(|d| d.contains("onepw"))),
        "capabilities.invoke must run the fulfiller's API audited as the fulfiller \
         with the caller in the chain detail"
    );
    // And the fulfiller's storage write inside the API succeeded (performer =
    // form-services, an allowed storage:persistent write recorded during the
    // invocation).
    let fs_storage_writes = history
        .iter()
        .filter(|e| {
            e.plugin.as_str() == "form-services"
                && e.operation == "storage:persistent"
                && e.decision == AuditDecision::Allow
        })
        .count();
    assert!(
        fs_storage_writes >= 2,
        "the picker's storage write (under the fulfiller's perms) should be recorded; \
         saw {fs_storage_writes} allowed form-services storage writes"
    );

    // --- (7) exclusive double-claim fails ----------------------------------
    let err = runtime
        .load(SECOND_EXCLUSIVE_SRC, identity(), &policy)
        .expect_err("a second claim on the exclusive ui:urlbar_provider must fail");
    assert!(
        matches!(
            err,
            LoadError::ExclusiveCapabilityConflict { ref capability, .. }
                if capability == "ui:urlbar_provider"
        ),
        "expected exclusive-capability conflict, got {err:?}"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

// ---------------------------------------------------------------------------
// password-manager:provider composability test (Task 4, Phase 4 secrets)
// ---------------------------------------------------------------------------

/// First password-manager plugin — fulfills the non-exclusive
/// `password-manager:provider` capability.
/// Implements the required API contract: `list_credentials` + `fill_credential`.
const PM_PROVIDER_A_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "pm-provider-a",
  version = "1.0.0",
  permissions = {},
  capabilities = { "password-manager:provider" },
  identity_scope = "global",
}
M.api = {
  list_credentials = function(query) return {} end,
  fill_credential  = function(id, field) return "" end,
}
function M.setup() end
return M
"#;

/// Second password-manager plugin — also fulfills `password-manager:provider`.
/// Loading this alongside `pm-provider-a` must NOT produce an exclusive-conflict
/// error (composability = non-exclusive per Task 4).
/// Implements the required API contract: `list_credentials` + `fill_credential`.
const PM_PROVIDER_B_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "pm-provider-b",
  version = "1.0.0",
  permissions = {},
  capabilities = { "password-manager:provider" },
  identity_scope = "global",
}
M.api = {
  list_credentials = function(query) return {} end,
  fill_credential  = function(id, field) return "" end,
}
function M.setup() end
return M
"#;

/// Two plugins both fulfilling `password-manager:provider` must coexist —
/// `password-manager:provider` is non-exclusive (composability = `NonExclusive`).
/// Neither load call may return [`LoadError::ExclusiveCapabilityConflict`].
///
/// This proves the runtime load path (`check_exclusive_claims` /
/// `probe.claim(…)`) permits multiple `password-manager:provider` claimants,
/// which is the core runtime invariant asserted by Task 4.
#[test]
fn two_password_manager_providers_coexist() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    // First PM provider loads without error.
    let a = runtime
        .load(PM_PROVIDER_A_SRC, identity(), &policy)
        .expect("first password-manager:provider must load without error");
    assert_eq!(a.name, plugin("pm-provider-a"));
    assert!(
        a.capabilities
            .contains(&"password-manager:provider".to_owned()),
        "pm-provider-a must expose password-manager:provider"
    );

    // Second PM provider also loads without error — no exclusive conflict.
    let b = runtime.load(PM_PROVIDER_B_SRC, identity(), &policy).expect(
        "second password-manager:provider must load without exclusive-capability-conflict \
             error (composability = NonExclusive, Task 4 Phase 4)",
    );
    assert_eq!(b.name, plugin("pm-provider-b"));
    assert!(
        b.capabilities
            .contains(&"password-manager:provider".to_owned()),
        "pm-provider-b must expose password-manager:provider"
    );

    // Both are simultaneously loaded.
    assert!(runtime.is_loaded(&plugin("pm-provider-a")));
    assert!(runtime.is_loaded(&plugin("pm-provider-b")));

    log.shutdown().expect("audit log shuts down cleanly");
}

// ---------------------------------------------------------------------------
// Task 5: invoke_capability_on targeted dispatch + SecretProviderRouter impl
// ---------------------------------------------------------------------------

/// First `secret:provider` fulfiller.  Its `resolve_secret` returns a
/// distinctive string so the test can prove WHICH provider was reached.
const SECRET_PROVIDER_A_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sp-provider-a",
  version = "1.0.0",
  permissions = {},
  capabilities = { "secret:provider" },
  identity_scope = "global",
}
M.api = {
  resolve_secret = function(reference) return "from-provider-a:" .. reference end,
}
function M.setup() end
return M
"#;

/// Second `secret:provider` fulfiller.  Returns a DIFFERENT value for the same
/// reference — proves that only the targeted provider was consulted.
const SECRET_PROVIDER_B_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sp-provider-b",
  version = "1.0.0",
  permissions = {},
  capabilities = { "secret:provider" },
  identity_scope = "global",
}
M.api = {
  resolve_secret = function(reference) return "from-provider-b:" .. reference end,
}
function M.setup() end
return M
"#;

/// **Targeted-not-other:** `invoke_capability_on` through the runtime's
/// `SecretProviderRouter` impl hits EXACTLY the named provider; the other
/// provider is never consulted (proven by the audit log having no record for it).
///
/// Also covers: naming an unloaded provider → `ResolveError::ProviderNotLoaded`.
#[test]
fn invoke_capability_on_targets_named_provider_only() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    // Load both providers — both must load without error (non-exclusive).
    runtime
        .load(SECRET_PROVIDER_A_SRC, identity(), &policy)
        .expect("sp-provider-a loads");
    runtime
        .load(SECRET_PROVIDER_B_SRC, identity(), &policy)
        .expect("sp-provider-b loads");
    assert!(runtime.is_loaded(&plugin("sp-provider-a")));
    assert!(runtime.is_loaded(&plugin("sp-provider-b")));

    // Build the runtime-backed SecretProviderRouter (the Task-5 impl).
    let router: Rc<dyn SecretProviderRouter> = runtime.make_secret_router();

    // --- targeted: provider A is asked, B must NOT be called ----------------
    let val_a = router
        .resolve("sp-provider-a", "myref")
        .expect("provider-a should resolve");
    assert_eq!(
        val_a.expose_secret(),
        "from-provider-a:myref",
        "router must return the value from the NAMED provider (A)"
    );

    // Give the audit thread time to drain then assert B left no trace.
    drain(&log);
    let history = log.query().history().expect("audit history");
    let b_records: Vec<_> = history
        .iter()
        .filter(|e| e.plugin.as_str() == "sp-provider-b")
        .collect();
    assert!(
        b_records.is_empty(),
        "sp-provider-b must NEVER be invoked when A is targeted; \
         found audit records for B: {b_records:?}"
    );

    // --- targeted the other way: provider B is asked, A must NOT be called --
    // Reset log history by flushing and re-reading from a fresh snapshot point.
    // We can't clear the log, so instead we count ONLY newly-added entries by
    // snapshotting the length before the second call.
    let history_before = log.query().history().expect("history before B call");
    let a_count_before = history_before
        .iter()
        .filter(|e| e.plugin.as_str() == "sp-provider-a")
        .count();

    let val_b = router
        .resolve("sp-provider-b", "myref")
        .expect("provider-b should resolve");
    assert_eq!(
        val_b.expose_secret(),
        "from-provider-b:myref",
        "router must return the value from the NAMED provider (B)"
    );

    drain(&log);
    let history_after = log.query().history().expect("history after B call");
    let a_count_after = history_after
        .iter()
        .filter(|e| e.plugin.as_str() == "sp-provider-a")
        .count();
    assert_eq!(
        a_count_before, a_count_after,
        "sp-provider-a must NOT be invoked when B is targeted; \
         new A records appeared after targeting B"
    );

    // --- provider not loaded → ProviderNotLoaded --------------------------
    let err = router
        .resolve("nonexistent-pm", "ref")
        .expect_err("nonexistent provider must return an error");
    assert!(
        matches!(err, ResolveError::ProviderNotLoaded { .. }),
        "unloaded/unknown provider must yield ProviderNotLoaded, got {err:?}"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

// ---------------------------------------------------------------------------
// Task 6: secrets.get host API gated on secret:read:<name>
//
// Behavior is observed through filter-chain dispatch, mirroring the idiom
// established in the rest of this file: assertions go through the AUDIT TRAIL
// and DISPATCH OUTCOMES, never through raw module-field readback.
//
// Each test plugin declares `net:intercept_request` so its hook registers, and
// returns `{ action = "modify", payload = <value> }` from that hook.
// `dispatch_filter_chain` surfaces the handler's return as
// `ChainResolution::Allowed { payload: <HostValue> }`, making the secret value
// (or nil) observable without any extra public API on the runtime.
// ---------------------------------------------------------------------------

/// Build a `SecretResolver` backed by a single env-var secret. The env var
/// must already be set in the test process before calling this.
fn env_resolver(secret_name: &str, var: &str) -> mote_secrets::SecretResolver {
    use mote_secrets::{BackendKind, SecretDef};
    mote_secrets::SecretResolver::new(
        [SecretDef {
            name: secret_name.to_owned(),
            backend: BackendKind::Env {
                var: var.to_owned(),
            },
        }],
        None,
    )
}

/// Build a runtime pre-loaded with the given resolver.
fn make_runtime_with_resolver(resolver: mote_secrets::SecretResolver) -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let store = Store::open_in_memory().expect("in-memory store");
    let config = Config {
        ring_capacity: 256,
        flush_threshold: 1,
        flush_interval: Duration::from_millis(5),
    };
    let log = AuditLog::new(&store, config).expect("audit log starts");
    let mut runtime = Runtime::new(registry, store, log.producer());
    runtime.set_secret_resolver(Rc::new(resolver));
    (runtime, log)
}

/// Granted plugin: reads `MY_SECRET` from the hook and returns it as the
/// modified payload.  Uses `net:intercept_request` so the hook registers.
const SECRETS_GRANTED_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "secrets-granted",
  version = "1.0.0",
  permissions = { "net:intercept_request", "secret:read:MY_SECRET" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    return { action = "modify", payload = secrets.get("MY_SECRET") }
  end,
}
function M.setup() end
return M
"#;

/// Ungranted plugin: same hook, but NO `secret:read:MY_SECRET`.
/// `secrets.get` must return nil; the payload propagates that nil.
const SECRETS_UNGRANTED_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "secrets-ungranted",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    return { action = "modify", payload = secrets.get("MY_SECRET") }
  end,
}
function M.setup() end
return M
"#;

/// Granted plugin reading a name that has NO def in the resolver — must return
/// nil from the hook payload (no panic).
const SECRETS_UNDEFINED_NAME_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "secrets-undefined-name",
  version = "1.0.0",
  permissions = { "net:intercept_request", "secret:read:UNDEFINED_KEY" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    return { action = "modify", payload = secrets.get("UNDEFINED_KEY") }
  end,
}
function M.setup() end
return M
"#;

/// Plugin that probes for the enumeration surface:
/// - `secrets.list == nil` → payload is `true`  (list does not exist — good)
/// - payload is `false` if `secrets.list` somehow exists
///
/// Additionally wraps the key count in the modify payload so the test can
/// confirm exactly one key (`get`) is reachable via `pairs(secrets)`.
/// The payload is a Lua boolean for the `list == nil` check; the key-count
/// check is done separately by asserting the count embedded in the payload.
///
/// Strategy: return the string `"list_nil=<bool>,count=<n>"` as the payload so
/// both observations travel through a single dispatch call.
const SECRETS_NO_ENUMERATE_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "secrets-no-enumerate",
  version = "1.0.0",
  permissions = { "net:intercept_request", "secret:read:MY_SECRET" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local list_nil = (secrets.list == nil)
    local count = 0
    for _ in pairs(secrets) do count = count + 1 end
    return { action = "modify", payload = "list_nil=" .. tostring(list_nil) .. ",count=" .. tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// (T6-1) GRANTED plugin: `secrets.get("MY_SECRET")` returns the env-backed
/// value as the filter-chain modify payload.  Audit records Allow with
/// `detail` containing the backend label.
#[test]
fn secrets_get_granted_returns_value() {
    let home = std::env::var("HOME").expect("HOME must be set");
    let (mut runtime, mut log) = make_runtime_with_resolver(env_resolver("MY_SECRET", "HOME"));
    let policy = GrantAsRequested;

    runtime
        .load(SECRETS_GRANTED_SRC, identity(), &policy)
        .expect("secrets-granted must load");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Str(home),
                "secrets.get must return the resolved env-backed secret value as the payload"
            );
        }
        other @ ChainResolution::Blocked { .. } => panic!("expected Allowed, got {other:?}"),
    }

    // Audit: exactly one Allow for secret:read:MY_SECRET, with backend:env detail.
    drain(&log);
    let history = log.query().history().expect("audit history");
    let allow_events: Vec<_> = history
        .iter()
        .filter(|e| {
            e.plugin.as_str() == "secrets-granted"
                && e.operation == "secret:read:MY_SECRET"
                && e.decision == AuditDecision::Allow
        })
        .collect();
    assert!(
        !allow_events.is_empty(),
        "a successful secrets.get must record Allow for secret:read:MY_SECRET"
    );
    assert!(
        allow_events
            .iter()
            .any(|e| e.detail.as_deref().is_some_and(|d| d.contains("env"))),
        "the Allow audit must carry the backend label in its detail; \
         events: {allow_events:?}"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

/// (T6-2) UNGRANTED plugin: `secrets.get` returns nil → payload is
/// `HostValue::Nil`.  Gate records a Deny for `secret:read:MY_SECRET`.
#[test]
fn secrets_get_ungranted_returns_nil_and_denies() {
    let (mut runtime, mut log) = make_runtime_with_resolver(env_resolver("MY_SECRET", "HOME"));
    let policy = GrantAsRequested;

    runtime
        .load(SECRETS_UNGRANTED_SRC, identity(), &policy)
        .expect("secrets-ungranted must load");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Nil,
                "secrets.get without the grant must yield nil as the payload"
            );
        }
        other @ ChainResolution::Blocked { .. } => {
            panic!("expected Allowed (with nil payload), got {other:?}")
        }
    }

    // Audit: a Deny for secret:read:MY_SECRET under the ungranted plugin.
    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history
            .iter()
            .any(|e| e.plugin.as_str() == "secrets-ungranted"
                && e.operation == "secret:read:MY_SECRET"
                && e.decision == AuditDecision::Deny),
        "an ungranted secrets.get must record Deny for secret:read:MY_SECRET"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

/// (T6-3) GRANTED plugin reading a name with no def → nil payload, no panic.
/// The Allow audit is still recorded (the permission was granted; the resolver
/// simply has no def for the key).
#[test]
fn secrets_get_undefined_name_returns_nil() {
    let (mut runtime, mut log) = make_runtime_with_resolver(env_resolver("MY_SECRET", "HOME"));
    let policy = GrantAsRequested;

    runtime
        .load(SECRETS_UNDEFINED_NAME_SRC, identity(), &policy)
        .expect("secrets-undefined-name must load");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Nil,
                "secrets.get for an undefined name must yield nil — no panic"
            );
        }
        other @ ChainResolution::Blocked { .. } => {
            panic!("expected Allowed with nil payload, got {other:?}")
        }
    }

    log.shutdown().expect("audit log shuts down cleanly");
}

/// (T6-4) No enumeration surface: `secrets.list` is nil; `pairs(secrets)`
/// yields exactly one key (`get`).  Both observations are surfaced through
/// the filter-chain modify payload so no extra public API is needed.
#[test]
fn secrets_table_has_only_get_no_enumerate() {
    let (mut runtime, mut log) = make_runtime_with_resolver(env_resolver("MY_SECRET", "HOME"));
    let policy = GrantAsRequested;

    runtime
        .load(SECRETS_NO_ENUMERATE_SRC, identity(), &policy)
        .expect("secrets-no-enumerate must load");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            let HostValue::Str(report) = payload else {
                panic!("expected a string payload from the enumerate probe, got {payload:?}");
            };
            assert!(
                report.contains("list_nil=true"),
                "secrets.list must not exist (nil); probe returned: {report}"
            );
            assert!(
                report.contains("count=1"),
                "secrets table must expose exactly one key ('get'); probe returned: {report}"
            );
        }
        other @ ChainResolution::Blocked { .. } => panic!("expected Allowed, got {other:?}"),
    }

    log.shutdown().expect("audit log shuts down cleanly");
}
