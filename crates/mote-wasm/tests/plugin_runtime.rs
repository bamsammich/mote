//! Integration tests for the `mote-wasm` plugin runtime.
//!
//! All tests are black-box: they only use the public API surface.
//! Guest modules are written as inline WAT text (compiled to bytes by the
//! `wat` crate) so they are readable and self-contained.

use mote_wasm::{HostImports, HostState, PluginEngine, WasmError, WasmPlugin};

// ---------------------------------------------------------------------------
// Shared test state
// ---------------------------------------------------------------------------

/// Minimal state that records `host::log` calls and an arbitrary counter.
#[derive(Debug, Default)]
struct TestState {
    log_lines: Vec<String>,
    counter: i32,
}

impl HostState for TestState {
    fn on_log(&mut self, message: &str) {
        self.log_lines.push(message.to_owned());
    }
}

// ---------------------------------------------------------------------------
// Helper: build a PluginEngine + default HostImports for TestState
// ---------------------------------------------------------------------------

fn engine() -> PluginEngine {
    PluginEngine::new().expect("PluginEngine::new must succeed on this platform")
}

fn default_imports(engine: &PluginEngine) -> HostImports<TestState> {
    HostImports::new(engine.as_raw())
}

// ---------------------------------------------------------------------------
// Test 1 — load + instantiate and call an exported function returning a known
// value.
// ---------------------------------------------------------------------------

/// A tiny module that exports `answer() -> i32` returning the literal 42.
const ANSWER_WAT: &str = r#"
(module
  (import "host" "log" (func (param i32 i32)))
  (memory (export "memory") 1)
  (func (export "answer") (result i32)
    i32.const 42
  )
)
"#;

#[test]
fn load_and_call_returning_known_value() {
    let engine = engine();
    let bytes = wat::parse_str(ANSWER_WAT).expect("WAT must compile");

    let mut plugin = WasmPlugin::load(
        &engine,
        &bytes,
        default_imports(&engine),
        TestState::default(),
    )
    .expect("plugin must load");

    let result: i32 = plugin.call("answer", ()).expect("call must succeed");
    assert_eq!(result, 42, "guest answer() must return 42");
}

// ---------------------------------------------------------------------------
// Test 2 — guest calling a registered host import observes the host effect.
//
// The guest calls `host::add_to_counter(i32)` and then returns the counter
// value via `host::read_counter() -> i32`. We verify via state() after the
// call.
// ---------------------------------------------------------------------------

/// Module that calls `host::add_to_counter(7)` in its exported `run()`.
const HOST_EFFECT_WAT: &str = r#"
(module
  (import "host" "log" (func (param i32 i32)))
  (import "host" "add_to_counter" (func $add (param i32)))
  (memory (export "memory") 1)
  (func (export "run")
    i32.const 7
    call $add
  )
)
"#;

#[test]
fn guest_calling_registered_host_import_observes_effect() {
    let engine = engine();
    let bytes = wat::parse_str(HOST_EFFECT_WAT).expect("WAT must compile");

    let imports = default_imports(&engine)
        .register(
            "add_to_counter",
            |mut caller: wasmtime::Caller<'_, TestState>, v: i32| {
                caller.data_mut().counter += v;
            },
        )
        .expect("register must succeed");

    let mut plugin =
        WasmPlugin::load(&engine, &bytes, imports, TestState::default()).expect("plugin must load");

    plugin.call::<(), ()>("run", ()).expect("run must succeed");

    assert_eq!(
        plugin.state().counter,
        7,
        "host counter must reflect guest's call with argument 7"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — guest calling host::log routes the message through HostState.
// ---------------------------------------------------------------------------

/// Module that stores a string in memory and calls `host::log` with it.
///
/// The string "hello" is laid out at offset 0 in the data segment.
const LOG_WAT: &str = r#"
(module
  (import "host" "log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello")
  (func (export "emit_log")
    i32.const 0   ;; ptr
    i32.const 5   ;; len ("hello" = 5 bytes)
    call $log
  )
)
"#;

#[test]
fn guest_log_call_routes_through_host_state() {
    let engine = engine();
    let bytes = wat::parse_str(LOG_WAT).expect("WAT must compile");

    let mut plugin = WasmPlugin::load(
        &engine,
        &bytes,
        default_imports(&engine),
        TestState::default(),
    )
    .expect("plugin must load");

    plugin
        .call::<(), ()>("emit_log", ())
        .expect("emit_log must succeed");

    assert_eq!(
        plugin.state().log_lines,
        vec!["hello"],
        "host state must record the string the guest passed to host::log"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — calling a missing export returns MissingExport, not a panic.
// ---------------------------------------------------------------------------

#[test]
fn missing_export_returns_clear_error() {
    let engine = engine();
    let bytes = wat::parse_str(ANSWER_WAT).expect("WAT must compile");

    let mut plugin = WasmPlugin::load(
        &engine,
        &bytes,
        default_imports(&engine),
        TestState::default(),
    )
    .expect("plugin must load");

    let result = plugin.call::<(), i32>("does_not_exist", ());
    assert!(
        matches!(result, Err(WasmError::MissingExport { ref name }) if name == "does_not_exist"),
        "calling a missing export must return WasmError::MissingExport, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 5 — passing a malformed/invalid WASM byte slice returns InvalidModule.
// ---------------------------------------------------------------------------

#[test]
fn malformed_wasm_returns_invalid_module_error() {
    let engine = engine();
    let garbage = b"this is not wasm";

    let result = WasmPlugin::load(
        &engine,
        garbage,
        default_imports(&engine),
        TestState::default(),
    );

    assert!(
        matches!(result, Err(WasmError::InvalidModule(_))),
        "malformed bytes must return WasmError::InvalidModule, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 6 — a module missing an import that the host did not register fails at
// instantiation time with Instantiation, not InvalidModule.
// ---------------------------------------------------------------------------

/// Module that imports `host::not_provided`, which we never register.
const MISSING_IMPORT_WAT: &str = r#"
(module
  (import "host" "log" (func (param i32 i32)))
  (import "host" "not_provided" (func (param i32)))
  (memory (export "memory") 1)
  (func (export "run"))
)
"#;

#[test]
fn module_with_unregistered_import_fails_at_instantiation() {
    let engine = engine();
    let bytes = wat::parse_str(MISSING_IMPORT_WAT).expect("WAT must compile");

    // Deliberately do NOT register "not_provided".
    let result = WasmPlugin::load(
        &engine,
        &bytes,
        default_imports(&engine),
        TestState::default(),
    );

    assert!(
        matches!(result, Err(WasmError::Instantiation(_))),
        "a module importing an unregistered function must fail with \
         WasmError::Instantiation, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 7 — multiple calls accumulate state correctly.
// ---------------------------------------------------------------------------

/// Module that exports `increment()` calling `host::add_to_counter(1)`.
const INCREMENT_WAT: &str = r#"
(module
  (import "host" "log" (func (param i32 i32)))
  (import "host" "add_to_counter" (func $add (param i32)))
  (memory (export "memory") 1)
  (func (export "increment")
    i32.const 1
    call $add
  )
)
"#;

#[test]
fn multiple_calls_accumulate_state() {
    let engine = engine();
    let bytes = wat::parse_str(INCREMENT_WAT).expect("WAT must compile");

    let imports = default_imports(&engine)
        .register(
            "add_to_counter",
            |mut caller: wasmtime::Caller<'_, TestState>, v: i32| {
                caller.data_mut().counter += v;
            },
        )
        .expect("register must succeed");

    let mut plugin =
        WasmPlugin::load(&engine, &bytes, imports, TestState::default()).expect("plugin must load");

    for _ in 0..5 {
        plugin
            .call::<(), ()>("increment", ())
            .expect("increment must succeed");
    }

    assert_eq!(
        plugin.state().counter,
        5,
        "five increment calls must result in counter == 5"
    );
}
