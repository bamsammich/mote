//! Black-box behavioral tests for the bundled `workspace-manager` first-party
//! plugin.
//!
//! All assertions drive the workspace-manager plugin through a tiny consumer
//! plugin that calls `capabilities.invoke("workspace:provider", fn, arg)` —
//! the pure Lua→Lua path (no Rust host method needed).
//!
//! The consumer uses the `net:intercept_request` hook side-channel (the same
//! observable path as `bookmarks_behavior.rs` and `history_behavior.rs`) to
//! return results that the Rust test can read via `dispatch_filter_chain`.
//!
//! Arg-shape note (lessons.md L2/L3): `list_workspaces` takes no argument
//! (pass `nil`); `switch_workspace` takes a Map `{id = <string>}` (matching
//! the `add_bookmark`-style pattern — the shell will produce a Map when it
//! calls this via Rust's `invoke_capability`).  An empty list return marshals
//! as `HostValue::Map({})` in the worst case (L3), so consumers must handle
//! both `nil` and the empty-map shape.
//!
//! Tests:
//!   1. `lists_builtin_workspaces`        — boot → list has ≥ 2 entries,
//!      every record has id+name+active, exactly one has active=true.
//!   2. `switch_persists_active`          — switch to a valid id → returns
//!      true; subsequent list shows that id as active.
//!   3. `switch_rejects_unknown_id`       — switch to unknown id → returns
//!      false; list still shows the previous active unchanged.
//!   4. `active_workspace_survives_reload`— switch to a non-default workspace,
//!      reload the runtime, list still shows that workspace active.
//!   5. `switch_emits_workspaces_on_change`— switch triggers the
//!      `workspaces:on_change` event; a consumer plugin observing that event
//!      records the new active id.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{ChainResolution, GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

// ---------------------------------------------------------------------------
// Shared helpers — mirror bookmarks_behavior.rs / history_behavior.rs
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

/// The bundled workspace-manager plugin source.
const WORKSPACE_MANAGER_SRC: &str = include_str!("../../../plugins/workspace-manager/init.lua");

// ---------------------------------------------------------------------------
// Consumer plugin strings
// ---------------------------------------------------------------------------

/// Consumer: calls `list_workspaces()` and returns a summary string:
/// `"count=N,active=<active_id>"`
/// so the test can assert count ≥ 2, one active id present, etc.
/// Also verifies that every entry has id, name, and active fields.
const CONSUMER_LIST: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ws-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "workspace:provider" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local ws = capabilities.invoke("workspace:provider", "list_workspaces", nil)
    local count = 0
    local active_id = "none"
    local active_count = 0
    local all_have_fields = true
    if type(ws) == "table" then
      count = #ws
      for _, entry in ipairs(ws) do
        if type(entry) ~= "table" then
          all_have_fields = false
        else
          if entry.id == nil or entry.name == nil or entry.active == nil then
            all_have_fields = false
          end
          if entry.active == true then
            active_id = tostring(entry.id)
            active_count = active_count + 1
          end
        end
      end
    end
    local payload = "count=" .. tostring(count)
                 .. ",active=" .. active_id
                 .. ",active_count=" .. tostring(active_count)
                 .. ",fields_ok=" .. tostring(all_have_fields)
    return { action = "modify", payload = payload }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: switches to `work` workspace, then lists; returns:
/// `"switch=<true/false>,active=<active_id>"`
const CONSUMER_SWITCH_VALID: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ws-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "workspace:provider" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local ok = capabilities.invoke("workspace:provider", "switch_workspace",
                                   { id = "work" })
    local ws = capabilities.invoke("workspace:provider", "list_workspaces", nil)
    local active_id = "none"
    if type(ws) == "table" then
      for _, entry in ipairs(ws) do
        if type(entry) == "table" and entry.active == true then
          active_id = tostring(entry.id)
        end
      end
    end
    return {
      action  = "modify",
      payload = "switch=" .. tostring(ok) .. ",active=" .. active_id,
    }
  end,
}
function M.setup() end
return M
"#;

/// Consumer: switches to `NONEXISTENT_ID` (invalid), then lists;
/// returns `"switch=<true/false>,active=<active_id>"`.
/// The active id should remain "default" (unchanged).
const CONSUMER_SWITCH_INVALID: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ws-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "workspace:provider" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local ok = capabilities.invoke("workspace:provider", "switch_workspace",
                                   { id = "NONEXISTENT_ID" })
    local ws = capabilities.invoke("workspace:provider", "list_workspaces", nil)
    local active_id = "none"
    if type(ws) == "table" then
      for _, entry in ipairs(ws) do
        if type(entry) == "table" and entry.active == true then
          active_id = tostring(entry.id)
        end
      end
    end
    return {
      action  = "modify",
      payload = "switch=" .. tostring(ok) .. ",active=" .. active_id,
    }
  end,
}
function M.setup() end
return M
"#;

/// Seed consumer: switches to `work` in the hook; used in session 1 of the
/// reload test.
const CONSUMER_SWITCH_SEED: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ws-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "workspace:provider" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    capabilities.invoke("workspace:provider", "switch_workspace", { id = "work" })
    return { action = "allow" }
  end,
}
function M.setup() end
return M
"#;

/// List-only consumer used in session 2 of the reload test.  Reports the
/// active workspace id.
const CONSUMER_LIST_ACTIVE: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ws-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "workspace:provider" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(req)
    local ws = capabilities.invoke("workspace:provider", "list_workspaces", nil)
    local active_id = "none"
    if type(ws) == "table" then
      for _, entry in ipairs(ws) do
        if type(entry) == "table" and entry.active == true then
          active_id = tostring(entry.id)
        end
      end
    end
    return { action = "modify", payload = active_id }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load the workspace-manager provider and the given consumer into a runtime.
fn load_pair(rt: &mut Runtime, consumer_src: &str) {
    let policy = GrantAsRequested;
    rt.load(WORKSPACE_MANAGER_SRC, identity(), &policy)
        .expect("workspace-manager plugin must load cleanly");
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

/// Boot the workspace-manager and list workspaces.  The built-in set must
/// contain at least 2 entries; every entry must have `id`, `name`, and
/// `active` fields; exactly one entry must have `active = true`.
#[test]
fn lists_builtin_workspaces() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_LIST);

    let payload = dispatch_and_read(&mut rt);
    // Extract count from "count=N,active=<id>,active_count=M,fields_ok=<bool>"
    let pairs: std::collections::HashMap<&str, &str> = payload
        .split(',')
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            Some((it.next()?, it.next()?))
        })
        .collect();

    let count: usize = pairs.get("count").and_then(|s| s.parse().ok()).unwrap_or(0);
    let active_count: usize = pairs
        .get("active_count")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let fields_ok = pairs.get("fields_ok").copied().unwrap_or("false");
    let active_id = pairs.get("active").copied().unwrap_or("none");

    assert!(
        count >= 2,
        "built-in workspace set must have at least 2 entries; got count={count} (payload={payload:?})"
    );
    assert_eq!(
        active_count, 1,
        "exactly one workspace must have active=true; got active_count={active_count} (payload={payload:?})"
    );
    assert_eq!(
        fields_ok, "true",
        "every workspace entry must have id, name, and active fields (payload={payload:?})"
    );
    assert_ne!(
        active_id, "none",
        "active workspace id must not be 'none' (payload={payload:?})"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// Switching to a valid workspace id returns true and the subsequent
/// `list_workspaces` call shows that id as the active workspace.
#[test]
fn switch_persists_active() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_SWITCH_VALID);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "switch=true,active=work",
        "switch to 'work' must return true and list must show active='work'; \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// Switching to an unknown workspace id returns false and the active workspace
/// is left unchanged (remains "default").
#[test]
fn switch_rejects_unknown_id() {
    let (mut rt, mut log) = make_runtime();
    load_pair(&mut rt, CONSUMER_SWITCH_INVALID);

    let payload = dispatch_and_read(&mut rt);
    assert_eq!(
        payload, "switch=false,active=default",
        "switch to unknown id must return false and active must remain 'default'; \
         got payload={payload:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

/// The active workspace persists across a full runtime reload against the same
/// on-disk store.  Switching to `work` in session 1 must still be visible in
/// session 2's `list_workspaces`.
#[test]
fn active_workspace_survives_reload() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path: &Path = &dir.path().join("test-workspaces.db");

    // --- Session 1: switch to "work" via the filter-chain hook ---------------
    {
        let store = Store::open(db_path).expect("open store session 1");
        let (mut rt, mut log) = make_runtime_on_store(store);
        let policy = GrantAsRequested;
        rt.load(WORKSPACE_MANAGER_SRC, identity(), &policy)
            .expect("workspace-manager loads in session 1");
        rt.load(CONSUMER_SWITCH_SEED, identity(), &policy)
            .expect("seed consumer loads in session 1");

        let outcome = rt.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
        assert!(
            matches!(outcome.resolution, ChainResolution::Allowed { .. }),
            "seed hook must succeed"
        );

        drain(&log);
        log.shutdown().expect("session-1 audit shuts down");
        // `rt` (and thus the store) is dropped here.
    }

    // --- Session 2: reopen the same store, assert active is still "work" -----
    {
        let store = Store::open(db_path).expect("open store session 2");
        let (mut rt, mut log) = make_runtime_on_store(store);
        load_pair(&mut rt, CONSUMER_LIST_ACTIVE);

        let active = dispatch_and_read(&mut rt);
        assert_eq!(
            active, "work",
            "active workspace switched to 'work' in session 1 must survive reload; \
             got active={active:?}"
        );

        drain(&log);
        log.shutdown().expect("session-2 audit shuts down");
    }
}

/// Switching workspace emits the `workspaces:on_change` event.  A separate
/// observer plugin that has `events:on` listens for the event and records
/// the new active id into a shared Mutex<String>.  After the switch, the
/// recorded value must equal the switched-to id.
///
/// Implementation note: the observer plugin writes the event payload into the
/// `net:intercept_request` hook return so we can read it back via the standard
/// `dispatch_and_read` path.  The consumer plugin:
///   1. Registers a `workspaces:on_change` event handler that stores the
///      active id in a module-level variable.
///   2. Calls `switch_workspace` from the `net:intercept_request` hook.
///   3. Returns the stored active id so the test can read it.
///
/// This design avoids any shared Rust state — the observer logic runs entirely
/// in Lua, within the same runtime tick.
#[test]
fn switch_emits_workspaces_on_change() {
    // Consumer + observer in ONE plugin: the events table reacts to
    // workspaces:on_change and stashes the id; the hook reads the stash after
    // calling switch_workspace.
    const CONSUMER_OBSERVE: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ws-consumer",
  version = "1.0.0",
  permissions = { "net:intercept_request", "events:on" },
  consumes = { "workspace:provider" },
  identity_scope = "global",
}

-- Stash for the on_change payload (module-level so the handler can write it
-- and the hook can read it).
local last_change_id = "not_received"

M.events = {
  ["workspaces:on_change"] = function(payload)
    if type(payload) == "table" and payload.active ~= nil then
      last_change_id = tostring(payload.active)
    else
      last_change_id = "bad_payload"
    end
  end,
}

M.hooks = {
  ["net:intercept_request"] = function(req)
    -- Trigger the switch; the on_change handler fires synchronously within
    -- the same Lua→Rust→Lua→events dispatch cycle.
    capabilities.invoke("workspace:provider", "switch_workspace", { id = "work" })
    return { action = "modify", payload = last_change_id }
  end,
}
function M.setup() end
return M
"#;

    let (mut rt, mut log) = make_runtime();
    let policy = GrantAsRequested;
    rt.load(WORKSPACE_MANAGER_SRC, identity(), &policy)
        .expect("workspace-manager loads");
    rt.load(CONSUMER_OBSERVE, identity(), &policy)
        .expect("observer consumer loads");

    let received_id = dispatch_and_read(&mut rt);
    assert_eq!(
        received_id, "work",
        "workspaces:on_change must be emitted with active='work' after switch; \
         got received_id={received_id:?}"
    );

    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
}

// ---------------------------------------------------------------------------
// Arc<Mutex> helper used only if needed — kept as dead_code to show the
// alternative pattern from the brief.  The synchronous Lua stash approach
// used above is simpler and equally correct since emit is synchronous within
// the runtime's single-threaded Lua execution model.
// ---------------------------------------------------------------------------
#[allow(dead_code)]
fn _unused_mutex_pattern() -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(String::new()))
}
