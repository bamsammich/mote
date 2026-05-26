//! The runtime's [`HookInvoker`] — routes a dispatch to a plugin's `M.hooks`
//! handler by reading the plugin's live Lua state out of the shared [`Core`].
//!
//! `mote-dispatch` ships a [`LuaHookInvoker`](mote_dispatch::LuaHookInvoker)
//! that holds its own per-plugin context table, but
//! [`DispatchEngine`](mote_dispatch::DispatchEngine) consumes the invoker by
//! value and exposes no accessor to register contexts after construction. The
//! runtime needs to add and remove plugin contexts across the engine's lifetime
//! (load / unload / reload), so it implements its own invoker over a clone of
//! the shared [`Core`]: registering a plugin in the core (which the load
//! pipeline already does) makes it dispatchable, and removing it on unload makes
//! its hooks resolve to "no handler" — a caught error, never a panic.
//!
//! The marshalling (host payload ↔ Lua, decision read-back) and the
//! deadline-enforced call mirror `LuaHookInvoker` exactly, via the same
//! [`call_hook_with_deadline`](mote_lua::call_hook_with_deadline) primitive and
//! the runtime's [`HostMarshal`](crate::marshal::HostMarshal).

use std::time::Instant;

use mote_dispatch::{Decision, HookInvoker, HookOutcome, InvokeError, LuaMarshal};
use mote_lua::{HookInvokeError, HookTable, call_hook_with_deadline};
use mote_types::PluginName;

use crate::core::Core;
use crate::marshal::HostMarshal;
use crate::value::HostValue;

/// A [`HookInvoker`] backed by the shared [`Core`]'s live plugin table.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeInvoker {
    core: Core,
    marshal: HostMarshal,
}

impl RuntimeInvoker {
    /// An invoker reading plugin states from `core`.
    pub(crate) const fn new(core: Core) -> Self {
        Self {
            core,
            marshal: HostMarshal,
        }
    }
}

impl HookInvoker<HostValue> for RuntimeInvoker {
    fn invoke(
        &self,
        plugin: &PluginName,
        key: &str,
        payload: HostValue,
        deadline: Instant,
    ) -> Result<HookOutcome<HostValue>, InvokeError> {
        // Clone the plugin's Lua handles out of the core without holding the
        // borrow across the call (the handler may emit/invoke and re-borrow).
        let handles = self.core.with_mut(|state| {
            state
                .plugins
                .get(plugin)
                .map(|rec| (rec.lua.clone(), rec.module.clone()))
        });
        let Some((lua, module)) = handles else {
            return Err(InvokeError::Lua(format!(
                "no Lua context registered for plugin `{plugin}`"
            )));
        };

        let arg = self
            .marshal
            .encode(&lua, &payload)
            .map_err(InvokeError::Lua)?;

        let returned = call_hook_with_deadline(&lua, &module, HookTable::Hooks, key, arg, deadline)
            .map_err(map_invoke_error)?;

        let decision: Decision<HostValue> = self
            .marshal
            .decode(&lua, returned)
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
        other => InvokeError::Lua(other.to_string()),
    }
}
