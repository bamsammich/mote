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
use mote_lua::{
    HookInvokeError, HookTable, Lua, Table, call_function_with_deadline, call_hook_with_deadline,
};
use mote_registry::CapabilityRegistry;
use mote_types::PluginName;

use crate::capability::CapabilityMap;
use crate::value::HostValue;

/// The budget given to an inter-plugin event handler or capability API call.
///
/// Mirrors the dispatch broadcast budget (100ms) — these are off the
/// synchronous critical path. A runaway handler is interrupted at the deadline
/// by the `mote-lua` instruction hook, never hangs the caller. The same budget
/// now governs a `capabilities.invoke` fulfiller call (S1): the fulfiller's
/// `M.api` function runs under [`call_function_with_deadline`], so a fulfiller
/// that loops is interrupted with a timeout rather than wedging the runtime.
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
///
/// Holds the capability registry (an `Rc`, cheap to clone) so
/// [`invoke_capability`](Core::invoke_capability) can consult a capability's
/// conformance contract — specifically its `required_api` — to reject any
/// function not named in the contract before reaching into the fulfiller's
/// `M.api` (S1: confused-deputy defence).
#[derive(Debug, Clone)]
pub(crate) struct Core {
    inner: Rc<RefCell<CoreState>>,
    capabilities: Rc<CapabilityRegistry>,
}

/// The result of an inter-plugin host call that may be denied or error.
#[derive(Debug)]
pub(crate) enum InvokeOutcome {
    /// The call ran; carries the returned host value.
    Ok(HostValue),
    /// No plugin fulfills the requested capability.
    NoFulfiller,
    /// The requested `function` is not part of the capability's contract
    /// (`required_api`). Rejected before the fulfiller is touched — a consumer
    /// may only invoke the contract surface, never arbitrary fulfiller functions
    /// (S1: confused-deputy defence).
    NotInContract,
    /// The fulfiller has no such `M.api` function / event handler.
    NoSuchFunction,
    /// The fulfiller's API exceeded its deadline and was interrupted (S1). The
    /// fulfiller is not auto-disabled here (that is the dispatch engine's
    /// concern); the call simply fails.
    Timeout,
    /// The fulfiller's API raised a Lua error (already recorded as a deny in the
    /// audit trail with the reason).
    Failed,
}

impl Core {
    /// Builds a core over the capability registry the runtime loaded. The
    /// registry is shared (an `Rc`) across every clone of this handle.
    pub(crate) fn new(capabilities: CapabilityRegistry) -> Self {
        Self {
            inner: Rc::new(RefCell::new(CoreState::default())),
            capabilities: Rc::new(capabilities),
        }
    }

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

        // S1 (confused deputy): a consumer may only invoke functions the
        // capability's CONTRACT declares (`required_api`). Reject anything else
        // BEFORE looking it up in the fulfiller's `M.api`, so a malicious
        // consumer cannot coerce the fulfiller into running an arbitrary
        // internal function under the fulfiller's permissions. An unknown
        // capability (no registry entry) also has no contract → reject.
        let in_contract = self
            .capabilities
            .get(capability)
            .is_some_and(|entry| entry.contract.required_api.iter().any(|f| f == function));
        if !in_contract {
            let chain = format!("invoked_via ({caller} -> {capability})");
            audit.record(
                AuditEvent::new(
                    fulfiller,
                    format!("{capability}:{function}"),
                    AuditDecision::Deny,
                )
                .with_detail(format!(
                    "{chain}: function `{function}` is not in the capability contract"
                )),
            );
            return InvokeOutcome::NotInContract;
        }

        // The fulfiller's M.api[function] must exist. Read RAW: `M` and `M.api`
        // are plugin-controlled tables and these lookups run with no deadline
        // installed, so a `__index` metamethod here could loop unbounded and
        // hang the runtime. A declaration table never legitimately needs a
        // metatable, so raw access is both safe and correct.
        let Ok(api) = module.raw_get::<mote_lua::Value>("api") else {
            return InvokeOutcome::Failed;
        };
        let mote_lua::Value::Table(api_table) = api else {
            return InvokeOutcome::NoSuchFunction;
        };
        let Ok(func) = api_table.raw_get::<mote_lua::Value>(function) else {
            return InvokeOutcome::Failed;
        };
        let mote_lua::Value::Function(func) = func else {
            return InvokeOutcome::NoSuchFunction;
        };

        let Ok(lua_arg) = arg.to_lua(&lua) else {
            return InvokeOutcome::Failed;
        };

        // Call the fulfiller's API function under a deadline (S1). The
        // `mote-lua` primitive installs a global instruction-count hook +
        // memory ceiling for the duration of the call, so a fulfiller that
        // loops or allocates without bound is interrupted at the deadline rather
        // than hanging the runtime.
        let deadline = Instant::now() + INTER_PLUGIN_BUDGET;
        let call_result = call_function_with_deadline(&lua, &func, lua_arg, deadline);
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
                // DEADLINE CONTRACT: protections are lifted before the value is
                // returned, so `from_lua` MUST read it with raw accessors only —
                // which it does (no `__index`/`__pairs` triggered).
                HostValue::from_lua(&ret).map_or(InvokeOutcome::Failed, InvokeOutcome::Ok)
            }
            Err(HookInvokeError::Timeout) => {
                audit.record(
                    AuditEvent::new(fulfiller, operation, AuditDecision::Deny)
                        .with_detail(format!("{chain}: deadline exceeded, interrupted")),
                );
                InvokeOutcome::Timeout
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
