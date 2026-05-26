//! Versioned permission, capability, and hook/event registries for Mote, plus
//! the two registry-driven enforcement steps.
//!
//! This crate encodes the **v1 security schema**. The registry data lives in
//! TOML files under `data/` and is embedded into the binary via `include_str!`,
//! so the registries ship with the source and cannot drift from the code that
//! consumes them.
//!
//! ## What this crate owns
//!
//! - [`PermissionRegistry`] — every `domain:action` permission term and its
//!   resource shape (`none` / `glob` / `dynamic`; DESIGN risk C4), with a
//!   description and risk note.
//! - [`CapabilityRegistry`] — every capability role: [`Composability`]
//!   (exclusive / non-exclusive), the non-exclusive [`Dispatch`] shape, the
//!   `critical` flag, and the loose conformance [`Contract`] (required API
//!   functions + required event handlers).
//! - [`EventRegistry`] — the hook/event-name vocabulary, a namespace **separate**
//!   from permissions (risk C1/G6), used to validate `M.hooks` / `M.events` keys.
//! - [`CombinationRegistry`] — dangerous permission combinations (DISCIPLINES §4).
//!
//! ## The two enforcement steps
//!
//! Both are methods on a loaded [`Registry`]:
//!
//! - [`Registry::validate_schema`] — **step 1**: every `permissions` /
//!   `capabilities` / `consumes` term references a known registry entry with
//!   correct grammar (reusing `mote-permissions` parsing).
//! - [`Registry::check_conformance`] — **step 3**: a [`mote_lua::LoadedPlugin`]
//!   declares the required API and event surface for each capability it claims.
//!   Loose: extra surface is allowed; missing required surface fails.
//!
//! Steps 2 (module load) and 4 (permission approval) live in `mote-lua` and the
//! runtime, respectively.
//!
//! ## v1 coverage
//!
//! The v1 files enumerate the full DESIGN permission and capability set, with
//! these deliberate adjustments (see `docs/plans/risks-and-inconsistencies.md`):
//! `introspect:` is excluded (risk C2, lands in v0.2); `sys:clipboard:read/write`
//! become `sys:clipboard_read` / `sys:clipboard_write` (risk C3);
//! `mcp:client:<name>` and `secret:read:<name>` use the `dynamic` resource shape
//! (risk C4).

mod capabilities;
mod combinations;
mod error;
mod events;
mod permissions;
mod registry;

pub use capabilities::{CapabilityEntry, CapabilityRegistry, Composability, Contract, Dispatch};
pub use combinations::{CombinationEntry, CombinationRegistry, Severity};
pub use error::{ConformanceError, RegistryLoadError, SchemaValidationError};
pub use events::{EventDispatch, EventEntry, EventKind, EventRegistry};
pub use permissions::{PermissionEntry, PermissionRegistry, ResourceShape};
pub use registry::Registry;
