//! End-to-end dispatch over the real `LuaHookInvoker` (no mock).
//!
//! Proves the production glue: a real sandboxed `LuaJIT` plugin's handler is
//! invoked through the dispatch engine with a HOST-owned payload marshaled into
//! the plugin's state, the four-decision protocol is interpreted from its
//! return value, and `modify` cascades across plugins that live in **separate**
//! Lua states (the per-plugin isolation invariant). The D1 proof: a runaway
//! filter-chain handler is interrupted at the 10ms budget and resolved as
//! `defer` (allowed), never blocking the engine.

use mote_dispatch::{
    ChainResolution, Decision, DispatchEngine, HookType, LuaHookInvoker, LuaMarshal, NullAudit,
    PluginContext, Registration,
};
use mote_lua::{HookTable, Lua, Value, new_sandbox};
use mote_types::PluginName;

fn plugin(name: &str) -> PluginName {
    PluginName::new(name).unwrap()
}

/// The host-owned filter-chain payload: an intercepted request with a URL and
/// the set of header tags accumulated by upstream `modify`s. Plain Rust data —
/// crosses state boundaries freely.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Request {
    url: String,
    headers: Vec<String>,
}

/// Marshals [`Request`] across the Lua boundary and interprets the handler's
/// return as a [`Decision<Request>`] per DESIGN's `{ action = ... }` protocol.
struct RequestMarshal;

impl LuaMarshal<Request> for RequestMarshal {
    fn encode(&self, lua: &Lua, payload: &Request) -> Result<Value, String> {
        let t = lua.create_table().map_err(|e| e.to_string())?;
        t.set("url", payload.url.clone())
            .map_err(|e| e.to_string())?;
        let headers = lua.create_table().map_err(|e| e.to_string())?;
        for (i, h) in payload.headers.iter().enumerate() {
            headers.set(i + 1, h.clone()).map_err(|e| e.to_string())?;
        }
        t.set("headers", headers).map_err(|e| e.to_string())?;
        Ok(Value::Table(t))
    }

    fn decode(&self, _lua: &Lua, value: Value) -> Result<Decision<Request>, String> {
        let Value::Table(t) = value else {
            return Ok(Decision::Defer);
        };
        let action: Option<String> = match t.get::<Value>("action") {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        };
        match action.as_deref() {
            Some("block") => {
                let reason = t.get::<String>("reason").unwrap_or_default();
                Ok(Decision::Block { reason })
            }
            Some("modify") => {
                // The handler returns the full modified request table.
                let url = t.get::<String>("url").unwrap_or_default();
                let mut headers = Vec::new();
                if let Ok(Value::Table(hs)) = t.get::<Value>("headers") {
                    for v in hs.sequence_values::<String>() {
                        headers.push(v.map_err(|e| e.to_string())?);
                    }
                }
                Ok(Decision::Modify {
                    payload: Request { url, headers },
                })
            }
            Some("allow") => Ok(Decision::Allow),
            _ => Ok(Decision::Defer),
        }
    }
}

type Engine = DispatchEngine<Request, LuaHookInvoker<RequestMarshal>, NullAudit>;

fn register_lua(invoker: &mut LuaHookInvoker<RequestMarshal>, name: &str, source: &str) {
    let lua = new_sandbox().expect("sandbox");
    let module: mote_lua::Table = lua.load(source).eval().expect("eval module");
    invoker.register_plugin(
        plugin(name),
        PluginContext {
            lua,
            module,
            table: HookTable::Hooks,
        },
    );
}

fn req() -> Request {
    Request {
        url: "https://tracker.example.com/pixel.gif".into(),
        headers: vec![],
    }
}

#[test]
fn lua_block_decision_blocks_the_chain() {
    let mut invoker = LuaHookInvoker::new(RequestMarshal);
    register_lua(
        &mut invoker,
        "adblock",
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          return { action = "block", reason = "easylist" }
        end }
        return M
    "#,
    );

    let mut engine: Engine = DispatchEngine::new(invoker, NullAudit);
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::new(plugin("adblock")),
        )
        .unwrap();

    let out = engine.dispatch_filter_chain("net:intercept_request", req());
    match out.resolution {
        ChainResolution::Blocked { reason, .. } => assert_eq!(reason, "easylist"),
        ChainResolution::Allowed { .. } => panic!("expected block, got allowed"),
    }
}

#[test]
fn lua_nil_return_is_defer_and_allows() {
    let mut invoker = LuaHookInvoker::new(RequestMarshal);
    register_lua(
        &mut invoker,
        "observer",
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req) end }
        return M
    "#,
    );

    let mut engine: Engine = DispatchEngine::new(invoker, NullAudit);
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::new(plugin("observer")),
        )
        .unwrap();

    let out = engine.dispatch_filter_chain("net:intercept_request", req());
    assert!(matches!(out.resolution, ChainResolution::Allowed { .. }));
}

/// Modify cascades across plugins living in SEPARATE Lua states: `ph` adds a
/// header, `inspector` must observe the modified request (proving the cascade
/// went host-side, not by passing a Lua value between states).
#[test]
fn lua_modify_cascades_across_separate_states() {
    let mut invoker = LuaHookInvoker::new(RequestMarshal);
    register_lua(
        &mut invoker,
        "ph",
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          local headers = req.headers
          headers[#headers + 1] = "DNT:1"
          return { action = "modify", url = req.url, headers = headers }
        end }
        return M
    "#,
    );
    register_lua(
        &mut invoker,
        "inspector",
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req)
          -- Block only if it sees the header ph added upstream.
          for _, h in ipairs(req.headers) do
            if h == "DNT:1" then return { action = "block", reason = "saw-dnt" } end
          end
          return nil
        end }
        return M
    "#,
    );

    let mut engine: Engine = DispatchEngine::new(invoker, NullAudit);
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("ph"), 70),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("inspector"), 50),
        )
        .unwrap();

    let out = engine.dispatch_filter_chain("net:intercept_request", req());
    match out.resolution {
        ChainResolution::Blocked { reason, .. } => assert_eq!(
            reason, "saw-dnt",
            "inspector must have seen ph's modification across states"
        ),
        ChainResolution::Allowed { .. } => panic!("expected block proving cascade, got allowed"),
    }
}

/// **D1 proof, in dispatch context.** A runaway Lua handler on a filter chain
/// is interrupted at the 10ms budget and the chain resolves as `defer`
/// (allowed) — it does NOT block, modify, or hang.
#[test]
fn lua_runaway_filter_handler_times_out_to_defer() {
    let mut invoker = LuaHookInvoker::new(RequestMarshal);
    register_lua(
        &mut invoker,
        "runaway",
        r#"
        local M = {}
        M.hooks = { ["net:intercept_request"] = function(req) while true do end end }
        return M
    "#,
    );

    let mut engine: Engine = DispatchEngine::new(invoker, NullAudit);
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::new(plugin("runaway")),
        )
        .unwrap();

    let started = std::time::Instant::now();
    let out = engine.dispatch_filter_chain("net:intercept_request", req());
    let elapsed = started.elapsed();

    assert!(
        matches!(out.resolution, ChainResolution::Allowed { .. }),
        "runaway must resolve to defer (allowed), got {:?}",
        out.resolution
    );
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "engine must not hang on a runaway handler; took {elapsed:?}"
    );
    assert!(out.auto_disabled.is_empty(), "one timeout does not disable");
}
