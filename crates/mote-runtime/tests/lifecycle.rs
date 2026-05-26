//! Lifecycle integration tests: load / reload (the three hot-reload scenarios)
//! / unload, and the host-API gating and `permissions.effective()` surface.

use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config, Decision as AuditDecision};
use mote_registry::Registry;
use mote_runtime::{
    GrantAsRequested, HostValue, IdentityContext, LifecycleError, LoadError, Runtime,
};
use mote_storage::Store;
use mote_types::{IdentityId, PluginName, SchemaVersion};

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

fn name(s: &str) -> PluginName {
    PluginName::new(s).unwrap()
}

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(7))
}

fn drain(log: &AuditLog) {
    thread::sleep(Duration::from_millis(40));
    let _ = log;
}

/// A behavioural plugin holding `tabs:list` and `storage:persistent`. Its
/// `setup()` exercises the gated host API: `permissions.effective()`,
/// `tabs.list()` (allowed), and a `storage.set` (allowed).
fn behavioural(version: &str, perms: &[&str]) -> String {
    let perm_list = perms
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
local M = {{}}
M.manifest = {{
  schema = "v1",
  name = "behaviour",
  version = "{version}",
  permissions = {{ {perm_list} }},
  identity_scope = "global",
}}
M.hooks = {{
  ["tabs:on_change"] = function(p) end,
}}
function M.setup()
  local eff = permissions.effective()
  M.effective_count = #eff
  tabs.list()
  storage.set("k", "v")
end
return M
"#
    )
}

#[test]
fn load_then_unload_frees_the_name() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let src = behavioural("1.0.0", &["tabs:list", "storage:persistent"]);

    rt.load(&src, identity(), &policy).unwrap();
    assert!(rt.is_loaded(&name("behaviour")));

    // setup() made an allowed tabs:list and storage write.
    drain(&log);
    let history = log.query().history().unwrap();
    assert!(history.iter().any(|e| e.plugin.as_str() == "behaviour"
        && e.operation == "tabs:list"
        && e.decision == AuditDecision::Allow));

    rt.unload(&name("behaviour")).unwrap();
    assert!(!rt.is_loaded(&name("behaviour")));

    // Unloading an absent plugin errors.
    assert!(matches!(
        rt.unload(&name("behaviour")),
        Err(LifecycleError::NotLoaded { .. })
    ));

    // After unload the name is free to load again.
    rt.load(&src, identity(), &policy).unwrap();
    assert!(rt.is_loaded(&name("behaviour")));

    log.shutdown().unwrap();
}

#[test]
fn double_load_same_name_fails() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let src = behavioural("1.0.0", &["tabs:list", "storage:persistent"]);
    rt.load(&src, identity(), &policy).unwrap();
    assert!(matches!(
        rt.load(&src, identity(), &policy),
        Err(LoadError::AlreadyLoaded { .. })
    ));
    log.shutdown().unwrap();
}

#[test]
fn reload_code_only_change_needs_no_reapproval() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let v1 = behavioural("1.0.0", &["tabs:list", "storage:persistent"]);
    // Same four approval-relevant fields, different version string + body =>
    // code-only change. Reload must succeed even with re-approval disallowed.
    let v2 = behavioural("2.0.0", &["tabs:list", "storage:persistent"]);

    let first = rt.load(&v1, identity(), &policy).unwrap();
    let reloaded = rt
        .reload(&name("behaviour"), &v2, identity(), &policy, false)
        .expect("code-only reload proceeds without re-approval");

    // The approval fingerprint is unchanged across a code-only reload.
    assert_eq!(first.approval, reloaded.approval);
    assert!(rt.is_loaded(&name("behaviour")));
    log.shutdown().unwrap();
}

#[test]
fn reload_expanding_permissions_requires_reapproval() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let v1 = behavioural("1.0.0", &["tabs:list", "storage:persistent"]);
    // Adds history:read => expansion of `permissions` => re-approval required.
    let v2 = behavioural(
        "2.0.0",
        &["tabs:list", "storage:persistent", "history:read"],
    );

    rt.load(&v1, identity(), &policy).unwrap();

    // With re-approval disallowed, an expanding reload is refused and the old
    // instance keeps running (awaiting approval).
    let err = rt
        .reload(&name("behaviour"), &v2, identity(), &policy, false)
        .expect_err("expanding reload without approval must be refused");
    assert!(matches!(
        err,
        LifecycleError::Load(LoadError::ApprovalDenied { .. })
    ));
    assert!(
        rt.is_loaded(&name("behaviour")),
        "a refused expansion leaves the working instance running until approval"
    );

    // With re-approval granted, the expanding reload proceeds.
    let reloaded = rt
        .reload(&name("behaviour"), &v2, identity(), &policy, true)
        .expect("expanding reload proceeds when re-approval is granted");
    assert!(
        reloaded
            .effective_permissions
            .iter()
            .any(|p| p.starts_with("history:read")),
        "the expanded permission is now effective"
    );
    log.shutdown().unwrap();
}

#[test]
fn reload_nonexpanding_manifest_change_needs_no_reapproval() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let v1 = behavioural(
        "1.0.0",
        &["tabs:list", "storage:persistent", "history:read"],
    );
    // Removes history:read => contraction, not expansion => no re-approval.
    let v2 = behavioural("2.0.0", &["tabs:list", "storage:persistent"]);

    rt.load(&v1, identity(), &policy).unwrap();
    rt.reload(&name("behaviour"), &v2, identity(), &policy, false)
        .expect("a non-expanding (contracting) manifest change reloads without approval");
    assert!(rt.is_loaded(&name("behaviour")));
    log.shutdown().unwrap();
}

#[test]
fn reload_unloaded_plugin_errors() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let src = behavioural("1.0.0", &["tabs:list"]);
    assert!(matches!(
        rt.reload(&name("behaviour"), &src, identity(), &policy, false),
        Err(LifecycleError::NotLoaded { .. })
    ));
    log.shutdown().unwrap();
}

#[test]
fn denied_host_call_is_audited_as_denial() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    // No storage:persistent → setup()'s storage.set is denied; tabs:list is
    // also absent → tabs.list() denied.
    let src = behavioural("1.0.0", &["tabs:list"]);
    rt.load(&src, identity(), &policy).unwrap();

    drain(&log);
    let history = log.query().history().unwrap();
    assert!(
        history.iter().any(|e| e.plugin.as_str() == "behaviour"
            && e.operation == "storage:persistent"
            && e.decision == AuditDecision::Deny),
        "storage.set without storage:persistent must be denied + audited"
    );
    // tabs:list IS granted here, so it is allowed.
    assert!(history.iter().any(|e| e.plugin.as_str() == "behaviour"
        && e.operation == "tabs:list"
        && e.decision == AuditDecision::Allow));
    log.shutdown().unwrap();
}

#[test]
fn unknown_permission_term_fails_step1() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    let src = behavioural("1.0.0", &["nonsense:action"]);
    let err = rt.load(&src, identity(), &policy).unwrap_err();
    assert!(matches!(err, LoadError::Schema(_)), "got {err:?}");
    // Nothing loaded.
    assert!(!rt.is_loaded(&name("behaviour")));
    let _ = HostValue::Nil; // keep the import exercised
    log.shutdown().unwrap();
}
