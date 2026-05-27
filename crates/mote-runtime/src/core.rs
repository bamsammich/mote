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
use mote_registry::{CapabilityRegistry, Composability, Dispatch};
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
    /// **Exclusive capability**: the single fulfiller ran and returned a value.
    Ok(HostValue),
    /// **Non-exclusive capability (stack / aggregate / fan-out)**: every
    /// fulfiller was called in registration order; the results are collected
    /// as a list. Empty list means all fulfillers returned `nil`.
    ///
    /// The dispatch shape (stack / aggregate / fan-out) tells callers *how* to
    /// interpret the list:
    /// - `stack` (theme:provider): results in priority order; consumer merges
    ///   depth-first.
    /// - `aggregate` (`mcp:server`, `adblock:rule_source`): results are
    ///   concatenated into a unified surface.
    /// - `fan-out` (password-manager-form-services, secret:provider): each
    ///   result is independent; consumer picks the appropriate one.
    Multi {
        /// The registry-declared dispatch shape for this capability.
        dispatch: Dispatch,
        /// Results from each fulfiller, in registration order.  A fulfiller
        /// that times out or errors contributes `HostValue::Nil` so the list
        /// length always equals the number of fulfillers.
        results: Vec<HostValue>,
    },
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

/// Internal result of calling one fulfiller's `M.api[function]`.
///
/// Used by [`Core::call_api_function`] to avoid returning a full
/// [`InvokeOutcome`] from the shared inner primitive.
enum CallResult {
    /// The call succeeded; carries the marshalled return value.
    Ok(HostValue),
    /// The function was not found in `M.api`.
    NoSuchFunction,
    /// The call exceeded its deadline.
    Timeout,
    /// The call failed for another reason; carries a detail string for the
    /// audit record.
    Failed(String),
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

    /// Routes `capabilities.invoke(capability, function, arg)` to the
    /// fulfiller(s), executing each fulfiller's `M.api[function]` under the
    /// **fulfiller's** permissions (DESIGN §Permissions and capability
    /// invocation, D4).
    ///
    /// The dispatch shape is sourced from the capability registry:
    ///
    /// - **Exclusive** capability: a single fulfiller is called; the result is
    ///   returned as [`InvokeOutcome::Ok`].
    /// - **Non-exclusive** capability (`stack` / `aggregate` / `fan-out`): all
    ///   fulfillers are called in registration order; results are collected into
    ///   [`InvokeOutcome::Multi`]. A fulfiller that times out or errors
    ///   contributes `HostValue::Nil` and the audit records the failure; the
    ///   remaining fulfillers are still called (isolation).
    ///
    /// `audit` records each call with the **performer = fulfiller** and a
    /// detail noting the invocation chain (`caller -> capability`).
    pub(crate) fn invoke_capability(
        &self,
        caller: &PluginName,
        capability: &str,
        function: &str,
        arg: &HostValue,
        audit: &EventProducer,
    ) -> InvokeOutcome {
        // Look up the capability registry entry to determine dispatch shape and
        // validate the contract.
        let Some(cap_entry) = self.capabilities.get(capability) else {
            // Unknown capability — no contract → reject. We need a dummy
            // fulfiller name for the audit record; use the caller.
            audit.record(
                AuditEvent::new(
                    caller.clone(),
                    format!("{capability}:{function}"),
                    AuditDecision::Deny,
                )
                .with_detail(format!(
                    "invoked_via ({caller} -> {capability}): \
                     capability is not in the registry"
                )),
            );
            return InvokeOutcome::NotInContract;
        };

        // S1 (confused deputy): validate the function is in the contract BEFORE
        // reaching into any fulfiller's M.api, regardless of dispatch shape.
        let in_contract = cap_entry
            .contract
            .required_api
            .iter()
            .any(|f| f == function);
        if !in_contract {
            // Audit under the caller (no fulfiller is reached).
            audit.record(
                AuditEvent::new(
                    caller.clone(),
                    format!("{capability}:{function}"),
                    AuditDecision::Deny,
                )
                .with_detail(format!(
                    "invoked_via ({caller} -> {capability}): \
                     function `{function}` is not in the capability contract"
                )),
            );
            return InvokeOutcome::NotInContract;
        }

        // `Composability` is #[non_exhaustive]; the wildcard arm handles any
        // future variants introduced before this crate is updated.
        if cap_entry.composability == Composability::Exclusive {
            // Single-fulfiller path (unchanged semantics from Phase 1).
            self.invoke_exclusive(caller, capability, function, arg, audit)
        } else {
            // Multi-fulfiller path (NonExclusive, or any future unknown variant
            // — default to the safe multi-fulfiller path): resolve the declared
            // dispatch shape and call every fulfiller, collecting results.
            let Some(dispatch) = cap_entry.dispatch else {
                // Registry consistency check (from_toml enforces this, so
                // this branch is unreachable in practice; guarded for safety).
                audit.record(
                    AuditEvent::new(
                        caller.clone(),
                        format!("{capability}:{function}"),
                        AuditDecision::Deny,
                    )
                    .with_detail(format!(
                        "invoked_via ({caller} -> {capability}): \
                         non-exclusive capability has no dispatch shape (registry error)"
                    )),
                );
                return InvokeOutcome::Failed;
            };
            self.invoke_non_exclusive(caller, capability, function, arg, audit, dispatch)
        }
    }

    /// Invokes an **exclusive** capability: exactly one fulfiller, returns
    /// [`InvokeOutcome::Ok`] or an error outcome.
    fn invoke_exclusive(
        &self,
        caller: &PluginName,
        capability: &str,
        function: &str,
        arg: &HostValue,
        audit: &EventProducer,
    ) -> InvokeOutcome {
        // Resolve the single fulfiller without holding the borrow across the call.
        let resolved = {
            let state = self.inner.borrow();
            state
                .capabilities
                .exclusive_fulfiller(capability)
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

        let chain = format!("invoked_via ({caller} -> {capability})");
        let operation = format!("{capability}:{function}");

        match Self::call_api_function(&lua, &module, function, arg) {
            CallResult::Ok(ret) => {
                audit.record(
                    AuditEvent::new(fulfiller, operation, AuditDecision::Allow).with_detail(chain),
                );
                InvokeOutcome::Ok(ret)
            }
            CallResult::NoSuchFunction => InvokeOutcome::NoSuchFunction,
            CallResult::Timeout => {
                audit.record(
                    AuditEvent::new(fulfiller, operation, AuditDecision::Deny)
                        .with_detail(format!("{chain}: deadline exceeded, interrupted")),
                );
                InvokeOutcome::Timeout
            }
            CallResult::Failed(reason) => {
                audit.record(
                    AuditEvent::new(fulfiller, operation, AuditDecision::Deny)
                        .with_detail(format!("{chain}: {reason}")),
                );
                InvokeOutcome::Failed
            }
        }
    }

    /// Invokes a **non-exclusive** capability: calls every fulfiller in
    /// registration order and collects results into [`InvokeOutcome::Multi`].
    ///
    /// Errors/timeouts in one fulfiller are isolated (logged, contribute
    /// `HostValue::Nil`) so the remaining fulfillers still run.
    fn invoke_non_exclusive(
        &self,
        caller: &PluginName,
        capability: &str,
        function: &str,
        arg: &HostValue,
        audit: &EventProducer,
        dispatch: Dispatch,
    ) -> InvokeOutcome {
        // Snapshot the fulfiller list without holding the borrow across calls.
        let fulfiller_handles: Vec<(PluginName, Lua, Table)> = {
            let state = self.inner.borrow();
            state
                .capabilities
                .fulfillers_for(capability)
                .iter()
                .filter_map(|fulfiller| {
                    state
                        .plugins
                        .get(fulfiller)
                        .map(|rec| (fulfiller.clone(), rec.lua.clone(), rec.module.clone()))
                })
                .collect()
        };

        if fulfiller_handles.is_empty() {
            return InvokeOutcome::NoFulfiller;
        }

        let chain = format!("invoked_via ({caller} -> {capability})");
        let operation = format!("{capability}:{function}");

        let mut results = Vec::with_capacity(fulfiller_handles.len());
        for (fulfiller, lua, module) in fulfiller_handles {
            match Self::call_api_function(&lua, &module, function, arg) {
                CallResult::Ok(ret) => {
                    audit.record(
                        AuditEvent::new(fulfiller, operation.clone(), AuditDecision::Allow)
                            .with_detail(chain.clone()),
                    );
                    results.push(ret);
                }
                CallResult::NoSuchFunction => {
                    // The fulfiller does not expose this function in M.api.
                    // Contract conformance (step 3) should have caught this;
                    // record as a deny and contribute Nil so the other fulfillers
                    // are still reached.
                    audit.record(
                        AuditEvent::new(fulfiller, operation.clone(), AuditDecision::Deny)
                            .with_detail(format!("{chain}: no such function in M.api")),
                    );
                    results.push(HostValue::Nil);
                }
                CallResult::Timeout => {
                    audit.record(
                        AuditEvent::new(fulfiller, operation.clone(), AuditDecision::Deny)
                            .with_detail(format!("{chain}: deadline exceeded, interrupted")),
                    );
                    results.push(HostValue::Nil);
                }
                CallResult::Failed(reason) => {
                    audit.record(
                        AuditEvent::new(fulfiller, operation.clone(), AuditDecision::Deny)
                            .with_detail(format!("{chain}: {reason}")),
                    );
                    results.push(HostValue::Nil);
                }
            }
        }

        InvokeOutcome::Multi { dispatch, results }
    }

    /// Calls `M.api[function](arg)` in `lua` under a deadline.
    ///
    /// This is the shared low-level call primitive used by both
    /// [`invoke_exclusive`](Self::invoke_exclusive) and
    /// [`invoke_non_exclusive`](Self::invoke_non_exclusive).  It does NOT
    /// perform the S1 contract check (that is the caller's responsibility,
    /// done once before dispatch).
    fn call_api_function(lua: &Lua, module: &Table, function: &str, arg: &HostValue) -> CallResult {
        // M.api and M.api[function] are read RAW: plugin-controlled tables must
        // not be able to intercept lookups via __index (a metamethod here could
        // loop unbounded with no deadline installed).
        let Ok(api) = module.raw_get::<mote_lua::Value>("api") else {
            return CallResult::Failed("cannot read M.api".to_owned());
        };
        let mote_lua::Value::Table(api_table) = api else {
            return CallResult::NoSuchFunction;
        };
        let Ok(func_val) = api_table.raw_get::<mote_lua::Value>(function) else {
            return CallResult::Failed("cannot read M.api[fn]".to_owned());
        };
        let mote_lua::Value::Function(func) = func_val else {
            return CallResult::NoSuchFunction;
        };

        let Ok(lua_arg) = arg.to_lua(lua) else {
            return CallResult::Failed("cannot marshal argument".to_owned());
        };

        // Call under a deadline (S1): a fulfiller that loops or allocates
        // without bound is interrupted rather than hanging the runtime.
        let deadline = Instant::now() + INTER_PLUGIN_BUDGET;
        match call_function_with_deadline(lua, &func, lua_arg, deadline) {
            Ok(ret) => {
                // DEADLINE CONTRACT: protections are lifted; read with raw
                // accessors only (which from_lua does).
                HostValue::from_lua(&ret).map_or_else(
                    |e| CallResult::Failed(format!("cannot marshal return value: {e}")),
                    CallResult::Ok,
                )
            }
            Err(HookInvokeError::Timeout) => CallResult::Timeout,
            Err(e) => CallResult::Failed(e.to_string()),
        }
    }
}
