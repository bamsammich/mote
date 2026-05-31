//! Integration tests for `mote.events.collect(event, payload)` — the Lua
//! host-API surface an exclusive provider uses to gather contributions from
//! subscriber plugins on a collector event (ADR-0010, Task BC3).
//!
//! Each test drives the full runtime stack (real registry, in-memory store,
//! audit log, Lua states). Observation uses the same side-channel idiom as the
//! secrets / `storage.list_keys` host-API tests: the calling plugin declares a
//! `net:intercept_request` filter-chain hook that returns a string summary of
//! the `collect` result as the modify payload, surfaced through
//! `dispatch_filter_chain` as `ChainResolution::Allowed { payload }`.
//!
//! `urlbar:suggest` is the registry's collector event; `workspaces:on_change`
//! is a broadcast event (`crates/mote-registry/data/events/v1.toml`).

use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{ChainResolution, GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

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

/// A subscriber to `urlbar:suggest` that returns a single suggestion record.
/// Gated by `events:on`.
const SUBSCRIBER_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sub",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["urlbar:suggest"] = function(p)
    return { title = "match-" .. tostring(p.text) }
  end,
}
function M.setup() end
return M
"#;

/// A provider holding `events:emit` (the collect gate). Its
/// `net:intercept_request` hook calls `mote.events.collect("urlbar:suggest",
/// {text="a"})` and reports the count of contributions plus the first
/// contribution's `title` as the modify payload.
const PROVIDER_GRANTED_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "provider-granted",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:emit" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local contribs = mote.events.collect("urlbar:suggest", { text = "a" })
    local n = #contribs
    local first_title = "none"
    if n > 0 and type(contribs[1]) == "table" then
      first_title = tostring(contribs[1].title)
    end
    return { action = "modify", payload = "count=" .. tostring(n) .. ",first=" .. first_title }
  end,
}
function M.setup() end
return M
"#;

/// A provider WITHOUT `events:emit`. `collect` must default-deny → empty table
/// (count 0), never an error.
const PROVIDER_DENIED_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "provider-denied",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local contribs = mote.events.collect("urlbar:suggest", { text = "a" })
    return { action = "modify", payload = "count=" .. tostring(#contribs) }
  end,
}
function M.setup() end
return M
"#;

/// A provider holding `events:emit` that calls `collect` on a BROADCAST event
/// (`workspaces:on_change`). The host API must refuse to collect from a
/// non-collector event → empty table (count 0).
const PROVIDER_BROADCAST_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "provider-broadcast",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:emit" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local contribs = mote.events.collect("workspaces:on_change", { active = "x" })
    return { action = "modify", payload = "count=" .. tostring(#contribs) }
  end,
}
function M.setup() end
return M
"#;

/// BC3-1: a provider with `events:emit` gets back a Lua array of the
/// subscribers' returns from `mote.events.collect`.
#[test]
fn events_collect_returns_contributions() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(SUBSCRIBER_SRC, identity(), &policy)
        .expect("subscriber loads");
    runtime
        .load(PROVIDER_GRANTED_SRC, identity(), &policy)
        .expect("provider-granted loads");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Str("count=1,first=match-a".to_owned()),
                "collect must return one contribution carrying the subscriber's marshalled return"
            );
        }
        other @ ChainResolution::Blocked { .. } => panic!("expected Allowed, got {other:?}"),
    }

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// BC3-2: a provider WITHOUT `events:emit` gets an empty table (default-deny),
/// not an error.
#[test]
fn events_collect_denied_without_permission() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(SUBSCRIBER_SRC, identity(), &policy)
        .expect("subscriber loads");
    runtime
        .load(PROVIDER_DENIED_SRC, identity(), &policy)
        .expect("provider-denied loads");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Str("count=0".to_owned()),
                "collect without events:emit must yield an empty table (count 0), not error"
            );
        }
        other @ ChainResolution::Blocked { .. } => panic!("expected Allowed, got {other:?}"),
    }

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// BC3-3: collect on a non-collector (broadcast) event yields an empty table
/// even with `events:emit` — only collector-dispatch events are collectable.
#[test]
fn events_collect_rejects_non_collector_event() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(PROVIDER_BROADCAST_SRC, identity(), &policy)
        .expect("provider-broadcast loads");

    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload,
                HostValue::Str("count=0".to_owned()),
                "collect on a broadcast event must gather nothing (count 0)"
            );
        }
        other @ ChainResolution::Blocked { .. } => panic!("expected Allowed, got {other:?}"),
    }

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}
