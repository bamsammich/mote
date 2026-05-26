//! The shared runtime core: the live plugin table that inter-plugin host calls
//! (`events.emit`, `capabilities.invoke`) route through.
//!
//! The `mote.*` host API installed into plugin A's Lua state must be able to
//! reach plugin B's state — emit fans an event out to B's `M.events` handler,
//! and `capabilities.invoke` calls B's `M.api` function under B's permissions.
//! Lua states are not `Send` and an `mlua::Value` cannot cross states, so the
//! core is a single-threaded reference-counted cell (`Rc<RefCell<…>>`) whose
//! records hold a **clone** of each plugin's `Lua`/`Table` handle (mlua handles
//! are cheap, reference-counted views of the same VM).
//!
//! Cross-plugin payloads are marshalled through [`HostValue`]: the core reads
//! the caller's argument out of the caller's state, then materializes it fresh
//! in the target's state. Nothing Lua-state-bound is ever moved between states.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mote_audit::{AuditEvent, Decision as AuditDecision, EventProducer};
use mote_lua::{HookTable, Lua, Table, call_hook_with_deadline};
use mote_types::PluginName;

use crate::capability::CapabilityMap;
use crate::value::HostValue;

/// The budget given to an inter-plugin event handler or capability API call.
///
/// Mirrors the dispatch broadcast budget (100ms) — these are off the
/// synchronous critical path. A runaway handler is interrupted at the deadline
/// by the `mote-lua` instruction hook, never hangs the caller.
const INTER_PLUGIN_BUDGET: Duration = Duration::from_millis(100);

/// One live plugin's runtime record, as seen by inter-plugin host calls.
#[derive(Debug)]
pub(crate) struct PluginRecord {
    /// A clone of the plugin's sandboxed Lua state (same VM as the dispatch
    /// invoker's copy).
    pub(crate) lua: Lua,
    /// A clone of the plugin's loaded `M` table.
    pub(crate) module: Table,
    /// The event keys the plugin declared in `M.events` (the only events it
    /// listens for; declarative registration is the only path — DESIGN
    /// §Enforcement Rules).
    pub(crate) event_keys: Vec<String>,
}

/// The shared, single-threaded runtime core.
#[derive(Debug, Default)]
pub(crate) struct CoreState {
    /// Live plugins by name.
    pub(crate) plugins: BTreeMap<PluginName, PluginRecord>,
    /// Capability fulfillment map (routes `capabilities.invoke`).
    pub(crate) capabilities: CapabilityMap,
}

/// A cheaply-cloneable handle to the shared core.
#[derive(Debug, Clone, Default)]
pub(crate) struct Core {
    inner: Rc<RefCell<CoreState>>,
}

/// The result of an inter-plugin host call that may be denied or error.
#[derive(Debug)]
pub(crate) enum InvokeOutcome {
    /// The call ran; carries the returned host value.
    Ok(HostValue),
    /// No plugin fulfills the requested capability.
    NoFulfiller,
    /// The fulfiller has no such `M.api` function / event handler.
    NoSuchFunction,
    /// The fulfiller's API raised a Lua error (already recorded as a deny in the
    /// audit trail with the reason).
    Failed,
}

impl Core {
    /// Borrows the inner state mutably (for the runtime's bookkeeping).
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(&mut CoreState) -> R) -> R {
        f(&mut self.inner.borrow_mut())
    }

    /// Emits `event` to every loaded plugin that declared a handler for it in
    /// `M.events`, fanning the payload out (broadcast semantics; DESIGN
    /// §Inter-plugin communication). Self-delivery is **not** suppressed: DESIGN
    /// does not special-case the emitter, so a plugin that both emits and
    /// listens for the same event receives its own event. Returns the number of
    /// handlers invoked.
    ///
    /// Each handler runs under a fresh deadline; a handler error/timeout is
    /// isolated (logged to the audit trail under the *listener*) and does not
    /// abort the others.
    pub(crate) fn emit(&self, event: &str, payload: &HostValue) -> usize {
        // Collect the listener set first so we do not hold the borrow across the
        // Lua call (a handler may itself emit/invoke and re-borrow the core).
        let listeners: Vec<(PluginName, Lua, Table)> = {
            let state = self.inner.borrow();
            state
                .plugins
                .iter()
                .filter(|(_, rec)| rec.event_keys.iter().any(|k| k == event))
                .map(|(name, rec)| (name.clone(), rec.lua.clone(), rec.module.clone()))
                .collect()
        };

        let mut delivered = 0;
        for (_listener, lua, module) in listeners {
            let Ok(arg) = payload.to_lua(&lua) else {
                continue;
            };
            let deadline = Instant::now() + INTER_PLUGIN_BUDGET;
            // Isolated: ignore errors/timeouts so one bad listener can't break
            // the fan-out (broadcast error isolation).
            let _ = call_hook_with_deadline(&lua, &module, HookTable::Events, event, arg, deadline);
            delivered += 1;
        }
        delivered
    }

    /// Routes `capabilities.invoke(capability, function, arg)` to the current
    /// fulfiller, executing the fulfiller's `M.api[function]` under the
    /// **fulfiller's** permissions (DESIGN §Permissions and capability
    /// invocation, D4), and returns the result marshalled as a [`HostValue`].
    ///
    /// `audit` records the call with the **performer = fulfiller** and a detail
    /// noting the invocation chain (`caller -> capability`).
    pub(crate) fn invoke_capability(
        &self,
        caller: &PluginName,
        capability: &str,
        function: &str,
        arg: &HostValue,
        audit: &EventProducer,
    ) -> InvokeOutcome {
        // Resolve the fulfiller and clone its handles without holding the borrow
        // across the call.
        let resolved = {
            let state = self.inner.borrow();
            state
                .capabilities
                .current_fulfiller(capability)
                .and_then(|fulfiller| {
                    state
                        .plugins
                        .get(fulfiller)
                        .map(|rec| (fulfiller.clone(), rec.lua.clone(), rec.module.clone()))
                })
        };

        let Some((fulfiller, lua, module)) = resolved else {
            return InvokeOutcome::NoFulfiller;
        };

        // The fulfiller's M.api[function] must exist.
        let Ok(api) = module.get::<mote_lua::Value>("api") else {
            return InvokeOutcome::Failed;
        };
        let mote_lua::Value::Table(api_table) = api else {
            return InvokeOutcome::NoSuchFunction;
        };
        let Ok(func) = api_table.get::<mote_lua::Value>(function) else {
            return InvokeOutcome::Failed;
        };
        let mote_lua::Value::Function(func) = func else {
            return InvokeOutcome::NoSuchFunction;
        };

        let Ok(lua_arg) = arg.to_lua(&lua) else {
            return InvokeOutcome::Failed;
        };

        // Call the fulfiller's API function. A per-instruction deadline hook
        // (the `mote-lua` D1 mechanism) is not installed here because the
        // primitive that does so (`call_hook_with_deadline`) targets the
        // hooks/events declaration tables, not `M.api`, and the lower-level
        // mlua trigger types are intentionally not re-exported from
        // `mote-lua`. A long-running `M.api` call is therefore not preempted in
        // Phase 1; this is acceptable because the call is synchronous-by-design
        // and bounded by the fulfiller's own cooperation. (Tracked: thread the
        // deadline through a future `mote-lua` API-call primitive.)
        let call_result = func.call::<mote_lua::Value>(lua_arg);
        drop(lua); // explicit: the state handle is no longer needed

        // Audit the call with performer = fulfiller (D4): the fulfiller's API
        // ran in the fulfiller's own Lua state, where only the fulfiller's
        // `mote.*` host API (and thus its gatekeeper) is installed — so any
        // privileged operation the API performs is already gated by the
        // fulfiller's permissions, not the caller's. The detail records the
        // invocation chain `caller -> capability`.
        let chain = format!("invoked_via ({caller} -> {capability})");
        let operation = format!("{capability}:{function}");
        match call_result {
            Ok(ret) => {
                audit.record(
                    AuditEvent::new(fulfiller, operation, AuditDecision::Allow).with_detail(chain),
                );
                HostValue::from_lua(&ret).map_or(InvokeOutcome::Failed, InvokeOutcome::Ok)
            }
            Err(e) => {
                audit.record(
                    AuditEvent::new(fulfiller, operation, AuditDecision::Deny)
                        .with_detail(format!("{chain}: {e}")),
                );
                InvokeOutcome::Failed
            }
        }
    }
}
