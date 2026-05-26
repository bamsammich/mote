//! Lock-free permission and network audit log for Mote.
//!
//! The append-only audit pipeline: a `crossbeam-channel` MPSC feed from every
//! gatekeeper check and dispatch decision into a dedicated audit thread that
//! writes an in-memory ring buffer and periodically flushes to `SQLite` (via
//! `mote-storage`). One atomic append per logged call, never a mutex on the
//! hot path. Surfaces query APIs for the integrity panel (per-plugin call
//! counts, network decisions, denials, MCP activity).
//!
//! This crate is a stub awaiting the `1B.2` implementation wave.
