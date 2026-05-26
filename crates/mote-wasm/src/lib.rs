//! WASM plugin runtime for Mote.
//!
//! This crate embeds [`wasmtime`] with a Cranelift JIT backend and exposes a
//! safe, typed Rust API for loading, instantiating, and calling WASM plugins.
//! Plugins are **pure WASM**: the only way they can interact with the host is
//! through host functions that are explicitly registered before instantiation.
//! No ambient capability leaks from the sandbox.
//!
//! # Design alignment
//!
//! From `DESIGN.md` § "Plugin Language Choice":
//! > WASM via `wasmtime`. Cranelift-based JIT, low-overhead transitions between
//! > embedder and WASM, scalable concurrent instances. Inherently more sandboxed
//! > than Lua — can only call exported host functions.
//!
//! # Architecture overview
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  Caller (mote-runtime / mote-dispatch)      │
//! │  WasmPlugin::load(bytes)                    │
//! │  WasmPlugin::call("export_name", args)      │
//! └──────────────────┬──────────────────────────┘
//!                    │ Rust API (this crate)
//! ┌──────────────────▼──────────────────────────┐
//! │  PluginEngine  (wasmtime Engine — shared)   │
//! │  WasmPlugin    (per-plugin instance handle) │
//! │  HostImports   (typed host-fn registrar)    │
//! └──────────────────┬──────────────────────────┘
//!                    │ wasmtime embedder API
//! ┌──────────────────▼──────────────────────────┐
//! │  wasmtime (Cranelift JIT, WASM sandbox)      │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Instance pooling (future work)
//!
//! For v0.1 each plugin occupies its own [`wasmtime::Store`].  The performance
//! architecture in `DESIGN.md` § "Performance Architecture" calls for instance
//! pooling via [`wasmtime::PoolingAllocationConfig`] to share linear-memory
//! and table slots across instances and hit the <500 µs WASM call-overhead
//! target.  The pooling allocator will live in [`PluginEngine`], which already
//! owns the [`wasmtime::Engine`] and is therefore the right home for it; the
//! [`WasmPlugin`] handle API stays unchanged.  Tracking item: implement
//! pooling in the "1C.3 WASM pooling" work wave once baseline throughput is
//! measured.

pub mod error;
pub mod host;
pub mod plugin;

pub use error::WasmError;
pub use host::{HostImports, HostState};
pub use plugin::{PluginEngine, WasmPlugin};
