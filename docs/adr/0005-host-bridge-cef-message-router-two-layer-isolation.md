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
- [ADR-0021 — Test-mode CEF DevTools-Protocol surface](0021-test-mode-cef-devtools-protocol-surface.md) — **intentionally** opens an out-of-band CDP channel that bypasses the isolation guarantee above, confined to dev/test by tested invariants (off-by-default, loopback-only). The one sanctioned relaxation of this ADR's boundary; never enabled in a shipped default run.

## Amendment (2026-05-26): origin/scheme-based gating via a privileged `mote://` scheme

The mechanism above gated the privileged binding on a runtime `chrome_url` string that had to be configured identically in two processes (main + renderer) — stringly-typed agreement across a process boundary, and the one remaining safe-by-convention seam. (A mismatch fails *closed*: the chrome silently loses the bridge rather than leaking it — but it is still fragile and unrepresentable-by-construction was the goal.)

**Resolution (maintainer-approved):** serve the chrome from a fixed, privileged internal scheme — **`mote://chrome`** — registered by `mote-cef` and backed by the embedded chrome assets. The gate matches the document's **origin** (`mote://chrome`), a compile-time constant, not a runtime string:

- There is no `chrome_url` parameter to pass to two places, so nothing can diverge — the fragility is removed by construction.
- Web content is `http(s)` and **cannot be served from the privileged `mote://` scheme**, so origin-based gating is structurally unforgeable — the standard browser approach for internal pages (`chrome://`, `about:`). This is *stronger* than URL-string matching, not merely more convenient.

This **supersedes** the runtime-`chrome_url` URL-string gate in the Decision Outcome. The scheme is registered standard/secure/local; `mote-cef` exposes a host API to register the served chrome resources (fed by `mote-ui`'s embedded assets) without `mote-cef` depending on `mote-ui`. The `mote-cef` HostBridge drops its `chrome_url`/`bootstrap_with_bridge(url)` parameters in favor of the fixed scheme origin. (Also resolves the plan's open D6 — `mote://` vs `file://` chrome serving — in favor of the custom scheme.)

## Amendment (2026-06-09): browser-side router scope = all `mote://chrome` privileged pages (settings included), gated by the type marker

The original design attached the **Layer-2** browser-side router to a single client — the chrome root document (`ChromePage` via `HostBridge::for_chrome`). Settings pages (`mote://chrome/settings/*`), though served from the privileged `mote://chrome` origin and armed with `window.cefQuery` by the **Layer-1** renderer origin gate, were opened as router-less `PageRole::Overlay` content tabs. Result: their queries reached a client with no router and hung — settings section-nav and the in-settings integrity transport (ADR-0017 surfaces) were silently broken.

**Resolution (maintainer-approved):** Layer 2's router is attached to **every privileged `mote://chrome` page — the chrome root *and* the enumerated settings sections** — bringing Layer 2's scope into alignment with Layer 1's renderer origin gate. The two layers now enforce **one consistent rule**: a `mote://chrome` page gets the full bridge (renderer binding + browser-side router); every other origin gets neither.

**Safe-by-construction is preserved — the trust unit is the type marker, not a runtime URL string.** Router attachment remains driven by the `ChromePage` / `PageRole::Chrome` handle (`ChromePageRequest`, unrepresentable for a content `Page`), **not** a runtime `is_chrome_origin(url)` check at browser-side client construction. (`is_chrome_origin` continues to gate Layer 1 on the *committed frame origin*, which the `mote://` scheme factory makes unforgeable.) Settings deep-links are routed onto the bridge-bearing path through the enumerated settings **whitelist** — never a raw `mote://chrome/*` prefix — so a crafted `mote://chrome/<arbitrary>` URL cannot mint a privileged tab outside the settings set (unregistered paths also 404). The runtime-URL-string trust seam the 2026-05-26 amendment closed stays closed.

**Content backstop intact.** Non-`mote://chrome` clients — `http(s)` web content and `mote://overlay` surfaces (the tab picker, the integrity *panel*) — remain router-less. Layer 1's unforgeable committed-origin gate stays the primary boundary; a router is inert on any client whose frame lacks the `cefQuery` binding (which only `mote://chrome` frames receive). Extending the router to trusted settings pages does not lower the ceiling for untrusted content. An adversarial security audit (2026-06-09) found no path from untrusted content to the OpRegistry under these constraints; in particular an `http` page that *iframes* `mote://chrome/settings/*` is blocked because the subframe's query is delivered to the **http** browser's (router-less) client.

**Defense-in-depth hardening (same change set).** That audit also surfaced that the `mote` scheme was registered without the `DISPLAY_ISOLATED`/`LOCAL` option its code comment (and the 2026-05-26 amendment above) claimed — so the engine did not, on its own, refuse `http(s)`→`mote://chrome` framing; that path was held only by the router-per-browser layer below. As part of this work the scheme gains **`DISPLAY_ISOLATED`** (mote pages displayable only from same-scheme content), restoring the documented first line of the framing defense, with a test pinning the registered scheme flags.

**Tested invariant.** The live two-layer isolation test is extended: a `mote://chrome/settings/*` page **can** reach the OpRegistry (proves the fix), while an `http(s)` page **and** a `mote://overlay` page **cannot** (proves the backstop holds), including the http-iframes-`mote://chrome` case.

This is a scope refinement of the existing router mechanism, not a reversal — the decision (two independent layers, safe-by-construction, content-denied) is unchanged. No superseding ADR is warranted.
