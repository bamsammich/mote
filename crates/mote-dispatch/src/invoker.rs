//! The handler-invocation seam.
//!
//! The dispatch engine is generic over [`HookInvoker`]: it owns the
//! *composition and policy* (ordering, first-block-wins, error counting,
//! coalescing) while delegating the actual "call this plugin's handler with
//! this payload under this deadline" to an invoker. The production invoker
//! ([`crate::LuaHookInvoker`]) bridges to `mote-lua`; tests use a mock with
//! fast / slow / erroring / blocking handlers, so the entire policy layer is
//! verifiable without Lua.

use std::time::Instant;

use thiserror::Error;

use crate::decision::Decision;

/// What a single handler invocation produced.
///
/// The invoker translates the handler's native return into one of these. For a
/// filter-chain hook this is a [`Decision`]; for a broadcast or keybind the
/// payload is irrelevant and [`HookOutcome::Done`] signals successful
/// completion with no return semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome<P> {
    /// A filter-chain decision.
    Decision(Decision<P>),
    /// A broadcast / keybind handler ran to completion with no return value.
    Done,
}

/// Why a handler invocation failed.
///
/// These two failure modes are what the budget contract keys on: a
/// [`InvokeError::Timeout`] on a filter chain becomes `defer`, and either a
/// timeout or a [`InvokeError::Lua`] counts toward the per-plugin auto-disable
/// counter (except for keybinds — see [`crate::HookType`]).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InvokeError {
    /// The handler exceeded its deadline and was interrupted.
    #[error("handler timed out")]
    Timeout,
    /// The handler raised an error (caught, not a panic). The string is the
    /// underlying error rendered for the audit log.
    #[error("handler errored: {0}")]
    Lua(String),
}

/// The seam between dispatch policy and handler execution.
///
/// Implementors call the plugin handler identified by `(plugin, key)` with
/// `payload`, enforcing `deadline`, and return a [`HookOutcome`] or an
/// [`InvokeError`]. The engine never panics on a misbehaving handler because
/// this contract requires failures to be returned, not unwound.
///
/// `P` is the filter-chain payload type. `H` is the opaque handle identifying a
/// registered handler's plugin (so the engine can stay agnostic to how a plugin
/// is named beyond [`mote_types::PluginName`], which it uses for counting and
/// audit).
pub trait HookInvoker<P> {
    /// Invokes the handler for `key` belonging to `plugin`, passing `payload`
    /// and enforcing `deadline`.
    ///
    /// # Errors
    ///
    /// Returns [`InvokeError::Timeout`] if the handler exceeds `deadline`, or
    /// [`InvokeError::Lua`] if it raises an error. A handler that does not exist
    /// is an [`InvokeError::Lua`] (a wiring bug surfaced, not a silent skip).
    fn invoke(
        &self,
        plugin: &mote_types::PluginName,
        key: &str,
        payload: P,
        deadline: Instant,
    ) -> Result<HookOutcome<P>, InvokeError>;
}
