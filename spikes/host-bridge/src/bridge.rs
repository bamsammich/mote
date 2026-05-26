//! Shared host-bridge wiring, included by BOTH the browser binary (`main.rs`) and
//! the subprocess `helper.rs` via `#[path = "../bridge.rs"]`.
//!
//! The cef-rs 148 `wrapper::message_router` is a pure-Rust reimplementation of
//! CEF's upstream `CefMessageRouterBrowserSide` / `CefMessageRouterRendererSide`.
//! It wires a JS `window.cefQuery({request, onSuccess, onFailure})` function in the
//! renderer to a Rust `BrowserSideHandler` in the browser process, over CEF process
//! messages. We drive it from the standard CEF handler callbacks.
//!
//! ISOLATION STRATEGY (the security-critical bit):
//!   The renderer-side router's `on_context_created` is what injects `window.cefQuery`
//!   into a V8 context. We GATE that call on the frame URL: only the privileged chrome
//!   document gets the binding. Untrusted web content never has `cefQuery` installed,
//!   so it has no name to call. This is defense-in-depth ON TOP OF Chromium's
//!   process-per-site isolation (chrome and content are distinct renderer processes).

use cef::rc::Rc as _;
use cef::wrapper::message_router::{
    BrowserSideCallback, BrowserSideHandler, BrowserSideRouter, MessageRouterBrowserSide,
    MessageRouterBrowserSideHandlerCallbacks, MessageRouterConfig, MessageRouterRendererSide,
    MessageRouterRendererSideHandlerCallbacks, RendererSideRouter,
};
use cef::{
    wrap_render_process_handler, App, Browser, DictionaryValue, Frame, ImplApp, ImplFrame,
    ImplRenderProcessHandler, ProcessId, ProcessMessage, RenderProcessHandler, V8Context,
    WrapRenderProcessHandler,
};
use std::sync::{Arc, Mutex};

/// The URL of the privileged chrome document. The renderer only installs the
/// bridge into contexts whose frame URL matches this. Anything else (web content)
/// is denied the binding.
pub fn chrome_url() -> String {
    let dir = std::env::var("HOST_BRIDGE_DIR").unwrap_or_else(|_| {
        std::env::current_dir().unwrap().to_string_lossy().into_owned()
    });
    format!("file://{dir}/chrome/chrome.html")
}

/// Shared router config (must be identical on both sides).
pub fn router_config() -> MessageRouterConfig {
    MessageRouterConfig::default() // window.cefQuery / window.cefQueryCancel
}

// =====================================================================================
// BROWSER PROCESS SIDE
// =====================================================================================

/// The privileged operation handler. In production this is the permission-dispatch
/// layer that fans structured ops out to Lua/WASM. Here it answers `list_tabs`.
pub struct MoteOpHandler;

impl BrowserSideHandler for MoteOpHandler {
    fn on_query_str(
        &self,
        _browser: Option<Browser>,
        _frame: Option<Frame>,
        _query_id: i64,
        request: &str,
        _persistent: bool,
        callback: Arc<Mutex<dyn BrowserSideCallback>>,
    ) -> bool {
        // Parse the STRUCTURED request: { "op": "...", "params": {...} }.
        // (Hand-rolled tiny parse to keep the spike dependency-free; production uses serde.)
        let op = extract_json_string_field(request, "op").unwrap_or_default();
        let cb = callback.lock().unwrap();
        match op.as_str() {
            "list_tabs" => {
                // A structured response. The verb is fixed; we never eval the request.
                let resp = r#"{"tabs":["motesh.dev — themes","example.com","docs"],"handled_by":"rust:MoteOpHandler"}"#;
                cb.success_str(resp);
                true
            }
            other => {
                cb.failure(404, &format!("unknown op: {other}"));
                true
            }
        }
    }
}

// =====================================================================================
// RENDERER PROCESS SIDE — the custom App + RenderProcessHandler
// =====================================================================================

thread_local! {
    /// The renderer-side router is created lazily on the render thread (its methods
    /// must run on the render process main thread).
    static RENDERER_ROUTER: std::cell::RefCell<Option<Arc<RendererSideRouter>>> =
        const { std::cell::RefCell::new(None) };
}

fn renderer_router() -> Arc<RendererSideRouter> {
    RENDERER_ROUTER.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| RendererSideRouter::new(router_config()))
            .clone()
    })
}

wrap_render_process_handler! {
    struct BridgeRenderProcessHandler {}

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            // GATE: only the privileged chrome document gets window.cefQuery.
            let url = frame
                .as_ref()
                .map(|f| cef::CefString::from(&f.url()).to_string())
                .unwrap_or_default();
            // CONTROL KNOB (spike-only): set HOST_BRIDGE_NO_GATE=1 to DISABLE the URL
            // gate and prove it is load-bearing — content then leaks the binding.
            let gate_on = std::env::var("HOST_BRIDGE_NO_GATE").as_deref() != Ok("1");
            if gate_on && url != chrome_url() {
                // Untrusted content (or any non-chrome frame): install NOTHING.
                eprintln!("[renderer] context created for NON-CHROME url={url} -> bridge NOT installed");
                return;
            }
            eprintln!("[renderer] context created for CHROME url={url} -> installing bridge");
            renderer_router().on_context_created(
                browser.map(|b| b.clone()),
                frame.map(|f| f.clone()),
                context.map(|c| c.clone()),
            );
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            renderer_router().on_context_released(
                browser.map(|b| b.clone()),
                frame.map(|f| f.clone()),
                context.map(|c| c.clone()),
            );
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let handled = renderer_router().on_process_message_received(
                browser.map(|b| b.clone()),
                frame.map(|f| f.clone()),
                Some(source_process),
                message.map(|m| m.clone()),
            );
            handled as ::std::os::raw::c_int
        }
    }
}

use cef::{wrap_app, WrapApp};

wrap_app! {
    struct BridgeApp {}

    impl App {
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(BridgeRenderProcessHandler::new())
        }
    }
}

pub fn make_app() -> App {
    BridgeApp::new()
}

/// Build a browser-side router with the MoteOpHandler attached.
pub fn make_browser_side_router() -> Arc<BrowserSideRouter> {
    let router = BrowserSideRouter::new(router_config());
    router.add_handler(Arc::new(MoteOpHandler), false);
    router
}

// re-export the callbacks trait for the Client glue in main.rs
pub use cef::wrapper::message_router::MessageRouterBrowserSideHandlerCallbacks as BrowserSideCallbacks;

// silence unused-import warnings for traits brought in for method resolution
#[allow(unused_imports)]
use {DictionaryValue as _MaybeUnusedDict, ImplApp as _ImplApp, MessageRouterRendererSideHandlerCallbacks as _RR, MessageRouterBrowserSideHandlerCallbacks as _BR};

fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    // crude but sufficient: find "field" : "value"
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let after = after.strip_prefix('"')?;
    let end = after.find('"')?;
    Some(after[..end].to_string())
}
