//! Deadline- and allocation-bounded invocation of Lua handlers.
//!
//! This is the Lua-side primitive the dispatch engine (`mote-dispatch`) and the
//! capability layer (`mote-runtime`) need to honor the per-hook-type time
//! budgets in DESIGN §Runtime guarantees. It runs a Lua function (a declarative
//! `M.hooks` / `M.events` handler, or an arbitrary `M.api` capability function)
//! under a wall-clock deadline **and** a memory ceiling, and returns one of:
//!
//! - the function's return [`Value`] on success;
//! - [`HookInvokeError::Timeout`] if the deadline elapses mid-execution (which
//!   the filter-chain layer treats as `defer`);
//! - [`HookInvokeError::Lua`] if the function raised a Lua error (caught, never
//!   a panic — the plugin is skipped for that dispatch).
//!
//! # The deadline contract
//!
//! A call run through [`call_function_with_deadline`] (and therefore through
//! [`call_hook_with_deadline`], which delegates to it) is bounded on two axes:
//!
//! - **CPU / bytecode, including coroutines.** A *global* instruction-count hook
//!   ([`Lua::set_global_hook`]) fires on every bytecode instruction and aborts
//!   the call once the deadline passes. Because it is a *global* hook rather
//!   than a per-thread one, it also governs code running inside child
//!   coroutines created by the handler (`coroutine.create` / `coroutine.resume`
//!   / `coroutine.wrap`) — a per-thread hook would not, leaving such code able
//!   to spin forever (security review finding M3). Empirically verified against
//!   `LuaJIT`: see `tests/invoke.rs` (`coroutine_spin_*` and the D1 proofs).
//!
//! - **Allocation.** A memory limit ([`Lua::set_memory_limit`], empirically
//!   functional under vendored `LuaJIT`) is installed for the duration of the
//!   call so that a single unbounded builtin — `string.rep("A", 5e8)`,
//!   `table.concat`, a large `string.gsub` — fails with a Lua memory error
//!   instead of allocating gigabytes before any instruction hook can fire
//!   (finding M4). The hook fires *between* bytecode ops, not *inside* a C
//!   builtin, so wall-clock alone cannot bound a single huge C-call; the memory
//!   ceiling is what bounds it.
//!
//! What the budget does **not** bound: the *wall-clock duration* of a single C
//! builtin that stays under the memory ceiling (e.g. a moderately large
//! `string.find` over a few MB). Such a call runs to completion before the next
//! instruction hook; this is a best-effort residual, made small by the memory
//! ceiling capping the input sizes a builtin can be handed.
//!
//! # Reading the returned value (finding M5)
//!
//! Both the deadline hook and the memory limit are **removed before this
//! function returns** the handler's [`Value`]. Any Lua metamethod triggered
//! *after* return — including `__index` / `__tostring` fired by a consumer that
//! reads the value through a non-raw accessor — runs **un-deadlined and
//! un-bounded**. Callers MUST therefore read the returned value with *raw*
//! accessors only (`Table::raw_get`, `Table::raw_len`, …) and MUST NOT invoke
//! metamethods on it. Marshalling a handler's return into a host decision is a
//! consumer responsibility (`mote-runtime`) and must observe this rule.
//!
//! ## Why the per-instruction (`n = 1`) granularity (risks D1)
//!
//! `mlua::Lua::set_interrupt` is Luau-only and absent under `LuaJIT`; the
//! count hook is the host-side preemption primitive available. The hook must
//! fire on **every** instruction (`n = 1`) to preempt a pathological
//! single-statement loop such as `while true do end` under `LuaJIT`. At
//! `n > 1`, `LuaJIT`'s hook accounting does not reach the threshold inside such
//! a loop and the handler runs forever. `n = 1` is the correctness floor for
//! the 10ms filter-chain hard timeout; handler bodies are expected to be short.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use mlua::{Function, HookTriggers, IntoLuaMulti, Lua, Table, Value, VmState};

use crate::error::HookInvokeError;

/// Which declarative table on the plugin module a handler lives in.
///
/// DESIGN distinguishes `M.hooks` (filter chains, broadcasts, keybinds) from
/// `M.events` (capability-contract event bus). The dispatch layer knows which
/// applies for a given dispatch and selects it here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HookTable {
    /// `M.hooks` — runtime hooks (`net:intercept_request`, `tabs:on_change`,
    /// `keys:bind`, …).
    Hooks,
    /// `M.events` — capability-contract event handlers.
    Events,
}

impl HookTable {
    /// The Lua module field name this table is read from.
    const fn field(self) -> &'static str {
        match self {
            Self::Hooks => "hooks",
            Self::Events => "events",
        }
    }
}

/// The instruction granularity at which the deadline hook fires.
///
/// Must be `1`: see the module documentation. Anything larger fails to preempt
/// a degenerate `while true do end` under `LuaJIT`.
const HOOK_EVERY_INSTRUCTION: u32 = 1;

/// Allocation ceiling enforced for the duration of a single bounded call.
///
/// This bounds the blast radius of a single unbounded builtin (finding M4): an
/// allocation that would exceed this fails with a Lua memory error rather than
/// being serviced. Sized generously so legitimate handler working sets are
/// unaffected while a pathological `string.rep("A", 5e8)` (≈500 MB) is refused.
/// The previous limit (typically "no limit") is restored after the call.
const CALL_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// A private, non-forgeable marker that a deadline abort — not a plugin error —
/// terminated a call (finding N1).
///
/// It is raised from the hook closure via [`mlua::Error::external`] and matched
/// back out with [`mlua::Error::downcast_ref`]. Because the type is private to
/// this crate and identified by its Rust `TypeId`, a plugin cannot forge a
/// [`HookInvokeError::Timeout`] classification by `error()`-ing a crafted
/// string: a thrown string is a `RuntimeError`, never an `ExternalError`
/// carrying this type.
#[derive(Debug)]
struct DeadlineExceeded;

impl std::fmt::Display for DeadlineExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("hook handler exceeded its deadline and was interrupted")
    }
}

impl std::error::Error for DeadlineExceeded {}

/// Invokes the handler at `module.<table>[key]` with `payload`, enforcing
/// `deadline` and an allocation ceiling (see the module-level deadline
/// contract).
///
/// Returns the handler's return value on success. The handler is called with a
/// single argument (`payload`); the four-decision filter-chain protocol
/// (`{ action = "block" | "modify" | "allow" }` or nothing) is interpreted by
/// the dispatch layer from the returned [`Value`], not here.
///
/// The returned [`Value`] MUST be read with raw accessors only — see the
/// module-level "Reading the returned value" contract; protections are lifted
/// before this returns.
///
/// # Errors
///
/// - [`HookInvokeError::NoSuchHandler`] if `module.<table>` is absent, not a
///   table, or has no function under `key`.
/// - [`HookInvokeError::Timeout`] if execution exceeds `deadline`.
/// - [`HookInvokeError::Lua`] if the handler raises a Lua error, or a Lua
///   operation while reading the handler fails.
///
/// This function never panics on plugin misbehavior: a runaway loop, a thrown
/// error, an unbounded allocation, or a missing handler all map to a returned
/// `Err`.
pub fn call_hook_with_deadline(
    lua: &Lua,
    module: &Table,
    table: HookTable,
    key: &str,
    payload: Value,
    deadline: Instant,
) -> Result<Value, HookInvokeError> {
    let handler = resolve_handler(module, table, key)?;
    call_function_with_deadline(lua, &handler, payload, deadline)
}

/// Runs an arbitrary Lua [`Function`] with `args` under the deadline +
/// coroutine + allocation protections described in the module-level deadline
/// contract.
///
/// This is the general primitive the capability layer (`mote-runtime`) uses to
/// invoke `M.api` functions under the same guarantees that the dispatch layer
/// gets for hooks; [`call_hook_with_deadline`] is a thin wrapper that resolves a
/// declarative handler and then calls this.
///
/// `args` may be any [`IntoLuaMulti`]: a single [`Value`], a tuple, `()`, etc.
///
/// The returned [`Value`] MUST be read with raw accessors only — see the
/// module-level "Reading the returned value" contract; protections are lifted
/// before this returns.
///
/// # Errors
///
/// - [`HookInvokeError::Timeout`] if execution exceeds `deadline` (including an
///   allocation-bounded abort surfaced as a memory error is reported as
///   [`HookInvokeError::Lua`], not `Timeout` — only a wall-clock abort is a
///   `Timeout`).
/// - [`HookInvokeError::Lua`] if the function raises a Lua error or exceeds the
///   allocation ceiling.
///
/// Never panics on plugin misbehavior.
pub fn call_function_with_deadline(
    lua: &Lua,
    function: &Function,
    args: impl IntoLuaMulti,
    deadline: Instant,
) -> Result<Value, HookInvokeError> {
    // Non-forgeable deadline signal. The hook closure sets this flag *and*
    // returns the typed external error; the flag is a defence-in-depth backstop
    // in case an intervening `pcall` inside the handler swallows the error — we
    // still classify the outcome as a timeout if the flag was tripped.
    let tripped = Arc::new(AtomicBool::new(false));
    let hook_flag = Arc::clone(&tripped);

    // Install a *global* instruction-count hook so the deadline also governs
    // child coroutines (finding M3), not just the calling thread.
    lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(HOOK_EVERY_INSTRUCTION),
        move |_lua, _debug| {
            if Instant::now() >= deadline {
                hook_flag.store(true, Ordering::Relaxed);
                Err(mlua::Error::external(DeadlineExceeded))
            } else {
                Ok(VmState::Continue)
            }
        },
    )
    .map_err(HookInvokeError::Lua)?;

    // Bound a single unbounded builtin's allocation (finding M4). The hook fires
    // between bytecode ops, not inside a C-call, so wall-clock alone cannot stop
    // `string.rep("A", huge)`; the memory ceiling does. `set_memory_limit`
    // returns the previous limit (0 = unlimited), which we restore afterwards.
    let prev_limit = lua.set_memory_limit(CALL_MEMORY_LIMIT_BYTES).ok();

    let result: mlua::Result<Value> = function.call(args);

    // Always lift both protections so the next call on this state runs at full
    // speed and is not governed by this (now-stale) deadline or ceiling.
    lua.remove_global_hook();
    if let Some(prev) = prev_limit {
        // Restoring the prior limit cannot fail in a way we can act on; ignore.
        let _ = lua.set_memory_limit(prev);
    }

    result.map_err(|err| classify_error(err, &tripped))
}

/// Reads the handler function for `key` out of `module.<table>`.
fn resolve_handler(
    module: &Table,
    table: HookTable,
    key: &str,
) -> Result<Function, HookInvokeError> {
    let field = table.field();
    let decl: Value = module.get(field).map_err(HookInvokeError::Lua)?;
    let Value::Table(decl) = decl else {
        return Err(HookInvokeError::NoSuchHandler {
            table,
            key: key.to_owned(),
        });
    };

    match decl.get::<Value>(key).map_err(HookInvokeError::Lua)? {
        Value::Function(f) => Ok(f),
        _ => Err(HookInvokeError::NoSuchHandler {
            table,
            key: key.to_owned(),
        }),
    }
}

/// Maps an `mlua` error from the call into a [`HookInvokeError`], recognizing
/// the deadline abort as a [`HookInvokeError::Timeout`].
///
/// The classification is driven by the non-forgeable [`DeadlineExceeded`]
/// marker (matched via `downcast_ref`) or the host-side `tripped` flag — never
/// by a plugin-controllable error string (finding N1).
fn classify_error(err: mlua::Error, tripped: &AtomicBool) -> HookInvokeError {
    if is_deadline_error(&err) || tripped.load(Ordering::Relaxed) {
        HookInvokeError::Timeout
    } else {
        HookInvokeError::Lua(err)
    }
}

/// Whether `err` (or a callback error it wraps) carries our private
/// [`DeadlineExceeded`] marker.
fn is_deadline_error(err: &mlua::Error) -> bool {
    // `downcast_ref` already walks `WithContext` chains; the hook error
    // propagates wrapped in `CallbackError`, so unwrap that arm explicitly.
    if err.downcast_ref::<DeadlineExceeded>().is_some() {
        return true;
    }
    match err {
        mlua::Error::CallbackError { cause, .. } => is_deadline_error(cause),
        _ => false,
    }
}
