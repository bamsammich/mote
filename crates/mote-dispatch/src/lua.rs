//! The production [`HookInvoker`] bridging dispatch to `mote-lua`.
//!
//! [`LuaHookInvoker`] holds, per plugin, the sandboxed Lua state and loaded
//! module table needed to call a handler, calls through
//! [`mote_lua::call_hook_with_deadline`] (so the 10ms hard timeout is enforced
//! by the `mlua` instruction-count hook — the D1 mechanism), and translates the
//! handler's return into a filter-chain [`Decision`] / [`HookOutcome`]. A
//! runaway handler surfaces as [`InvokeError::Timeout`], which the engine maps
//! to `defer`.
//!
//! ## Why the payload is host-owned, not `mlua::Value`
//!
//! Each plugin runs in its **own** sandboxed Lua state (per-plugin isolation;
//! DESIGN §Script Injection and Isolated Worlds reflects the same principle at
//! the JS layer). An `mlua::Value` belongs to exactly one state — passing one
//! into a *different* state is a hard panic in mlua
//! (`"Lua instance passed Value created from a different main Lua state"`).
//! Filter-chain `modify` **cascades the payload across plugins**, i.e. across
//! states, so the cascaded payload cannot be a raw Lua value.
//!
//! The engine is therefore generic over a *host-owned* payload `P`, and
//! [`LuaHookInvoker`] is parameterized by a [`LuaMarshal`] that, per call,
//! (a) materializes `&P` into a fresh value **in the target plugin's state**,
//! and (b) interprets the handler's return value (also in that state) back into
//! a host-owned [`Decision<P>`]. Nothing crosses a state boundary except `P`,
//! which is plain Rust data.
//!
//! `mlua` stays isolated to `mote-lua`: this bridge names only the re-exported
//! [`Lua`] / [`Table`] / [`Value`] handles, never `mlua` directly (DISCIPLINES
//! §1, mirrored).
//!
//! ## The filter-chain decision protocol (DESIGN §What plugin authors need)
//!
//! A [`LuaMarshal`] implementation interprets a handler's return value as one
//! of `block` / `modify` / `allow` / `defer`. The reference contract DESIGN
//! describes is a table `{ action = "block" | "modify" | "allow", ... }`, or
//! `nil` for `defer`; the marshaling is left to the consumer because the
//! payload type `P` is theirs (an intercepted request, a tab-change event, …).

use std::collections::HashMap;
use std::time::Instant;

use mote_lua::{HookInvokeError, HookTable, Lua, Table, Value, call_hook_with_deadline};
use mote_types::PluginName;

use crate::decision::Decision;
use crate::invoker::{HookInvoker, HookOutcome, InvokeError};

/// Marshals a host payload across the Lua boundary, per call, in one direction
/// each way — never moving a Lua value between states.
///
/// Implementors own the wire shape of `P`. Both methods receive the **target
/// plugin's** [`Lua`] state so any value they create belongs to that state.
pub trait LuaMarshal<P> {
    /// Builds the argument value to pass to the handler, in `lua`'s state, from
    /// the host payload.
    ///
    /// # Errors
    ///
    /// Returns an error string if the payload cannot be materialized (surfaced
    /// as [`InvokeError::Lua`]); this never panics.
    fn encode(&self, lua: &Lua, payload: &P) -> Result<Value, String>;

    /// Interprets the handler's return value (in `lua`'s state) as a
    /// host-owned [`Decision<P>`].
    ///
    /// Implementations should map `nil` / unrecognized shapes to
    /// [`Decision::Defer`] (DESIGN's default for a handler that returns
    /// nothing).
    ///
    /// # Errors
    ///
    /// Returns an error string if a `modify` payload cannot be read back
    /// (surfaced as [`InvokeError::Lua`]).
    fn decode(&self, lua: &Lua, value: Value) -> Result<Decision<P>, String>;
}

/// A plugin's invocation context: its sandboxed state and module table.
///
/// Held by [`LuaHookInvoker`] keyed by plugin name. The `Lua` state owns the
/// module, so this keeps both alive for the lifetime of the registration.
#[derive(Debug)]
pub struct PluginContext {
    /// The sandboxed Lua state the plugin's module lives in.
    pub lua: Lua,
    /// The plugin's loaded `M` table.
    pub module: Table,
    /// Which declaration table handlers are read from for this plugin's hooks.
    ///
    /// Runtime hooks (`net:intercept_request`, `tabs:on_change`, keybinds) live
    /// in `M.hooks`; capability-contract events live in `M.events`.
    pub table: HookTable,
}

/// The production invoker: routes `(plugin, key)` to a Lua handler under a
/// deadline, marshaling the host payload across the state boundary via `M`.
#[derive(Debug)]
pub struct LuaHookInvoker<M> {
    contexts: HashMap<PluginName, PluginContext>,
    marshal: M,
}

impl<M> LuaHookInvoker<M> {
    /// An invoker with no registered plugins, using `marshal` for payload
    /// conversion.
    pub fn new(marshal: M) -> Self {
        Self {
            contexts: HashMap::new(),
            marshal,
        }
    }

    /// Registers `plugin`'s invocation context. Replaces any prior context for
    /// the same plugin (e.g. on hot reload).
    pub fn register_plugin(&mut self, plugin: PluginName, context: PluginContext) {
        self.contexts.insert(plugin, context);
    }

    /// Removes a plugin's context (e.g. on unload).
    pub fn remove_plugin(&mut self, plugin: &PluginName) {
        self.contexts.remove(plugin);
    }
}

impl<P, M> HookInvoker<P> for LuaHookInvoker<M>
where
    M: LuaMarshal<P>,
{
    fn invoke(
        &self,
        plugin: &PluginName,
        key: &str,
        payload: P,
        deadline: Instant,
    ) -> Result<HookOutcome<P>, InvokeError> {
        let Some(ctx) = self.contexts.get(plugin) else {
            return Err(InvokeError::Lua(format!(
                "no Lua context registered for plugin `{plugin}`"
            )));
        };

        // Marshal IN: build the argument in THIS plugin's state.
        let arg = self
            .marshal
            .encode(&ctx.lua, &payload)
            .map_err(InvokeError::Lua)?;

        let returned =
            call_hook_with_deadline(&ctx.lua, &ctx.module, ctx.table, key, arg, deadline)
                .map_err(map_invoke_error)?;

        // Marshal OUT: interpret the return in THIS plugin's state into a
        // host-owned decision. Nothing Lua-state-bound escapes.
        let decision = self
            .marshal
            .decode(&ctx.lua, returned)
            .map_err(InvokeError::Lua)?;

        Ok(HookOutcome::Decision(decision))
    }
}

/// Maps a `mote-lua` invocation error into the dispatch [`InvokeError`].
fn map_invoke_error(err: HookInvokeError) -> InvokeError {
    match err {
        HookInvokeError::Timeout => InvokeError::Timeout,
        HookInvokeError::Lua(e) => InvokeError::Lua(e.to_string()),
        HookInvokeError::NoSuchHandler { table, key } => {
            InvokeError::Lua(format!("no handler at M.{table:?}[{key:?}]"))
        }
        // `HookInvokeError` is `#[non_exhaustive]`; any future variant degrades
        // to a caught Lua-side error (never a panic).
        other => InvokeError::Lua(other.to_string()),
    }
}
