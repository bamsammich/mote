//! Black-box tests for the bookmarks plugin's `urlbar:suggest` collector
//! contribution (Task B3, Commit 1).
//!
//! Each test loads the real bundled bookmarks plugin PLUS a tiny consumer
//! plugin that holds `events:emit` and calls
//! `mote.events.collect("urlbar:suggest", {text=...})`.  The consumer seeds
//! bookmarks via `capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
//! ...)` and then reads back the contribution list via the same
//! `net:intercept_request` side-channel used throughout the suite.
//!
//! Mirrors the harness pattern of `events_collect.rs` and
//! `bookmarks_behavior.rs`.

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
    thread::sleep(Duration::from_millis(40));
    let _ = log;
}

/// The bundled bookmarks plugin source.
const BOOKMARKS_SRC: &str = include_str!("../../../plugins/bookmarks/init.lua");

// ---------------------------------------------------------------------------
// Consumer plugin source strings
// ---------------------------------------------------------------------------

/// A consumer that:
/// 1. Seeds `<https://foo.example>` (title "Foo Site") and `<https://bar.example>`
///    (title "Bar Site") via `add_bookmark`.
/// 2. Calls `mote.events.collect("urlbar:suggest", {text="foo"})`.
/// 3. Returns "count=N,url=first-url" so the test can inspect the
///    contribution count and which bookmark was returned.
///
/// Expected: N=1 (only the "foo.example" bookmark matches "foo"), the
/// contribution is tagged `source="bookmark"`.
const CONSUMER_FOO_SEARCH: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "contrib-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:emit", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://foo.example", title = "Foo Site" })
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://bar.example", title = "Bar Site" })
    local contribs = mote.events.collect("urlbar:suggest", { text = "foo" })
    -- contribs is a Lua array; each element is the return value of one
    -- subscriber's handler (for bookmarks: a Lua array of suggestion records).
    local n = 0
    local first_url = "none"
    local first_source = "none"
    if type(contribs) == "table" and #contribs > 0 then
      local bm_list = contribs[1]
      if type(bm_list) == "table" then
        n = #bm_list
        if n > 0 then
          first_url    = tostring(bm_list[1].url    or "none")
          first_source = tostring(bm_list[1].source or "none")
        end
      end
    end
    return {
      action  = "modify",
      payload = "count=" .. tostring(n)
             .. ",url=" .. first_url
             .. ",source=" .. first_source,
    }
  end,
}
function M.setup() end
return M
"#;

/// Consumer that calls `collect` with `text=""` (empty string).
/// Bookmarks must return an empty contribution list for blank text.
const CONSUMER_EMPTY_TEXT: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "contrib-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:emit", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://foo.example", title = "Foo Site" })
    local contribs = mote.events.collect("urlbar:suggest", { text = "" })
    -- For empty text bookmarks must return {} (no contribution), but the
    -- collect itself still returns a table (possibly empty if the subscriber
    -- returned {}). Count the inner contribution list length.
    local n = 0
    if type(contribs) == "table" and #contribs > 0 then
      local bm_list = contribs[1]
      if type(bm_list) == "table" then
        n = #bm_list
      end
    end
    return { action = "modify", payload = "count=" .. tostring(n) }
  end,
}
function M.setup() end
return M
"#;

/// Consumer that seeds bookmarks then collects with text that matches nothing.
const CONSUMER_NO_MATCH: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "contrib-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:emit", "events:on" },
  consumes = { "ui:bookmarks_provider" },
  identity_scope = "per_identity",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://foo.example", title = "Foo Site" })
    capabilities.invoke("ui:bookmarks_provider", "add_bookmark",
      { url = "https://bar.example", title = "Bar Site" })
    local contribs = mote.events.collect("urlbar:suggest", { text = "zzznomatch" })
    local n = 0
    if type(contribs) == "table" and #contribs > 0 then
      local bm_list = contribs[1]
      if type(bm_list) == "table" then
        n = #bm_list
      end
    end
    return { action = "modify", payload = "count=" .. tostring(n) }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// Test helpers
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

/// B3-bm-1: bookmarks handler for `urlbar:suggest` contributes matching
/// bookmarks.  After seeding "foo.example" and "bar.example", collecting with
/// `text="foo"` must yield exactly one bookmark (`source="bookmark"`,
/// `url="https://foo.example"`).
#[test]
fn bookmarks_contribute_to_urlbar_suggest() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_FOO_SEARCH);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=1,url=https://foo.example,source=bookmark",
        "bookmarks must contribute exactly the matching record tagged source=bookmark; \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// B3-bm-2: collecting with empty text must yield zero contributions from
/// bookmarks (early-exit path in the handler).
#[test]
fn bookmarks_contribute_empty_when_text_blank() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_EMPTY_TEXT);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=0",
        "bookmarks must return an empty list when text is empty; got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// B3-bm-3: collecting with text that matches no bookmark yields an empty
/// contribution from bookmarks.
#[test]
fn bookmarks_contribute_when_no_matches() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_NO_MATCH);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "count=0",
        "bookmarks must return empty contribution when no bookmarks match the text; \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}
