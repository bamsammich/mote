//! Integration tests for `mote.json.encode` / `mote.json.decode` — the
//! `serde_json`-backed JSON utility exposed to every Lua plugin as a host API.
//!
//! Each test drives the full runtime stack (real registry, in-memory store,
//! audit log, Lua states) via the same side-channel idiom used by
//! `storage_list_keys.rs` and `events_collect.rs`: a plugin declares a
//! `net:intercept_request` filter-chain hook that exercises `mote.json.*` and
//! returns a string payload, observed through `ChainResolution::Allowed { payload }`.
//!
//! `mote.json` is a pure data utility — no I/O, no side effect, no information
//! disclosure. No permission gate is required or exercised.

use std::thread;
use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{ChainResolution, GrantAsRequested, HostValue, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

// ---------------------------------------------------------------------------
// Shared helpers (mirror events_collect.rs / storage_list_keys.rs)
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

/// Helper: load a single plugin source and dispatch the filter chain with a
/// nil payload; assert `Allowed` and return the string payload.
fn run_plugin(src: &str) -> String {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;
    runtime
        .load(src, identity(), &policy)
        .expect("plugin loads");
    let outcome = runtime.dispatch_filter_chain("net:intercept_request", HostValue::Nil);
    drain(&log);
    log.shutdown().expect("audit log shuts down cleanly");
    match outcome.resolution {
        ChainResolution::Allowed { payload } => match payload {
            HostValue::Str(s) => s,
            other => panic!("expected Str payload, got {other:?}"),
        },
        other @ ChainResolution::Blocked { .. } => panic!("expected Allowed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test plugin sources
// ---------------------------------------------------------------------------

/// Scalar round-trips: encode then decode for booleans, integers, floats, strings.
/// Reports "ok" if all round-trips produce the original Lua values.
const SCALAR_ROUNDTRIP_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-scalar-rt",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local function rt(v)
      return mote.json.decode(mote.json.encode(v))
    end
    -- booleans
    if rt(true) ~= true then return { action = "modify", payload = "fail:bool_true" } end
    if rt(false) ~= false then return { action = "modify", payload = "fail:bool_false" } end
    -- integer (Lua integers are lossless in JSON)
    if rt(42) ~= 42 then return { action = "modify", payload = "fail:int" } end
    if rt(0) ~= 0 then return { action = "modify", payload = "fail:int_zero" } end
    if rt(-7) ~= -7 then return { action = "modify", payload = "fail:int_neg" } end
    -- float
    if rt(3.14) ~= 3.14 then return { action = "modify", payload = "fail:float" } end
    -- string
    if rt("hello") ~= "hello" then return { action = "modify", payload = "fail:string" } end
    if rt("") ~= "" then return { action = "modify", payload = "fail:empty_string" } end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// Array round-trip: {1, 2, 3} → JSON array → Lua sequence.
const ARRAY_ROUNDTRIP_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-array-rt",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local arr = { 1, 2, 3 }
    local encoded = mote.json.encode(arr)
    -- must be a non-nil string
    if type(encoded) ~= "string" then
      return { action = "modify", payload = "fail:encode_not_string" }
    end
    -- must look like a JSON array
    if encoded ~= "[1,2,3]" then
      return { action = "modify", payload = "fail:wrong_json:" .. encoded }
    end
    local decoded = mote.json.decode(encoded)
    if type(decoded) ~= "table" then
      return { action = "modify", payload = "fail:decoded_not_table" }
    end
    if #decoded ~= 3 then
      return { action = "modify", payload = "fail:length:" .. tostring(#decoded) }
    end
    for i = 1, 3 do
      if decoded[i] ~= i then
        return { action = "modify", payload = "fail:elem:" .. tostring(i) }
      end
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// Object round-trip: {name="x", count=3} → JSON object → Lua map.
const OBJECT_ROUNDTRIP_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-object-rt",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local obj = { name = "x", count = 3 }
    local decoded = mote.json.decode(mote.json.encode(obj))
    if type(decoded) ~= "table" then
      return { action = "modify", payload = "fail:not_table" }
    end
    if decoded.name ~= "x" then
      return { action = "modify", payload = "fail:name:" .. tostring(decoded.name) }
    end
    if decoded.count ~= 3 then
      return { action = "modify", payload = "fail:count:" .. tostring(decoded.count) }
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// Nested round-trip: array of objects { {a=1}, {a=2} } round-trips.
const NESTED_ROUNDTRIP_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-nested-rt",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local nested = { { a = 1 }, { a = 2 } }
    local decoded = mote.json.decode(mote.json.encode(nested))
    if type(decoded) ~= "table" then
      return { action = "modify", payload = "fail:not_table" }
    end
    if #decoded ~= 2 then
      return { action = "modify", payload = "fail:length:" .. tostring(#decoded) }
    end
    if type(decoded[1]) ~= "table" or decoded[1].a ~= 1 then
      return { action = "modify", payload = "fail:elem1" }
    end
    if type(decoded[2]) ~= "table" or decoded[2].a ~= 2 then
      return { action = "modify", payload = "fail:elem2" }
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// Encoding a Lua function returns nil (no error raised).
const ENCODE_FUNCTION_NIL_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-encode-fn",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local result = mote.json.encode(function() end)
    -- Must be nil, never an error
    if result ~= nil then
      return { action = "modify", payload = "fail:expected_nil_got:" .. tostring(result) }
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// Decoding malformed JSON returns nil (no error raised).
const DECODE_MALFORMED_NIL_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-decode-malformed",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local result = mote.json.decode("{not json")
    if result ~= nil then
      return { action = "modify", payload = "fail:expected_nil_got:" .. tostring(result) }
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// Decoding a non-string argument (integer 42) returns nil (no error raised).
const DECODE_NON_STRING_NIL_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-decode-nonstring",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    local result = mote.json.decode(42)
    if result ~= nil then
      return { action = "modify", payload = "fail:expected_nil_got:" .. tostring(result) }
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

/// `HostValue` has no `Bytes` variant: the "bytes policy" is N/A.
/// This test documents that nil (unrepresentable Lua types) encodes to the
/// JSON null string and decodes back to nil, matching the nil/null equivalence.
const NULL_NIL_ROUNDTRIP_SRC: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "json-null-nil",
  version = "1.0.0",
  permissions = { "net:intercept_request" },
  identity_scope = "global",
}
M.hooks = {
  ["net:intercept_request"] = function(_req)
    -- Encoding nil produces "null"
    local encoded = mote.json.encode(nil)
    if encoded ~= "null" then
      return { action = "modify", payload = "fail:encode_nil:" .. tostring(encoded) }
    end
    -- Decoding "null" produces nil
    local decoded = mote.json.decode("null")
    if decoded ~= nil then
      return { action = "modify", payload = "fail:decode_null:" .. tostring(decoded) }
    end
    return { action = "modify", payload = "ok" }
  end,
}
function M.setup() end
return M
"#;

// ---------------------------------------------------------------------------
// Tests — RED first, then implement
// ---------------------------------------------------------------------------

/// `mote.json.decode(mote.json.encode(v))` round-trips for booleans, integers,
/// floats, and strings.
#[test]
fn json_round_trips_scalars() {
    assert_eq!(run_plugin(SCALAR_ROUNDTRIP_SRC), "ok");
}

/// `{1, 2, 3}` encodes as `[1,2,3]` and decodes back to a 1-indexed Lua
/// sequence of the same length and values.
#[test]
fn json_round_trips_array() {
    assert_eq!(run_plugin(ARRAY_ROUNDTRIP_SRC), "ok");
}

/// `{name="x", count=3}` round-trips as a JSON object with the same key/value
/// pairs.
#[test]
fn json_round_trips_object() {
    assert_eq!(run_plugin(OBJECT_ROUNDTRIP_SRC), "ok");
}

/// `{ {a=1}, {a=2} }` (array of objects) round-trips faithfully.
#[test]
fn json_round_trips_nested() {
    assert_eq!(run_plugin(NESTED_ROUNDTRIP_SRC), "ok");
}

/// Encoding a Lua function returns `nil` — no Lua error is raised.
#[test]
fn json_encode_returns_nil_for_function() {
    assert_eq!(run_plugin(ENCODE_FUNCTION_NIL_SRC), "ok");
}

/// Decoding malformed JSON returns `nil` — no Lua error is raised.
#[test]
fn json_decode_returns_nil_for_malformed() {
    assert_eq!(run_plugin(DECODE_MALFORMED_NIL_SRC), "ok");
}

/// Decoding a non-string (integer 42) returns `nil` — no Lua error is raised.
#[test]
fn json_decode_returns_nil_for_non_string() {
    assert_eq!(run_plugin(DECODE_NON_STRING_NIL_SRC), "ok");
}

/// Documents the Bytes policy: `HostValue` has no `Bytes` variant, so the
/// bytes question is moot. This test verifies that `nil` (which maps to
/// `HostValue::Nil` / JSON `null`) encodes to `"null"` and `"null"` decodes
/// back to `nil`.
#[test]
fn json_encode_handles_bytes_per_documented_policy() {
    assert_eq!(run_plugin(NULL_NIL_ROUNDTRIP_SRC), "ok");
}
