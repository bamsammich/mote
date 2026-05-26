//! WASM plugin runtime for Mote.
//!
//! Embeds `wasmtime` (Cranelift JIT, instance pooling) and exposes the same
//! host-function surface to WASM plugins as Lua plugins — but via explicitly
//! exported host functions only, with no ambient capability. Minimum-viable in
//! v0.1: the host-call ABI and the `adblock` rule-engine path must work.
//!
//! This crate is a stub awaiting the `1C.2` implementation wave; the heavy
//! `wasmtime` dependency lands with that work.
