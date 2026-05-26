//! Sandboxed Lua runtime for Mote.
//!
//! Embeds `mlua` + `LuaJIT` and provides two things:
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
//! 3. A **deadline-enforced handler invoker** ([`call_hook_with_deadline`])
//!    that calls a named function out of a loaded module's `M.hooks` /
//!    `M.events` table with a payload, enforcing a wall-clock deadline via an
//!    `mlua` instruction-count hook, and returning [`HookInvokeError::Timeout`]
//!    if exceeded or catching Lua errors as [`HookInvokeError::Lua`]. This is
//!    the Lua-side primitive `mote-dispatch` builds the per-hook budget contract
//!    on; see [`invoke`] for the `LuaJIT` deadline-enforcement findings (D1).
//!
//! Registry validation of `permissions` / `capabilities` / `consumes`
//! (Enforcement step 1) and contract conformance (step 3) live in
//! `mote-registry`; this crate provides the sandbox and the static declarative
//! surface those steps consume.

mod error;
mod invoke;
mod load;
mod sandbox;

pub use error::{HookInvokeError, LuaError};
pub use invoke::{HookTable, call_hook_with_deadline};
pub use load::{IdentityScope, LoadedPlugin, Manifest, load_plugin, load_plugin_in};
pub use sandbox::new_sandbox;

// Re-export the mlua handles the invoker primitive traffics in, so consumers
// (notably `mote-dispatch`'s `LuaHookInvoker`) can name them without taking a
// direct dependency on `mlua` — keeping `mlua` isolated to this crate, mirroring
// the `mote-cef` discipline (DISCIPLINES §1).
pub use mlua::{Lua, Table, Value};
