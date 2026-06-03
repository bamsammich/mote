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
//! - `mote.storage.get/set/delete/list_keys(key[, value])` → the plugin's own
//!   [`mote_storage::Namespace`], honoring `identity_scope`. Gated by
//!   `storage:persistent`. `list_keys()` returns a Lua array of the plugin's
//!   storage keys in lexicographic order; returns an empty table on denial or
//!   error.
//! - `mote.events.emit(name, payload)` → fan out to other plugins' declarative
//!   `M.events` handlers (broadcast). Gated by `events:emit`.
//! - `mote.events.collect(name, payload)` → gather contributions from every
//!   subscriber of a **collector** event (ADR-0010), returned as a Lua array of
//!   the subscribers' marshalled returns. Restricted to `Collector`-dispatch
//!   events; gated by `events:emit`. Returns an empty table on denial, an
//!   unknown event, or a non-collector event (default-deny).
//! - `mote.capabilities.invoke(capability, fn, arg)` → route to the current
//!   fulfiller, executing under the fulfiller's permissions (D4). The
//!   caller-side gate is `events:on` (consumer participation); the fulfiller's
//!   own permissions gate what the call does.
//! - `mote.tabs.list()` → a representative read-only host call gated by
//!   `tabs:list`, returning an (empty) list (the real tab table is
//!   `mote-session`, Phase 2+). Present so the gate-and-audit path has a
//!   concrete host operation in the e2e proof.
//! - `mote.secrets.get(name)` → resolves the named secret through the
//!   per-identity [`mote_secrets::SecretResolver`], gated by
//!   `secret:read:<name>`. Returns the secret value as a Lua string on success;
//!   `nil` on denial or resolution failure. No enumeration surface is exposed.
//! - `mote.json.encode(value)` → serializes a Lua value to a JSON string via
//!   `serde_json`. Returns `nil` on unencodable input (functions, userdata, or
//!   any type `HostValue::from_lua` maps to `HostValue::Nil`). Never raises a
//!   Lua error. **No permission gate** — this is a pure data utility with no
//!   I/O, no side effect, and no information disclosure (the plugin already owns
//!   the input). Array vs. object disambiguation follows `HostValue::from_lua`:
//!   a contiguous 1-indexed sequence → JSON array; a string-keyed table → JSON
//!   object.
//! - `mote.json.decode(s)` → parses a JSON string and returns the equivalent
//!   Lua value (objects → tables with string keys; arrays → 1-indexed Lua
//!   sequences; scalars → their Lua equivalents; `null` → `nil`). Returns `nil`
//!   on any failure (non-string input, malformed JSON, depth cap exceeded).
//!   Never raises a Lua error. **No permission gate** (same rationale as
//!   `encode`).
//!
//! The same tables are also exposed as bare globals (`permissions`, `events`,
//! `capabilities`, `storage`, `tabs`) because DESIGN's examples call them
//! unqualified (`events.emit(...)`, `tabs.list()`, `permissions.effective()`).

use std::rc::Rc;

use mote_audit::{AuditEvent, Decision as AuditDecision, EventProducer};
use mote_lua::{Lua, Value};
use mote_permissions::{Decision, Gatekeeper, GrantSetGatekeeper};
use mote_secrets::SecretResolver;
use mote_storage::Namespace;
use mote_types::{PluginName, StatusColor};
use secrecy::ExposeSecret as _;

use crate::core::{Core, InvokeOutcome};
use crate::json::{host_to_json, json_to_host};
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
    /// The per-identity secret resolver for `mote.secrets.get`.
    ///
    /// Uses `Rc` because the runtime core is single-threaded; `unsafe` is
    /// denied workspace-wide so `Arc` is not an option here.  An empty
    /// resolver (no defs) is the safe default when the shell has not yet
    /// supplied the real per-identity resolver.
    pub(crate) resolver: Rc<SecretResolver>,
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
        resolver,
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

    // --- storage.get / set / list_keys / delete ----------------------------
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
        let ns = storage_ns.clone();
        let g = gate.clone();
        let list_keys = lua
            .create_function(move |lua, ()| {
                let arr = lua.create_table()?;
                if !g.check("storage", "persistent", "*", None) {
                    return Ok(arr);
                }
                if let Ok(keys) = ns.list_keys() {
                    for (i, key) in keys.into_iter().enumerate() {
                        arr.set(i + 1, key)?;
                    }
                }
                Ok(arr)
            })
            .map_err(stringify)?;
        storage.set("list_keys", list_keys).map_err(stringify)?;
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

    // --- events.collect(name, payload) -------------------------------------
    //
    // Collector dispatch (ADR-0010): an exclusive provider gathers contributions
    // from every subscriber of a `Collector`-dispatch event, returned as a Lua
    // array (1-indexed) of each subscriber's marshalled return value. Gated by
    // the SAME `events:emit` permission as `emit` (the caller owns/emits the
    // collector surface). Default-deny: missing permission, an unknown event, or
    // a non-collector event all yield an empty table — never a Lua error,
    // matching the host-call denial idiom used everywhere else in this module.
    {
        let core = core.clone();
        let g = gate.clone();
        let audit = audit.clone();
        let collect = lua
            .create_function(move |lua, (name, payload): (String, Value)| {
                let arr = lua.create_table()?;
                if !g.check("events", "emit", "*", Some(format!("collect {name}"))) {
                    return Ok(arr);
                }
                let Ok(hv) = HostValue::from_lua(&payload) else {
                    return Ok(arr);
                };
                // `Core::collect` rejects non-collector / unknown events with an
                // `Err`; the host surface maps that to the empty default-deny
                // table (the rejection condition is observable to tests as "no
                // contributions").
                let Ok(contributions) = core.collect(&name, &hv, &audit) else {
                    return Ok(arr);
                };
                for (i, hv) in contributions.iter().enumerate() {
                    // 1-indexed Lua array. A contribution that cannot be
                    // re-materialized (OOM) becomes `nil` rather than aborting.
                    let lv = hv.to_lua(lua).unwrap_or(Value::Nil);
                    arr.set(i + 1, lv)?;
                }
                Ok(arr)
            })
            .map_err(stringify)?;
        events.set("collect", collect).map_err(stringify)?;
    }

    // --- json.encode(value) / json.decode(s) ----------------------------------
    //
    // Pure data utility backed by `serde_json`. No permission gate is required
    // or recorded: this call has no I/O, no side effect, and discloses no
    // information the plugin does not already hold (the plugin supplied the
    // input). Adding a gate here by reflex would be wrong — consult the module
    // doc before changing this.
    //
    // Both closures follow the universal host-API idiom: return `nil` on any
    // failure, never raise a Lua error.
    let json = lua.create_table().map_err(stringify)?;
    {
        let encode = lua
            .create_function(move |lua, value: Value| {
                // Unrepresentable Lua types (functions, userdata, threads) are
                // mapped to `HostValue::Nil` by `from_lua`, which then encodes
                // as JSON `null`. To honour the "return nil for unencodable
                // input" contract we treat a `Nil` result from a non-nil Lua
                // input as "unencodable" and return nil.
                let hv = HostValue::from_lua(&value).unwrap_or(HostValue::Nil);
                // A Lua nil *input* is valid — `null` is the correct output.
                // Any other Lua type that collapses to HostValue::Nil (functions,
                // userdata, threads) must return nil instead.
                let is_lua_nil = matches!(value, Value::Nil);
                if matches!(hv, HostValue::Nil) && !is_lua_nil {
                    return Ok(Value::Nil);
                }
                let json_val = host_to_json(&hv);
                serde_json::to_string(&json_val).map_or_else(
                    |_| Ok(Value::Nil),
                    |s| {
                        lua.create_string(&s).map_or_else(
                            // OOM or other Lua-level error: return nil gracefully.
                            |_| Ok(Value::Nil),
                            |ls| Ok(Value::String(ls)),
                        )
                    },
                )
            })
            .map_err(stringify)?;
        json.set("encode", encode).map_err(stringify)?;
    }
    {
        let decode = lua
            .create_function(move |lua, value: Value| {
                // Only string inputs are valid; any other type returns nil.
                let s = match &value {
                    Value::String(s) => match s.to_str() {
                        Ok(s) => s.to_owned(),
                        Err(_) => return Ok(Value::Nil),
                    },
                    _ => return Ok(Value::Nil),
                };
                let json_val: serde_json::Value = match serde_json::from_str(&s) {
                    Ok(v) => v,
                    Err(_) => return Ok(Value::Nil),
                };
                let hv = json_to_host(&json_val);
                Ok(hv.to_lua(lua).unwrap_or(Value::Nil))
            })
            .map_err(stringify)?;
        json.set("decode", decode).map_err(stringify)?;
    }

    // --- capabilities.invoke(capability, fn, arg) --------------------------
    let capabilities = lua.create_table().map_err(stringify)?;
    {
        let core = core.clone();
        let g = gate.clone();
        // `audit` is moved into the closure; this is the last use of the outer
        // `audit` binding (the gate carries its own clone). The `move` closure
        // takes ownership below.
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
                        // Non-exclusive capability: return the collected results
                        // as a Lua table `{ dispatch = "<shape>", results = { … } }`
                        // so the caller knows both the dispatch shape and the
                        // per-fulfiller return values (in registration order).
                        // `nil` entries in the results list mark fulfillers that
                        // timed out or errored (see InvokeOutcome::Multi).
                        // If the table cannot be created (OOM), fall through to `nil`.
                        InvokeOutcome::Multi { dispatch, results } => {
                            // dispatch shape string so the consumer knows how to
                            // interpret the results array.
                            let shape: &str = match dispatch {
                                mote_registry::Dispatch::Stack => "stack",
                                mote_registry::Dispatch::Aggregate => "aggregate",
                                mote_registry::Dispatch::FanOut => "fan-out",
                                // #[non_exhaustive] — future variants
                                _ => "unknown",
                            };
                            // Build the outer envelope; fall back to nil on OOM.
                            let Ok(outer) = lua.create_table() else {
                                return Ok(Value::Nil);
                            };
                            let _ = outer.raw_set("dispatch", shape);
                            // Build the results array; fall back to envelope
                            // without results on OOM.
                            if let Ok(arr) = lua.create_table() {
                                for (i, v) in results.iter().enumerate() {
                                    let lv = v.to_lua(lua).unwrap_or(Value::Nil);
                                    let _ = arr.raw_set(i + 1, lv);
                                }
                                let _ = outer.raw_set("results", arr);
                            }
                            Ok(Value::Table(outer))
                        }
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

    // --- statusline.set(id, payload) (ADR-0016) --------------------------------
    //
    // Updates the mutable fields of a statusline element that was declared in
    // the plugin's `M.statusline` table. `id` is the *unqualified* id (as
    // declared in the table, without the plugin-name prefix): the host
    // automatically prepends `<plugin>.` so a plugin can only update its own
    // elements (typo-protection: an unknown id returns `false`).
    //
    // `payload` is a Lua table with OPTIONAL fields:
    //   `text`    — string; replaces the current text.
    //   `icon`    — string (`"lucide:<name>"`); replaces the current icon.
    //   `color`   — string (`"fg"` | `"accent"` | `"warn"` | `"mute"`).
    //   `tooltip` — string | nil; replaces or clears the tooltip.
    //
    // Reserved v2 fields (`action`, `disabled`) in `payload` are logged as a
    // warning and ignored — the element still updates (forward-compatibility).
    //
    // No permission gate in v0.1: a plugin can always update elements it
    // declared. When `statusline.publish-clickable` is fulfilled (v2), that
    // capability will gate clickable state changes instead.
    let statusline = lua.create_table().map_err(stringify)?;
    {
        let core = core; // last use — move
        let plugin_name_str = gate.plugin.as_str().to_owned();
        let set = lua
            .create_function(move |_, (id, payload): (String, Value)| {
                // Build the fully-qualified id: `<plugin>.<id>`.
                let fq_id = format!("{plugin_name_str}.{id}");

                // Extract payload fields (all optional).
                //
                // We marshal through `HostValue` to stay within the public
                // `mote_lua` API surface (mlua's `LuaString` is not directly
                // coercible to `&str` without going through mlua internals).
                //
                // Signal semantics for each `Option<Option<_>>`:
                //   `Some(Some(v))` — field present, update to `v`.
                //   `None`          — field absent or unknown, no change.
                // For `tooltip` an explicit nil maps to `Some(None)` (clear).
                let (text, icon, color, tooltip) = if let Value::Table(ref t) = payload {
                    // Helper: read a table field, marshal to HostValue, extract
                    // as owned string. Returns `None` if absent or non-string.
                    let sl_str = |key: &str| -> Option<String> {
                        t.raw_get::<Value>(key)
                            .ok()
                            .and_then(|v| HostValue::from_lua(&v).ok())
                            .and_then(|hv| hv.as_str().map(str::to_owned))
                    };

                    let text = sl_str("text").map(Some);
                    let icon = sl_str("icon").map(Some);
                    let color = sl_str("color").and_then(|s| StatusColor::from_wire(&s));
                    // tooltip: explicit nil (`Value::Nil`) → clear; string → set;
                    // absent / other → no change.
                    let tooltip = match t.raw_get::<Value>("tooltip") {
                        Ok(Value::Nil) => Some(None),
                        Err(_) => None,
                        Ok(v) => HostValue::from_lua(&v)
                            .ok()
                            .and_then(|hv| hv.as_str().map(|s| Some(s.to_owned()))),
                    };

                    // Reserved v2 fields: warn + ignore (forward-compat).
                    if !matches!(t.raw_get::<Value>("action"), Ok(Value::Nil) | Err(_)) {
                        log::warn!(
                            "mote.statusline.set(`{id}`): payload field `action` is reserved \
                             for v2 (ADR-0016); ignored in v0.1"
                        );
                    }
                    if !matches!(t.raw_get::<Value>("disabled"), Ok(Value::Nil) | Err(_)) {
                        log::warn!(
                            "mote.statusline.set(`{id}`): payload field `disabled` is reserved \
                             for v2 (ADR-0016); ignored in v0.1"
                        );
                    }

                    (text, icon, color, tooltip)
                } else {
                    (None, None, None, None)
                };

                // Flatten: `Some(Some(s))` → pass the inner `Some(s)` as the
                // "update this field" signal; `None` (key absent) → skip.
                //
                // Security hardening (post-polish-phase security review): if
                // an icon override is being set, validate it against the ADR-
                // 0013 contract here at the host boundary, the same way the
                // load-time statusline validator does. Without this, a plugin
                // could push an `icon = "lucide:bogus"` (or any pack/name
                // outside the bundled set) and the chrome would silently fail
                // to render it. Not a privilege escalation today (the chrome
                // render path is safe), but defense-in-depth: every plugin
                // path that touches an icon string fails closed at v0.1.
                let icon_update = icon.flatten();
                if let Some(ref icon_str) = icon_update
                    && let Err(reason) = crate::runtime::check_statusline_icon_source(icon_str)
                {
                    log::warn!(
                        "mote.statusline.set(`{id}`): icon update rejected — {reason}; \
                         element will not be updated"
                    );
                    return Ok(false);
                }
                let found =
                    core.statusline_set(&fq_id, text.flatten(), icon_update, color, tooltip);

                if !found {
                    log::warn!(
                        "mote.statusline.set(`{id}`): element not declared by this plugin \
                         (fully-qualified id `{fq_id}` not found); call ignored"
                    );
                }

                Ok(found)
            })
            .map_err(stringify)?;
        statusline.set("set", set).map_err(stringify)?;
    }

    // --- tabs.list() (representative gated read) ----------------------------
    let tabs = lua.create_table().map_err(stringify)?;
    {
        let g = gate.clone();
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

    // --- secrets.get(name) (DESIGN §5; gated by secret:read:<name>) ---------
    //
    // Only `get` is installed — no `list`, no enumeration surface, by design
    // (DESIGN §Secret Management: plugins may only pull named secrets they were
    // granted; they may not discover the set of configured secrets).
    //
    // Audit semantics (exactly-once per call):
    //   - Deny path  → Gate::check records a Deny for `secret:read:<name>`.
    //                   No second record is emitted here.
    //   - Allow path → Gate::check records an Allow for `secret:read:<name>`
    //                   with the backend label as the audit detail.
    //   - Resolve-failure after Allow → returns nil; the Allow audit already
    //                   reflects that the grant was given.  A resolution error
    //                   (undefined name / backend error) is not a second event:
    //                   the guard already allowed the call; the failure is a
    //                   runtime/config issue, not a policy event.
    let secrets = lua.create_table().map_err(stringify)?;
    {
        let g = gate; // last use — move
        let get = lua
            .create_function(move |lua, name: String| {
                // Look up the backend label BEFORE the gate check so we can
                // pass it as the audit detail in the Allow path.  `backend_label`
                // does NOT resolve the value — it only reads the def map, which
                // is safe to call at any time.
                let detail = resolver
                    .backend_label(&name)
                    .map(|label| format!("backend:{label}"));

                if !g.check("secret", "read", &name, detail) {
                    return Ok(Value::Nil);
                }

                // Gate allowed the call.  Resolve the value; return nil on any
                // error (undefined name / backend unavailable / I/O error).
                // The plaintext is unwrapped ONLY at the point of constructing
                // the Lua string and never bound to a named variable that
                // outlives that expression.
                match resolver.resolve(&name) {
                    Ok(secret) => {
                        // ExposeSecret unwrap is local to this expression.
                        // The Lua string owns a copy; `secret` is dropped immediately.
                        Ok(Value::String(lua.create_string(secret.expose_secret())?))
                    }
                    Err(_) => Ok(Value::Nil),
                }
            })
            .map_err(stringify)?;
        secrets.set("get", get).map_err(stringify)?;
    }

    // Assemble mote.* and expose the bare globals DESIGN's examples use.
    mote_set(
        lua,
        &permissions,
        &storage,
        &events,
        &capabilities,
        &tabs,
        &secrets,
        &json,
        &statusline,
    )?;

    Ok(())
}

/// Wires the sub-tables into a `mote` table and also into bare globals.
///
/// `json` is wired into `mote.json` only — no bare global is added because
/// `json` is not part of DESIGN's unqualified-global examples and adding one
/// would risk shadowing any plugin-defined local named `json`.
/// `statusline` is similarly wired into `mote.statusline` only — no bare
/// global, because `statusline` is a new API not present in existing plugin
/// examples.
#[allow(clippy::too_many_arguments)]
fn mote_set(
    lua: &Lua,
    permissions: &mote_lua::Table,
    storage: &mote_lua::Table,
    events: &mote_lua::Table,
    capabilities: &mote_lua::Table,
    tabs: &mote_lua::Table,
    secrets: &mote_lua::Table,
    json: &mote_lua::Table,
    statusline: &mote_lua::Table,
) -> Result<(), String> {
    let mote = lua.create_table().map_err(stringify)?;
    mote.set("permissions", permissions).map_err(stringify)?;
    mote.set("storage", storage).map_err(stringify)?;
    mote.set("events", events).map_err(stringify)?;
    mote.set("capabilities", capabilities).map_err(stringify)?;
    mote.set("tabs", tabs).map_err(stringify)?;
    mote.set("secrets", secrets).map_err(stringify)?;
    mote.set("json", json).map_err(stringify)?;
    mote.set("statusline", statusline).map_err(stringify)?;

    let globals = lua.globals();
    globals.set("mote", mote).map_err(stringify)?;
    globals.set("permissions", permissions).map_err(stringify)?;
    globals.set("storage", storage).map_err(stringify)?;
    globals.set("events", events).map_err(stringify)?;
    globals
        .set("capabilities", capabilities)
        .map_err(stringify)?;
    globals.set("tabs", tabs).map_err(stringify)?;
    globals.set("secrets", secrets).map_err(stringify)?;
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
