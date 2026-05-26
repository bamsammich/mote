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
//! Registry validation of `permissions` / `capabilities` / `consumes`
//! (Enforcement step 1) and contract conformance (step 3) live in
//! `mote-registry`; this crate provides the sandbox and the static declarative
//! surface those steps consume.

mod error;
mod load;
mod sandbox;

pub use error::LuaError;
pub use load::{IdentityScope, LoadedPlugin, Manifest, load_plugin, load_plugin_in};
pub use sandbox::new_sandbox;
