//! Differentiated hook dispatch for Mote.
//!
//! The hook-type-differentiated dispatch engine. The dispatch contract varies
//! by **hook type, not by plugin** (DESIGN §Plugin Dispatch and Composition);
//! registration *requires* the hook type and the engine enforces the matching
//! model and budget (DISCIPLINES §3):
//!
//! - **Filter chains** ([`HookType::FilterChain`]) — `net:intercept_request`,
//!   response interception. 10ms sync hard timeout. Handlers compose by
//!   middleware semantics: first [`Decision::Block`] wins (later handlers still
//!   notified for observability but cannot override), [`Decision::Modify`]
//!   cascades the payload, [`Decision::Allow`] / [`Decision::Defer`] continue,
//!   nothing/error/timeout → `defer`. A timeout is treated as `defer` and
//!   counts toward auto-disable.
//! - **Broadcasts** ([`HookType::Broadcast`]) — `tabs:on_change`,
//!   `workspaces:on_change`, `events:on`. 100ms budget, all handlers run, no
//!   return semantics, errors isolated. "Async-allowed" means off the
//!   synchronous critical path with a generous budget, **not** tokio await
//!   (risks-and-inconsistencies.md D2).
//! - **Keybinds** ([`HookType::Keybind`]) — `keys:bind`. Input-coalescing
//!   ([`KeybindQueue`]); no raw-timeout auto-disable; **exempt** from the
//!   failure counter.
//!
//! Across all: Lua errors are caught and the plugin is skipped for that
//! dispatch while others continue; **three timeouts/errors per plugin in a 24h
//! window auto-disables it** ([`FailureCounter`], D3 — per-plugin), surfaced as
//! an [`AutoDisable`] signal the shell turns into a system notification;
//! keybinds are exempt. Every dispatched step is recorded with **performer
//! attribution** through the [`DispatchAudit`] seam (D4).
//!
//! ## Architecture: mlua stays isolated to `mote-lua`
//!
//! The engine ([`DispatchEngine`]) is generic over a [`HookInvoker`] so the
//! entire composition/policy layer is tested with a mock invoker — no Lua. The
//! production [`LuaHookInvoker`] bridges to `mote-lua`'s deadline-enforced
//! [`call_hook_with_deadline`](mote_lua::call_hook_with_deadline), which is what
//! makes the 10ms filter-chain timeout real on `LuaJIT` (see the D1 verdict in
//! that crate's `invoke` module documentation).
//!
//! ## The D1 verdict (filter-chain hard timeout on `LuaJIT`)
//!
//! `mlua::Lua::set_interrupt` is **Luau-only**; under `LuaJIT` the only host
//! preemption primitive is [`set_hook`](mote_lua) with an instruction-count
//! trigger. A spike (recorded in `mote-lua`'s `invoke` module and proven by its
//! `tests/invoke.rs`) established that the count hook **must fire every
//! instruction (`n = 1`)** to preempt a degenerate `while true do end`; at
//! `n > 1` `LuaJIT` never reaches the hook threshold inside such a loop and the
//! handler runs forever. At `n = 1` the runaway is reliably interrupted at the
//! deadline (~10ms), with or without JIT compilation. So the 10ms filter-chain
//! hard timeout **is** enforceable on `LuaJIT` — via the per-instruction hook,
//! not `set_interrupt`. No PUC-Lua fallback is required.

mod audit;
mod counter;
mod decision;
mod engine;
mod hook;
mod invoker;
mod keybind;
mod lua;

pub use audit::{ChainStep, DispatchAudit, NullAudit};
pub use counter::{AUTO_DISABLE_THRESHOLD, AutoDisable, FailureCounter, WINDOW};
pub use decision::{ChainResolution, Decision};
pub use engine::{
    BroadcastOutcome, Clock, DispatchEngine, FilterChainOutcome, KeybindOutcome, RegisterError,
    SystemClock,
};
pub use hook::{BROADCAST_BUDGET, DEFAULT_PRIORITY, FILTER_CHAIN_BUDGET, HookType, Registration};
pub use invoker::{HookInvoker, HookOutcome, InvokeError};
pub use keybind::KeybindQueue;
pub use lua::{LuaHookInvoker, LuaMarshal, PluginContext};
