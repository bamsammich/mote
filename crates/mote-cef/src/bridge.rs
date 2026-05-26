//! The **host bridge** — the privileged chrome↔Rust transport (ADR-0005).
//!
//! This is Mote's crown-jewel attack surface: the seam by which the privileged
//! HTML/CSS *chrome* document talks to the Rust runtime. If untrusted web
//! *content* could reach it, a hostile page would gain the runtime's authority.
//! The whole point of this module is to make that **impossible by
//! construction**, not merely discouraged.
//!
//! # What it exposes to JavaScript
//!
//! The chrome document gets a `window.mote.invoke(op, params)` function (a thin,
//! trusted bootstrap over CEF's `window.cefQuery`). It serialises a **structured
//! request** `{op, params}` and resolves with a **structured response**. The
//! browser side dispatches `op` against a **closed, registered set** of Rust
//! handlers ([`OpHandler`]). There is **no `eval` / raw-string-execution path**:
//! the request never becomes code, it selects a handler by name. An unknown op
//! returns a structured failure.
//!
//! # Two-layer isolation, safe-by-construction (the load-bearing mitigation)
//!
//! Per ADR-0005 the bridge is scoped to the chrome browser in **two independent
//! layers** on top of Chromium's process-per-site isolation:
//!
//! 1. **Renderer-side URL gate** ([`render_process_app`]'s `on_context_created`):
//!    `window.cefQuery` / `window.mote` is installed *only* into V8 contexts
//!    whose frame URL equals the configured chrome URL. Untrusted content has no
//!    binding name to call.
//! 2. **Browser-side router scoping**: the [`BrowserSideRouter`] is attached
//!    *only* to the chrome browser's `Client` (via [`ChromePage`]). A content
//!    [`crate::Page`]'s client carries no router, so even a stray query message
//!    goes nowhere.
//!
//! The spike (`docs/research/host-bridge-spike.md`) proved **both** layers are
//! load-bearing: disabling either leaks the binding. So the API makes the
//! unscoped configuration *unrepresentable*:
//!
//! - The renderer gate is wired internally whenever a chrome [`App`] is built;
//!   there is no public knob to install the binding without the URL gate.
//! - The browser-side router is only ever attached through [`HostBridge::for_chrome`],
//!   which takes a [`ChromePage`]. A content [`crate::Page`] has **no method** that
//!   yields a router, a [`ChromePage`], or a [`HostBridge`]. There is no API path to
//!   attach the router to a content browser.
//!
//! # Discipline the *caller* must uphold (stated here, enforced chrome-side)
//!
//! The transport carries **structured data**, never markup. But any *page-derived*
//! string that the chrome document later inserts into its own DOM (a tab title, a
//! URL, a favicon alt, plugin output) is an injection vector into the *privileged*
//! document. The chrome bootstrap MUST insert such strings as **text nodes /
//! structured DOM construction**, never `innerHTML` of a raw string, and the
//! chrome document MUST ship a **strict CSP** (no remote script, no inline script
//! beyond the trusted bootstrap). The message router helps: a response is *data*
//! the bootstrap parses, not markup it splices.
#![allow(
    unsafe_code,
    reason = "App / RenderProcessHandler / Client wiring is CEF FFI; contained per DISCIPLINES.md §1"
)]
// The `wrap_*!` macros expand to refcount glue we do not author (an `as_base`
// that transmutes a reference). These lints fire on macro-generated code.
#![allow(
    clippy::transmute_ptr_to_ptr,
    reason = "cef-rs wrap_* macros transmute &base internally; macro-generated, not authored here"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-internal wiring; pub(crate) documents intended visibility"
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use cef::rc::Rc as _;
use cef::wrapper::message_router::{
    BrowserSideCallback, BrowserSideHandler, BrowserSideRouter, MessageRouterBrowserSide,
    MessageRouterBrowserSideHandlerCallbacks, MessageRouterConfig, MessageRouterRendererSide,
    MessageRouterRendererSideHandlerCallbacks, RendererSideRouter,
};
use cef::{
    App, Browser, Client, Frame, ImplApp, ImplClient, ImplFrame, ImplRenderProcessHandler,
    ProcessId, ProcessMessage, RenderHandler, RenderProcessHandler, V8Context, WrapApp, WrapClient,
    WrapRenderProcessHandler, wrap_app, wrap_client, wrap_render_process_handler,
};

use crate::error::Result;

/// Runs a callback body, catching any panic so it never unwinds across the C ABI
/// (UB under `panic = "abort"`). On panic, logs and returns `default`. Mirrors
/// the `ffi` module's guard — the bridge's CEF callbacks need the same shield.
fn guard<T>(default: T, body: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        eprintln!("mote-cef: panic in host-bridge CEF callback was caught and contained");
        default
    })
}

/// The shared message-router configuration. Must be byte-identical on both the
/// renderer and browser sides for the router to pair up. The default installs
/// `window.cefQuery` / `window.cefQueryCancel`; the chrome bootstrap wraps the
/// former into `window.mote.invoke`.
fn router_config() -> MessageRouterConfig {
    MessageRouterConfig::default()
}

// =============================================================================
// Structured operations — the closed dispatch set (never eval).
// =============================================================================

/// A structured response from a host op.
///
/// Carries **data**, not markup: the chrome bootstrap parses it (as JSON) and
/// constructs DOM from it, so a response is never spliced as HTML. Construct via
/// [`OpResponse::ok`] / [`OpResponse::err`].
#[derive(Debug, Clone)]
pub struct OpResponse(OpResponseInner);

#[derive(Debug, Clone)]
enum OpResponseInner {
    /// Success carrying a JSON document (serialised by the caller's handler).
    Ok(String),
    /// Failure carrying an error code + message delivered to JS `onFailure`.
    Err { code: i32, message: String },
}

impl OpResponse {
    /// A successful response whose `json` payload is handed verbatim to the JS
    /// `onSuccess` callback. Handlers serialise their structured result into this
    /// JSON string (e.g. with `serde_json` in the caller crate).
    #[must_use]
    pub fn ok(json: impl Into<String>) -> Self {
        Self(OpResponseInner::Ok(json.into()))
    }

    /// A failure response delivered to the JS `onFailure(code, message)` callback.
    #[must_use]
    pub fn err(code: i32, message: impl Into<String>) -> Self {
        Self(OpResponseInner::Err {
            code,
            message: message.into(),
        })
    }
}

/// A registered host operation: a single named verb the chrome document invokes.
///
/// Implementations receive the **raw structured request string** (the `params`
/// JSON the chrome bootstrap sent) and return a structured [`OpResponse`]. They
/// MUST NOT treat the request as code — match on your op's shape and return data.
///
/// Handlers run on the CEF browser-process UI thread (the engine's pump thread)
/// and must be `Send + Sync` because the router may hold them across threads.
pub trait OpHandler: Send + Sync {
    /// Handle an invocation. `params_json` is the JSON-serialised `params` object
    /// the chrome side passed to `window.mote.invoke(op, params)`.
    fn handle(&self, params_json: &str) -> OpResponse;
}

impl<F> OpHandler for F
where
    F: Fn(&str) -> OpResponse + Send + Sync,
{
    fn handle(&self, params_json: &str) -> OpResponse {
        self(params_json)
    }
}

/// The closed registry of ops.
///
/// Built before the bridge is wired; once attached it is the *only* set of verbs
/// the chrome document can reach. There is no path for the page to add, name, or
/// execute anything outside this map.
#[derive(Default)]
pub struct OpRegistry {
    ops: BTreeMap<String, Arc<dyn OpHandler>>,
}

impl std::fmt::Debug for OpRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpRegistry")
            .field("ops", &self.ops.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl OpRegistry {
    /// A new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` under the verb `op`. A later registration of the same
    /// `op` replaces the earlier one (builder convenience). Returns `self` for
    /// chaining.
    #[must_use]
    pub fn register(mut self, op: impl Into<String>, handler: impl OpHandler + 'static) -> Self {
        self.ops.insert(op.into(), Arc::new(handler));
        self
    }

    /// The set of registered op names (for diagnostics / tests).
    #[must_use]
    pub fn op_names(&self) -> Vec<&str> {
        self.ops.keys().map(String::as_str).collect()
    }

    fn dispatch(&self, op: &str, params_json: &str) -> OpResponse {
        self.ops.get(op).map_or_else(
            || OpResponse::err(404, format!("unknown op: {op}")),
            |handler| handler.handle(params_json),
        )
    }
}

/// The browser-side query handler: parses the structured `{op, params}` request
/// and dispatches against the closed [`OpRegistry`]. Never evaluates the request.
struct RegistryHandler {
    registry: Arc<OpRegistry>,
}

impl BrowserSideHandler for RegistryHandler {
    fn on_query_str(
        &self,
        _browser: Option<Browser>,
        _frame: Option<Frame>,
        _query_id: i64,
        request: &str,
        _persistent: bool,
        callback: Arc<Mutex<dyn BrowserSideCallback>>,
    ) -> bool {
        guard(true, || {
            let parsed = parse_request(request);
            let cb = callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some((op, params)) = parsed else {
                cb.failure(400, "malformed host-bridge request");
                return true;
            };
            match self.registry.dispatch(&op, &params).0 {
                OpResponseInner::Ok(json) => cb.success_str(&json),
                OpResponseInner::Err { code, message } => cb.failure(code, &message),
            }
            true
        })
    }
}

/// Extract `(op, params_json)` from a `{"op":"...","params":{...}}` request.
///
/// Dependency-free, deliberately minimal: it reads the string `op` field and
/// re-extracts the `params` sub-object as a JSON string to hand to the handler.
/// We never interpret the request as anything but `(verb, opaque data)`. Returns
/// `None` if `op` is absent/non-string.
fn parse_request(request: &str) -> Option<(String, String)> {
    let op = json_string_field(request, "op")?;
    let params = json_object_field(request, "params").unwrap_or_else(|| "{}".to_string());
    Some((op, params))
}

/// Find a top-level `"field": "value"` string field. Crude but sufficient: the
/// request is host-bootstrap-authored JSON, not arbitrary content.
fn json_string_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let after = after.strip_prefix('"')?;
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Extract a top-level `"field": { ... }` object as a JSON string (balanced
/// braces). Returns `None` if the field is absent or not an object.
fn json_object_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let bytes = after.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0_i32;
    let mut in_str = false;
    let mut escaped = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[..=idx].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// =============================================================================
// Renderer side — the custom App + RenderProcessHandler that GATES the binding.
// =============================================================================

thread_local! {
    /// The renderer-side router, created lazily on the render-process main thread
    /// (its methods must run there). One per render process.
    static RENDERER_ROUTER: RefCell<Option<Arc<RendererSideRouter>>> =
        const { RefCell::new(None) };
}

fn renderer_router() -> Arc<RendererSideRouter> {
    RENDERER_ROUTER.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| RendererSideRouter::new(router_config()))
            .clone()
    })
}

wrap_render_process_handler! {
    struct BridgeRenderProcessHandler {
        chrome_url: String,
    }

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            guard((), || {
                // LAYER 1 — the renderer URL gate. on_context_created is the call
                // that installs window.cefQuery into a V8 context. We invoke it
                // ONLY for the privileged chrome document's frame URL. Every other
                // frame (untrusted content, subframes) gets NOTHING — no binding
                // name to call. There is no code path here that installs the
                // binding without first matching the gate.
                let url = frame
                    .as_ref()
                    .map(|f| cef::CefString::from(&f.url()).to_string())
                    .unwrap_or_default();
                if url != self.chrome_url {
                    return;
                }
                renderer_router().on_context_created(
                    browser.map(|b| b.clone()),
                    frame.map(|f| f.clone()),
                    context.map(|c| c.clone()),
                );
            });
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            guard((), || {
                renderer_router().on_context_released(
                    browser.map(|b| b.clone()),
                    frame.map(|f| f.clone()),
                    context.map(|c| c.clone()),
                );
            });
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            guard(0, || {
                let handled = renderer_router().on_process_message_received(
                    browser.map(|b| b.clone()),
                    frame.map(|f| f.clone()),
                    Some(source_process),
                    message.map(|m| m.clone()),
                );
                ::std::os::raw::c_int::from(handled)
            })
        }
    }
}

wrap_app! {
    struct BridgeApp {
        chrome_url: String,
    }

    impl App {
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            guard(None, || {
                Some(BridgeRenderProcessHandler::new(self.chrome_url.clone()))
            })
        }
    }
}

/// Build the custom CEF [`App`] that installs the renderer-side bridge gated to
/// `chrome_url`. The **same** app must be passed to `execute_process` (so the
/// renderer subprocess installs the gated `RenderProcessHandler`) and to
/// `initialize` (browser process). `mote-cef` wires this internally:
/// [`crate::bootstrap_with_bridge`] (subprocess) and [`crate::Engine::init`]
/// (browser) both call here. There is no public API to obtain an app that
/// installs the binding *without* the URL gate — the gate is unconditional.
pub(crate) fn render_process_app(chrome_url: &str) -> App {
    BridgeApp::new(chrome_url.to_string())
}

// =============================================================================
// Browser side — the chrome Client that carries the router (LAYER 2).
// =============================================================================

/// A wired chrome [`Client`] that forwards `on_process_message_received` to the
/// browser-side router. Content clients (built in `ffi`) carry no router.
#[derive(Clone)]
struct ChromeClientState {
    /// The inner content-client handlers (render/load/request) we wrap.
    inner: Client,
    router: Arc<BrowserSideRouter>,
}

wrap_client! {
    struct ChromeClient {
        state: ChromeClientState,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            guard(None, || self.state.inner.render_handler())
        }

        fn load_handler(&self) -> Option<cef::LoadHandler> {
            guard(None, || self.state.inner.load_handler())
        }

        fn request_handler(&self) -> Option<cef::RequestHandler> {
            guard(None, || self.state.inner.request_handler())
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            guard(0, || {
                // LAYER 2 — the router is attached ONLY to this chrome client.
                let handled = self.state.router.on_process_message_received(
                    browser.map(|b| b.clone()),
                    frame.map(|f| f.clone()),
                    source_process,
                    message.map(|m| m.clone()),
                );
                ::std::os::raw::c_int::from(handled)
            })
        }
    }
}

/// Wrap a content [`Client`] into a chrome client carrying `router`. Crate-internal:
/// only [`HostBridge::for_chrome`] reaches here, and only for a [`ChromePage`].
pub(crate) fn chrome_client(inner: Client, router: Arc<BrowserSideRouter>) -> Client {
    ChromeClient::new(ChromeClientState { inner, router })
}

/// Build a browser-side router with the registry's dispatch handler attached.
pub(crate) fn browser_side_router(registry: Arc<OpRegistry>) -> Arc<BrowserSideRouter> {
    let router = BrowserSideRouter::new(router_config());
    router.add_handler(Arc::new(RegistryHandler { registry }), false);
    router
}

// re-export trait so call sites resolve `on_process_message_received` on the router
#[allow(unused_imports)]
use cef::wrapper::message_router::MessageRouterBrowserSideHandlerCallbacks as _RouterCallbacks;

// =============================================================================
// HostBridge — the single safe-by-construction entry point.
// =============================================================================

/// The privileged chrome↔Rust bridge.
///
/// Created **only** via [`HostBridge::for_chrome`], which takes a
/// [`ChromePageRequest`]. Constructing one wires both isolation layers for that
/// chrome browser:
///
/// - the chrome browser's client carries the browser-side router bound to the
///   closed [`OpRegistry`] (layer 2), and
/// - the renderer-side URL gate (layer 1) is already installed for the chrome
///   URL by the [`App`] the engine was initialised with.
///
/// A content [`crate::Page`] cannot produce a [`ChromePageRequest`], a router, or
/// a `HostBridge`, so there is no API path to expose the binding to web content.
///
/// Hold the `HostBridge` for as long as the chrome page is live; dropping it
/// drops the registry reference (the router is owned by the chrome client).
#[derive(Debug)]
pub struct HostBridge {
    chrome: crate::ChromePage,
    #[allow(
        dead_code,
        reason = "retained so the registry outlives the live bridge"
    )]
    registry: Arc<OpRegistry>,
}

impl HostBridge {
    /// Wire the bridge for the chrome page described by `request`, dispatching to
    /// the closed op `registry`.
    ///
    /// This is the *only* constructor. It takes a [`ChromePageRequest`] (only
    /// producible from a [`crate::PageOptions`] whose role is
    /// [`crate::PageRole::Chrome`]): the page's chrome client — carrying the
    /// browser-side router — is created here, so there is no way to attach the
    /// router to a content browser. The renderer URL gate must already be
    /// installed via [`crate::EngineConfig::chrome_url`] /
    /// [`crate::bootstrap_with_bridge`].
    ///
    /// # Errors
    /// [`CefError::BrowserCreate`] if the chrome browser could not be created.
    pub fn for_chrome(request: crate::ChromePageRequest, registry: OpRegistry) -> Result<Self> {
        let registry = Arc::new(registry);
        let router = browser_side_router(Arc::clone(&registry));
        let chrome = request.open(router)?;
        Ok(Self { chrome, registry })
    }

    /// The chrome page this bridge drives.
    #[must_use]
    pub const fn page(&self) -> &crate::ChromePage {
        &self.chrome
    }

    /// The op verbs registered on this bridge (diagnostics / tests).
    #[must_use]
    pub fn op_names(&self) -> Vec<&str> {
        self.registry.op_names()
    }
}

#[cfg(test)]
mod tests {
    //! These cover the CEF-free core: structured-request parsing and the closed
    //! dispatch set (the no-eval guarantee). Live two-layer isolation is proven by
    //! the `host_bridge_isolation` example, which needs the real CEF process split.
    use super::{OpRegistry, OpResponse, OpResponseInner, json_object_field, json_string_field};

    #[test]
    fn parses_top_level_op_field() {
        assert_eq!(
            json_string_field(r#"{"op":"list_tabs","params":{}}"#, "op").as_deref(),
            Some("list_tabs")
        );
    }

    #[test]
    fn extracts_balanced_params_object() {
        let req = r#"{"op":"x","params":{"a":1,"nested":{"b":"}"}}}"#;
        // The `}` inside the string must not end the object early.
        assert_eq!(
            json_object_field(req, "params").as_deref(),
            Some(r#"{"a":1,"nested":{"b":"}"}}"#)
        );
    }

    #[test]
    fn missing_op_is_none() {
        assert!(json_string_field(r#"{"params":{}}"#, "op").is_none());
    }

    #[test]
    fn unknown_op_yields_structured_404_not_eval() {
        let registry = OpRegistry::new().register("known", |_p: &str| OpResponse::ok("{}"));
        // An unregistered verb is rejected as data, never executed.
        match registry.dispatch("definitely_not_registered", "{}").0 {
            OpResponseInner::Err { code, .. } => assert_eq!(code, 404),
            OpResponseInner::Ok(_) => panic!("unknown op must not succeed"),
        }
    }

    #[test]
    fn dispatch_routes_to_registered_handler() {
        let registry = OpRegistry::new().register("echo", |p: &str| OpResponse::ok(p.to_string()));
        match registry.dispatch("echo", r#"{"hi":1}"#).0 {
            OpResponseInner::Ok(json) => assert_eq!(json, r#"{"hi":1}"#),
            OpResponseInner::Err { .. } => panic!("registered op must succeed"),
        }
    }

    #[test]
    fn registry_reports_registered_names() {
        let registry = OpRegistry::new()
            .register("a", |_: &str| OpResponse::ok("{}"))
            .register("b", |_: &str| OpResponse::ok("{}"));
        assert_eq!(registry.op_names(), vec!["a", "b"]);
    }
}
