//! Deadline-enforced invocation of a single declarative Lua handler.
//!
//! This is the Lua-side primitive the dispatch engine (`mote-dispatch`) needs
//! to honor the per-hook-type time budgets in DESIGN §Runtime guarantees. It
//! calls a named function out of a plugin module's `M.hooks` / `M.events`
//! declaration table with a payload, enforcing a wall-clock deadline, and
//! returns one of:
//!
//! - the handler's return [`Value`] (which the dispatch layer maps to a
//!   `Decision`);
//! - [`HookInvokeError::Timeout`] if the deadline elapses mid-execution (which
//!   the filter-chain layer treats as `defer`);
//! - [`HookInvokeError::Lua`] if the handler raised a Lua error (caught, never
//!   a panic — the plugin is skipped for that dispatch).
//!
//! ## Deadline enforcement under `LuaJIT` (risks-and-inconsistencies.md D1)
//!
//! `mlua::Lua::set_interrupt` is **Luau-only** and does not exist under
//! `LuaJIT`; the host-side preemption primitive available to us is
//! [`mlua::Lua::set_hook`] with [`HookTriggers::every_nth_instruction`]. The
//! hook callback checks the deadline and aborts the running handler by
//! returning an error.
//!
//! The empirically-verified subtlety (see the crate's `tests/invoke.rs` D1
//! proof, and the spike recorded in the dispatch crate's documentation): the
//! count hook must fire on **every** instruction (`n = 1`) to preempt a
//! pathological single-statement loop such as `while true do end` under
//! `LuaJIT`. At `n > 1`, `LuaJIT`'s hook accounting does not reach the
//! threshold inside such a loop and the handler runs forever. `n = 1` reliably
//! interrupts both interpreted and JIT-prone loops at the deadline. The
//! per-instruction hook is comparatively heavy, but it is the correctness floor
//! for the 10ms filter-chain hard timeout, and handler bodies are expected to
//! be short.
//!
//! The hook is installed for the duration of one call and removed afterwards,
//! so a state can host many sequential calls without a stale deadline leaking
//! into the next one.

use std::time::Instant;

use mlua::{HookTriggers, Lua, Table, Value, VmState};

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

/// Invokes the handler at `module.<table>[key]` with `payload`, enforcing
/// `deadline`.
///
/// Returns the handler's return value on success. The handler is called with a
/// single argument (`payload`); the four-decision filter-chain protocol
/// (`{ action = "block" | "modify" | "allow" }` or nothing) is interpreted by
/// the dispatch layer from the returned [`Value`], not here.
///
/// A wall-clock deadline is enforced via a per-instruction interrupt hook
/// installed for the duration of this call only and removed before returning
/// (including on error). If the deadline has already passed when the call
/// starts, the handler is interrupted near-immediately.
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
/// error, or a missing handler all map to a returned `Err`.
pub fn call_hook_with_deadline(
    lua: &Lua,
    module: &Table,
    table: HookTable,
    key: &str,
    payload: Value,
    deadline: Instant,
) -> Result<Value, HookInvokeError> {
    let handler = resolve_handler(module, table, key)?;

    // Install a per-instruction hook that aborts once the deadline passes. The
    // hook closure carries the deadline by copy; `Instant` is `Copy`.
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(HOOK_EVERY_INSTRUCTION),
        move |_lua, _debug| {
            if Instant::now() >= deadline {
                // Aborts the running handler. We tag the message so we can
                // distinguish a deadline abort from a handler-raised error.
                Err(mlua::Error::RuntimeError(DEADLINE_SENTINEL.to_owned()))
            } else {
                Ok(VmState::Continue)
            }
        },
    )
    .map_err(HookInvokeError::Lua)?;

    let result: mlua::Result<Value> = handler.call(payload);

    // Always remove the hook so the next call on this state runs at full speed
    // and is not governed by this (now-stale) deadline.
    lua.remove_hook();

    result.map_err(classify_error)
}

/// The marker embedded in the abort error so a deadline interrupt is
/// distinguishable from an ordinary handler error.
const DEADLINE_SENTINEL: &str = "__mote_hook_deadline_exceeded__";

/// Reads the handler function for `key` out of `module.<table>`.
fn resolve_handler(
    module: &Table,
    table: HookTable,
    key: &str,
) -> Result<mlua::Function, HookInvokeError> {
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

/// Maps an `mlua` error from the handler call into a [`HookInvokeError`],
/// recognizing the deadline-abort sentinel as a [`HookInvokeError::Timeout`].
fn classify_error(err: mlua::Error) -> HookInvokeError {
    if is_deadline_error(&err) {
        HookInvokeError::Timeout
    } else {
        HookInvokeError::Lua(err)
    }
}

/// Whether `err` (or a callback error it wraps) is our deadline-abort sentinel.
fn is_deadline_error(err: &mlua::Error) -> bool {
    match err {
        mlua::Error::RuntimeError(msg) => msg.contains(DEADLINE_SENTINEL),
        // A hook error propagates wrapped in `CallbackError`; unwrap to inspect.
        mlua::Error::CallbackError { cause, .. } => is_deadline_error(cause),
        _ => false,
    }
}
