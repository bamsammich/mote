//! Secret subsystem for Mote.
//!
//! Owns `secrets.lua` parsing, `$secret:<name>` resolution at plugin-launch,
//! the five backends (`keyring`, `password-manager` → `secret:provider`
//! plugin, `age`, `env`, `file` opt-in), per-secret permission enforcement
//! (`secret:read:<name>`), and per-identity `secrets.lua` override. Never
//! exposes backend metadata or other secret names to a plugin.
//!
//! This crate is a stub awaiting the Phase 4 implementation wave.
