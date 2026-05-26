//! Host-import ABI for WASM plugins.
//!
//! WASM plugins are pure sandboxes: the *only* way they interact with the host
//! is through functions the host explicitly registers before instantiation.
//! [`HostImports`] is the builder that collects those registrations; its
//! generic parameter `S` is the caller-supplied per-plugin state that host
//! functions receive via `wasmtime::Caller<'_, S>`.
//!
//! # Sandboxing guarantee
//!
//! `wasmtime` enforces the sandbox automatically — a guest cannot call any
//! function that was not registered as a named import.  [`HostImports`] does
//! not weaken that guarantee: it only *names* functions in the `"host"`
//! namespace that the guest may reference in its import section.
//!
//! # Built-in host functions
//!
//! One built-in import is registered for every plugin regardless of what the
//! caller adds:
//!
//! | WASM import | Rust signature | Purpose |
//! |---|---|---|
//! | `host::log(ptr: i32, len: i32)` | `fn(&mut S, i32, i32)` | Guest emits a UTF-8 message; host routes it to `S::on_log`. |
//!
//! Callers wishing to handle guest log output implement [`HostState::on_log`].
//!
//! # Extending the ABI
//!
//! Additional host functions are registered with [`HostImports::register`].
//! The closure receives `(caller: wasmtime::Caller<'_, S>, args…)` — the
//! standard wasmtime typed-call signature — so callers have full access to
//! guest memory via `caller.get_export("memory")`.

use std::fmt;

use wasmtime::{Caller, Engine, Linker};

/// Per-plugin state threaded through all host-function calls.
///
/// Implement this trait on your state type and supply a value when calling
/// [`crate::plugin::WasmPlugin::load`].  Host functions receive `&mut S`
/// through the `wasmtime::Caller`.
///
/// # Minimal implementation
///
/// ```rust
/// use mote_wasm::HostState;
///
/// #[derive(Debug)]
/// struct MyState {
///     log_messages: Vec<String>,
/// }
///
/// impl HostState for MyState {
///     fn on_log(&mut self, message: &str) {
///         self.log_messages.push(message.to_owned());
///     }
/// }
/// ```
pub trait HostState: Send + 'static {
    /// Called when the guest invokes the `host::log` import.
    ///
    /// `message` is the UTF-8 string the guest produced.  Invalid UTF-8 is
    /// replaced with the Unicode replacement character (U+FFFD) before this
    /// method is called; host functions never panic on bad guest output.
    fn on_log(&mut self, message: &str);
}

/// Builder that registers host imports into a [`wasmtime::Linker`] before
/// module instantiation.
///
/// # Type parameter
///
/// `S: HostState` is the per-plugin state type.  The same `S` is used for
/// every host function registered through this builder.
///
/// # Example
///
/// ```rust
/// use mote_wasm::{HostImports, HostState};
///
/// #[derive(Debug)]
/// struct State { calls: Vec<i32> }
///
/// impl HostState for State {
///     fn on_log(&mut self, msg: &str) { eprintln!("guest: {msg}"); }
/// }
///
/// let engine = wasmtime::Engine::default();
/// let imports = HostImports::<State>::new(&engine)
///     .register("add_to_calls", |mut caller: wasmtime::Caller<'_, State>, v: i32| {
///         caller.data_mut().calls.push(v);
///     })
///     .expect("failed to register import");
/// ```
pub struct HostImports<S: HostState> {
    linker: Linker<S>,
}

impl<S: HostState> fmt::Debug for HostImports<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostImports").finish_non_exhaustive()
    }
}

impl<S: HostState> HostImports<S> {
    /// Creates a new builder and registers the built-in `host::log` import.
    ///
    /// # Panics
    ///
    /// Panics only if the wasmtime `Engine` rejects the built-in `host::log`
    /// function definition — which would be a programming error (wrong
    /// signature) and cannot happen in practice.
    #[must_use]
    pub fn new(engine: &Engine) -> Self {
        let mut linker: Linker<S> = Linker::new(engine);

        // Register `host::log(ptr: i32, len: i32)`.
        //
        // The guest passes a pointer and byte-length into its linear memory.
        // We read the bytes, decode as UTF-8 (replacing invalid sequences),
        // and forward to `S::on_log`.
        linker
            .func_wrap(
                "host",
                "log",
                |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
                    // WASM linear-memory addresses are unsigned 32-bit values.
                    // The WASM ABI passes them as i32; reinterpret the bits.
                    let ptr = ptr.cast_unsigned() as usize;
                    let len = len.cast_unsigned() as usize;

                    // Read the guest's linear memory export.
                    let mem = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory);

                    let message = mem.map_or_else(
                        || String::from("<guest has no memory export>"),
                        |memory| {
                            let bytes = memory.data(&caller);
                            let end = ptr.saturating_add(len).min(bytes.len());
                            String::from_utf8_lossy(&bytes[ptr..end]).into_owned()
                        },
                    );

                    caller.data_mut().on_log(&message);
                },
            )
            .expect("built-in host::log registration must succeed");

        Self { linker }
    }

    /// Registers an additional host function under the `"host"` module
    /// namespace.
    ///
    /// `name` is the import name the guest uses; `func` must match the
    /// signature the guest expects.  The closure follows wasmtime's typed
    /// closure convention — see [`wasmtime::Linker::func_wrap`] for the
    /// supported arities and value types.
    ///
    /// Returns `self` for chaining.
    ///
    /// # Errors
    ///
    /// Returns [`crate::WasmError::Runtime`] if the function cannot be
    /// registered (e.g. a duplicate name under `"host"`).
    pub fn register<Params, Results>(
        mut self,
        name: &str,
        func: impl wasmtime::IntoFunc<S, Params, Results>,
    ) -> Result<Self, crate::WasmError> {
        self.linker
            .func_wrap("host", name, func)
            .map_err(crate::WasmError::Runtime)?;
        Ok(self)
    }

    /// Consumes the builder and returns the finished [`wasmtime::Linker`].
    ///
    /// Called internally by [`crate::plugin::WasmPlugin::load`]; callers
    /// typically do not need this method directly.
    pub(crate) fn into_linker(self) -> Linker<S> {
        self.linker
    }
}
