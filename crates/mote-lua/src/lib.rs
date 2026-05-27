//! Sandboxed Lua runtime for Mote.
//!
//! Embeds `mlua` + `LuaJIT` and provides three facilities:
//!
//! 1. A **sandboxed Lua state factory** ([`new_sandbox`]) that constructs an
//!    mlua state with a deliberately constrained standard library: no `io`,
//!    `os`, `package`/`require`, `debug`, `ffi`, and no dynamic code loading
//!    (`load`/`loadstring`/`loadfile`/`dofile`). See [`sandbox`] for the exact
//!    removed-vs-kept inventory and the security rationale. This is the only
//!    environment in which untrusted plugin code runs.
//!
//! 2. **Declarative module loading** ([`load_plugin`]) implementing DESIGN
//!    §Enforcement Rules step 2 and ADR-0001: evaluate a plugin chunk to obtain
//!    its `M` table and extract the declarative surface — the [`Manifest`] and
//!    the key names declared in `M.hooks` / `M.events` / `M.api` — **without
//!    calling `setup()`**. The result is a [`LoadedPlugin`] holding the
//!    extracted metadata plus a handle to the loaded module for a later
//!    `setup()` / dispatch layer.
//!
//! 3. A **deadline- and allocation-bounded invoker** — a general primitive
//!    ([`call_function_with_deadline`]) that runs an arbitrary Lua function
//!    under a wall-clock deadline, a *global* instruction-count hook (so the
//!    deadline also governs child coroutines), and a memory ceiling (so a single
//!    unbounded builtin cannot allocate gigabytes), plus a thin declarative
//!    wrapper ([`call_hook_with_deadline`]) that resolves a named handler out of
//!    a loaded module's `M.hooks` / `M.events` table and calls it. A deadline
//!    abort surfaces as [`HookInvokeError::Timeout`] (via a non-forgeable typed
//!    marker, not a sniffable string); Lua errors surface as
//!    [`HookInvokeError::Lua`]. `mote-dispatch` builds the per-hook budget
//!    contract on the hook wrapper; `mote-runtime`'s `capabilities.invoke` uses
//!    the general primitive. See [`invoke`] for the precise deadline contract
//!    and the `LuaJIT` findings (M3/M4/M5/N1, D1) — in particular the rule that
//!    returned values MUST be read with raw accessors.
//!
//! 4. A **restricted config-Lua context** ([`eval_config`]) that evaluates a
//!    user config chunk (e.g. `plugins.lua`) in a sandboxed state with the same
//!    hardening as the plugin sandbox but exposing **only** config-capture
//!    functions (`mote.plugins`, `mote.dev_mode`, `mote.updates.configure`) and
//!    **no** plugin host API. Returns a typed [`ConfigSpec`] capturing the
//!    declared plugin set, dev-mode config, and update policy.
//!
//! Registry validation of `permissions` / `capabilities` / `consumes`
//! (Enforcement step 1) and contract conformance (step 3) live in
//! `mote-registry`; this crate provides the sandbox and the static declarative
//! surface those steps consume.

mod config;
mod error;
mod invoke;
mod load;
mod sandbox;

pub use config::{
    ConfigError, ConfigSpec, DevModeConfig, PluginEntry, UpdateCadence, UpdatesConfig, eval_config,
};
pub use error::{HookInvokeError, LuaError};
pub use invoke::{HookTable, call_function_with_deadline, call_hook_with_deadline};
pub use load::{IdentityScope, LoadedPlugin, Manifest, load_plugin, load_plugin_in};
pub use sandbox::new_sandbox;

// Re-export the mlua handles the invoker primitive traffics in, so consumers
// (notably `mote-dispatch`'s `LuaHookInvoker`) can name them without taking a
// direct dependency on `mlua` — keeping `mlua` isolated to this crate, mirroring
// the `mote-cef` discipline (DISCIPLINES §1).
pub use mlua::{Function, Lua, Table, Value};
