//! Black-box behavioral tests for the bundled `history` first-party plugin.
//!
//! All assertions drive the history plugin through a tiny consumer plugin
//! that calls `capabilities.invoke("ui:history_provider", fn, arg)` — the
//! pure Lua→Lua path (no Rust host method for capability invocation needed).
//! The consumer pattern mirrors the `bookmarks_behavior.rs` form.
//!
//! The consumer plugin returns results from `capabilities.invoke` as a
//! `net:intercept_request` hook payload so the Rust test can read them back
//! via `dispatch_filter_chain` — the same observable side-channel used
//! throughout the test suite.
//!
//! Tests:
//!   1. `record_visit_dedupes_and_counts`   — same URL visited twice → one
//!      entry with `visit_count`=2.
//!   2. `record_visit_updates_title`        — title is updated by a subsequent
//!      visit; empty/nil title does NOT overwrite a real title.
//!   3. `query_history_filters_by_substring`— filter keeps only matching entries.
//!   4. `query_history_ranks_by_visits_then_recency` — higher `visit_count` wins;
//!      equal `visit_count` → more recent (`last_visited`) wins.
//!   5. `history_survives_reload`           — visits persisted across runtime
//!      reload against same on-disk store.

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

/// The bundled history plugin source.
const HISTORY_SRC: &str = include_str!("../../../plugins/history/init.lua");

// ---------------------------------------------------------------------------
// Consumer plugin templates
// ---------------------------------------------------------------------------

/// Consumer: calls `record_visit` twice for the same URL, then calls
/// `query_history(nil)` and returns the `visit_count` of the first result.
const CONSUMER_DEDUPE: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://example.com", title = "Example" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://example.com", title = "Example" })
    local results = capabilities.invoke("ui:history_provider", "query_history", nil)
    local count = 0
    if results ~= nil and results[1] ~= nil then
      count = results[1].visit_count or 0
    end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: records with title "A", then with title "B", then with no title;
/// queries and returns the title of the entry.
const CONSUMER_TITLE_UPDATE: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://title-test.com", title = "A" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://title-test.com", title = "B" })
    -- visit with no title — must NOT overwrite "B"
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://title-test.com" })
    local results = capabilities.invoke("ui:history_provider", "query_history", nil)
    local title = ""
    if results ~= nil and results[1] ~= nil then
      title = results[1].title or ""
    end
    return { action = "modify", payload = title }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: seeds 3 URLs, queries with a substring that matches only 1.
const CONSUMER_FILTER: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://rust-lang.org", title = "Rust" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://example.com", title = "Example" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://github.com", title = "GitHub" })
    local results = capabilities.invoke("ui:history_provider", "query_history", "rust")
    local count = 0
    if results ~= nil then count = #results end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: seeds URL A with 3 visits, URL B with 1 visit (but B is added
/// after A so B has a higher `last_visited`).
/// Returns the URL of the first-ranked result — must be A (higher `visit_count`).
const CONSUMER_RANK_VISITS: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    -- A gets 3 visits first
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.com", title = "A" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.com", title = "A" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.com", title = "A" })
    -- B gets 1 visit after A — so B has a higher last_visited seq
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://b.com", title = "B" })
    local results = capabilities.invoke("ui:history_provider", "query_history", nil)
    local first_url = ""
    if results ~= nil and results[1] ~= nil then
      first_url = results[1].url or ""
    end
    return { action = "modify", payload = first_url }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: seeds URL A with 1 visit, URL B with 1 visit (but B is added
/// after A so B has a higher `last_visited`).
/// Returns the URL of the first-ranked result — must be B (equal visits, B
/// is more recent).
const CONSUMER_RANK_RECENCY: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    -- A visited first (lower seq number)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a-recency.com", title = "A" })
    -- B visited after A (higher seq number = more recent)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://b-recency.com", title = "B" })
    local results = capabilities.invoke("ui:history_provider", "query_history", nil)
    local first_url = ""
    if results ~= nil and results[1] ~= nil then
      first_url = results[1].url or ""
    end
    return { action = "modify", payload = first_url }
  end,
}
function M.setup() end
return M
"#;

/// Seed consumer: records one visit in setup's hook trigger.
const CONSUMER_SEED: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://persisted.example.com", title = "Persisted" })
    return { action = "allow" }
  end,
}
function M.setup() end
return M
"#;

/// List-only consumer: queries all history and returns count.
const CONSUMER_LIST_ONLY: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local results = capabilities.invoke("ui:history_provider", "query_history", nil)
    local count = 0
    if results ~= nil then count = #results end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Load the history fulfiller and the given consumer plugin into a runtime.
fn load_pair(rt: &mut Runtime, consumer_src: &str) {
    let policy = GrantAsRequested;
    rt.load(HISTORY_SRC, identity(), &policy)
        .expect("history plugin must load cleanly");
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

/// Visiting the same URL twice produces exactly one history entry with
/// `visit_count` = 2.
#[test]
fn record_visit_dedupes_and_counts() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_DEDUPE);

    let count_str = dispatch_and_read(&mut rt);
    assert_eq!(
        count_str, "2",
        "after two visits to the same URL, visit_count must be 2; got {count_str}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `record_visit` updates the title on subsequent visits when the new title is
/// non-empty; a visit with no title must NOT overwrite an existing title.
#[test]
fn record_visit_updates_title() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_TITLE_UPDATE);

    let title = dispatch_and_read(&mut rt);
    assert_eq!(
        title, "B",
        "title must be 'B' after three visits (A then B then no-title); got {title:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query_history` with a filter substring keeps only entries whose url or
/// title contains the filter (case-insensitive ASCII match).
#[test]
fn query_history_filters_by_substring() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_FILTER);

    let count_str = dispatch_and_read(&mut rt);
    assert_eq!(
        count_str, "1",
        "filtering by 'rust' must return exactly 1 entry (rust-lang.org); got {count_str}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query_history` ranks by `visit_count` descending; with equal `visit_count` the
/// more recent entry (higher `last_visited` seq) comes first.
#[test]
fn query_history_ranks_by_visits_then_recency() {
    // Sub-test 1: A has 3 visits, B has 1 (but B is more recent) → A ranks first.
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_RANK_VISITS);

        let first = dispatch_and_read(&mut rt);
        assert_eq!(
            first, "https://a.com",
            "URL with 3 visits must rank above URL with 1 visit; got first={first:?}"
        );

        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }

    // Sub-test 2: A and B each have 1 visit, but B is more recent → B ranks first.
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_RANK_RECENCY);

        let first = dispatch_and_read(&mut rt);
        assert_eq!(
            first, "https://b-recency.com",
            "with equal visit_count, the more recent URL must rank first; got first={first:?}"
        );

        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }
}

/// Visits written in one runtime session survive when the same on-disk store
/// is reopened in a new runtime session.
#[test]
fn history_survives_reload() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path: &Path = &dir.path().join("test-history.db");

    // --- Session 1: seed a visit via the filter-chain hook -------------------
    {
        let store = Store::open(db_path).expect("open store session 1");
        let (mut rt, mut log) = make_runtime_on_store(store);
        let policy = GrantAsRequested;
        rt.load(HISTORY_SRC, identity(), &policy)
            .expect("history loads in session 1");
        rt.load(CONSUMER_SEED, identity(), &policy)
            .expect("seed consumer loads in session 1");

        // Trigger the hook which records the visit.
        let outcome = rt.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
        assert!(
            matches!(outcome.resolution, ChainResolution::Allowed { .. }),
            "seed hook must succeed"
        );

        drain(&log);
        log.shutdown().expect("session-1 audit shuts down");
        // `rt` (and thus the store connection) is dropped here, closing SQLite.
    }

    // --- Session 2: reopen the same store, query history --------------------
    {
        let store = Store::open(db_path).expect("open store session 2");
        let (mut rt, mut log) = make_runtime_on_store(store);
        load_pair(&mut rt, CONSUMER_LIST_ONLY);

        let count_str = dispatch_and_read(&mut rt);
        assert_eq!(
            count_str, "1",
            "visit recorded in session 1 must still be present in session 2; \
             got count={count_str}"
        );

        drain(&log);
        log.shutdown().expect("session-2 audit shuts down");
    }
}
