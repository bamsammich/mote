//! Error type for the WASM plugin runtime.
//!
//! All fallible operations in `mote-wasm` return [`WasmError`].  Callers do
//! not need to import `wasmtime` directly to handle or display errors — the
//! variants carry the information needed for clear diagnostics.

use thiserror::Error;

/// Errors that can occur while loading, instantiating, or calling a WASM plugin.
///
/// The variants map onto the three distinct failure modes callers care about:
///
/// | Variant | When it arises |
/// |---|---|
/// | [`WasmError::InvalidModule`] | Bytes are not valid WASM (bad magic, malformed sections, …) |
/// | [`WasmError::Instantiation`] | Module is valid but cannot be linked (missing import, type mismatch, …) |
/// | [`WasmError::MissingExport`] | Caller requested an export name that does not exist in the module |
/// | [`WasmError::TypeMismatch`] | The export exists but its signature does not match the expected one |
/// | [`WasmError::Trap`] | A WASM trap fired during execution (unreachable, OOB memory, …) |
/// | [`WasmError::Runtime`] | Any other wasmtime error not covered above |
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WasmError {
    /// The byte slice is not a valid WASM module.
    #[error("invalid WASM module: {0}")]
    InvalidModule(#[source] wasmtime::Error),

    /// The module is valid but could not be instantiated — typically a missing
    /// or type-mismatched import.
    #[error("WASM instantiation failed: {0}")]
    Instantiation(#[source] wasmtime::Error),

    /// The requested export does not exist in the WASM module.
    #[error("WASM module has no export named {name:?}")]
    MissingExport {
        /// The name that was looked up.
        name: String,
    },

    /// The export exists but its type does not match the expected function
    /// signature.
    #[error("export {name:?} has wrong type: expected {expected}, got {actual}")]
    TypeMismatch {
        /// The export name.
        name: String,
        /// Human-readable description of the expected type.
        expected: String,
        /// Human-readable description of the actual type found.
        actual: String,
    },

    /// A WASM trap fired during execution.
    #[error("WASM trap in {export:?}: {trap}")]
    Trap {
        /// The export name that was being called.
        export: String,
        /// The trap description.
        #[source]
        trap: wasmtime::Error,
    },

    /// Any other wasmtime runtime error.
    #[error("WASM runtime error: {0}")]
    Runtime(#[source] wasmtime::Error),
}
