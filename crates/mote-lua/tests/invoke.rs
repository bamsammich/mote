//! Deadline-enforced Lua hook invocation tests (DESIGN §Runtime guarantees;
//! risks-and-inconsistencies.md D1).
//!
//! These exercise [`mote_lua::call_hook_with_deadline`] against a real
//! sandboxed `LuaJIT` state. The headline test is the D1 proof: a runaway
//! handler (`while true do end`) must be interrupted at ~the deadline and
//! surface as [`mote_lua::HookInvokeError::Timeout`], never hang.

use std::time::{Duration, Instant};

use mlua::Value;
use mote_lua::{
    HookInvokeError, HookTable, call_function_with_deadline, call_hook_with_deadline, new_sandbox,
};

/// Loads a module body and returns the named declaration table (`hooks` /
/// `events`).
fn load_module(source: &str) -> (mlua::Lua, mlua::Table) {
    let lua = new_sandbox().expect("sandbox");
    let module: mlua::Table = lua.load(source).eval().expect("eval module");
    (lua, module)
}

#[test]
fn returning_handler_yields_its_value() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = {
          ["net:intercept_request"] = function(req) return { action = "block", reason = req.why } end,
        }
        return M
    "#,
    );

    let payload = lua.create_table().unwrap();
    payload.set("why", "easylist").unwrap();

    let deadline = Instant::now() + Duration::from_millis(10);
    let out = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Table(payload),
        deadline,
    )
    .expect("handler call ok");

    let table = match out {
        Value::Table(t) => t,
        other => panic!("expected table, got {}", other.type_name()),
    };
    assert_eq!(table.get::<String>("action").unwrap(), "block");
    assert_eq!(table.get::<String>("reason").unwrap(), "easylist");
}

#[test]
fn nil_returning_handler_yields_nil() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req) end }
        return M
    "#,
    );

    let deadline = Instant::now() + Duration::from_millis(10);
    let out = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect("ok");
    assert!(matches!(out, Value::Nil));
}

#[test]
fn lua_error_is_caught_not_panicked() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req) error("boom") end }
        return M
    "#,
    );

    let deadline = Instant::now() + Duration::from_millis(10);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("should error");
    assert!(matches!(err, HookInvokeError::Lua(_)), "got {err:?}");
}

#[test]
fn missing_handler_key_is_an_error() {
    let (lua, module) = load_module(
        r"
        local M = {}
        M.hooks = {}
        return M
    ",
    );

    let deadline = Instant::now() + Duration::from_millis(10);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("missing key");
    assert!(
        matches!(err, HookInvokeError::NoSuchHandler { .. }),
        "got {err:?}"
    );
}

#[test]
fn events_table_is_addressable() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.events = { ["x:happened"] = function(p) return 42 end }
        return M
    "#,
    );

    let deadline = Instant::now() + Duration::from_millis(10);
    let out = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Events,
        "x:happened",
        Value::Nil,
        deadline,
    )
    .expect("ok");
    assert_eq!(out.as_i64(), Some(42));
}

/// **The D1 proof.** A runaway `while true do end` handler must be preempted at
/// ~the deadline and surface as [`HookInvokeError::Timeout`] — not hang.
///
/// This is the single most important guarantee of the invoker: without it the
/// 10ms filter-chain hard timeout is unenforceable on `LuaJIT`, and the whole
/// dispatch budget model collapses.
#[test]
fn runaway_handler_is_interrupted_at_deadline() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req) while true do end end }
        return M
    "#,
    );

    let budget = Duration::from_millis(10);
    let started = Instant::now();
    let deadline = started + budget;
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("runaway must not complete");
    let elapsed = started.elapsed();

    assert!(
        matches!(err, HookInvokeError::Timeout),
        "expected Timeout, got {err:?}"
    );
    // Generous upper bound: the hook fires every instruction, so the overrun
    // past the 10ms budget is tiny. Allow slack for CI scheduling jitter.
    assert!(
        elapsed < Duration::from_millis(500),
        "interrupt took too long: {elapsed:?} (the count hook should fire near the deadline)"
    );
}

/// A handler that does real work in a tight loop (the JIT-prone hot path) is
/// also preempted — confirms the interrupt holds whether or not `LuaJIT` would
/// trace-compile the loop.
#[test]
fn busy_runaway_handler_is_interrupted() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          local x = 0
          while true do x = x + 1 end
        end }
        return M
    "#,
    );

    let deadline = Instant::now() + Duration::from_millis(10);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("busy runaway must not complete");
    assert!(matches!(err, HookInvokeError::Timeout), "got {err:?}");
}

/// A handler that finishes within budget is not falsely flagged as a timeout,
/// even though the per-instruction hook is active throughout.
#[test]
fn fast_handler_under_budget_completes() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          local s = 0
          for i = 1, 1000 do s = s + i end
          return s
        end }
        return M
    "#,
    );

    let deadline = Instant::now() + Duration::from_millis(50);
    let out = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect("fast handler ok");
    assert_eq!(out.as_i64(), Some(500_500));
}

/// The interrupt hook must be removed after the call so a subsequent call on
/// the same state runs at full speed and is not affected by the prior deadline.
#[test]
fn hook_is_cleared_between_calls() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["h"] = function(p) local s=0 for i=1,1000 do s=s+i end return s end }
        return M
    "#,
    );

    // First call with an already-expired deadline would interrupt immediately;
    // use a generous one so it completes.
    let d1 = Instant::now() + Duration::from_millis(50);
    call_hook_with_deadline(&lua, &module, HookTable::Hooks, "h", Value::Nil, d1).expect("first");

    // Second call: also completes, proving the first call's hook didn't linger
    // with a stale (now-expired) deadline that would wrongly trip this one.
    let d2 = Instant::now() + Duration::from_millis(50);
    let out = call_hook_with_deadline(&lua, &module, HookTable::Hooks, "h", Value::Nil, d2)
        .expect("second");
    assert_eq!(out.as_i64(), Some(500_500));
}

/// **M3 proof.** A handler that spins inside `coroutine.create` / `resume` must
/// be preempted at the deadline. A per-thread hook would not cover the child
/// coroutine and the call would hang; the global hook does (verified
/// empirically under `LuaJIT`).
#[test]
fn coroutine_resume_spin_is_interrupted_at_deadline() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          local co = coroutine.create(function() while true do end end)
          coroutine.resume(co)
        end }
        return M
    "#,
    );

    let started = Instant::now();
    let deadline = started + Duration::from_millis(10);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("coroutine spin must not complete");
    let elapsed = started.elapsed();

    assert!(
        matches!(err, HookInvokeError::Timeout),
        "expected Timeout, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "coroutine interrupt took too long: {elapsed:?}"
    );
}

/// **M3 proof (wrap variant).** The same must hold for `coroutine.wrap`, which
/// resumes implicitly when the returned function is called.
#[test]
fn coroutine_wrap_spin_is_interrupted_at_deadline() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          local w = coroutine.wrap(function() while true do end end)
          w()
        end }
        return M
    "#,
    );

    let started = Instant::now();
    let deadline = started + Duration::from_millis(10);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("coroutine.wrap spin must not complete");
    let elapsed = started.elapsed();

    assert!(
        matches!(err, HookInvokeError::Timeout),
        "expected Timeout, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "coroutine.wrap interrupt took too long: {elapsed:?}"
    );
}

/// **M4 proof.** A single unbounded builtin (`string.rep("A", 5e8)` ≈ 500 MB)
/// runs to completion before any instruction hook fires, so the wall-clock
/// deadline cannot bound it. The memory ceiling must turn it into a bounded Lua
/// error instead of allocating gigabytes. With a generous deadline, the only
/// thing that can stop it is the allocation limit, so this asserts a
/// `HookInvokeError::Lua` (the memory error) — not a Timeout.
#[test]
fn huge_allocation_is_bounded_by_memory_limit() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          return string.rep("A", 500000000)
        end }
        return M
    "#,
    );

    // Deadline far in the future: if the call returns at all, it is because the
    // memory ceiling refused the allocation, not because of a timeout.
    let deadline = Instant::now() + Duration::from_secs(30);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("huge allocation must be refused, not serviced");

    assert!(
        matches!(err, HookInvokeError::Lua(_)),
        "expected a bounded Lua (memory) error, got {err:?}"
    );
}

/// **N1 proof.** A plugin that throws the literal old sentinel string must NOT
/// be misclassified as a `Timeout`. The classification is driven by a private
/// typed marker, not a sniffable error string.
#[test]
fn forged_timeout_sentinel_is_not_misclassified() {
    let (lua, module) = load_module(
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          error("__mote_hook_deadline_exceeded__")
        end }
        return M
    "#,
    );

    let deadline = Instant::now() + Duration::from_millis(50);
    let err = call_hook_with_deadline(
        &lua,
        &module,
        HookTable::Hooks,
        "net:intercept_request",
        Value::Nil,
        deadline,
    )
    .expect_err("forged sentinel still errors");

    assert!(
        matches!(err, HookInvokeError::Lua(_)),
        "a forged sentinel must be a plain Lua error, not a Timeout; got {err:?}"
    );
}

/// The general `call_function_with_deadline` primitive runs an arbitrary Lua
/// function (not a declarative handler) and returns its value, sharing the same
/// deadline + memory mechanism as the hook wrapper.
#[test]
fn general_primitive_calls_arbitrary_function() {
    let lua = new_sandbox().expect("sandbox");
    let f: mlua::Function = lua
        .load("return function(a, b) return a + b end")
        .eval()
        .expect("compile fn");

    let deadline = Instant::now() + Duration::from_millis(50);
    let out = call_function_with_deadline(&lua, &f, (40_i64, 2_i64), deadline)
        .expect("arbitrary call ok");
    assert_eq!(out.as_i64(), Some(42));
}

/// The general primitive enforces the deadline on a runaway arbitrary function,
/// including one spinning in a coroutine.
#[test]
fn general_primitive_enforces_deadline_through_coroutine() {
    let lua = new_sandbox().expect("sandbox");
    let f: mlua::Function = lua
        .load(
            r"return function()
              local w = coroutine.wrap(function() while true do end end)
              w()
            end",
        )
        .eval()
        .expect("compile fn");

    let started = Instant::now();
    let deadline = started + Duration::from_millis(10);
    let err =
        call_function_with_deadline(&lua, &f, (), deadline).expect_err("runaway must not complete");
    assert!(
        matches!(err, HookInvokeError::Timeout),
        "expected Timeout, got {err:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(500));
}
