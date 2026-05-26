//! Plugin lifecycle orchestrator for Mote.
//!
//! The heart of Phase 1. Drives the four-step load pipeline (schema validation
//! → module load → contract conformance → permission approval) in order, each
//! step gated by the preceding one (ADR-0001). Owns the live plugin table, the
//! capability fulfillment map (exclusive resolution, non-exclusive
//! dispatch-shape resolution, dangling-consumer detection per ADR-0002), hot
//! reload (file-watch, the three reload scenarios, re-approval triggering), and
//! assembly of the `mote.*` host API exposed through `mote-lua`/`mote-wasm`.
//! Threads the gatekeeper, audit sink, and dispatcher into every plugin call,
//! and invokes `setup()` only after all four checks pass.
//!
//! This crate is a stub awaiting the `1E.*` implementation wave.
