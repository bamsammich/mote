//! Collector-dispatch engine proof (ADR-0010, Tasks BC2/BC3).
//!
//! Drives `Runtime::collect_event` (the host-side seam over `Core::collect`)
//! against real inline Lua plugins to assert the four load-bearing properties
//! of the collecting dispatch path:
//!
//! 1. Subscriber RETURN values are gathered (unlike `emit`, which discards
//!    them) and marshalled to `HostValue` — `collect_gathers_subscriber_returns`.
//! 2. A subscriber that errors is isolated (dropped from results, audit-logged
//!    under the *subscriber*) while the good subscriber still contributes —
//!    `collect_isolates_failing_subscriber`.
//! 3. A subscriber that busy-spins past its per-call cap is dropped via timeout
//!    isolation; the other still contributes (per-subscriber bounding) —
//!    `collect_drops_slow_subscriber`.
//! 4. Only `Collector`-dispatch events are collectable; a broadcast event
//!    yields no contributions — `collect_rejects_non_collector_event`.
//!
//! `urlbar:suggest` is the registry's collector event; `workspaces:on_change`
//! is a broadcast event (`crates/mote-registry/data/events/v1.toml`).

use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config, Decision as AuditDecision};
use mote_registry::Registry;
use mote_runtime::{GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_types::{IdentityId, PluginName, SchemaVersion};

/// Subscriber A: contributes a single suggestion record for `urlbar:suggest`.
/// `events:on` is the subscriber gate; it returns a list of records.
const SUB_A_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sub-a",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["urlbar:suggest"] = function(p)
    return { { source = "a", title = "alpha-" .. tostring(p.text) } }
  end,
}
function M.setup() end
return M
"#;

/// Subscriber B: contributes a different single suggestion record.
const SUB_B_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sub-b",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["urlbar:suggest"] = function(p)
    return { { source = "b", title = "beta-" .. tostring(p.text) } }
  end,
}
function M.setup() end
return M
"#;

/// A subscriber whose `urlbar:suggest` handler raises a Lua error: must be
/// isolated (dropped + audited under this plugin) without aborting the
/// collection.
const SUB_BOOM_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sub-boom",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["urlbar:suggest"] = function(p)
    error("boom")
  end,
}
function M.setup() end
return M
"#;

/// A subscriber whose `urlbar:suggest` handler busy-spins forever: must be
/// dropped via the per-subscriber deadline cap. Sorted before `sub-z` by name
/// so the good subscriber is reached after the slow one times out.
const SUB_SLOW_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sub-slow",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["urlbar:suggest"] = function(p)
    while true do end
  end,
}
function M.setup() end
return M
"#;

/// A fast, well-behaved subscriber sorted AFTER `sub-slow` by name, to prove
/// that the slow subscriber's timeout does not starve later contributors.
const SUB_Z_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "sub-z",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["urlbar:suggest"] = function(p)
    return { { source = "z", title = "zeta" } }
  end,
}
function M.setup() end
return M
"#;

/// A subscriber to a BROADCAST event (`workspaces:on_change`) that returns a
/// value. `collect` must refuse to gather from a non-collector event, so this
/// return is never captured.
const BROADCAST_SUB_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bcast-sub",
  version = "1.0.0",
  permissions = { "events:on" },
  identity_scope = "global",
}
M.events = {
  ["workspaces:on_change"] = function(p)
    return { { source = "bcast" } }
  end,
}
function M.setup() end
return M
"#;

fn make_runtime() -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let store = mote_storage::Store::open_in_memory().expect("in-memory store");
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

/// Let the audit thread drain pending events before reading history.
fn drain(log: &AuditLog) {
    thread::sleep(Duration::from_millis(40));
    let _ = log;
}

/// A payload carrying `{ text = "ab" }`, the urlbar query shape.
fn query_payload() -> HostValue {
    let mut m = std::collections::BTreeMap::new();
    m.insert("text".to_owned(), HostValue::Str("ab".to_owned()));
    HostValue::Map(m)
}

/// BC2-1: two subscribers each return a contribution → `collect` gathers both,
/// marshalled to `HostValue`, in deterministic name-sorted order (a before b).
#[test]
fn collect_gathers_subscriber_returns() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(SUB_A_SRC, identity(), &policy)
        .expect("sub-a loads");
    runtime
        .load(SUB_B_SRC, identity(), &policy)
        .expect("sub-b loads");

    let contributions = runtime.collect_event("urlbar:suggest", &query_payload());
    assert_eq!(
        contributions.len(),
        2,
        "both subscribers must contribute one return value each"
    );

    // Each contribution is a list with a single record map. Name-sorted order:
    // sub-a first, sub-b second.
    let titles: Vec<String> = contributions
        .iter()
        .filter_map(|c| match c {
            HostValue::List(items) => items.first().and_then(|first| {
                first
                    .get("title")
                    .and_then(HostValue::as_str)
                    .map(str::to_owned)
            }),
            _ => None,
        })
        .collect();
    assert_eq!(
        titles,
        vec!["alpha-ab".to_owned(), "beta-ab".to_owned()],
        "contributions must be marshalled correctly and ordered by subscriber name"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

/// BC2-2: one subscriber errors → it is dropped from results, the other still
/// contributes, and the failure is audited under the FAILING plugin.
#[test]
fn collect_isolates_failing_subscriber() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(SUB_A_SRC, identity(), &policy)
        .expect("sub-a loads");
    runtime
        .load(SUB_BOOM_SRC, identity(), &policy)
        .expect("sub-boom loads");

    let contributions = runtime.collect_event("urlbar:suggest", &query_payload());
    assert_eq!(
        contributions.len(),
        1,
        "only the well-behaved subscriber contributes; the failing one is dropped"
    );
    let title = match &contributions[0] {
        HostValue::List(items) => items
            .first()
            .and_then(|f| f.get("title"))
            .and_then(HostValue::as_str),
        _ => None,
    };
    assert_eq!(
        title,
        Some("alpha-ab"),
        "the surviving contribution is sub-a's"
    );

    // The failure is audited under the SUBSCRIBER (sub-boom) as a Deny.
    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| e.plugin == plugin("sub-boom")
            && e.operation == "urlbar:suggest"
            && e.decision == AuditDecision::Deny),
        "a failing subscriber's error must be audited under the subscriber as a Deny"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

/// BC2-3: a busy-spinning subscriber is dropped via per-subscriber timeout
/// isolation; a later well-behaved subscriber still contributes (proving the
/// per-subscriber bound — one slow contributor does not starve the rest).
#[test]
fn collect_drops_slow_subscriber() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(SUB_SLOW_SRC, identity(), &policy)
        .expect("sub-slow loads");
    runtime
        .load(SUB_Z_SRC, identity(), &policy)
        .expect("sub-z loads");

    let contributions = runtime.collect_event("urlbar:suggest", &query_payload());
    assert_eq!(
        contributions.len(),
        1,
        "the busy-spinning subscriber is dropped on timeout; sub-z still contributes"
    );
    let title = match &contributions[0] {
        HostValue::List(items) => items
            .first()
            .and_then(|f| f.get("title"))
            .and_then(HostValue::as_str),
        _ => None,
    };
    assert_eq!(title, Some("zeta"), "the surviving contribution is sub-z's");

    // The slow subscriber's timeout is audited under it as a Deny.
    drain(&log);
    let history = log.query().history().expect("audit history");
    assert!(
        history.iter().any(|e| e.plugin == plugin("sub-slow")
            && e.operation == "urlbar:suggest"
            && e.decision == AuditDecision::Deny),
        "a timed-out subscriber must be audited under the subscriber as a Deny"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}

/// BC2-4: `collect` only gathers from `Collector`-dispatch events. A broadcast
/// event (`workspaces:on_change`) yields no contributions even though a
/// subscriber returns a value.
#[test]
fn collect_rejects_non_collector_event() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    runtime
        .load(BROADCAST_SUB_SRC, identity(), &policy)
        .expect("bcast-sub loads");

    let contributions = runtime.collect_event("workspaces:on_change", &query_payload());
    assert!(
        contributions.is_empty(),
        "collect on a non-collector (broadcast) event must gather nothing"
    );

    log.shutdown().expect("audit log shuts down cleanly");
}
