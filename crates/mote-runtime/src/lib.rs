//! Plugin lifecycle orchestrator for Mote — the Phase-1 capstone.
//!
//! This crate integrates the plugin runtime end-to-end. It drives the load
//! pipeline, each step gated by the preceding one (DESIGN §Enforcement Rules;
//! ADR-0001). The steps run in this **actual** order:
//!
//! 1. **Sandboxed module load / manifest parse** — the Lua module is evaluated
//!    in the constrained sandbox and its declarative surface (including the
//!    manifest) extracted ([`mote_lua::load_plugin`]); **`setup()` is not
//!    called**. This is first because the steps below need the parsed manifest
//!    terms, and it is sandboxed-safe: only the module body runs, never a plugin
//!    side effect.
//! 2. **Schema validation** — every `permissions` / `capabilities` / `consumes`
//!    term references a known registry entry
//!    ([`mote_registry::Registry::validate_schema`]), the plugin's `consumes`
//!    are all fulfilled by a loaded plugin (else the dangling-consumer error),
//!    and no exclusive capability is double-claimed.
//! 3. **Contract conformance** — each claimed capability's required API/event
//!    surface is present ([`mote_registry::Registry::check_conformance`]).
//! 4. **Permission approval** — an injected [`ApprovalPolicy`] approves or
//!    narrows the requested set, producing the effective grants.
//!
//! Only when all four pass does the runtime install the `mote.*` host API into
//! the plugin's Lua state, register its `M.hooks` into the
//! [`DispatchEngine`](mote_dispatch::DispatchEngine), record its capabilities
//! and `M.events` subscriptions, and finally call `setup()` (which binds the
//! declared handlers). A failure at any step surfaces a precise [`LoadError`]
//! and the plugin never runs.
//!
//! ## What is integrated
//!
//! - **Capability fulfillment + consumes resolution** ([`CapabilityMap`]):
//!   exclusive double-claim → second load fails; dangling consumer → load
//!   fails; non-exclusive capabilities may have multiple fulfillers.
//! - **The `mote.*` host API** (private `hostapi` module): `permissions.effective`,
//!   per-plugin identity-scoped `storage`, `events.emit` (declarative
//!   `M.events` fan-out), `capabilities.invoke` (routes to the current
//!   fulfiller, executes under the **fulfiller's** permissions — D4), and a
//!   representative gated `tabs.list`. Every privileged call is gatekept by the
//!   plugin's effective permissions and audited with the performer set to the
//!   calling plugin (the fulfiller, for a capability invocation).
//! - **Hook registration into dispatch** via a core-backed
//!   [`HookInvoker`](mote_dispatch::HookInvoker); hook-name → `HookType` is
//!   sourced from the [`EventRegistry`](mote_registry::EventRegistry) dispatch
//!   shape (see the resolved ambiguity below).
//! - **Lifecycle**: [`Runtime::load`], [`Runtime::reload`] (re-approval hash
//!   over `{permissions, capabilities, consumes, identity_scope}`;
//!   code-only/non-expanding → no prompt, expansion → re-approval), and
//!   [`Runtime::unload`].
//!
//! OS file-watching is out of scope (that is `mote-pluginmgr`, Phase 3); this
//! crate provides the programmatic lifecycle API only.
//!
//! ## Resolved DESIGN ambiguities (flagged for review)
//!
//! - **Host-API payload marshalling.** Each plugin runs in its own Lua state and
//!   an `mlua::Value` cannot cross states. Inter-plugin payloads
//!   (`events.emit`, `capabilities.invoke`) and filter-chain `modify` cascades
//!   are therefore carried as a host-owned [`HostValue`] — a small JSON-ish tree
//!   — and materialized fresh in each target state. DESIGN does not specify the
//!   wire shape; `HostValue` is the chosen minimum-viable interchange.
//! - **Hook-name → `HookType` mapping.** Sourced from the registry's
//!   [`EventDispatch`](mote_registry::EventDispatch): `filter-chain` →
//!   `FilterChain`; `broadcast` / `collector` / `fan-out-per-origin` →
//!   `Broadcast` (all "every handler runs" shapes the engine models as broadcast
//!   in Phase 1); a `keys:*` key → `Keybind` (the registry does not enumerate
//!   keybinds); an unknown key (e.g. a capability-contract event placed in
//!   `M.hooks`) defaults to broadcast.
//! - **`capabilities.invoke` deadline + contract restriction.** The fulfiller's
//!   `M.api` function is called under
//!   [`mote_lua::call_function_with_deadline`], so a fulfiller that loops or
//!   allocates without bound is interrupted at a 100ms deadline rather than
//!   hanging the runtime. The invocable surface is restricted to the
//!   capability's contract `required_api`: a consumer cannot coerce the
//!   fulfiller into running an arbitrary internal function under the fulfiller's
//!   permissions (S1 confused-deputy defence). The returned value is read with
//!   raw accessors only, per the `mote-lua` deadline contract.

// This crate's internal modules hold crate-private support types (the shared
// `Core`, the host-API installer, the dispatch invoker, the marshal) that are
// deliberately *not* part of the public API and never re-exported. Marking them
// `pub(crate)` is the only annotation that both keeps them crate-internal and
// satisfies `unreachable_pub`. Clippy's nursery `redundant_pub_crate` then fires
// because, from its view, `pub(crate)` inside a non-`pub` module is "redundant"
// — but the two lints are mutually exclusive here (`pub` trips `unreachable_pub`;
// `pub(crate)` trips `redundant_pub_crate`), and `unreachable_pub` is the rustc
// lint the workspace policy elevates. We relax the weaker, conflicting nursery
// lint in this one crate, mirroring the `mote-cef` precedent (DISCIPLINES note
// on the FFI module). No behavior is affected; visibility is unchanged.
#![allow(clippy::redundant_pub_crate)]

pub(crate) mod approval;
pub(crate) mod capability;
pub(crate) mod core;
pub(crate) mod error;
pub(crate) mod hostapi;
pub(crate) mod invoker;
pub(crate) mod json;
pub(crate) mod marshal;
pub(crate) mod runtime;
pub(crate) mod secrets_router;
pub(crate) mod value;

pub use approval::{Approval, ApprovalHash, ApprovalPolicy, GrantAsRequested, Narrowing};
pub use capability::{CapabilityMap, ClaimError};
pub use error::{LifecycleError, LoadError};
pub use runtime::{IdentityContext, RunningPlugin, Runtime};
pub use value::HostValue;

// Re-export the `mote-dispatch` outcome vocabulary that appears in the
// runtime's public dispatch methods, so consumers (and integration tests) can
// name the results of [`Runtime::dispatch_filter_chain`] etc. without taking a
// direct dependency on `mote-dispatch`.
pub use mote_dispatch::{
    BroadcastOutcome, ChainResolution, Decision, FilterChainOutcome, KeybindOutcome,
};
