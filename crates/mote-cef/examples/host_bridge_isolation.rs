//! Host-bridge isolation proof (ADR-0005) — the security-critical integration
//! test the message router can't be exercised by a plain `#[test]` (it needs the
//! real CEF process/subprocess split + a live renderer).
//!
//! It reproduces the spike's two-browser proof against the **production**
//! `mote-cef` public API, now over the privileged `mote://chrome` scheme
//! (ADR-0005 amendment) — there is **no `chrome_url` parameter anywhere**:
//!
//!   1. ROUND-TRIP: the chrome assets are registered with the engine via
//!      `ChromeResources`, and the chrome page is loaded from
//!      `mote://chrome/index.html` (wired via `HostBridge::for_chrome`). It runs
//!      `window.mote.invoke("ping", {n:3})`. The Rust `ping` op replies with
//!      `{"n":3}`; the chrome JS then calls `window.mote.invoke("ack", {got:3})`.
//!      The `ack` op firing with the echoed value proves a full bidirectional
//!      JS→Rust→JS→Rust round-trip with structured data (never eval).
//!
//!   2. SETTINGS (ADR-0005 amendment 2026-06-09): a SECOND privileged
//!      `mote://chrome` page (a settings section) opened via
//!      `HostBridge::open_chrome_page` — its client carries a router wired to the
//!      SAME registry. It calls a uniquely-named `settings_probe` op; that op
//!      firing proves every `mote://chrome` page (root + settings) is
//!      bridge-bearing, not just the root.
//!
//!   3. ISOLATION: a `Content`-role page (an untrusted-web-content stand-in,
//!      loaded from a `data:` URL — it can never carry the `mote://chrome`
//!      origin) probes for `window.mote` / `window.cefQuery` and tries to call a
//!      uniquely-named `content_probe` op. Because the renderer ORIGIN gate
//!      (layer 1) never installs the binding for a non-`mote://chrome` origin AND
//!      the browser-side router (layer 2) is attached only to `mote://chrome`
//!      clients (via the `ChromePageRequest` type marker), the `content_probe` op
//!      MUST NEVER fire.
//!
//! Evidence is collected entirely in Rust via the op registry (atomic flags the
//! op handlers set) — no title side-channel needed. The op handlers are the
//! ground truth: `ping`+`ack` firing == round-trip OK; `content_probe` never
//! firing == content isolated.
//!
//! Run (libcef.so resolves via the crate's `$ORIGIN` rpath; force ozone under X11):
//!
//! ```sh
//! DISPLAY=:1 mise exec -- cargo run -p mote-cef --example host_bridge_isolation -- \
//!     --ozone-platform=x11
//! ```
//!
//! Exit code 0 with `ROUND-TRIP: PASS` and `ISOLATION: PASS` is the evidence.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use mote_cef::{
    ChromePageRequest, ChromeResources, Engine, EngineConfig, HostBridge, OpRegistry, OpResponse,
    Page, PageOptions, PageRole, ProcessRole, chrome_url,
};

// The privileged chrome document, served from `mote://chrome`. A strict CSP
// (script only from this trusted inline bootstrap; connect/default scoped to the
// `mote:` scheme) + the structured window.mote wrapper. The bootstrap fires a
// real round-trip on load.
const CHROME_HTML: &str = "<!doctype html><html><head>\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self' mote:; script-src 'unsafe-inline'; connect-src mote:\">\
</head><body><div id=out>pending</div><script>\
(function(){\
if(typeof window.cefQuery!=='function'){document.getElementById('out').textContent='NO-CEFQUERY';return;}\
window.mote={invoke:function(op,params){return new Promise(function(res,rej){\
window.cefQuery({request:JSON.stringify({op:op,params:params||{}}),\
onSuccess:function(r){try{res(JSON.parse(r));}catch(e){res(r);}},\
onFailure:function(c,m){rej({code:c,message:m});}});});}};\
window.mote.invoke('ping',{n:3}).then(function(r){\
return window.mote.invoke('ack',{got:r.n});\
}).then(function(){document.getElementById('out').textContent='done';})\
.catch(function(e){document.getElementById('out').textContent='err';});\
})();</script></body></html>";

// Untrusted web content stand-in. Probes for the privileged binding and tries to
// reach a uniquely-named op. If isolation holds it finds nothing reachable.
const CONTENT_HTML: &str = "<!doctype html><html><body><div id=out>content</div><script>\
(function(){\
var hasMote=(typeof window.mote==='object'&&window.mote!==null);\
var hasCef=(typeof window.cefQuery==='function');\
try{\
if(hasCef){window.cefQuery({request:JSON.stringify({op:'content_probe',params:{}}),\
onSuccess:function(){},onFailure:function(){}});}\
else if(hasMote){window.mote.invoke('content_probe',{});}\
}catch(e){}\
document.getElementById('out').textContent=(!hasMote&&!hasCef)?'isolated':'LEAK';\
})();</script></body></html>";

// A privileged SETTINGS page (ADR-0005 amendment 2026-06-09). Served from the
// same `mote://chrome` origin as the root, opened via `HostBridge::open_chrome_page`
// so its client carries a router wired to the SAME OpRegistry. It fires a
// uniquely-named `settings_probe` op; that op firing proves a second bridge-bearing
// `mote://chrome` page reaches the registry (the fix), while content stays isolated.
const SETTINGS_HTML: &str = "<!doctype html><html><head>\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self' mote:; script-src 'unsafe-inline'; connect-src mote:\">\
</head><body><div id=out>pending</div><script>\
(function(){\
if(typeof window.cefQuery!=='function'){document.getElementById('out').textContent='NO-CEFQUERY';return;}\
window.mote={invoke:function(op,params){return new Promise(function(res,rej){\
window.cefQuery({request:JSON.stringify({op:op,params:params||{}}),\
onSuccess:function(r){try{res(JSON.parse(r));}catch(e){res(r);}},\
onFailure:function(c,m){rej({code:c,message:m});}});});}};\
window.mote.invoke('settings_probe',{n:7}).then(function(){\
document.getElementById('out').textContent='done';})\
.catch(function(e){document.getElementById('out').textContent='err';});\
})();</script></body></html>";

fn data_url(html: &str) -> String {
    // The untrusted content stand-in is loaded from a `data:` URL — its origin is
    // opaque and can NEVER be the privileged `mote://chrome` origin, so the
    // renderer origin gate never opens for it.
    format!("data:text/html,{html}")
}

fn pump(engine: &Engine, ms: u64) {
    let start = Instant::now();
    while start.elapsed().as_millis() < u128::from(ms) {
        engine.pump();
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "linear integration-proof narrative; splitting hurts the end-to-end flow"
)]
fn main() -> ExitCode {
    // The chrome page is served from the privileged `mote://chrome` origin. The
    // origin gate is a compile-time constant — there is NO chrome_url parameter to
    // pass to bootstrap or the engine, so nothing can diverge across processes.
    let chrome_entry = chrome_url("index.html");

    // STEP 1: process split. Subprocesses install the ORIGIN-gated renderer
    // handler (isolation layer 1) and declare the `mote` scheme. No URL is passed.
    match mote_cef::bootstrap_with_bridge() {
        ProcessRole::Subprocess { exit_code } => {
            return ExitCode::from(u8::try_from(exit_code.clamp(0, 255)).unwrap_or(0));
        }
        ProcessRole::Browser => {}
    }

    // STEP 2: engine. no_sandbox for the headless/dev environment. The bridge App
    // (always installed) declares the `mote` scheme and carries the origin gate.
    let config = EngineConfig {
        no_sandbox: true,
        ..EngineConfig::default()
    };
    let engine = match Engine::init(&config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: engine init: {e}");
            return ExitCode::FAILURE;
        }
    };

    // STEP 2b: the HOST registers the chrome assets served from mote://chrome/...
    // (in production these are mote-ui's embedded assets; mote-cef never depends
    // on mote-ui). An unregistered path would 404.
    engine.register_chrome_resources(
        ChromeResources::new()
            .register("index.html", CHROME_HTML, "text/html; charset=utf-8")
            // A second privileged `mote://chrome` page (a settings section).
            .register(
                "settings/integrity",
                SETTINGS_HTML,
                "text/html; charset=utf-8",
            ),
    );

    // Ground-truth flags the op handlers flip. These are the ONLY evidence
    // channel — there is no way for content to set them without the bridge.
    let ping_n = Arc::new(AtomicI64::new(-1));
    let ack_got = Arc::new(AtomicI64::new(-1));
    let content_reached = Arc::new(AtomicBool::new(false));
    // ADR-0005 amendment: a second bridge-bearing `mote://chrome` page reaching
    // the SAME registry. `settings_probe` firing proves the fix.
    let settings_n = Arc::new(AtomicI64::new(-1));

    // STEP 3: the closed op registry — three structured ops, never eval.
    let registry = {
        let ping_got = Arc::clone(&ping_n);
        let ag = Arc::clone(&ack_got);
        let cr = Arc::clone(&content_reached);
        let sn = Arc::clone(&settings_n);
        OpRegistry::new()
            .register("settings_probe", move |params: &str| {
                // Fired by the second bridge-bearing `mote://chrome` page. Reaching
                // here proves `HostBridge::open_chrome_page` wired its router to the
                // SAME registry.
                let n = extract_i64(params, "n").unwrap_or(-1);
                sn.store(n, Ordering::SeqCst);
                OpResponse::ok("{\"ok\":true}")
            })
            .register("ping", move |params: &str| {
                // JS→Rust proven. Echo the structured `n` back as structured data.
                let n = extract_i64(params, "n").unwrap_or(0);
                ping_got.store(n, Ordering::SeqCst);
                OpResponse::ok(format!("{{\"n\":{n}}}"))
            })
            .register("ack", move |params: &str| {
                // Rust→JS→Rust proven: the chrome JS received `ping`'s response
                // and called back with the echoed value.
                let got = extract_i64(params, "got").unwrap_or(-1);
                ag.store(got, Ordering::SeqCst);
                OpResponse::ok("{\"ok\":true}")
            })
            .register("content_probe", move |_params: &str| {
                // If THIS ever fires, isolation has been breached.
                cr.store(true, Ordering::SeqCst);
                OpResponse::ok("{\"leaked\":true}")
            })
    };
    eprintln!("registered ops: {:?}", registry.op_names());

    // STEP 4: the chrome page, wired through the ONLY constructor. There is no API
    // path to attach this router to a content page.
    let chrome_req = ChromePageRequest::new(
        &chrome_entry,
        &PageOptions {
            width: 320,
            height: 240,
            frame_rate: 30,
            // role is forced to Chrome by ChromePageRequest regardless of this.
            role: PageRole::Chrome,
        },
    );
    let bridge = match HostBridge::for_chrome(chrome_req, registry) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAIL: HostBridge::for_chrome: {e}");
            return ExitCode::FAILURE;
        }
    };
    if bridge.page().role() != PageRole::Chrome {
        eprintln!("FAIL: bridge page is not Chrome role");
        return ExitCode::FAILURE;
    }

    // STEP 4b: a SECOND privileged `mote://chrome` page (a settings section),
    // opened via `HostBridge::open_chrome_page` — the ADR-0005 amendment path.
    // Its client carries a router wired to the SAME registry. Only a
    // `ChromePageRequest` (the type marker) can mint it; a content page cannot.
    let settings_req = ChromePageRequest::new(
        &chrome_url("settings/integrity"),
        &PageOptions {
            width: 320,
            height: 240,
            frame_rate: 30,
            role: PageRole::Chrome,
        },
    );
    let settings = match bridge.open_chrome_page(settings_req) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: HostBridge::open_chrome_page: {e}");
            return ExitCode::FAILURE;
        }
    };
    if settings.role() != PageRole::Chrome {
        eprintln!("FAIL: settings page is not Chrome role");
        return ExitCode::FAILURE;
    }

    // STEP 5: an untrusted content page (default Content role, no bridge).
    let content = match Page::new(
        &data_url(CONTENT_HTML),
        &PageOptions {
            width: 320,
            height: 240,
            frame_rate: 30,
            role: PageRole::Content,
        },
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: content page: {e}");
            return ExitCode::FAILURE;
        }
    };

    // STEP 6: pump until the chrome round-trip completes (ping + ack) AND the
    // second bridge-bearing settings page reaches the registry (settings_probe),
    // or timeout. Content runs its probe on load in the same window.
    let start = Instant::now();
    loop {
        engine.pump();
        std::thread::sleep(Duration::from_millis(5));
        let done = ping_n.load(Ordering::SeqCst) >= 0
            && ack_got.load(Ordering::SeqCst) >= 0
            && settings_n.load(Ordering::SeqCst) >= 0;
        if done {
            break;
        }
        if start.elapsed().as_secs() > 20 {
            eprintln!("TIMEOUT waiting for chrome round-trip / settings probe");
            break;
        }
    }
    // Give content's probe ample time to (fail to) reach the bridge.
    pump(&engine, 800);

    let ping_got = ping_n.load(Ordering::SeqCst);
    let ag = ack_got.load(Ordering::SeqCst);
    let settings_got = settings_n.load(Ordering::SeqCst);
    let leaked = content_reached.load(Ordering::SeqCst);

    let roundtrip_ok = ping_got == 3 && ag == 3;
    let settings_ok = settings_got == 7;
    let isolated_ok = !leaked;

    println!("\n================ HOST-BRIDGE ISOLATION PROOF ================");
    println!("ping op got n         = {ping_got} (expected 3)");
    println!("ack  op got got       = {ag} (expected 3, proves Rust->JS->Rust)");
    println!("settings_probe got n  = {settings_got} (expected 7, 2nd bridge-bearing page)");
    println!("content_probe op fired = {leaked} (expected false)");
    println!("-------------------------------------------------------------");
    println!(
        "ROUND-TRIP (chrome JS -> Rust -> chrome JS -> Rust): {}",
        if roundtrip_ok { "PASS" } else { "FAIL" }
    );
    println!(
        "SETTINGS   (2nd mote://chrome page reaches registry): {}",
        if settings_ok { "PASS" } else { "FAIL" }
    );
    println!(
        "ISOLATION  (content cannot reach the bridge):        {}",
        if isolated_ok { "PASS" } else { "FAIL" }
    );
    println!(
        "OVERALL: {}",
        if roundtrip_ok && settings_ok && isolated_ok {
            "GO"
        } else {
            "NO-GO"
        }
    );
    println!("=============================================================\n");

    // Tidy up.
    bridge.page().close();
    settings.close();
    content.close();
    for _ in 0..30 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(3));
    }
    engine.shutdown();

    if roundtrip_ok && settings_ok && isolated_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Extract a top-level integer `"field": <int>` from a tiny JSON object. The
/// params are host-bootstrap-authored, so a minimal parse is sufficient.
fn extract_i64(json: &str, field: &str) -> Option<i64> {
    let key = format!("\"{field}\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(after.len());
    after[..end].parse().ok()
}
