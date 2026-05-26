//! S1 — `capabilities.invoke` confused-deputy + no-deadline regression tests.
//!
//! These prove two consumer-side hardening properties of `capabilities.invoke`:
//!
//! 1. A consumer may only invoke functions named in the capability's CONTRACT
//!    (`required_api`). Invoking any other fulfiller function is rejected before
//!    the fulfiller is touched, and audited as a denial (confused-deputy
//!    defence).
//! 2. A fulfiller whose invoked function loops forever is interrupted at the
//!    deadline (Timeout) rather than hanging the runtime, and audited as a
//!    denial noting the deadline.
//!
//! Both are driven end-to-end through `events.emit` → the consumer's `M.events`
//! handler → `capabilities.invoke`, then asserted through the audit log (the
//! fulfiller is the audited performer for an invocation; D4).

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config, Decision as AuditDecision};
use mote_registry::Registry;
use mote_runtime::{GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

fn make_runtime() -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let store = Store::open_in_memory().expect("in-memory store");
    let config = Config {
        ring_capacity: 256,
        flush_threshold: 1,
        flush_interval: Duration::from_millis(5),
    };
    let log = AuditLog::new(&store, config).expect("audit log starts");
    let runtime = Runtime::new(registry, store, log.producer());
    (runtime, log)
}

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(1))
}

fn drain(log: &AuditLog) {
    thread::sleep(Duration::from_millis(60));
    let _ = log;
}

/// The form-services fulfiller. Its contract requires `show_autofill_picker` +
/// `inject_isolated`. It ALSO exposes an undeclared `secret_internal` function
/// (not in the contract) and a `looper` that spins forever — neither of which a
/// consumer should be able to reach / hang on.
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
  show_autofill_picker = function(items) return { ok = true } end,
  inject_isolated = function(script, world) return true end,
  -- NOT part of the contract's required_api:
  secret_internal = function(x) return { leaked = true } end,
  -- A function (also not in contract) that would hang without a deadline.
  looper = function(x) while true do end end,
}
M.hooks = { ["page:on_load"] = function(p) end }
function M.setup() end
return M
"#;

/// Builds a consumer that, on the form-detected event, invokes `fn_name` on the
/// form-services capability.
fn consumer_invoking(fn_name: &str) -> String {
    format!(
        r#"
local M = {{}}
M.manifest = {{
  schema = "v1",
  name = "onepw",
  version = "1.0.0",
  permissions = {{ "events:on", "events:emit" }},
  consumes = {{ "password-manager-form-services" }},
  identity_scope = "global",
}}
M.events = {{
  ["password-manager-form-services:form-detected"] = function(form)
    capabilities.invoke(
      "password-manager-form-services",
      "{fn_name}",
      {{ choice = "x" }}
    )
  end,
}}
function M.setup() end
return M
"#
    )
}

fn load_pair(rt: &mut Runtime, consumer_fn: &str) {
    let policy = GrantAsRequested;
    rt.load(FORM_SERVICES_SRC, identity(), &policy)
        .expect("form-services loads");
    rt.load(&consumer_invoking(consumer_fn), identity(), &policy)
        .expect("consumer loads (capability fulfilled)");
}

#[test]
fn invoking_a_function_outside_the_contract_is_rejected() {
    let (mut rt, log) = make_runtime();
    // The consumer tries to invoke `secret_internal`, which exists in the
    // fulfiller's M.api but is NOT in the capability's required_api contract.
    load_pair(&mut rt, "secret_internal");

    let delivered = rt.emit_event(
        "password-manager-form-services:form-detected",
        &HostValue::Map(BTreeMap::default()),
    );
    assert_eq!(delivered, 1, "onepw's handler fires");

    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| {
            e.operation == "password-manager-form-services:secret_internal"
                && e.decision == AuditDecision::Deny
                && e.detail
                    .as_deref()
                    .is_some_and(|d| d.contains("not in the capability contract"))
        }),
        "an out-of-contract function must be rejected + audited as a denial; history: {:?}",
        history
            .iter()
            .map(|e| (e.operation.clone(), e.decision))
            .collect::<Vec<_>>()
    );
}

#[test]
fn in_contract_function_is_allowed() {
    // Control: a function that IS in the contract runs and is audited as Allow.
    let (mut rt, log) = make_runtime();
    load_pair(&mut rt, "show_autofill_picker");

    let _ = rt.emit_event(
        "password-manager-form-services:form-detected",
        &HostValue::Map(BTreeMap::default()),
    );

    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| {
            e.plugin.as_str() == "form-services"
                && e.operation == "password-manager-form-services:show_autofill_picker"
                && e.decision == AuditDecision::Allow
        }),
        "an in-contract function runs under the fulfiller and is audited Allow"
    );
}

#[test]
fn a_looping_fulfiller_function_is_interrupted_not_hung() {
    let (mut rt, log) = make_runtime();
    // `looper` is not in the contract, so it would be rejected before running.
    // To prove the DEADLINE specifically, point the contract-valid name at a
    // looping body: redefine `show_autofill_picker` to loop.
    let looping_fulfiller = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "form-services",
  version = "1.0.0",
  permissions = { "page:read_dom" },
  capabilities = { "password-manager-form-services" },
  identity_scope = "global",
}
M.api = {
  show_autofill_picker = function(items) while true do end end,
  inject_isolated = function(script, world) return true end,
}
M.hooks = { ["page:on_load"] = function(p) end }
function M.setup() end
return M
"#;
    let policy = GrantAsRequested;
    rt.load(looping_fulfiller, identity(), &policy)
        .expect("looping form-services loads");
    rt.load(
        &consumer_invoking("show_autofill_picker"),
        identity(),
        &policy,
    )
    .expect("consumer loads");

    // This must RETURN (not hang): the fulfiller's looping function is
    // interrupted at the deadline.
    let _ = rt.emit_event(
        "password-manager-form-services:form-detected",
        &HostValue::Map(BTreeMap::default()),
    );

    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| {
            e.operation == "password-manager-form-services:show_autofill_picker"
                && e.decision == AuditDecision::Deny
                && e.detail.as_deref().is_some_and(|d| d.contains("deadline"))
        }),
        "a looping fulfiller function must time out + be audited as a deadline denial"
    );
}
