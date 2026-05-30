//! Integration tests for `mote.storage.list_keys()` — the Lua host-API surface
//! that enumerates a plugin's own storage keys in lexicographic order.
//!
//! Both tests drive the full runtime stack (real registry, real in-memory
//! store, real audit log, real Lua state) so that the permission gate, the
//! storage namespace scoping, and the Lua return type are all exercised at once.

use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{ChainResolution, GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

// ---------------------------------------------------------------------------
// Shared helpers (mirror lifecycle.rs / end_to_end.rs)
// ---------------------------------------------------------------------------

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

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(1))
}

fn drain(log: &AuditLog) {
    thread::sleep(Duration::from_millis(40));
    let _ = log;
}

// ---------------------------------------------------------------------------
// Test plugins
//
// Both declare `net:intercept_request` so the hook registers and
// `dispatch_filter_chain` can be used as the observable side-channel,
// exactly as the secrets tests do in end_to_end.rs.  The hook returns the
// key list (or its length) as the modify payload.
// ---------------------------------------------------------------------------

/// Plugin granted `storage:persistent`.
/// Its `setup()` writes two keys ("b" then "a") — deliberately out of order
/// so the test proves lexicographic sorting, not insertion order.
/// The hook calls `list_keys()` and returns the keys joined with ","
/// as the modify payload.
const LIST_KEYS_GRANTED_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "list-keys-granted",
  version = "1.0.0",
  permissions = { "net:intercept_request", "storage:persistent" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    -- Write two keys out of lexicographic order to prove sorting.
    storage.set("b", "second")
    storage.set("a", "first")
    local keys = storage.list_keys()
    -- Serialise as "a,b" (or empty string if no keys) for assertion.
    local result = table.concat(keys, ",")
    return { action = "modify", payload = result }
  end,
}
function M.setup() end
return M
"#;

/// Plugin NOT granted `storage:persistent`.
/// Its hook calls `list_keys()` — the gate must deny and return an empty table.
/// The hook encodes the result length in the payload so the test can observe it.
const LIST_KEYS_DENIED_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "list-keys-denied",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local keys = storage.list_keys()
    -- Must be an empty table; encode length so we can assert == 0.
    return { action = "modify", payload = tostring(#keys) }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A plugin granted `storage:persistent` that writes "b" then "a" gets back
/// exactly `{"a","b"}` in lexicographic order from `list_keys()`.
#[test]
fn list_keys_returns_scoped_keys() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(LIST_KEYS_GRANTED_SRC, identity(), &policy)
        .expect("list-keys-granted must load");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Str("a,b".to_owned()),
                "list_keys() must return all written keys in lexicographic order; \
                 expected \"a,b\", got {payload:?}"
            );
        }
        other @ ChainResolution::Blocked { .. } => {
            panic!("expected Allowed, got {other:?}")
        }
    }

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// A plugin without `storage:persistent` calling `list_keys()` gets back an
/// empty table (default-deny), not an error, and does not panic.
#[test]
fn list_keys_denied_without_permission() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(LIST_KEYS_DENIED_SRC, identity(), &policy)
        .expect("list-keys-denied must load");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Str("0".to_owned()),
                "list_keys() without storage:persistent must return an empty table \
                 (length == 0); got payload {payload:?}"
            );
        }
        other @ ChainResolution::Blocked { .. } => {
            panic!("expected Allowed (with '0' payload), got {other:?}")
        }
    }

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}
