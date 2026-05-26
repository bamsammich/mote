//! Core plugin-loading and execution types.
//!
//! [`PluginEngine`] wraps a shared `wasmtime::Engine` configured for
//! Cranelift JIT compilation.  A single engine is typically shared across all
//! loaded plugins for the lifetime of the application — engine creation is
//! expensive (initialises the Cranelift backend) while module compilation and
//! instance creation are cheap relative to it.
//!
//! [`WasmPlugin`] is a per-plugin handle that owns:
//! - the compiled [`wasmtime::Module`],
//! - the [`wasmtime::Store`] (linear memory, globals, tables), and
//! - the [`wasmtime::Instance`] connected to the registered host imports.
//!
//! Callers interact with a plugin exclusively through [`WasmPlugin::call`],
//! which resolves the named export, type-checks the signature, invokes the
//! function, and maps wasmtime errors to [`crate::WasmError`] variants.
//!
//! # Instance pooling (future optimisation)
//!
//! See the module-level crate documentation for the instance-pooling note.
//! When pooling is added, it will live on [`PluginEngine`] via
//! [`wasmtime::PoolingAllocationConfig`], and the public API of [`WasmPlugin`]
//! and [`PluginEngine`] will remain unchanged.

use std::fmt;

use wasmtime::{Engine, Instance, Module, Store};

use crate::{HostImports, HostState, WasmError};

/// A shared `wasmtime` engine configured for Cranelift JIT compilation.
///
/// Create one per application and share it across all plugins.  The engine
/// is `Clone`-able at zero cost (it wraps an internal `Arc`).
///
/// # Example
///
/// ```rust
/// # fn main() -> Result<(), mote_wasm::WasmError> {
/// use mote_wasm::PluginEngine;
///
/// let engine = PluginEngine::new()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct PluginEngine {
    engine: Engine,
}

impl fmt::Debug for PluginEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginEngine").finish_non_exhaustive()
    }
}

impl PluginEngine {
    /// Creates a new engine with Cranelift JIT enabled (the default wasmtime
    /// configuration for this platform).
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Runtime`] if wasmtime cannot initialise the
    /// Cranelift backend on this platform.
    pub fn new() -> Result<Self, WasmError> {
        let engine = Engine::default();
        Ok(Self { engine })
    }

    /// Returns a reference to the underlying [`wasmtime::Engine`].
    ///
    /// This is exposed so callers can construct additional wasmtime types
    /// (e.g. `Store`, `Linker`) that must share the same engine.
    #[must_use]
    pub const fn as_raw(&self) -> &Engine {
        &self.engine
    }
}

/// A loaded and instantiated WASM plugin.
///
/// Created by [`WasmPlugin::load`].  Call exports with [`WasmPlugin::call`].
///
/// The type parameter `S: HostState` is the per-plugin host state; host
/// functions defined in [`HostImports`] receive `&mut S` on every call.
///
/// # Lifecycle
///
/// ```text
/// PluginEngine::new()
///   └─ WasmPlugin::load(engine, bytes, imports, state)
///        └─ WasmPlugin::call("export_name", (arg1, arg2), &mut output)
/// ```
pub struct WasmPlugin<S: HostState> {
    store: Store<S>,
    instance: Instance,
}

impl<S: HostState + fmt::Debug> fmt::Debug for WasmPlugin<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasmPlugin")
            .field("state", self.store.data())
            .finish_non_exhaustive()
    }
}

impl<S: HostState> WasmPlugin<S> {
    /// Compiles and instantiates a WASM module from raw bytes.
    ///
    /// `imports` carries the host-function registrations (built-in `host::log`
    /// plus any extras the caller added).  `state` is the initial value of the
    /// per-plugin state that host functions receive.
    ///
    /// # Errors
    ///
    /// | Error | Condition |
    /// |---|---|
    /// | [`WasmError::InvalidModule`] | `bytes` is not valid WASM |
    /// | [`WasmError::Instantiation`] | Linking failed (missing import, type mismatch, trap in `start`) |
    pub fn load(
        engine: &PluginEngine,
        bytes: &[u8],
        imports: HostImports<S>,
        state: S,
    ) -> Result<Self, WasmError> {
        let module = Module::new(&engine.engine, bytes).map_err(WasmError::InvalidModule)?;

        let linker = imports.into_linker();
        let mut store = Store::new(&engine.engine, state);

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(WasmError::Instantiation)?;

        Ok(Self { store, instance })
    }

    /// Calls a typed exported function by name.
    ///
    /// `Params` and `Results` must match the WASM function signature exactly.
    /// Use the wasmtime typed-call tuple convention: `()` for no arguments,
    /// `(i32,)` for one `i32` argument, `(i32, i64)` for two, and so on.
    ///
    /// # Errors
    ///
    /// | Error | Condition |
    /// |---|---|
    /// | [`WasmError::MissingExport`] | No export with the given name exists |
    /// | [`WasmError::TypeMismatch`] | The export exists but has a different signature |
    /// | [`WasmError::Trap`] | The guest trapped during execution |
    pub fn call<Params, Results>(
        &mut self,
        export: &str,
        params: Params,
    ) -> Result<Results, WasmError>
    where
        Params: wasmtime::WasmParams,
        Results: wasmtime::WasmResults,
    {
        let func = self
            .instance
            .get_typed_func::<Params, Results>(&mut self.store, export)
            .map_err(|e| {
                // wasmtime surfaces "not found" and "type mismatch" through the
                // same error type.  Inspect the message to route correctly.
                // Known wasmtime messages (wasmtime v45):
                //   - missing: "failed to find function export `<name>`"
                //   - type error: "type mismatch with parameters of exported function `<name>`"
                let msg = e.to_string();
                if msg.contains("failed to find")
                    || msg.contains("no export named")
                    || msg.contains("not found")
                {
                    WasmError::MissingExport {
                        name: export.to_owned(),
                    }
                } else {
                    WasmError::TypeMismatch {
                        name: export.to_owned(),
                        expected: std::any::type_name::<Results>().to_owned(),
                        actual: msg,
                    }
                }
            })?;

        func.call(&mut self.store, params)
            .map_err(|e| WasmError::Trap {
                export: export.to_owned(),
                trap: e,
            })
    }

    /// Returns a shared reference to the per-plugin host state.
    #[must_use]
    pub fn state(&self) -> &S {
        self.store.data()
    }

    /// Returns a mutable reference to the per-plugin host state.
    #[must_use]
    pub fn state_mut(&mut self) -> &mut S {
        self.store.data_mut()
    }
}
