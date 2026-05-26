# 0005 — Chrome↔content host bridge: CEF message router with two-layer isolation

- **Status:** Accepted
- **Date:** 2026-05-26
- **Deciders:** Mote maintainer
- **Informed by:** a validation spike — `docs/research/host-bridge-spike.md`

## Context and problem statement

ADR-0003 makes Mote's chrome an HTML/CSS document rendered by CEF and composited around web pages, and requires the chrome to be isolated from untrusted web content. It did **not** specify two load-bearing, security-critical details:

1. **The transport** — how privileged chrome JavaScript talks to the Rust runtime (read tab/workspace state, dispatch navigation, drive plugin `render(host)` → DOM).
2. **The isolation mechanism** — how the privileged bindings are guaranteed reachable *only* from the chrome browser and never from a web page.

This is the crown-jewel attack surface: if web content can reach the bridge, a hostile page gains the runtime's authority. The mechanism must also exist in `cef` 148, which was unproven.

## Decision drivers

- Bidirectional chrome↔Rust messaging with a **closed set of structured operations** (never arbitrary `eval`).
- Privileged bindings reachable **only** from the chrome browser; web content must be denied even if a page tries to call them.
- Available and working in `cef-rs` 148.
- Resistant to misconfiguration (the failure mode must be hard to introduce by accident).
- All `cef::` types stay inside `mote-cef` (DISCIPLINES §1).

## Considered options

1. **CEF message router** (`CefMessageRouterBrowserSide`/`RendererSide`, `window.cefQuery`) — **chosen**. Spike-proven in `cef-rs` 148 (the crate ships `cef::wrapper::message_router`).
2. `ExecuteJavaScript` (Rust→JS) + a custom scheme / `cef_process_message` (JS→Rust) — viable fallback; not needed since option 1 works.
3. Hand-rolled `cef_process_message` protocol — reinvents the router; more code, more bug surface.

## Decision outcome

Adopt the **`cef-rs` 148 message router**, wrapped entirely inside `mote-cef` as a `HostBridge` exposing `window.mote.invoke(op, params)` over `window.cefQuery`. The browser-side handler implements a **closed, enumerated set of structured operations** — never `eval`, never raw-string execution.

**Isolation is enforced in two independent layers, atop Chromium's process-per-site isolation:**

1. A **renderer-side URL/origin gate** in `on_context_created` that installs the `cefQuery`/`window.mote` binding *only* for the chrome document's URL.
2. The **browser-side router attached only to the chrome `Client`**, never to web-content browsers.

The spike confirmed content browsers receive neither `window.mote` nor `window.cefQuery`, and that **both** layers must be disabled to leak the bridge (each control run breached isolation when its layer was removed).

**Discipline:** every page-derived string crossing into the chrome world is inserted via text nodes / structured DOM construction (never `innerHTML`), and the chrome document ships a strict CSP.

**Hard API constraint (the load-bearing mitigation):** because the spike showed the two-place scoping is trivially misconfigured, `mote-cef` MUST make the unscoped/leaky configuration **unrepresentable** — the `HostBridge` is constructed bound to a specific chrome browser handle such that there is no API path to attach the router broadly or install the binding without the chrome-URL gate. Safe-by-construction, not safe-by-convention.

## Consequences

**Good**
- Spike-validated transport that exists in the pinned CEF version.
- Closed structured-op set bounds the attack surface (no `eval`).
- Defense in depth: two independent scoping layers + Chromium process isolation.
- `cef::` stays inside `mote-cef`, preserving DISCIPLINES §1.

**Bad / risks**
- The two-place scoping is easy to misconfigure (proven: removing both layers leaked the bridge instantly). Mitigated by the unrepresentable-misconfiguration API constraint above — which is now a requirement on the `mote-cef` `HostBridge` design and must be covered by tests (incl. a test asserting a content browser cannot reach the bridge).

**Neutral**
- The CSP + structured-DOM sanitization discipline must be enforced and tested at the boundary.

## Links

- [ADR-0003 — Chrome UI as HTML/CSS in CEF (OSR) with a thin wgpu compositor](0003-chrome-ui-html-css-in-cef-with-wgpu-compositor.md)
- DESIGN.md — *Script Injection and Isolated Worlds*; *AI-Native Architecture / MCP* (host-state exposure)
- DISCIPLINES.md §1 — CEF upgrade discipline (`mote-cef` is the only crate touching `cef::`)
- `docs/research/host-bridge-spike.md` — the validating spike
- `docs/plans/02-browser-shell.md` §1.4, §3 — host bridge in the shell architecture
