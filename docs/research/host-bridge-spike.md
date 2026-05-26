# Host-Bridge Spike — chrome↔Rust transport for Mote (basis for ADR-0005)

- **Date:** 2026-05-26
- **Project:** Mote (programmable, AI-native browser in Rust on CEF). Validates the
  security-critical seam by which the privileged HTML/CSS chrome talks to the Rust
  runtime — the gap the ui-cef-html spike left open (it used `innerHTML`, not a message
  router).
- **Crate:** `spikes/host-bridge/` (standalone THROWAWAY — empty `[workspace]`; the root
  workspace also `exclude = ["spikes"]`). mise toolchain, rust 1.95, edition 2024. No
  production crates touched.
- **cef-rs version:** `cef = "148"` → `148.2.0+148.0.8` (Chromium 148, CEF 148.0.8) — the
  version `mote-cef` targets.
- **Status:** **Built, linked, ran OSR off-screen under `DISPLAY=:1 --ozone-platform=x11`.
  Round-trip PASS, isolation PASS. Verdict: GO.**

---

## 1. Transport: the cef-rs 148 message router WORKS (PREFERRED path confirmed)

**cef-rs 148 ships a usable message router — and it is a pure-Rust reimplementation of
CEF's upstream `CefMessageRouterBrowserSide`/`CefMessageRouterRendererSide`, not thin
bindings to the C++ one.** It lives at `cef::wrapper::message_router` and exposes:

| Type / trait | Role |
|---|---|
| `MessageRouterConfig` | `{ js_query_function: "cefQuery", js_cancel_function: "cefQueryCancel", message_size_threshold }`. Same config must be passed to both sides. |
| `BrowserSideRouter` (`MessageRouterBrowserSide`) | Browser-process side. `new(config)`, `add_handler(Arc<dyn BrowserSideHandler>, first)`. |
| `BrowserSideHandler` (trait you implement) | `on_query_str(browser, frame, query_id, request: &str, persistent, callback)` → `bool`; reply via `callback.success_str(&str)` / `success_binary(&[u8])` / `failure(code, msg)`. |
| `RendererSideRouter` (`MessageRouterRendererSide`) | Render-process side. Installs `window.cefQuery` into V8 contexts. |
| `MessageRouter*HandlerCallbacks` | Glue you must call from the standard CEF handlers (see wiring below). |

This maps onto the standard CEF handler callbacks, which cef-rs 148 wraps via macros that
all exist and work: `wrap_app!`, `wrap_render_process_handler!`, `wrap_client!`,
`wrap_render_handler!`, `wrap_display_handler!`.

**Wiring (the production shape `mote-cef` would own):**

- **Renderer (subprocess):** a custom `App` (`wrap_app!`) returns a
  `RenderProcessHandler` (`wrap_render_process_handler!`) that forwards three callbacks to
  a thread-local `RendererSideRouter`:
  - `on_context_created` → `router.on_context_created(...)` — **this is the call that
    injects `window.cefQuery`** (GATED, see §2);
  - `on_context_released` → `router.on_context_released(...)`;
  - `on_process_message_received` → `router.on_process_message_received(...)`.
  The same `App` is passed to `execute_process` in BOTH the browser binary and the
  subprocess helper, so the renderer subprocess installs the handler.
- **Browser process:** a `BrowserSideRouter` with a `BrowserSideHandler` added; the
  chrome `Client::on_process_message_received` (`wrap_client!`) forwards to
  `router.on_process_message_received(...)`.
- **JS surface:** `window.cefQuery({request, onSuccess, onFailure})` is wrapped in a thin
  trusted bootstrap into a structured, promise-returning `window.mote.invoke(op, params)`.

**Evidence — round-trip succeeded (PASS).** `chrome.html` runs on load:
`window.mote.invoke("list_tabs", {window:1})`. The Rust `MoteOpHandler.on_query_str`
parsed the structured request, matched the `list_tabs` verb, and replied
`success_str('{"tabs":[...3 tabs...],"handled_by":"rust:MoteOpHandler"}')`. The chrome
JS `onSuccess` parsed it, rendered it into the DOM, and set `document.title` to
`ROUNDTRIP-OK:3`, observed by the browser process via `DisplayHandler::on_title_change`:

```
[browser] title(chrome) = ROUNDTRIP-OK:3
ROUND-TRIP (chrome JS -> Rust -> chrome JS): PASS
```

This is a full bidirectional JS→Rust→JS round-trip with a structured request and a
structured response. **The fallback (`ExecuteJavaScript` + custom scheme / raw
`cef_process_message`) was NOT needed** — the preferred message router is usable in
cef-rs 148.

### Sharp edges hit (all minor, all `mote-cef`-internal)

- The `wrap_*` macros reference the `Impl*` traits unqualified, so you must
  `use cef::{ImplRenderProcessHandler, ImplDisplayHandler, ImplRenderHandler, ImplCommandLine, ...}`
  into scope or you get `E0405 cannot find trait`.
- The `RenderProcessHandler` callbacks hand you `Option<&mut Browser>` /
  `Option<&mut Frame>` / `Option<&mut V8Context>`, but the router methods want owned
  `Option<Browser>` etc. These are all `RefGuard`-backed and `Clone`, so `.map(|x|
  x.clone())` bridges them cheaply (ref-counted clone).
- `RendererSideRouter::new` returns `Arc<Self>` and must be created on the render thread
  (used a `thread_local!`).
- No examples ship in the crate; the trait docs are the only guide. Once wrapped behind
  `mote-cef` this is a one-time cost.

---

## 2. Isolation: content browser CANNOT reach the bridge (PASS), via TWO independent layers

A second OSR browser loads `content.html` (stand-in for an untrusted web page). It probes
for `window.mote` and `window.cefQuery` and tries to invoke the bridge. **Result: the
binding does not exist and the call has no target.**

```
[renderer] context created for NON-CHROME url=.../content.html -> bridge NOT installed
[browser] title(content) = ISOLATED:no-bridge
ISOLATION (content cannot reach the bridge): PASS
```

Two **independent** isolation layers, each proven load-bearing by control runs:

**Layer 1 — renderer-side URL gate (binding scoping).** The bridge's
`RenderProcessHandler::on_context_created` only calls `router.on_context_created(...)`
(the call that installs `window.cefQuery`) when the frame URL equals the privileged
chrome document URL. For any other URL it installs nothing. So untrusted content has no
`cefQuery` name to call.

**Layer 2 — browser-side router scoping.** The `BrowserSideRouter` is attached only to the
chrome browser's `Client`. The content `Client::on_process_message_received` returns 0
(not handled). So even a query message that somehow originated from content goes nowhere.

These sit ON TOP OF Chromium's process-per-site isolation (chrome and content are distinct
renderer processes with no shared V8 context).

### Control runs prove the gate is real (not an accident of process separation)

| Config | chrome | content | Isolation |
|---|---|---|---|
| **Proper (both layers on)** | `ROUNDTRIP-OK:3` | `ISOLATED:no-bridge` | **PASS** |
| Gate OFF (layer 1 disabled) | `ROUNDTRIP-OK:3` | binding present, but query unhandled (content client has no router) → no response | FAIL (binding leaked) |
| **Worst case: gate OFF + router on content client (both layers disabled)** | `ROUNDTRIP-OK:3` | `LEAK:cefQuery-succeeded:{"tabs":[...],"handled_by":"rust:MoteOpHandler"}` | FAIL (full breach) |

The worst-case row is the strongest evidence the bridge truly works: content received the
**complete structured Rust response** — but ONLY after deliberately disabling both layers.
With either layer in place, content is denied. (Control knobs `HOST_BRIDGE_NO_GATE=1` /
`HOST_BRIDGE_CONTENT_ROUTER=1` are spike-only.)

---

## 3. Discipline (carry into ADR-0005 + DISCIPLINES.md)

- **Structured operations, never arbitrary eval.** The bridge transports a fixed verb +
  JSON-serializable params (`{op, params}`); Rust matches a closed set of ops and never
  evaluates request-supplied code. Unknown ops return a structured `failure(404, ...)`.
  This is the inverse of `ExecuteJavaScript(arbitrary_string)` — there is no string→code
  path from the page into Rust.
- **Sanitize every page-derived string crossing into the chrome world.** The round-trip
  payload here was host-authored, but in production any page-derived string (tab title,
  URL, favicon alt, plugin output) rendered into the chrome DOM is an injection vector
  into the PRIVILEGED document. Responses must be inserted as text nodes / structured DOM
  ops (the ui-cef-html spike's `host:text` escaping), never `innerHTML` of raw strings,
  and the chrome document should carry a strict CSP. The message router helps because the
  response is data the chrome bootstrap parses, not markup it splices.
- **Bindings scoped to chrome only, enforced in TWO places.** The renderer URL gate AND
  the browser-side router attachment must both exclude web content. `mote-cef` should make
  the unscoped path unrepresentable (e.g. the router is only constructible against the
  chrome client / chrome origin), so a future contributor can't accidentally ship the
  "gate OFF" config.

---

## 4. GO / NO-GO + recommendation for ADR-0005

**GO.** The cef-rs 148 message router (`cef::wrapper::message_router`) is the transport: a
full bidirectional chrome-JS→Rust→chrome-JS round-trip succeeded with structured
request/response, and an untrusted content browser was conclusively denied the binding
(`ISOLATED:no-bridge`), with control runs proving the isolation is mechanism, not luck.
**ADR-0005 recommendation:** adopt the cef-rs message router as the host bridge —
`window.mote.invoke(op, params)` wrapping `window.cefQuery`, a `BrowserSideHandler`
implementing a CLOSED set of structured ops (the permission-dispatch layer, never eval),
all wired and hidden inside `mote-cef`. Scope the privileged binding to the chrome
browser in TWO independent places — a renderer-side URL/origin gate in
`on_context_created` AND attaching the `BrowserSideRouter` only to the chrome `Client` —
on top of Chromium's process-per-site isolation, and require text-node/structured DOM
insertion + a chrome-document CSP for all page-derived strings. **Top risk:** the bridge
is the crown-jewel attack surface — the two-place scoping is easy to misconfigure (the
"gate OFF" control breached isolation instantly), so `mote-cef` must make the unscoped
configuration unrepresentable rather than merely discouraged, and the structured-ops +
sanitization discipline is non-negotiable.

---

## Files

- `spikes/host-bridge/Cargo.toml` — standalone, two bins (browser + subprocess helper).
- `spikes/host-bridge/src/bridge.rs` — shared bridge wiring (custom `App`,
  `RenderProcessHandler` with the URL gate, `BrowserSideHandler` = `MoteOpHandler`,
  router constructors). Included by both bins via `#[path]`.
- `spikes/host-bridge/src/main.rs` — browser process: process split, init, chrome+content
  OSR browsers, chrome `Client` forwards to the browser-side router, `DisplayHandler`
  observes outcomes, prints the verdict. Spike-only control knobs for the negative tests.
- `spikes/host-bridge/src/bin/helper.rs` — CEF subprocess: `execute_process` with the same
  custom `App` so the renderer installs the gated bridge.
- `spikes/host-bridge/chrome/chrome.html` — privileged chrome doc; wraps `cefQuery` into
  `window.mote.invoke`, fires the round-trip, reports via `document.title`.
- `spikes/host-bridge/chrome/content.html` — untrusted web-content stand-in; probes for
  and tries to call the bridge; reports `ISOLATED` / `LEAK` via `document.title`.

## Relationship to prior research / ADRs

- `docs/research/ui-spike-cef-html.md` §6 asserted isolation is structural and named the
  message router as the production target but used `innerHTML`. **This spike closes that
  gap with running code:** the message router is confirmed usable in cef-rs 148 and the
  isolation claim is now demonstrated (and its failure modes mapped) rather than asserted.
- ADR-0003 chose HTML/CSS chrome in CEF; its "Bad/risks" bullet on the chrome↔bridge
  boundary is exactly what ADR-0005 should formalize using this spike's transport choice
  and two-layer scoping discipline.
