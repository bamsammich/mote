//! Black-box behavioral tests for the bundled `bookmarks` first-party plugin.
//!
//! All assertions drive the bookmarks plugin through a tiny consumer plugin
//! that calls `capabilities.invoke("ui:bookmarks_provider", fn, arg)` — the
//! pure Lua→Lua path (no Rust host method for capability invocation needed).
//! The consumer pattern mirrors the `end_to_end.rs` form-services consumer.
//!
//! The consumer plugin returns results from `capabilities.invoke` as a
//! `net:intercept_request` hook payload so the Rust test can read them back
//! via `dispatch_filter_chain` — the same observable side-channel used
//! throughout the test suite.
//!
//! Tests:
//!   1. `add_then_list_round_trip` — added bookmark appears in list.
//!   2. `list_filters_by_query` — substring filter keeps only matching records.
//!   3. `remove_drops_entry` — removed bookmark is gone from list.
//!   4. `bookmarks_survive_reload` — data persists across a runtime reload
//!      against the same on-disk store (proves durable KV, not just in-memory).

use std::path::Path;
use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{ChainResolution, GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn make_runtime_on_store(store: Store) -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let config = Config {
        ring_capacity: 256,
        flush_threshold: 1,
        flush_interval: Duration::from_millis(5),
    };
    let log = AuditLog::new(&store, config).expect("audit log starts");
    let runtime = Runtime::new(registry, store, log.producer());
    (runtime, log)
}

fn make_runtime() -> (Runtime, AuditLog) {
    let store = Store::open_in_memory().expect("in-memory store");
    make_runtime_on_store(store)
}

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(1))
}

fn drain(log: &AuditLog) {
    thread::sleep(Duration::from_millis(40));
    let _ = log;
}

/// The bundled bookmarks plugin source.
const BOOKMARKS_SRC: &str = include_str!("../../../plugins/bookmarks/init.lua");

/// A consumer plugin that:
/// 1. Calls `add_bookmark` with `url=TEST_URL`, `title=TEST_TITLE`.
/// 2. Calls `list_bookmarks(nil)`.
/// 3. Returns the list length (as a string) via a `net:intercept_request`
///    hook so the test can observe it.
const CONSUMER_ADD_THEN_LIST: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bm-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    -- add a bookmark
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://example.com", title = "Example" })
    -- list all bookmarks
    local bms = capabilities.invoke("ui:bookmarks_provider", "list_bookmarks", nil)
    local count = 0
    if bms ~= nil then count = #bms end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer that calls `list_bookmarks` with a filter string and returns the
/// count of matching records.
const CONSUMER_FILTER: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bm-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    -- add two bookmarks with different titles
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://rust-lang.org", title = "Rust Programming Language" })
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://example.com", title = "Example Site" })
    -- filter for "Rust" — should return only the rust-lang entry
    local bms = capabilities.invoke("ui:bookmarks_provider", "list_bookmarks", "Rust")
    local count = 0
    if bms ~= nil then count = #bms end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer that adds a bookmark, removes it, then lists; reports count.
const CONSUMER_REMOVE: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bm-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://to-remove.com", title = "Gone" })
    capabilities.invoke("ui:bookmarks_provider", "remove_bookmark",
      { url = "https://to-remove.com" })
    local bms = capabilities.invoke("ui:bookmarks_provider", "list_bookmarks", nil)
    local count = 0
    if bms ~= nil then count = #bms end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer that seeds a bookmark in `setup()`, then lists it from the hook.
/// Used across two runtime sessions to prove persistence.
const CONSUMER_SEED: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bm-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://persisted.example.com", title = "Persisted" })
    return { action = "allow" }
  end,
}
function M.setup() end
return M
"#;

/// Consumer used in the second runtime session: just lists all bookmarks and
/// returns the count.
const CONSUMER_LIST_ONLY: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bm-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local bms = capabilities.invoke("ui:bookmarks_provider", "list_bookmarks", nil)
    local count = 0
    if bms ~= nil then count = #bms end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load the bookmarks fulfiller and the given consumer plugin into a runtime.
fn load_pair(rt: &mut Runtime, consumer_src: &str) {
    let policy = GrantAsRequested;
    rt.load(BOOKMARKS_SRC, identity(), &policy)
        .expect("bookmarks plugin must load cleanly");
    rt.load(consumer_src, identity(), &policy)
        .expect("consumer plugin must load cleanly");
}

/// Dispatch the filter-chain hook and return the `HostValue::Str` payload.
fn dispatch_and_read(rt: &mut Runtime) -> String {
    let outcome = rt.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    match outcome.resolution {
        ChainResolution::Allowed { payload } => match payload {
            HostValue::Str(s) => s,
            HostValue::Nil => String::new(),
            other => panic!("expected a string payload, got {other:?}"),
        },
        ChainResolution::Blocked { reason, .. } => {
            panic!("expected Allowed, got Blocked(reason={reason:?})")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Adding a bookmark then listing returns exactly one entry.
#[test]
fn add_then_list_round_trip() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_ADD_THEN_LIST);

    let count_str = dispatch_and_read(&mut rt);
    assert_eq!(
        count_str, "1",
        "after adding one bookmark, list_bookmarks must return exactly 1 entry; \
         got count={count_str}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `list_bookmarks` with a non-empty filter keeps only records whose url or
/// title contains the filter substring.
#[test]
fn list_filters_by_query() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_FILTER);

    let count_str = dispatch_and_read(&mut rt);
    assert_eq!(
        count_str, "1",
        "filtering by 'Rust' must return exactly 1 record (rust-lang.org); \
         got count={count_str}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// Removing a bookmark drops it from subsequent list results.
#[test]
fn remove_drops_entry() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_REMOVE);

    let count_str = dispatch_and_read(&mut rt);
    assert_eq!(
        count_str, "0",
        "after adding and removing a bookmark, list_bookmarks must return 0 entries; \
         got count={count_str}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// Bookmarks written in one runtime session survive when the same on-disk
/// store is reopened in a new runtime session.
#[test]
fn bookmarks_survive_reload() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path: &Path = &dir.path().join("test-bookmarks.db");

    // --- Session 1: seed a bookmark via the filter-chain hook ---------------
    {
        let store = Store::open(db_path).expect("open store session 1");
        let (mut rt, mut log) = make_runtime_on_store(store);
        let policy = GrantAsRequested;
        rt.load(BOOKMARKS_SRC, identity(), &policy)
            .expect("bookmarks loads in session 1");
        rt.load(CONSUMER_SEED, identity(), &policy)
            .expect("seed consumer loads in session 1");

        // Trigger the hook which adds the bookmark.
        let outcome = rt.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
        assert!(
            matches!(outcome.resolution, ChainResolution::Allowed { .. }),
            "seed hook must succeed"
        );

        drain(&log);
        log.shutdown().expect("session-1 audit shuts down");
        // `rt` (and thus the store connection) is dropped here, closing SQLite.
    }

    // --- Session 2: reopen the same store, list bookmarks -------------------
    {
        let store = Store::open(db_path).expect("open store session 2");
        let (mut rt, mut log) = make_runtime_on_store(store);
        load_pair(&mut rt, CONSUMER_LIST_ONLY);

        let count_str = dispatch_and_read(&mut rt);
        assert_eq!(
            count_str, "1",
            "bookmark added in session 1 must still be present in session 2; \
             got count={count_str}"
        );

        drain(&log);
        log.shutdown().expect("session-2 audit shuts down");
    }
}
