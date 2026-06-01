//! Black-box behavioral tests for the bundled `history` first-party plugin.
//!
//! All assertions drive the history plugin through a tiny consumer plugin
//! that calls `capabilities.invoke("ui:history_provider", fn, arg)` — the
//! pure Lua→Lua path (no Rust host method for capability invocation needed).
//! The consumer pattern mirrors the `bookmarks_behavior.rs` form.
//!
//! Data model (chronological visit log + URL-level title cache):
//!   URL records  — key `u:<url>`  `{ url, title, first_seen_ms, last_seen_ms, total_count }`
//!   Visit events — key `e:<padded_ms>` `{ url, time_ms }`  (append-only)
//!
//! The consumer plugin returns results from `capabilities.invoke` as a
//! `net:intercept_request` hook payload so the Rust test can read them back
//! via `dispatch_filter_chain` — the same observable side-channel used
//! throughout the test suite.
//!
//! Consumer plugins pass `time` (wall-clock ms) to `record_visit` — the plugin
//! requires this field.  Title is set separately via `update_title`.
//!
//! Storage introspection (key enumeration) is done through a helper consumer
//! that calls `storage.list_keys()` via the `storage:persistent` permission and
//! returns the result as a pipe-delimited string.
//!
//! Tests:
//!   1. `record_visit_creates_url_record_and_event`
//!   2. `record_visit_twice_creates_two_events_one_url_record`
//!   3. `update_title_propagates_to_all_historical_visits`
//!   4. `query_history_recent_returns_separate_events_per_visit`
//!   5. `query_history_relevance_dedups_by_url_and_ranks_by_count`
//!   6. `query_history_filter_substring_case_insensitive`
//!   7. `query_history_limit_overrides_default`
//!   8. `query_history_unknown_sort_falls_back_to_relevance`
//!   9. `update_title_returns_false_for_never_visited_url`
//!   10. `query_history_sort_relevance_orders_by_visit_count`
//!   11. `query_returns_history_matches`       (B3 urlbar)
//!   12. `query_merges_history_and_collected_bookmarks` (B3 urlbar)
//!   13. `query_degrades_with_zero_subscribers`
//!   14. `query_empty_text_returns_empty`
//!   15. `history_survives_reload`

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

/// Consumer: record one visit, then probe via `query_history` to verify
/// one URL record and one event exist.  Returns `"url_records=N,event_records=M"`
/// where N = count from `sort=relevance`, M = count from `sort=recent`.
const CONSUMER_LIST_KEYS_AFTER_VISIT: &str = r#"
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
      { url = "https://example.com", time = 1700000001000 })
    -- URL record count via sort=relevance (deduped by URL).
    local rel = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance", limit = 1000 })
    local url_n = (rel ~= nil) and #rel or 0
    -- Event count via sort=recent (one row per visit event).
    local rec = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "recent", limit = 1000 })
    local ev_n = (rec ~= nil) and #rec or 0
    return {
      action  = "modify",
      payload = "url_records=" .. tostring(url_n)
             .. ",event_records=" .. tostring(ev_n),
    }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: record two visits to the same URL (different times); verify via
/// `query_history` that there is 1 URL record, 2 events, and `total_count`=2.
const CONSUMER_TWO_VISITS_SAME_URL: &str = r#"
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
      { url = "https://same.test", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://same.test", time = 1700000002000 })
    -- URL record count (deduped) via sort=relevance.
    local rel = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance", limit = 1000 })
    local url_n = (rel ~= nil) and #rel or 0
    -- Event count (one row per visit) via sort=recent.
    local rec = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "recent", limit = 1000 })
    local ev_n = (rec ~= nil) and #rec or 0
    -- total_count from the URL record.
    local tc = 0
    if rel ~= nil and rel[1] ~= nil then
      tc = rel[1].total_count or 0
    end
    return {
      action  = "modify",
      payload = "url_records=" .. tostring(url_n)
             .. ",event_records=" .. tostring(ev_n)
             .. ",total_count=" .. tostring(tc),
    }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: record two visits to URL X, set title via `update_title`, then
/// `query(sort=recent)` and return `"titles=T1,T2"` (both visit rows' titles).
const CONSUMER_UPDATE_TITLE_PROPAGATES: &str = r#"
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
      { url = "https://title-test.com", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://title-test.com", time = 1700000002000 })
    capabilities.invoke("ui:history_provider", "update_title",
      { url = "https://title-test.com", title = "Page" })
    local results = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "recent" })
    -- Both rows should have title="Page".
    local t1 = (results ~= nil and results[1] ~= nil) and (results[1].title or "") or ""
    local t2 = (results ~= nil and results[2] ~= nil) and (results[2].title or "") or ""
    return { action = "modify", payload = "titles=" .. t1 .. "," .. t2 }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: record visits A, B, A at distinct timestamps; return
/// "count=N,urls=u1,u2,u3" from sort=recent.
const CONSUMER_RECENT_SEPARATE_ROWS: &str = r#"
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
      { url = "https://a.test", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://b.test", time = 1700000002000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.test", time = 1700000003000 })
    local results = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "recent" })
    local n = (results ~= nil) and #results or 0
    local parts = { "count=" .. tostring(n) }
    if results ~= nil then
      for _, r in ipairs(results) do
        parts[#parts + 1] = (r.url or "")
      end
    end
    return { action = "modify", payload = table.concat(parts, ",") }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: record A×3, B×1; query sort=relevance; return first url and count.
const CONSUMER_RELEVANCE_DEDUP: &str = r#"
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
      { url = "https://a.test", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.test", time = 1700000002000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.test", time = 1700000003000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://b.test", time = 1700000004000 })
    local results = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance" })
    local n = (results ~= nil) and #results or 0
    local first = (results ~= nil and results[1] ~= nil) and (results[1].url or "") or ""
    return {
      action  = "modify",
      payload = "count=" .. tostring(n) .. ",first=" .. first,
    }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: seed mixed-case URLs, query with "github" filter.
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
      { url = "https://GitHub.com/user/repo", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://example.com", time = 1700000002000 })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://news.ycombinator.com", time = 1700000003000 })
    local results = capabilities.invoke("ui:history_provider", "query_history",
      { filter = "github" })
    local count = (results ~= nil) and #results or 0
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: try `update_title` on a never-visited URL; return the boolean result.
const CONSUMER_UPDATE_TITLE_NEVER_VISITED: &str = r#"
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
    local ok = capabilities.invoke("ui:history_provider", "update_title",
      { url = "https://never-visited.test", title = "Ghost" })
    return { action = "modify", payload = tostring(ok or false) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: `sort=bogus` falls back to relevance; returns `"bogus_first==rel_first"`.
const CONSUMER_SORT_BOGUS: &str = r#"
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
    for _ = 1, 5 do
      capabilities.invoke("ui:history_provider", "record_visit",
        { url = "https://bogus-a.test", time = 1700000001000 })
    end
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://bogus-b.test", time = 1700000010000 })
    local r1 = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "bogus" })
    local r2 = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance" })
    local f1 = (r1 ~= nil and r1[1] ~= nil) and (r1[1].url or "") or ""
    local f2 = (r2 ~= nil and r2[1] ~= nil) and (r2[1].url or "") or ""
    return { action = "modify", payload = f1 .. "==" .. f2 }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: seeds visits and queries with different sort/limit variants.
const CONSUMER_SORT_RELEVANCE: &str = r#"
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
    -- A: 5 visits (older timestamps)
    for i = 1, 5 do
      capabilities.invoke("ui:history_provider", "record_visit",
        { url = "https://a-sort.test", time = 1700000000000 + i })
    end
    -- C: 2 visits (mid timestamps)
    for i = 1, 2 do
      capabilities.invoke("ui:history_provider", "record_visit",
        { url = "https://c-sort.test", time = 1700000000010 + i })
    end
    -- B: 1 visit — most recent
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://b-sort.test", time = 1700000000020 })
    local r = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance" })
    local first = (r ~= nil and r[1] ~= nil) and (r[1].url or "") or ""
    return { action = "modify", payload = first }
  end,
}
function M.setup() end
return M
"#;

const CONSUMER_SORT_RECENT: &str = r#"
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
    -- A: 5 visits (older timestamps)
    for i = 1, 5 do
      capabilities.invoke("ui:history_provider", "record_visit",
        { url = "https://a-sort.test", time = 1700000000000 + i })
    end
    -- C: 2 visits (mid timestamps)
    for i = 1, 2 do
      capabilities.invoke("ui:history_provider", "record_visit",
        { url = "https://c-sort.test", time = 1700000000010 + i })
    end
    -- B: 1 visit — most recent
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://b-sort.test", time = 1700000000020 })
    local r = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "recent" })
    local first = (r ~= nil and r[1] ~= nil) and (r[1].url or "") or ""
    return { action = "modify", payload = first }
  end,
}
function M.setup() end
return M
"#;

/// Seed consumer: records one visit in the hook.
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
      { url = "https://persisted.example.com", time = 1700000001000 })
    return { action = "allow" }
  end,
}
function M.setup() end
return M
"#;

/// List-only consumer: queries all history (relevance) and returns count.
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
    local results = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance" })
    local count = 0
    if results ~= nil then count = #results end
    return { action = "modify", payload = tostring(count) }
  end,
}
function M.setup() end
return M
"#;

/// Query-only consumer: queries with default limit (no payload).
const CONSUMER_QUERY_DEFAULT_LIMIT: &str = r#"
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
    local r = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance" })
    local n = (r ~= nil) and #r or 0
    return { action = "modify", payload = tostring(n) }
  end,
}
function M.setup() end
return M
"#;

/// Query-only consumer: queries with limit=5.
const CONSUMER_QUERY_LIMIT_FIVE: &str = r#"
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
    local r = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance", limit = 5 })
    local n = (r ~= nil) and #r or 0
    return { action = "modify", payload = tostring(n) }
  end,
}
function M.setup() end
return M
"#;

/// Query-only consumer: queries with limit=100.
const CONSUMER_QUERY_LIMIT_HUNDRED: &str = r#"
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
    local r = capabilities.invoke("ui:history_provider", "query_history",
      { sort = "relevance", limit = 100 })
    local n = (r ~= nil) and #r or 0
    return { action = "modify", payload = tostring(n) }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// B3 urlbar consumers
// ---------------------------------------------------------------------------

/// Consumer: seeds visits, calls query("alpha"), reports count + source0.
const CONSUMER_QUERY_HISTORY_ONLY: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider", "ui:urlbar_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://alpha.test", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "update_title",
      { url = "https://alpha.test", title = "Alpha" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://zzz.test", time = 1700000002000 })
    local results = capabilities.invoke("ui:urlbar_provider", "query", "alpha")
    local n = 0
    local src0 = "none"
    if type(results) == "table" then
      n = #results
      if n > 0 and type(results[1]) == "table" then
        src0 = tostring(results[1].source or "none")
      end
    end
    return { action = "modify", payload = "count=" .. tostring(n) .. ",source0=" .. src0 }
  end,
}
function M.setup() end
return M
"#;

const CONSUMER_QUERY_MERGED: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider", "ui:urlbar_provider", "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://foo-history.example", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "update_title",
      { url = "https://foo-history.example", title = "Foo History" })
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://foo-bookmark.example", title = "Foo Bookmark" })
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://nomatch.example", time = 1700000002000 })
    local results = capabilities.invoke("ui:urlbar_provider", "query", "foo")
    local n = 0
    local has_history  = false
    local has_bookmark = false
    if type(results) == "table" then
      n = #results
      for _, rec in ipairs(results) do
        if type(rec) == "table" then
          if rec.source == "history"  then has_history  = true end
          if rec.source == "bookmark" then has_bookmark = true end
        end
      end
    end
    local sources = ""
    if has_history  then sources = sources .. "history,"  end
    if has_bookmark then sources = sources .. "bookmark," end
    if #sources > 0 then sources = sources:sub(1, #sources - 1) end
    local capped = tostring(n <= 10)
    return {
      action  = "modify",
      payload = "count=" .. tostring(n)
             .. ",sources=" .. sources
             .. ",capped=" .. capped,
    }
  end,
}
function M.setup() end
return M
"#;

const CONSUMER_QUERY_ZERO_SUBSCRIBERS: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider", "ui:urlbar_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://alpha.test", time = 1700000001000 })
    capabilities.invoke("ui:history_provider", "update_title",
      { url = "https://alpha.test", title = "Alpha" })
    local results = capabilities.invoke("ui:urlbar_provider", "query", "alpha")
    local n = 0
    local src0 = "none"
    if type(results) == "table" then
      n = #results
      if n > 0 and type(results[1]) == "table" then
        src0 = tostring(results[1].source or "none")
      end
    end
    return { action = "modify", payload = "count=" .. tostring(n) .. ",source0=" .. src0 }
  end,
}
function M.setup() end
return M
"#;

const CONSUMER_QUERY_EMPTY_TEXT: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "hist-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "ui:history_provider", "ui:urlbar_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:history_provider", "record_visit",
      { url = "https://a.example", time = 1700000001000 })
    local results = capabilities.invoke("ui:urlbar_provider", "query", "")
    local n = 0
    if type(results) == "table" then n = #results end
    return { action = "modify", payload = "count=" .. tostring(n) }
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

/// Seed N distinct visits via `invoke_capability` directly (no hook budget consumed).
fn seed_n_visits(rt: &Runtime, n: usize) {
    use std::collections::BTreeMap;

    for i in 0..n {
        let mut m = BTreeMap::new();
        m.insert(
            "url".to_owned(),
            HostValue::Str(format!("https://limit-test.test/page-{i:03}")),
        );
        // Distinct timestamps so event keys don't collide.
        // Cast through u32 to avoid the usize→f64 precision-loss lint; n is
        // bounded in test call-sites to values that comfortably fit u32.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "n bounded to small test values"
        )]
        let time_offset = i as u32;
        m.insert(
            "time".to_owned(),
            HostValue::Number(1_700_000_000_000.0 + f64::from(time_offset)),
        );
        rt.invoke_capability("ui:history_provider", "record_visit", &HostValue::Map(m))
            .expect("record_visit must succeed");
    }
}

// ---------------------------------------------------------------------------
// Tests — new chronological-event model
// ---------------------------------------------------------------------------

/// Driving `record_visit` once creates one URL record and exactly one event.
///
/// Verified via `query_history`: `sort=relevance` returns 1 (deduped URL
/// records), `sort=recent` returns 1 (one event row).
#[test]
fn record_visit_creates_url_record_and_event() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_LIST_KEYS_AFTER_VISIT);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "url_records=1,event_records=1",
        "one visit must create exactly 1 URL record and 1 event; got {payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// Two `record_visit` calls to the same URL produce exactly 2 event keys and 1
/// URL record; `total_count` on the URL record is 2; `first_seen_ms` and
/// `last_seen_ms` reflect the two timestamps.
#[test]
fn record_visit_twice_creates_two_events_one_url_record() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_TWO_VISITS_SAME_URL);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "url_records=1,event_records=2,total_count=2",
        "two visits to the same URL must produce 1 URL record + 2 events with total_count=2; \
         got {payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `update_title` sets the title on the URL record; because title is URL-level,
/// both historical visit rows returned by `query_history(sort=recent)` carry
/// the updated title.
#[test]
fn update_title_propagates_to_all_historical_visits() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_UPDATE_TITLE_PROPAGATES);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "titles=Page,Page",
        "both historical visit rows must carry the title set by update_title; \
         got {payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query_history(sort=recent)` returns each visit as a separate row — visiting
/// URL A, then B, then A again yields 3 rows (A appears twice) in
/// time-descending order: A(t3), B(t2), A(t1).
#[test]
fn query_history_recent_returns_separate_events_per_visit() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_RECENT_SEPARATE_ROWS);

    let payload = dispatch_and_read(&mut rt);
    // Expected: count=3, then urls in reverse-time order: a, b, a.
    assert_eq!(
        payload, "count=3,https://a.test,https://b.test,https://a.test",
        "sort=recent must return 3 separate event rows in time-desc order; got {payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query_history(sort=relevance)` deduplicates by URL and ranks by
/// `total_count` descending; A×3 must precede B×1.
#[test]
fn query_history_relevance_dedups_by_url_and_ranks_by_count() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_RELEVANCE_DEDUP);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=2,first=https://a.test",
        "sort=relevance must dedup to 2 rows, A first (3 visits vs 1); got {payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query_history` with a filter applies a case-insensitive ASCII substring
/// match on url+title.  Seeding "GitHub.com" and filtering "github" must match.
#[test]
fn query_history_filter_substring_case_insensitive() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_FILTER);

    let count_str = dispatch_and_read(&mut rt);
    assert_eq!(
        count_str, "1",
        "filter='github' must match 'GitHub.com' case-insensitively and return 1 entry; \
         got {count_str}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `limit` param overrides the default 20-result cap.
///
/// Seeding is done via Rust-side `invoke_capability` (not inside the consumer
/// hook) to avoid exhausting the inter-plugin budget with many calls.
#[test]
fn query_history_limit_overrides_default() {
    // Default limit (20): 30 seeded → 20 returned.
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_QUERY_DEFAULT_LIMIT);
        seed_n_visits(&rt, 30);
        let count = dispatch_and_read(&mut rt);
        assert_eq!(count, "20", "default limit must cap at 20; got {count:?}");
        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }

    // Explicit limit=5: 30 seeded → 5 returned.
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_QUERY_LIMIT_FIVE);
        seed_n_visits(&rt, 30);
        let count = dispatch_and_read(&mut rt);
        assert_eq!(count, "5", "limit=5 must cap at 5; got {count:?}");
        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }

    // limit=100 with only 30 seeded → all 30 returned.
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_QUERY_LIMIT_HUNDRED);
        seed_n_visits(&rt, 30);
        let count = dispatch_and_read(&mut rt);
        assert_eq!(
            count, "30",
            "limit=100 with 30 entries must return all 30; got {count:?}"
        );
        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }
}

/// `sort="bogus"` falls back silently to `"relevance"` (closed enum /
/// default-deny idiom).
#[test]
fn query_history_unknown_sort_falls_back_to_relevance() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_SORT_BOGUS);

    let payload = dispatch_and_read(&mut rt);
    // Both sort="bogus" and sort="relevance" must produce the same first URL.
    assert_eq!(
        payload, "https://bogus-a.test==https://bogus-a.test",
        "sort='bogus' must fall back to relevance (same as sort='relevance'); \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `update_title` on a URL that was never visited must return false and must NOT
/// create a phantom URL record (i.e. `query_history(filter=…)` returns nothing).
#[test]
fn update_title_returns_false_for_never_visited_url() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_UPDATE_TITLE_NEVER_VISITED);

    let result = dispatch_and_read(&mut rt);
    assert_eq!(
        result, "false",
        "update_title on a never-visited URL must return false; got {result:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `sort="relevance"` and `sort="recent"` produce different orderings when
/// `total_count` and recency diverge.
#[test]
fn query_history_sort_relevance_orders_by_visit_count() {
    // Relevance: A first (5 visits).
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_SORT_RELEVANCE);
        let first = dispatch_and_read(&mut rt);
        assert_eq!(
            first, "https://a-sort.test",
            "sort=relevance: A (5 visits) must rank first; got {first:?}"
        );
        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }

    // Recent: B first (highest timestamp).
    {
        let (mut rt, mut log) = make_runtime();
        load_pair(&mut rt, CONSUMER_SORT_RECENT);
        let first = dispatch_and_read(&mut rt);
        assert_eq!(
            first, "https://b-sort.test",
            "sort=recent: B (most recent visit) must rank first; got {first:?}"
        );
        drain(&log);
        log.shutdown().expect("audit log shuts down cleanly");
    }
}

// ---------------------------------------------------------------------------
// B3 tests — history's `query` collects + merges
// ---------------------------------------------------------------------------

/// The bundled bookmarks plugin source.
const BOOKMARKS_SRC: &str = include_str!("../../../plugins/bookmarks/init.lua");

/// `query(text)` returns history matches tagged `source="history"`.
#[test]
fn query_returns_history_matches() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    rt.load(HISTORY_SRC, identity(), &policy)
        .expect("history plugin loads");
    rt.load(CONSUMER_QUERY_HISTORY_ONLY, identity(), &policy)
        .expect("consumer loads");

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=1,source0=history",
        "query must return only the matching history entry tagged source=history; \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query(text)` with both history and bookmarks loaded returns results from
/// both sources; all results are capped at 10; both `source` tags are present.
#[test]
fn query_merges_history_and_collected_bookmarks() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    rt.load(HISTORY_SRC, identity(), &policy)
        .expect("history plugin loads");
    rt.load(BOOKMARKS_SRC, identity(), &policy)
        .expect("bookmarks plugin loads");
    rt.load(CONSUMER_QUERY_MERGED, identity(), &policy)
        .expect("consumer loads");

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=2,sources=history,bookmark,capped=true",
        "query must merge history and bookmark contributions; got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query(text)` with only history loaded returns history-only results without
/// error.
#[test]
fn query_degrades_with_zero_subscribers() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    rt.load(HISTORY_SRC, identity(), &policy)
        .expect("history plugin loads");
    rt.load(CONSUMER_QUERY_ZERO_SUBSCRIBERS, identity(), &policy)
        .expect("consumer loads");

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=1,source0=history",
        "query with no subscribers must return history-only results; \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// `query("")` (empty text) returns an empty list immediately.
#[test]
fn query_empty_text_returns_empty() {
    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    rt.load(HISTORY_SRC, identity(), &policy)
        .expect("history plugin loads");
    rt.load(CONSUMER_QUERY_EMPTY_TEXT, identity(), &policy)
        .expect("consumer loads");

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=0",
        "query with empty text must return an empty list; got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// Visits written in one runtime session survive when the same on-disk store is
/// reopened in a new runtime session.
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
