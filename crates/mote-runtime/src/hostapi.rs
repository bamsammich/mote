//! Assembly of the `mote.*` host API exposed to a plugin's Lua state.
//!
//! Every privileged surface here is gated by the plugin's **effective**
//! permissions through a [`Gatekeeper`](mote_permissions::Gatekeeper) and
//! audited through [`mote_audit`] with the performer set to *this* plugin. A
//! denied call returns `nil` / `false` / `0` per the API and is recorded as a
//! denial.
//!
//! The surface installed (DESIGN §AI-Native, §Inter-plugin communication, and
//! the `mote.*` examples scattered through the doc):
//!
//! - `mote.permissions.effective()` → array of effective permission strings.
//! - `mote.storage.get/set/delete(key[, value])` → the plugin's own
//!   [`mote_storage::Namespace`], honoring `identity_scope`. Gated by
//!   `storage:persistent`.
//! - `mote.events.emit(name, payload)` → fan out to other plugins' declarative
//!   `M.events` handlers (broadcast). Gated by `events:emit`.
//! - `mote.capabilities.invoke(capability, fn, arg)` → route to the current
//!   fulfiller, executing under the fulfiller's permissions (D4). The
//!   caller-side gate is `events:on` (consumer participation); the fulfiller's
//!   own permissions gate what the call does.
//! - `mote.tabs.list()` → a representative read-only host call gated by
//!   `tabs:list`, returning an (empty) list (the real tab table is
//!   `mote-session`, Phase 2+). Present so the gate-and-audit path has a
//!   concrete host operation in the e2e proof.
//!
//! The same tables are also exposed as bare globals (`permissions`, `events`,
//! `capabilities`, `storage`, `tabs`) because DESIGN's examples call them
//! unqualified (`events.emit(...)`, `tabs.list()`, `permissions.effective()`).

use mote_audit::{AuditEvent, Decision as AuditDecision, EventProducer};
use mote_lua::{Lua, Value};
use mote_permissions::{Decision, Gatekeeper, GrantSetGatekeeper};
use mote_storage::Namespace;
use mote_types::PluginName;

use crate::core::{Core, InvokeOutcome};
use crate::value::HostValue;

/// Everything the `mote.*` closures need, owned per plugin and consumed by
/// [`install`].
pub(crate) struct HostContext {
    /// This plugin's name (the audit performer for its own calls).
    pub(crate) plugin: PluginName,
    /// This plugin's effective-permission gatekeeper.
    pub(crate) gatekeeper: GrantSetGatekeeper,
    /// The flat effective-permission strings for `permissions.effective()`.
    pub(crate) effective: Vec<String>,
    /// The plugin's storage namespace (identity-scoped at construction).
    pub(crate) storage: Namespace,
    /// Audit producer; every privileged call records through it.
    pub(crate) audit: EventProducer,
    /// Shared runtime core for inter-plugin emit / capability invocation.
    pub(crate) core: Core,
}

/// The gate-and-audit unit cloned into each host closure.
///
/// Holds only what a permission check needs: the performer name, the
/// gatekeeper, and the audit sink. All three are cheap to clone.
#[derive(Clone)]
struct Gate {
    plugin: PluginName,
    gatekeeper: GrantSetGatekeeper,
    audit: EventProducer,
}

impl Gate {
    /// Checks `domain:action:resource`, records the decision under this plugin,
    /// and returns whether the call is allowed (only [`Decision::Allow`]).
    fn check(&self, domain: &str, action: &str, resource: &str, detail: Option<String>) -> bool {
        let decision = self.gatekeeper.check(domain, action, resource);
        let audit_decision = match decision {
            Decision::Allow => AuditDecision::Allow,
            Decision::Deny | Decision::Unmatched => AuditDecision::Deny,
        };
        let op = if resource == "*" {
            format!("{domain}:{action}")
        } else {
            format!("{domain}:{action}:{resource}")
        };
        let mut event = AuditEvent::new(self.plugin.clone(), op, audit_decision);
        if let Some(d) = detail {
            event = event.with_detail(d);
        }
        self.audit.record(event);
        decision.is_allowed()
    }
}

/// Installs the `mote.*` host API into `lua` for the plugin described by `ctx`.
///
/// # Errors
///
/// Returns an error string if any table or function cannot be created or a
/// global cannot be set. Never panics.
#[allow(clippy::too_many_lines)]
pub(crate) fn install(lua: &Lua, ctx: HostContext) -> Result<(), String> {
    let HostContext {
        plugin,
        gatekeeper,
        effective,
        storage: storage_ns,
        audit,
        core,
    } = ctx;

    let gate = Gate {
        plugin,
        gatekeeper,
        audit: audit.clone(),
    };

    // --- permissions.effective() -------------------------------------------
    let permissions = lua.create_table().map_err(stringify)?;
    {
        let f = lua
            .create_function(move |lua, ()| {
                let arr = lua.create_table()?;
                for (i, p) in effective.iter().enumerate() {
                    arr.set(i + 1, p.as_str())?;
                }
                Ok(arr)
            })
            .map_err(stringify)?;
        permissions.set("effective", f).map_err(stringify)?;
    }

    // --- storage.get / set / delete ----------------------------------------
    let storage = lua.create_table().map_err(stringify)?;
    {
        let ns = storage_ns.clone();
        let g = gate.clone();
        let get = lua
            .create_function(move |lua, key: String| {
                if !g.check("storage", "persistent", "*", None) {
                    return Ok(Value::Nil);
                }
                match ns.get(&key) {
                    Ok(Some(bytes)) => Ok(Value::String(lua.create_string(&bytes)?)),
                    Ok(None) | Err(_) => Ok(Value::Nil),
                }
            })
            .map_err(stringify)?;
        storage.set("get", get).map_err(stringify)?;
    }
    {
        let ns = storage_ns.clone();
        let g = gate.clone();
        let set = lua
            .create_function(move |_, (key, value): (String, Value)| {
                if !g.check("storage", "persistent", "*", None) {
                    return Ok(false);
                }
                // Accept a Lua string value; other types store as their UTF-8
                // string form via HostValue, falling back to an empty value.
                let bytes: Vec<u8> = match &value {
                    Value::String(s) => s.as_bytes().to_vec(),
                    other => HostValue::from_lua(other)
                        .ok()
                        .and_then(|hv| hv.as_str().map(|s| s.as_bytes().to_vec()))
                        .unwrap_or_default(),
                };
                Ok(ns.set(&key, &bytes).is_ok())
            })
            .map_err(stringify)?;
        storage.set("set", set).map_err(stringify)?;
    }
    {
        let ns = storage_ns; // last use — move
        let g = gate.clone();
        let delete = lua
            .create_function(move |_, key: String| {
                if !g.check("storage", "persistent", "*", None) {
                    return Ok(false);
                }
                Ok(ns.delete(&key).is_ok())
            })
            .map_err(stringify)?;
        storage.set("delete", delete).map_err(stringify)?;
    }

    // --- events.emit(name, payload) ----------------------------------------
    let events = lua.create_table().map_err(stringify)?;
    {
        let core = core.clone();
        let g = gate.clone();
        let emit = lua
            .create_function(move |_, (name, payload): (String, Value)| {
                if !g.check("events", "emit", "*", Some(format!("emit {name}"))) {
                    return Ok(0i64);
                }
                let Ok(hv) = HostValue::from_lua(&payload) else {
                    return Ok(0i64);
                };
                let delivered = core.emit(&name, &hv);
                Ok(i64::try_from(delivered).unwrap_or(i64::MAX))
            })
            .map_err(stringify)?;
        events.set("emit", emit).map_err(stringify)?;
    }

    // --- capabilities.invoke(capability, fn, arg) --------------------------
    let capabilities = lua.create_table().map_err(stringify)?;
    {
        let core = core; // last use — move
        let g = gate.clone();
        let audit = audit; // last use — move
        let invoke = lua
            .create_function(
                move |lua, (capability, function, arg): (String, String, Value)| {
                    if !g.check(
                        "events",
                        "on",
                        "*",
                        Some(format!("invoke {capability}:{function}")),
                    ) {
                        return Ok(Value::Nil);
                    }
                    let Ok(hv) = HostValue::from_lua(&arg) else {
                        return Ok(Value::Nil);
                    };
                    match core.invoke_capability(&g.plugin, &capability, &function, &hv, &audit) {
                        InvokeOutcome::Ok(ret) => Ok(ret.to_lua(lua).unwrap_or(Value::Nil)),
                        // Every failure mode (no fulfiller, function outside the
                        // capability contract, missing function, deadline
                        // timeout, or a Lua error in the fulfiller) surfaces to
                        // the caller as `nil`; the reason is recorded in the
                        // audit trail (S1).
                        InvokeOutcome::NoFulfiller
                        | InvokeOutcome::NotInContract
                        | InvokeOutcome::NoSuchFunction
                        | InvokeOutcome::Timeout
                        | InvokeOutcome::Failed => Ok(Value::Nil),
                    }
                },
            )
            .map_err(stringify)?;
        capabilities.set("invoke", invoke).map_err(stringify)?;
    }

    // --- tabs.list() (representative gated read) ----------------------------
    let tabs = lua.create_table().map_err(stringify)?;
    {
        let g = gate; // last use — move
        let list = lua
            .create_function(move |lua, ()| {
                if !g.check("tabs", "list", "*", None) {
                    return Ok(Value::Nil);
                }
                Ok(Value::Table(lua.create_table()?))
            })
            .map_err(stringify)?;
        tabs.set("list", list).map_err(stringify)?;
    }

    // Assemble mote.* and expose the bare globals DESIGN's examples use.
    mote_set(lua, &permissions, &storage, &events, &capabilities, &tabs)?;

    Ok(())
}

/// Wires the five sub-tables into a `mote` table and also into bare globals.
fn mote_set(
    lua: &Lua,
    permissions: &mote_lua::Table,
    storage: &mote_lua::Table,
    events: &mote_lua::Table,
    capabilities: &mote_lua::Table,
    tabs: &mote_lua::Table,
) -> Result<(), String> {
    let mote = lua.create_table().map_err(stringify)?;
    mote.set("permissions", permissions).map_err(stringify)?;
    mote.set("storage", storage).map_err(stringify)?;
    mote.set("events", events).map_err(stringify)?;
    mote.set("capabilities", capabilities).map_err(stringify)?;
    mote.set("tabs", tabs).map_err(stringify)?;

    let globals = lua.globals();
    globals.set("mote", mote).map_err(stringify)?;
    globals.set("permissions", permissions).map_err(stringify)?;
    globals.set("storage", storage).map_err(stringify)?;
    globals.set("events", events).map_err(stringify)?;
    globals
        .set("capabilities", capabilities)
        .map_err(stringify)?;
    globals.set("tabs", tabs).map_err(stringify)?;
    Ok(())
}

/// Renders any error implementing [`Display`](std::fmt::Display) into a
/// `String` for the `Result<_, String>` host-API surface.
///
/// Takes the error by value so it slots directly into [`Result::map_err`]
/// (`FnOnce(E) -> String`); the body only borrows it to format, hence the
/// targeted lint relaxation — the owned parameter is dictated by the `map_err`
/// signature, not waste.
#[allow(clippy::needless_pass_by_value)]
fn stringify<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}
