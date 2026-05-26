//! Host-bridge spike — validates a bidirectional chrome JS <-> Rust bridge for Mote.
//!
//! Goal:
//!   1. Round-trip: chrome JS `window.mote.invoke("list_tabs")` -> Rust handler ->
//!      structured response resolves back in chrome JS (which renders it + sets title).
//!   2. Isolation: a SECOND "content" browser (untrusted web page) must NOT have the
//!      bridge binding and must NOT be able to reach the Rust handler.
//!
//! Transport under test: cef-rs 148 `wrapper::message_router` (the Rust port of CEF's
//! CefMessageRouterBrowserSide / CefMessageRouterRendererSide), driven via a custom
//! `App` + `RenderProcessHandler` (renderer side) and `Client::on_process_message_received`
//! (browser side). The renderer GATES window.cefQuery installation to the chrome URL.
//!
//! Both browsers are windowless (OSR) — reusing the ui-cef-html spike's setup. We
//! observe each browser's outcome out-of-band via `DisplayHandler::on_title_change`.

use cef::rc::Rc as _;
use cef::{
    api_hash, args::Args, browser_host_create_browser_sync, do_message_loop_work, execute_process,
    initialize, shutdown, sys, wrap_client, wrap_display_handler, wrap_render_handler, App, Browser,
    BrowserSettings, CefString, Client, DisplayHandler, ImplBrowser, ImplBrowserHost, ImplClient,
    ImplCommandLine, ImplDisplayHandler, ImplRenderHandler, PaintElementType, ProcessId,
    ProcessMessage, Rect, RenderHandler, Settings, WindowInfo, WrapClient, WrapDisplayHandler,
    WrapRenderHandler,
};
use cef::wrapper::message_router::{BrowserSideRouter, MessageRouterBrowserSideHandlerCallbacks};
use std::cell::RefCell;
use std::rc::Rc as StdRc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[path = "bridge.rs"]
mod bridge;

const W: i32 = 800;
const H: i32 = 600;

/// Observed titles, keyed by a label ("chrome" / "content"). on_title_change writes here.
type TitleLog = Arc<Mutex<Vec<(String, String)>>>;

// ---------- minimal OSR RenderHandler (OSR requires one) ----------
#[derive(Clone)]
struct OsrState {
    paints: StdRc<RefCell<u32>>,
}

wrap_render_handler! {
    struct RenderHandlerBuilder {
        st: OsrState,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(r) = rect { r.x = 0; r.y = 0; r.width = W; r.height = H; }
        }
        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty: Option<&[Rect]>,
            _buffer: *const u8,
            _width: ::std::os::raw::c_int,
            _height: ::std::os::raw::c_int,
        ) {
            *self.st.paints.borrow_mut() += 1;
        }
    }
}

// ---------- DisplayHandler: capture title changes (our result channel) ----------
#[derive(Clone)]
struct TitleState {
    label: String,
    log: TitleLog,
}

wrap_display_handler! {
    struct DisplayHandlerBuilder {
        st: TitleState,
    }
    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(t) = title {
                let s = t.to_string();
                eprintln!("[browser] title({}) = {s}", self.st.label);
                self.st.log.lock().unwrap().push((self.st.label.clone(), s));
            }
        }
    }
}

// ---------- Client ----------
// The chrome client carries the browser-side router (so JS->Rust queries are handled).
// The content client carries NO router -> even if a query message arrived it would be
// dropped; combined with the renderer gate, content has no binding to send one.
#[derive(Clone)]
struct ClientState {
    render: RenderHandler,
    display: DisplayHandler,
    // Some(router) only on the chrome client.
    router: Option<Arc<BrowserSideRouter>>,
}

wrap_client! {
    struct ClientBuilder {
        st: ClientState,
    }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> { Some(self.st.render.clone()) }
        fn display_handler(&self) -> Option<DisplayHandler> { Some(self.st.display.clone()) }
        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            match &self.st.router {
                Some(router) => {
                    let handled = router.on_process_message_received(
                        browser.map(|b| b.clone()),
                        frame.map(|f| f.clone()),
                        source_process,
                        message.map(|m| m.clone()),
                    );
                    handled as ::std::os::raw::c_int
                }
                None => 0, // content client: no router, nothing handled
            }
        }
    }
}
use cef::{Frame, ImplFrame};

fn make_browser(
    url: &str,
    label: &str,
    log: &TitleLog,
    router: Option<Arc<BrowserSideRouter>>,
) -> Browser {
    let render = RenderHandlerBuilder::new(OsrState { paints: StdRc::new(RefCell::new(0)) });
    let display = DisplayHandlerBuilder::new(TitleState { label: label.to_string(), log: log.clone() });
    let mut client = ClientBuilder::new(ClientState { render, display, router });

    let window_info = WindowInfo { windowless_rendering_enabled: 1, ..Default::default() };
    let settings = BrowserSettings { windowless_frame_rate: 30, ..Default::default() };
    browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&settings),
        None,
        None,
    )
    .expect("create OSR browser")
}

fn main() -> std::process::ExitCode {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let cmd = args.as_cmd_line().expect("cmd line");
    let switch = CefString::from("type");
    let is_browser_process = cmd.has_switch(Some(&switch)) != 1;

    // process split — pass the custom App so subprocesses install the renderer bridge.
    let mut app = bridge::make_app();
    let ret = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if !is_browser_process {
        return 0.into();
    }
    assert_eq!(ret, -1, "browser process: execute_process must return -1");

    // Make the chrome URL stable across processes.
    let dir = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    // SAFETY: single-threaded at this point (CEF not yet initialized).
    unsafe { std::env::set_var("HOST_BRIDGE_DIR", &dir); }

    let cache = std::env::current_dir().unwrap().join(".cef-cache");
    let _ = std::fs::create_dir_all(&cache);
    let settings = Settings {
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        no_sandbox: 1,
        root_cache_path: CefString::from(&*cache.to_string_lossy()),
        ..Default::default()
    };
    assert_eq!(
        initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()),
        1,
        "cef initialize failed"
    );

    // Browser-side router lives in the browser process, attached to the chrome client only.
    let router = bridge::make_browser_side_router();

    let log: TitleLog = Arc::new(Mutex::new(Vec::new()));
    let chrome_url = format!("file://{dir}/chrome/chrome.html");
    let content_url = format!("file://{dir}/chrome/content.html");

    // CONTROL KNOB (spike-only): HOST_BRIDGE_CONTENT_ROUTER=1 wires the browser-side
    // router onto the CONTENT client too — simulating a misconfiguration where the
    // privileged binding is NOT scoped to chrome. Combined with HOST_BRIDGE_NO_GATE=1
    // this is the true worst case (both isolation layers off).
    let content_router = if std::env::var("HOST_BRIDGE_CONTENT_ROUTER").as_deref() == Ok("1") {
        Some(router.clone())
    } else {
        None
    };

    let _chrome = make_browser(&chrome_url, "chrome", &log, Some(router.clone()));
    let _content = make_browser(&content_url, "content", &log, content_router);

    // Pump until both browsers report a title outcome, or timeout.
    let start = Instant::now();
    loop {
        do_message_loop_work();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let titles = log.lock().unwrap();
        let chrome_done = titles.iter().any(|(l, t)| l == "chrome" && (t.starts_with("ROUNDTRIP") || t.starts_with("BRIDGE-MISSING")));
        let content_done = titles.iter().any(|(l, t)| l == "content" && (t.starts_with("ISOLATED") || t.starts_with("LEAK") || t.starts_with("PARTIAL")));
        drop(titles);
        if chrome_done && content_done { break; }
        if start.elapsed().as_secs() > 20 {
            eprintln!("TIMEOUT waiting for outcomes");
            break;
        }
    }

    // ---- verdict ----
    let titles = log.lock().unwrap().clone();
    let last = |label: &str| -> Option<String> {
        titles.iter().rev().find(|(l, _)| l == label).map(|(_, t)| t.clone())
    };
    let chrome_t = last("chrome").unwrap_or_else(|| "<none>".into());
    let content_t = last("content").unwrap_or_else(|| "<none>".into());

    println!("\n================ HOST-BRIDGE SPIKE RESULTS ================");
    println!("chrome  final title: {chrome_t}");
    println!("content final title: {content_t}");

    let roundtrip_ok = chrome_t.starts_with("ROUNDTRIP-OK");
    let isolated_ok = content_t.starts_with("ISOLATED");
    println!("--------------------------------------------------------");
    println!("ROUND-TRIP (chrome JS -> Rust -> chrome JS): {}", if roundtrip_ok { "PASS" } else { "FAIL" });
    println!("ISOLATION  (content cannot reach the bridge): {}", if isolated_ok { "PASS" } else { "FAIL" });
    println!("OVERALL: {}", if roundtrip_ok && isolated_ok { "GO" } else { "NO-GO / NEEDS WORK" });
    println!("==========================================================\n");

    // tidy
    if let Some(host) = _chrome.host() { host.close_browser(1); }
    if let Some(host) = _content.host() { host.close_browser(1); }
    for _ in 0..30 {
        do_message_loop_work();
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    shutdown();

    if roundtrip_ok && isolated_ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
