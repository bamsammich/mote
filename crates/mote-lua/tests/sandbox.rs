//! Security-boundary tests for the sandboxed Lua state.
//!
//! Each dangerous global gets its own assertion so a regression names exactly
//! which escape hatch reopened. These run plugin-like chunks in a fresh
//! sandbox and assert the dangerous surface is `nil`/unavailable.

use mote_lua::new_sandbox;

/// Evaluates a boolean Lua expression in a fresh sandbox and returns it.
fn eval_bool(expr: &str) -> bool {
    let lua = new_sandbox().expect("sandbox builds");
    lua.load(expr).eval::<bool>().expect("expr evaluates")
}

#[test]
fn io_library_is_unavailable() {
    assert!(eval_bool("io == nil"), "`io` must be removed");
}

#[test]
fn os_library_is_unavailable() {
    assert!(eval_bool("os == nil"), "`os` must be removed");
}

#[test]
fn debug_library_is_unavailable() {
    assert!(eval_bool("debug == nil"), "`debug` must be removed");
}

#[test]
fn package_and_require_are_unavailable() {
    assert!(eval_bool("package == nil"), "`package` must be removed");
    assert!(eval_bool("require == nil"), "`require` must be removed");
}

#[test]
fn dynamic_code_loading_is_unavailable() {
    // Each its own assertion so a regression names the exact primitive.
    assert!(eval_bool("load == nil"), "`load` must be removed");
    assert!(
        eval_bool("loadstring == nil"),
        "`loadstring` must be removed"
    );
    assert!(eval_bool("loadfile == nil"), "`loadfile` must be removed");
    assert!(eval_bool("dofile == nil"), "`dofile` must be removed");
}

#[test]
fn ffi_library_is_unavailable() {
    // LuaJIT's `ffi` is a native-memory escape hatch; the safe constructor must
    // never load it.
    assert!(eval_bool("ffi == nil"), "`ffi` must be removed");
}

#[test]
fn collectgarbage_is_unavailable() {
    assert!(
        eval_bool("collectgarbage == nil"),
        "`collectgarbage` must be removed"
    );
}

#[test]
fn safe_libraries_are_present() {
    // Sanity check the boundary doesn't over-remove the legitimate surface.
    assert!(eval_bool("string ~= nil"), "`string` must be kept");
    assert!(eval_bool("table ~= nil"), "`table` must be kept");
    assert!(eval_bool("math ~= nil"), "`math` must be kept");
    assert!(eval_bool("coroutine ~= nil"), "`coroutine` must be kept");
    assert!(eval_bool("pcall ~= nil"), "`pcall` must be kept");
    assert!(eval_bool("type ~= nil"), "`type` must be kept");
    assert!(
        eval_bool("setmetatable ~= nil"),
        "`setmetatable` must be kept"
    );
}

#[test]
fn safe_computation_actually_works() {
    let lua = new_sandbox().expect("sandbox builds");
    let n: i64 = lua
        .load("return math.floor(string.len('mote') * 2.5)")
        .eval()
        .expect("safe computation runs");
    assert_eq!(n, 10);
}
