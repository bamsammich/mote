//! The sandboxed Lua state factory.
//!
//! This is a security boundary. The constrained state is the only environment
//! in which untrusted plugin code ever runs (DESIGN §Plugin Language Choice,
//! §Enforcement Rules "Sandboxed runtime"). The goal is that the *only* way a
//! plugin can affect the host is through the declared-permission host API —
//! never through the filesystem, the process environment, or dynamic code
//! loading.
//!
//! ## What is removed
//!
//! Two distinct removal mechanisms are at play, because Lua's dangerous surface
//! lives in two places:
//!
//! 1. **Standard-library modules**, gated by [`mlua::StdLib`] flags at state
//!    construction. We never load:
//!    - `io` — filesystem and process I/O.
//!    - `os` — `os.execute`, `os.getenv`, `os.remove`, `os.exit`, clock/time
//!      that leaks host state, etc.
//!    - `package` — `require`, C-module loading, the module search path. This
//!      is the dynamic host-escape vector and the inter-plugin `require` that
//!      DESIGN forbids.
//!    - `debug` — introspection / sandbox-escape primitives
//!      (`debug.getupvalue`, `debug.setmetatable`, raw stack access). mlua's
//!      safe constructor refuses to load it at all.
//!    - `ffi` (`LuaJIT`) — arbitrary native memory and code. mlua's safe
//!      constructor refuses to load it at all.
//!
//! 2. **Base-library globals.** Lua's *base* library is always installed by
//!    mlua and is not individually gateable by a [`mlua::StdLib`] flag. It
//!    carries the dynamic-code-loading primitives, so we explicitly set them to
//!    `nil` after construction:
//!    - `load`, `loadstring`, `loadfile`, `dofile` — compile/run arbitrary
//!      source or files at runtime. Removing these is what makes static
//!      contract conformance (ADR-0001) meaningful: a plugin cannot smuggle in
//!      code the loader never saw.
//!    - `require` — module loading. Defensive: it normally lives in `package`,
//!      which we already exclude, but we nil it unconditionally so its absence
//!      does not depend on the `package` flag.
//!    - `collectgarbage` — denied control over the host GC; allowing
//!      `collectgarbage("collect")` from a plugin is a needless availability
//!      lever.
//!
//! ## What is kept
//!
//! The safe, pure-computation subset that plugins legitimately need:
//!
//! - `string`, `table`, `math` — core data manipulation.
//! - `coroutine` — cooperative control flow; cannot touch the host.
//! - `bit` and `jit` (`LuaJIT`) — bit operations and JIT control; pure compute.
//! - Base-library safe primitives that survive the nil-out: `pcall`, `xpcall`,
//!   `error`, `assert`, `select`, `type`, `tostring`, `tonumber`, `pairs`,
//!   `ipairs`, `next`, `rawget`, `rawset`, `rawequal`, `rawlen`, `setmetatable`,
//!   `getmetatable`, `unpack`, `print`, and `_G`/`_VERSION`.
//!
//! `print` is retained deliberately: it writes to stdout, not to the user's
//! data or filesystem, and is the obvious debugging affordance for plugin
//! authors. A future revision may route it through the host log surface.

use mlua::{Lua, LuaOptions, StdLib};

use crate::error::LuaError;

/// Base-library globals removed from the sandbox after state construction.
///
/// These ship with Lua's always-on base library, which is not individually
/// gated by a [`StdLib`] flag, so they must be nil-ed out explicitly. See the
/// module-level documentation for the rationale behind each entry.
const DENIED_BASE_GLOBALS: &[&str] = &[
    "load",
    "loadstring",
    "loadfile",
    "dofile",
    "require",
    "collectgarbage",
];

/// The standard-library subset loaded into every sandboxed state.
///
/// Deliberately omits `io`, `os`, `package`, `debug`, and `ffi`. Built by
/// OR-ing only the safe modules rather than subtracting from
/// [`StdLib::ALL_SAFE`], so the allowed set is explicit and auditable at a
/// glance (and so a future mlua widening `ALL_SAFE` cannot silently grant a new
/// module to plugins).
///
/// `coroutine` is intentionally not listed: under `LuaJIT` it is not a
/// [`StdLib`] flag (the flag exists only for the 5.2+/Luau line) — `LuaJIT`
/// installs `coroutine` as part of its always-on base set, so it is present
/// without being requested here.
fn sandbox_libs() -> StdLib {
    StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::BIT | StdLib::JIT
}

/// Constructs a fresh sandboxed [`Lua`] state.
///
/// The returned state has the constrained standard library loaded and the
/// dangerous base-library globals removed (see the module documentation for the
/// exact removed-vs-kept inventory). It contains no host API yet; marshalling
/// the `mote.*` surface onto it is a later layer's responsibility.
///
/// # Errors
///
/// Returns [`LuaError::Sandbox`] if mlua cannot construct the state with the
/// requested library subset, or [`LuaError::Lua`] if removing a base global
/// fails (neither is expected in practice, but both are surfaced rather than
/// unwrapped).
pub fn new_sandbox() -> Result<Lua, LuaError> {
    // `new_with` guarantees the unsafe `debug`/`ffi` modules cannot be loaded;
    // it returns `SafetyError` if they are requested. Our `sandbox_libs()` never
    // requests them, so this path is purely the safe constructor.
    let lua = Lua::new_with(sandbox_libs(), LuaOptions::default()).map_err(LuaError::Sandbox)?;

    let globals = lua.globals();
    for name in DENIED_BASE_GLOBALS {
        globals.set(*name, mlua::Nil).map_err(LuaError::Lua)?;
    }

    Ok(lua)
}
