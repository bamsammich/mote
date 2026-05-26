//! Plugin management and provenance for Mote.
//!
//! Owns `plugins.lua` + `plugins.lock` parse/resolve; source types (`github:`,
//! `git+https://`, `path:`, `bundled`); the content-addressed cache; BLAKE3
//! directory-hash computation per the documented spec; the full `mote plugin`
//! CLI surface; capability-contract dependency resolution (dangling-consumer
//! detection per ADR-0002); the update flow with permission-change surfacing;
//! implicit-local detection; dev mode; and first-party bundled distribution.
//! Stores per-plugin approved permission/capability/consumes/`identity_scope`
//! hashes.
//!
//! This crate is a stub awaiting the Phase 3 implementation wave.
