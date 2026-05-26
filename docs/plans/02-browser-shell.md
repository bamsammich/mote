# Mote — Phase 2 Implementation Plan: The Browser Shell

**Status:** Draft for orchestrator/maintainer review.
**Scope:** ROADMAP Phase 2 — the browser shell. The first phase where Mote becomes a *browser you can launch*: an OS window, a chrome composited around live web pages, a tab model, the three-axis identity/workspace/session state, the integrity panel + permission-approval dialog, and minimal navigation. Builds directly on the Phase-1 foundation (`mote-cef`, `mote-runtime`, `mote-permissions`, `mote-storage`, `mote-audit`, `mote-registry`, `mote-lua`, `mote-dispatch`), all of which are built and green.
**Source of truth:** `DESIGN.md`, `docs/adr/0003-chrome-ui-html-css-in-cef-with-wgpu-compositor.md` (LOCKED chrome architecture), `spec/` (authoritative design system), `docs/research/ui-spike-{cef-html,wgpu}.md`, `docs/plans/00-master-plan.md`. `DISCIPLINES.md` §1/§5/§6 are binding. Where this plan and those documents disagree, those documents win.
**Companion:** `docs/plans/02-risks.md` — read it before starting any work unit. It carries the windowing decision (#1), the urlbar/workspace provider sequencing decision (#8), and the security prerequisites (#9), each flagged for who decides.

> ADR-0003 is LOCKED: the chrome is an HTML/CSS document rendered by a dedicated CEF off-screen browser, composited with each page's OSR texture by a thin wgpu compositor. **This plan does not relitigate that.** It turns the spike (`spikes/ui-cef-html/`, verdict GO) into production crates. The spike's compositor, two-OSR-browser, and `render(host)` learnings are reused verbatim where noted.

---

## Settled Phase-2 decisions

The following decisions were resolved before implementation began. They are recorded here so that work-unit authors do not relitigate them.

| Topic | Decision | Where documented |
|---|---|---|
| **Windowing library** | **winit** — owns the OS window and event loop; provides `raw-window-handle` to the wgpu surface. CEF never opens a top-level window (that would contradict the OSR-compositor architecture). | ADR-0004; `02-risks.md` D1 |
| **wgpu compositor crate** | The compositor lives in **`mote-ui`**, not `mote-cef`. The DISCIPLINES §1 boundary isolates CEF *FFI types*, not wgpu or winit. The one `unsafe` in `mote-ui` is the `raw-window-handle` → `wgpu::Surface` seam, scoped and documented. | §0, §1.1, §2 (this plan) |
| **Chrome ↔ content host-bridge transport** | Pending a spike on the CEF message-router availability in `cef` 148 (W-B2); decision deferred to ADR-0005 after the spike. See `02-risks.md` D12. | ADR-0005 (pending) |
| **Identity implementation** | Chromium profile (`mote-cef::ProfileHandle`, wrapping `RequestContext` with per-identity paths). Isolation is "isolated across [enumerated surfaces]" — see `docs/identity-isolation.md` for the exact boundary; NEVER "fully isolated". | §6.6, `docs/identity-isolation.md` |

---

## 0. Ground rules carried from the contract

These apply to every work unit and are not repeated per task (from `CLAUDE.md` / `00-master-plan.md` §0):

- Edition 2024, MSRV 1.95.0; `[lints] workspace = true`; `unsafe_code = "deny"` everywhere **except `mote-cef`** (CEF FFI types are the only reason that exception exists; the wgpu compositor lives in `mote-ui`, the winit window lives in `mote-shell` — see the crate mapping in §2; `mote-ui` requires one documented `unsafe` exception for the `raw-window-handle` → `wgpu::Surface` seam, scoped and justified the same way `mote-cef`'s FFI allows — see §1.1).
- All tooling through mise (`mise exec -- cargo …`); CI runs clippy `-D warnings`, `cargo test --workspace --all-features`, the CEF import-isolation gate, and the contract-conformance plugin.
- Shared dep versions in `[workspace.dependencies]`; `missing_docs = "warn"` (every public item ships docs from the first commit).
- **The cardinal rule (DISCIPLINES §1):** only `mote-cef` may import `cef`/`cef_rs` — this boundary is specifically about CEF FFI types, not about wgpu or windowing. `wgpu`, `winit`, and `raw-window-handle` are new workspace dependencies; the **wgpu compositor lives in `mote-ui`**, the winit window lives in `mote-shell`. The CEF accelerated-OSR seam (shared-texture handle) touches `mote-cef` because that is a CEF FFI type; the compositor itself does not live there. See §2 for the exact crate responsibilities.
- Branch + PR per work unit; conventional commits; never co-author AI.
- Any feature that writes user data ships the DISCIPLINES §6 PR-description section (what's saved, where, opt-in/out default, how to discover/clear).

---

## 1. Chrome / window / compositor / input architecture (the load-bearing design)

This section is the concrete realization of ADR-0003 and DESIGN §Window model / §Performance Architecture / §UI Composition. It is the part of the plan that, if wrong, costs the most.

### 1.1 The OS window and the wgpu surface

Mote opens **one OS window per browser window** (DESIGN §Window model: "Mote ships as a standard windowed application… the WM places"). The window is created with **`winit`** (decision #1 in `02-risks.md`; resolved to winit, not CEF's native windowing, because ADR-0003 puts the chrome in an *off-screen* CEF browser — Mote owns the surface and CEF never opens its own top-level window). Window geometry is **not** persisted (DESIGN §Session: "the WM handles window placement").

```
winit::Window  ──(raw-window-handle)──▶  wgpu::Surface  ──▶  wgpu::Device/Queue
                                                                  │
                                          the thin compositor (mote-ui::compositor)
                                          composites N OSR textures into the surface
```

- The `winit` event loop is owned by **`mote-shell`** (the composition root). It is the single OS event source.
- The `wgpu` surface, device, queue, and the **compositor** (blit pipeline, reused from `spikes/ui-cef-html/src/blit.wgsl` and the wgpu spike) live in **`mote-ui`**. The compositor is given a `raw-window-handle` from the window and the set of OSR textures to composite each frame. The `raw-window-handle` seam is the one unavoidable `unsafe` interaction outside `mote-cef`; it is isolated to a single documented function in `mote-ui` (wgpu's `create_surface` is `unsafe` on some versions — pin a version where it is safe, or wrap the one call with a justifying comment; flagged in `02-risks.md`).
- The CEF `Engine` runs with `external_message_pump = true` (already the `mote-cef` default). The shell pumps CEF (`Engine::pump`) once per loop iteration *and* on a timer (CEF needs pumping even when no winit events arrive, e.g. for in-flight network/paint), then requests a redraw when any OSR surface reports a new `PaintFrame`.

### 1.2 Chrome in one CEF OSR browser, pages in others

Two **kinds** of off-screen CEF browser (`mote-cef::Page`), distinguished by a new constructor variant:

1. **The chrome browser** — exactly one per window. Loads the Mote chrome HTML/CSS document (the materialized design system, §4). Renders the tab strip, omnibox, sidebar, integrity panel, dialogs, status line. Its viewport region is **transparent** where the page should show through (the spike proved this with `background: transparent` on `[data-slot]` page area). The privileged host bridge / `window.mote` JS bindings are installed **only on this browser** (§3, the security boundary).
2. **The page browsers** — one per *loaded* tab. Each loads a web URL. They get a plain `Client` with **no** host bindings. They are the untrusted surface; Chromium site isolation puts each in its own renderer process.

The chrome document defines the page viewport rect (the `<main data-slot>` element's geometry, read out via the bridge). The compositor composites in this order each frame (chrome-surrounds-content, per the spike's `out.png`):

```
1. clear surface
2. blit the FOCUSED page's OSR texture into the viewport rect the chrome reports
3. blit the chrome OSR texture over the full window (its transparent viewport lets the page through)
4. present
```

A **tab switch is a texture swap** — bind a different page texture in step 2; the chrome texture is reused (the sub-millisecond target, DESIGN §Performance, ADR-0003). Hidden/discarded tabs have **no** page browser alive (DESIGN: renderer destroyed at active→hidden, and discarded after 30 min idle) — they contribute no texture and cost no RAM.

`mote-cef` work: extend `Page`/`PageOptions` so a `Page` can be created as the chrome browser (transparent background, bridge installed) vs a content browser (no bridge). The chrome HTML is loaded from a local resource (a `mote://chrome/` scheme served by a `ResourceInterceptor`, or a bundled `file://` — see §4.4); a `data:`/`file:` load is acceptable for the first slice.

### 1.3 Input routing (winit → chrome browser vs focused page browser)

The single hardest correctness surface in Phase 2. `mote-shell` owns routing; `mote-cef` provides the injection primitives (new): `Page::send_mouse_move`, `send_mouse_click`, `send_mouse_wheel`, `send_key_event`, `send_focus` (thin wrappers over `CefBrowserHost::SendMouseMoveEvent` / `SendMouseClickEvent` / `SendMouseWheelEvent` / `SendKeyEvent` / `SetFocus`).

Routing rule, evaluated per winit input event in `mote-shell`:

- **Mouse events** route by **hit-test against the chrome-defined viewport rect.** If the cursor is inside the page viewport region → translate window coordinates into page-local coordinates (`x - viewport.x`, `y - viewport.y`) and inject into the **focused page browser**. Otherwise → inject into the **chrome browser** at window coordinates (the chrome HTML's own hit-testing/CSS handles tabs, omnibox, sidebar). The viewport rect comes from the chrome bridge (the chrome reports its `<main>` geometry; cached, updated on resize/layout change).
- **Keyboard events** route by **logical focus owner**, tracked in the shell: chrome has focus when the omnibox/palette/a chrome control is focused (the chrome JS tells the shell via the bridge: `mote.host.focusChanged("chrome"|"page")`); the focused page browser has focus otherwise. The shell mirrors focus into CEF with `send_focus` so only one browser thinks it is focused. Chrome keybinds that must always win (e.g. `⌘L` focus omnibox, `⌘T` new tab, `Mod+Space` tab picker) are intercepted in the shell *before* routing to the page (DESIGN §Tab Persistence keybinds, spec tabs/omnibox/sidebar keymaps).
- **Resize** (winit) → resize the wgpu surface, resize the chrome browser to the full window, resize each live page browser to the viewport rect (`CefBrowserHost::WasResized`), and re-read the viewport rect from the chrome.
- **Scroll** inside the viewport → mouse-wheel into the page; over chrome → into chrome.

Coordinate mapping and DPI: winit reports physical pixels and a scale factor; CEF OSR is told a device-scale-factor at `Page` creation. The shell maps logical→physical consistently and passes the same scale to both chrome and page browsers so hit-testing lines up. (Edge cases — drag across the chrome/page boundary, IME, fast resize — are enumerated in `02-risks.md`.)

### 1.4 Privileged chrome ↔ web-content isolation (the security review's mandate)

Isolation is **structural**, exactly as the spike's §6 and ADR-0003 §Consequences require:

- Chrome and each page are **distinct CEF browsers in distinct renderer processes**. They share no DOM, no V8 context, no IPC to each other (only each to the browser process, which mediates). A fully compromised page renderer cannot script the chrome renderer.
- The host bridge / `window.mote` bindings (§3) are registered **only on the chrome browser's** `Client`/`RenderProcessHandler`. Page browsers cannot name `window.mote`.
- **All page-derived strings** (tab titles, URLs, favicons, page-supplied plugin content) are **untrusted** and escaped/sanitized at the host boundary before they enter the chrome DOM — the crown-jewel injection vector (ADR-0003 §Consequences). The chrome document ships a **CSP** (no inline page-controlled script; restrict connect/img sources). DOM mutations from Rust into the chrome go through **structured (non-string) ops via the CEF message router**, not `innerHTML` string concat, wherever the slice handles page-derived data (the spike's escaping discipline becomes a hard rule).
- Plugin injection into *pages* uses per-plugin isolated V8 worlds (DESIGN §Script Injection) — orthogonal to the chrome boundary; it is `mote-runtime`/`mote-cef` territory, not new Phase-2 work, but the chrome bridge must never be reachable from a page world.

### 1.5 CPU OSR vs accelerated OSR

v0.1 baseline is **CPU `on_paint` (BGRA)** — already what `mote-cef::Page` does, the ANGLE-independent path the spike validated. The compositor uploads each `PaintFrame` (BGRA `Vec<u8>`) into a wgpu texture each time a new frame arrives (dirty-tracked — only re-upload on a new `paint_count`). Accelerated zero-copy shared-texture OSR (`on_accelerated_paint` + `SharedTextureHandle`, the `cef` crate's `accelerated_osr` feature) is the optimization, gated behind a `mote-cef` feature and proven on target hardware later. Both paths live behind `mote-cef` per DISCIPLINES §1. (Risk + path in `02-risks.md`.)

---

## 2. Crate / module mapping

Phase 2 fills in four crates the master plan stubbed (`mote-ui`, `mote-shell`, `mote-session`) plus `mote-app`, and adds input/bridge/profile primitives to `mote-cef`. Responsibilities, sharply divided:

### `mote-cef` (extend — the only CEF-touching crate)
New primitives the shell/compositor need, all behind the existing wrapper:
- **Chrome vs content `Page`**: a `PageRole { Chrome, Content }` (or a `Page::new_chrome`) — transparent background + bridge for chrome, plain client for content.
- **Input injection**: `send_mouse_move/click/wheel`, `send_key_event`, `send_focus`, `notify_resized` on `Page`.
- **The host bridge transport**: a `HostBridge` type wrapping the CEF **message router** (`CefMessageRouterBrowserSide` + a `query` handler) and JS-binding registration on the chrome `RenderProcessHandler`. Exposes a Rust-side `on_query(callback)` and a `send_to_chrome(structured op)` to push DOM updates. **No `cef::` type in its public signature** (carries `serde`-able request/response structs or a `HostValue`-like tree).
- **Profile/identity**: `ProfileHandle` (the master plan names it; it does **not** exist yet) — wraps a CEF `RequestContext` with a per-identity cache/storage path so each identity = a distinct Chromium profile. `Page::with_profile(profile, …)`. **This is net-new and is on the critical path for the identity axis** (§6).
- The accelerated-OSR feature flag (deferred path).

### `mote-ui` (build — chrome doc + compositor + design system + element/slot rendering)
- **The compositor** (`compositor` module): wgpu device/surface/queue, the blit pipeline (from the spike), texture upload from `PaintFrame`, per-frame composite (viewport page texture + full-window chrome texture). Owns the one `raw-window-handle`→`wgpu::Surface` seam.
- **The chrome document** (`chrome/` HTML/CSS assets): the materialized design system (§4) and the slot scaffold (`[data-slot]` divs per spec/01).
- **The slot/element/theme runtime** (§5): `Slot`, `ElementKind`, `Element`, `Theme`, `TokenResolver`; resolves the active theme's layout + tokens into the chrome document (CSS-var rewrite under `[data-theme]`, element→slot placement).
- **The `UiHost` trait** (the seam `mote-shell` talks to): render the slot graph, push tab/workspace/identity state into the chrome, surface dialogs (approval, integrity panel), receive chrome→shell events (omnibox submit, tab click, focus change). Defined early, `[UI-INDEPENDENT]`, so shell wiring isn't blocked.
- **The chrome-side JS** (`chrome/host.js`): receives structured ops from Rust over the bridge, mutates the DOM, sends user intents back (omnibox text, clicks the chrome can't handle in pure CSS, focus changes). This is the plugin `render(host)` target's runtime sibling — but Phase 2 only needs the *runtime-owned* chrome (tabstrip, omnibox, sidebar shell, integrity panel, dialogs, status line); plugin-provided elements are wired but only first-party panels fill them (Phase 5).
- **The chrome surfaces**: tab strip, omnibox host, sidebar shell, workspace tab picker, integrity panel, permission-approval dialog, status line, empty-slot motif — each per its `spec/components/*.md` contract.

### `mote-shell` (build — composition root, window, input routing, wiring)
- **The window + event loop**: own the `winit` window and event loop; create the chrome `Page` and the wgpu surface; drive the compose/pump cycle.
- **Input routing** (§1.3): the single place winit events become CEF input, by hit-test + focus owner.
- **Tab lifecycle bridging**: session (`mote-session`) ↔ CEF (`mote-cef::Page` create/close/navigate) ↔ UI (`UiHost` tab-state push). Owns the map `TabId → Option<Page>` (None when hidden/discarded).
- **The config loader**: user `init.lua` → runtime state (`mote.plugins{}`, `mote.theme_overrides`, `mote.keys.bind`, `mote.workspace.define`, `mote.session.configure`, `mote.tabs.configure`) — Lua-only via `mote-lua`/`mote-runtime`, `[UI-INDEPENDENT]`.
- **Minimal navigation** (§8): type-URL→navigate and back/forward/reload, provided by the shell directly so the browser is usable before the urlbar *provider* plugin exists.
- **The integrity panel + approval dialog wiring**: pull effective permissions (`mote-permissions`), audit (`mote-audit`), capability/provenance (`mote-runtime`) and push to `UiHost`; route user actions (revoke, approve, narrow) back.

### `mote-app` (build — the binary)
- `main`: `mote_cef::bootstrap()` first (subprocess shim), then dispatch to `mote-cli` (management subcommands — Phase 3) or boot `mote-shell` (the browser). Phase 2 wires the browser-boot path; CLI is a thin stub returning to Phase 3.

### `mote-session` (build — the state crate, `[UI-INDEPENDENT]`)
Pure state/persistence (§5, §6). No rendering. `Identity`, `Workspace`, `Session`, `Tab { state }`, `TabPicker`, `HiddenTabReaper`, `Discarder`, `FormDraftStore`. Persists to per-identity SQLite via `mote-storage` (WAL, continuous flush). The shell reads/writes session; the UI renders what the shell pushes.

### Unchanged Phase-1 crates consumed
`mote-runtime` (host API, plugin lifecycle, capability map), `mote-permissions` (gatekeeper, narrowing model for the dialog), `mote-storage` (session DB, namespaces), `mote-audit` (panel read API), `mote-registry` (token vocabulary, slot/kind sets, capability contracts), `mote-lua` (config + theme Lua), `mote-dispatch` (keybind dispatch for chrome keys).

---

## 3. The host bridge

The bridge is how chrome JS (and, later, plugin `render(host)` → DOM) talks to the Rust runtime. **It is distinct from the `mote.*` Lua host API** (`mote-runtime::hostapi`) — that is the *plugin* surface, in Lua, gated by plugin permissions. The host bridge is the *chrome document's* surface, in JS, privileged, and reachable only from the chrome browser.

### 3.1 Transport
The CEF **message router** (`CefMessageRouterBrowserSide`/`RendererSide`), wrapped by `mote-cef::HostBridge`. Two directions:
- **chrome JS → Rust**: `window.mote.host.query({ kind, payload })` → a `cefQuery` → `HostBridge::on_query` callback in `mote-shell`. Used for: omnibox submit, tab click/close/new, sidebar panel switch, focus change, integrity-panel actions (revoke/approve/narrow), workspace switch, viewport-rect report.
- **Rust → chrome JS**: `HostBridge::send_to_chrome(op)` → a structured DOM op delivered to `chrome/host.js`, which applies it. Used for: tab-state updates (the canonical tab list for this window), workspace/identity state, integrity-panel data, dialog show/hide, navigation state (loading/secure/url for the omnibox), status-line segments.

### 3.2 What it exposes (Phase 2 surface)
A small, versioned API on `window.mote.host`:
- `host.tabs.list()` / live `tabsChanged` events — the window's tab strip view (active tabs in this window) + a way to open the workspace tab picker.
- `host.workspace.current()` / `host.workspace.switch(id)` / `workspaceChanged`.
- `host.identity.current()` (read-only; identity is hidden-by-default per DESIGN — surfaced but not switchable in chrome v0.1 beyond what config defines).
- `host.omnibox.submit(text, mode)` → shell parses and navigates / runs command / finds.
- `host.nav.{back,forward,reload}()` and `navStateChanged` (loading, canGoBack/Forward, secure, displayed URL split into host-dim/host/path per the omnibox spec).
- `host.integrity.{revoke,approve,narrow,update,rollback,reload}(pluginName, …)` → routed to `mote-runtime`/`mote-permissions`/`mote-pluginmgr`-stub.
- `host.focusChanged(owner)` → drives keyboard routing (§1.3).
- `host.viewportRect()` → the page viewport geometry for compositing + mouse mapping.

### 3.3 Gating and isolation
- Bindings installed **only** on the chrome `RenderProcessHandler`. A page browser has no `window.mote`.
- Every chrome→Rust query that maps to a privileged action is handled in `mote-shell`/`mote-runtime` and **audited** (`mote-audit`) — the integrity panel's own actions (revoke a permission) are themselves recorded.
- Page-derived strings flowing *out* to chrome are sanitized at `HostBridge::send_to_chrome` (titles/URLs) — §1.4.
- The bridge carries structured values (`serde`/`HostValue`-shaped), never raw HTML, so the chrome side applies typed ops rather than evaluating strings.

### 3.4 Relationship to the plugin `render(host)` model
The spike's `render(host)` Lua probe (`host:el/text/token/close` → DOM subtree) is the *plugin element* path. In Phase 2 the runtime-owned chrome is authored directly in `chrome/*.html/css/js`; the plugin element host is wired through the same bridge (a plugin's element subtree is delivered as structured ops into the slot the theme placed it in) but only first-party panels (Phase 5) exercise it. The `UiHost`/bridge contract is designed now to accept plugin element subtrees so Phase 5 plugs in without reshaping the bridge.

---

## 4. Materialize the design system

The mote-design skill's canonical CSS files (`colors_and_type.css`, `ui_kits/browser/index.html`) **do not exist** — only the prose `spec/*.md` contracts. Phase 2 must produce the real stylesheet and per-component CSS. This is explicit, scheduled work (work units W2–W3 below), authored under the mote-design skill's hard rules (token-only, dual-theme, keycap construction, `[mote]` bracket lockup, hairline borders, sharp corners ≤6px, vim block cursor, lowercase labels, restrained 120ms motion, no AI UI).

### 4.1 The token stylesheet (`mote-ui/chrome/tokens.css`)
Materialize **every** token from `spec/03_tokens.md` + the type ramps/tracking from `spec/04_typography.md` + motion tokens from `spec/05_motion.md` as CSS custom properties:
- `:root` / `[data-theme="dusk"]` (default) — the dusk column.
- `[data-theme="vellum"]` — the vellum column (recomputed semantic colors, ink-tinted `--dots`, lighter shadows).
- Raw palette scales (`--ink-*`, `--paper-*`, accents), semantic colors, syntax colors, spacing (4px grid), radius, shadow, motion (easings + durations), Mote layout tokens (`--chrome-tabbar: 40px` etc.), `--dots` gradient.
- `@import` the Geist / JetBrains Mono / Instrument Serif stacks (substitution notes in `spec/fonts/README.md`); fonts load via `@import` per spec/04, replaceable with `@font-face` woff2 later. The `prefers-reduced-motion` global block from spec/05 ships here.

This file is the single CSS-side ground truth; the Lua bridge (§5.4) mirrors `--name` ↔ `theme.tokens.name`.

### 4.2 Per-component CSS (`mote-ui/chrome/components/*.css`)
One stylesheet per specced component, lifted from each `spec/components/*.md` contract (structure + tokens + states):
- `button.css` (keycap, 5 variants), `kbd.css` (the keycap origin), `field.css` (sunk well, toggle, reflect affordance), `card.css` (quiet, hairline), `badge.css` (slab, border-only status), `tabs.css` (horizontal strip default + underline/keycap/vertical variants), `omnibox.css` (sunk well, `[url]/[cmd]/[find]` mode tag bracket lockup, block cursor, host-dim/host/path coloring), `palette.css` (640px floating, `›_` prompt, fade-in), `status-line.css` (22px mono strip, segments, mode chip, dots), `sidebar.css` (activity bar + swappable panel, `[tabs]` header lockup), `empty-slot.css` (dot-grid motif + `[ ] <name>` card).
- The `[mote]` bracket lockup is materialized once as a reusable class and reused for omnibox modes and sidebar headers.

### 4.3 The chrome scaffold (`mote-ui/chrome/chrome.html`)
The `[data-slot]` HTML structure from spec/01 (`top-bar`, `left-sidebar`, page `<main>`, `right-sidebar`, `bottom-bar`, `tab-row`, `urlbar-inline`), `data-theme="dusk"` on root, page viewport `<main>` transparent. Loads `tokens.css` + the component CSS + `host.js`. The CSP from §1.4 is set here. Lucide icons load per spec/06 (bundle `lucide-static` SVGs rather than the CDN — no network at chrome boot, honors DESIGN's no-implicit-network posture).

### 4.4 Serving the chrome to CEF
Bundle the chrome assets into the binary (`include_dir`/`include_bytes!`) and serve them to the chrome CEF browser via a `mote://chrome/…` custom scheme handled by a `ResourceInterceptor` (so relative imports resolve, CSP applies, and there's no on-disk dependency). A `file://` to an extracted temp dir is an acceptable first-slice shortcut; `mote://` is the production target. (Flagged in `02-risks.md`.)

### 4.5 Verification
A design-review snapshot: render `chrome.html` in both themes (toggle `[data-theme]`) headless and diff against the spike's `out.png` treatment + the spec contracts. The frontend-design/mote-design rules are checked by review, not CI, but the dual-theme render is captured on `DISPLAY=:1`.

---

## 5. Slots / elements / themes runtime

Implements DESIGN §UI Composition + spec/00/01/07. Fixed v0.1 sets only.

### 5.1 Slots (fixed) and element kinds (fixed)
- Slots: `top-bar`, `left-sidebar`, `right-sidebar`, `bottom-bar`, `urlbar-inline`, `tab-row` (the runtime owns these as `[data-slot]` regions).
- Element kinds (8): `urlbar` (one, always present), `tabstrip` (one, always present), `bookmarks-bar`, `sidebar-panel`, `action-button`, `status-indicator`, `urlbar-extension`, `widget`.
- These come from `mote-registry` (the token/slot/kind registry validated alongside permissions/capabilities). `mote-ui` reads them, never hard-codes a second copy.

### 5.2 The default theme (`default-layout` + `dusk`)
Ship `dusk` (default) and `vellum` as bundled theme plugins (DESIGN §Themes are plugins; spec/07 standard themes). For Phase 2 the *runtime* hosts a built-in `default-layout` placement and the `dusk`/`vellum` token sets so the browser renders before any Phase-5 plugin loads (critical-capability bundled-from-binary discipline). `embers`/`gloam` are listed in spec/07 but can ship as additional bundled token sets later — `dusk`+`vellum` are the Phase-2 requirement (both first-class).
- Default layout: `top-bar` = `{ urlbar, tabstrip }`, `left-sidebar` = `{ sidebar-panel:* }`, `bottom-bar` = `{ status-indicator:* }`, `right-sidebar` = `{}` (empty-slot motif).

### 5.3 Token resolution
`TokenResolver` resolves: bundled theme defaults → active theme `M.theme.styling` → `mote.theme_overrides` (deep-merge, user wins) → CSS-var rewrite under `[data-theme="<name>"]` in the chrome document. A theme switch is a CSS-var swap (instant, spec/05). The radius-≤6px / no-gradient / no-filled-icon constraints (spec/07 "what themes can't do") are enforced at token-set time.

### 5.4 The Lua token bridge
Every CSS var is mirrored on `theme.tokens` (`--surface-1` ↔ `theme.tokens.surface_1`) so plugin `render` functions reference tokens by name and get `var(--…)` (the spike's `host:token` → `var()` model). The plugin element host (`ui.register_element`) is wired into `mote-runtime`'s host API so a plugin can register an element of a kind; the active theme places it; the runtime renders it into the slot via the bridge. Phase 2 wires the path; only first-party panels exercise it (Phase 5).

---

## 6. Tab model + session + the three-axis state (DESIGN §User State Model, §Tab Persistence)

All in `mote-session` (state) + `mote-shell` (lifecycle bridging) + `mote-cef` (`ProfileHandle`). `[UI-INDEPENDENT]` for the state crate.

### 6.1 Three tab states
`TabState { ActiveInWindow, HiddenInWorkspace, Closed }`. Transitions:
- active→hidden: closing a window releases its tabs to hidden (renderer destroyed); `⌘⇧H` hides the active tab.
- active→closed: `⌘W`/middle-click (recoverable via undo-close for a short window).
- A live `Page` exists only for active (non-discarded) tabs; hidden/closed/discarded tabs are SQLite rows, no RAM (DESIGN memory table).

### 6.2 Window tab strips are views, not state
The workspace's tab list is canonical; each window's strip is a *view* onto it. Multiple windows on one workspace have independent strips (DESIGN). `mote-shell` maintains, per window, the set of `TabId`s shown; `mote-session` holds the canonical per-workspace list.

### 6.3 Workspace tab picker (`Mod+Space`)
A fuzzy-finder (a `widget`-kind overlay, palette-styled per spec/palette) over all tabs in the current workspace (active in any window + hidden). Ranking per DESIGN: active first, pinned near top, held high, recent hidden by `released_at`, fuzzy score weighted by recency. Selecting an active tab focuses it in its window; a hidden tab reveals it into the current window (or new window with a modifier). Shell intercepts `Mod+Space` before page routing.

### 6.4 Session persistence
Per-identity SQLite at `~/.local/state/mote/<identity>/session.db` via `mote-storage` (WAL). **Continuous flush**, batched ~5s. Stored: open tabs (URL/title/favicon ref/last-visited), per-workspace tab order, scroll position, back/forward history stack, form drafts, active workspace + active tab per workspace, hidden-tab metadata (`released_at`, hold flag). **Crash recovery == clean exit** — no recovery prompt; a hard crash loses ≤5s (DESIGN). **Not** stored: page contents/DOM/JS heap, localStorage/cookies/IndexedDB (identity storage), plugin internal state, window geometry.

### 6.5 Aging + memory
- Hidden-tab TTL: default 30 **days** (configurable; `never` disables) — `HiddenTabReaper`. (Note: DESIGN body says 30d TTL; the orchestrator brief's "hidden-tab TTL" maps to this 30d default — *not* 30 min, which is active-tab discarding.)
- Active-tab discarding: kill the renderer of an active tab unfocused >30 **minutes** (`Discarder`); the tab stays in the strip, clicking reloads. `keep_pinned_loaded = true`.
- Hold (runtime, session-only, exempts from TTL) vs Pin (config, dotfile, promotes to workspace pinned tab).

### 6.6 Identity (Chromium profile)
`Identity` = a Chromium profile via the new `mote-cef::ProfileHandle` (per-identity cache/storage path). Hidden behind a single `default` identity (DESIGN); multi-identity only when explicitly created in config. `mote.workspace.define` carries `default_identity`; new tabs open in the workspace's default identity. **Identity isolation honesty (DISCIPLINES §5):** author `docs/identity-isolation.md` (work unit W1b) enumerating exactly what a Chromium profile isolates (cookies, localStorage/IndexedDB, history, cache directory) and what it does **not** fully isolate (HTTP cache key construction, service-worker storage, certain network state — Chromium known leakage). Code comments and any README copy say "isolated across [enumerated list]," **never** "fully isolated." This also requires amending DESIGN's glossary "fully isolated" wording (maintainer decision — `02-risks.md` E1).

### 6.7 Workspace definitions + config/session split
- Workspace *definitions* (name, icon, accent, default_identity, default_newtab, pinned_tabs, optional keybind layer) = **dotfile Lua** (`mote.workspace.define`), loaded by the config loader.
- Workspace *runtime UI state* (resized slot sizes, last-active tab per workspace) = **session SQLite** keyed by workspace id (resolves `02-risks.md` E2: resizable-slot state lives in session, keyed by workspace).
- `mote.session.configure` (TTL, soft-warn) and `mote.tabs.configure` (discard interval, keep-pinned) parsed by the config loader into `mote-session` config.

### 6.8 Form drafts (DISCIPLINES §6 — opt-in)
`FormDraftStore`: **opt-in by default** (off). When enabled, save inputs only after >20 chars in a field; never save `type=password`, `autocomplete=off`, `autocomplete=cc-*`, or sensitivity-marked fields; clear after 7 days. Surfaced in the integrity panel's "data Mote is keeping" view with clear/disable. Per-site opt-out is a v0.2+ `session:exclude_forms` concern. The PR that adds it carries the §6 data-persistence section.

---

## 7. Integrity panel + permission-approval dialog (the only GUI besides chrome)

DESIGN §Transparency; DISCIPLINES §4/§6/§9. Rendered in the chrome document (`mote-ui`), wired to runtime data via `mote-shell`/the bridge.

### 7.1 Integrity panel
A `sidebar-panel` (the `plugins` panel) rendered from runtime data:
- **Active plugins**: name, version, source/commit/provenance, integrity status badge (`verified`), fulfilled capabilities, consumed capabilities.
- **Permissions requested → effective** per plugin (the narrowing model from `mote-permissions`), with the multi-pattern editor for post-install scope changes.
- **Network audit log**, **storage audit**, **permission denials**, **MCP activity** — read from `mote-audit`'s query API (data exists from Phase 1; MCP from Phase 8).
- **"Data Mote is keeping" view** (DISCIPLINES §6): history entries, form drafts, plugin storage volumes, cached items per identity — with clear/disable controls.
- One-click actions: revoke permission, adjust scope, update, rollback, reload, settings — routed through the bridge to `mote-runtime`/`mote-permissions` (update/rollback land fully when `mote-pluginmgr` exists in Phase 3; Phase 2 wires the buttons + the actions that have backends now: revoke, adjust scope, reload).
- The panel is the runtime's surface, **not** a plugin (DESIGN: no `permissions:query` meta-introspection for plugins).

### 7.2 Permission-approval dialog
The only modal besides the palette/picker. Renders the four-step load pipeline's step-4 surface (`mote-runtime::Narrowing`/`ApprovalPolicy`): requested permissions across a plugin, the three-mode narrowing UI (grant fully / grant on specific origins with the inline glob-pattern editor / deny), `identity_scope` choice for `user_choice` plugins, and **dangerous-combination surfacing** (DISCIPLINES §4 — `combinations` from `mote-registry`). User decision flows back through the bridge to the runtime's approval policy. Dev-mode plugins are visually marked (`[dev]`); the dialog is skipped for them (Phase 3 dev-mode state machine; the marking is here).

---

## 8. Navigation floor + urlbar/workspace provider model (reconciled with DESIGN)

### 8.1 The mechanism/policy seam

The shell and runtime own the navigation **mechanism**. The navigation and workspace **policy** is owned by bundled first-party plugins. This seam must be explicit and must not drift: no future shell change should quietly absorb policy that belongs in a plugin.

**Shell + runtime own (mechanism):**
- Telling CEF to load a URL (`Page::load_url`)
- The back/forward/reload history stack and the API for it
- The `urlbar` element and the urlbar host: receiving text input, the `:`/`/` mode tags, the omnibox display (URL, host-dim/host/path coloring, secure indicator)
- The host API that provider plugins call to push suggestions
- The runtime-owned **integrity panel** as the recovery surface for any plugin failure

**Bundled first-party plugins own (policy):**
- `history` (Phase 5, `source = "bundled"`) → fulfills `ui:urlbar_provider`: what suggestions appear in the dropdown (history, bookmark, tab-search results), their ranking, and the `urlbar:suggest` collector surface that other plugins contribute to
- `workspace-manager` (Phase 5, `source = "bundled"`) → fulfills `workspace:provider`: Lua-driven workspace definition, picker UI extensibility, workspace lifecycle

These plugins are **embedded in the binary and present from first launch** (DESIGN: "there is no separate built-in fallback mechanism; bundled first-party plugins serve this role"). The browser is usable out of the box because the bundled plugins are there — not because the shell silently duplicates policy.

### 8.2 No silent shell fallback

If a critical-capability plugin is removed or broken, the outcome is a **loud failure surfaced in the integrity panel** — not a shell fallback that quietly absorbs the policy. DESIGN: "loud failure is better than partial functionality for browser-critical concerns." The integrity panel and the runtime mechanism (urlbar element, navigation API) remain available to fix the broken plugin or reinstate it.

### 8.3 Phase-2 sequencing: pull forward a minimal bundled provider slice

`history` and `workspace-manager` are Phase-5 work in their full form. However, the Phase-2 interactive slice must exercise the real runtime→plugin→capability→chrome path — not a throwaway shell hack that gets ripped out in Phase 5.

**Phase-2 deliverable (W-A0, added to Wave A):** a **minimal bundled urlbar + workspace provider** — a thin slice of the eventual Phase-5 plugins, embedded in the binary via `source = "bundled"`:
- The urlbar provider slice: accepts the host's `urlbar:suggest` callback, returns an empty suggestion list (no history yet), and exercises the full provider protocol so the omnibox-suggestion seam is real from day one.
- The workspace provider slice: registers a single default workspace and the `mote.workspace.define` declarations from config, fulfills `workspace:provider`, and drives the picker. The full multi-workspace management Lua surface comes in Phase 5.

These slices are not throwaway — they become the skeleton of the Phase-5 plugins. The Phase-5 work adds richness (history queries, bookmark search, full Lua workspace management) on top of a path that already works end-to-end.

**Why this matters:** building the omnibox suggestion seam in Phase 2 against the real capability contract (W-C7) means Phase 5 drops in a richer provider without reshaping any plumbing. The alternative — having the shell own the suggestion seam in Phase 2 — creates a shell-vs-plugin split that Phase 5 has to unpick. Don't build the seam twice.

### 8.4 Navigation never fails because of a plugin

The navigation **mechanism** is shell-owned and never blocked by a provider. If the urlbar provider plugin fails to load, the omnibox still accepts URLs and navigates (`Page::load_url`) — the suggestion dropdown is empty, not an error. The shell queries the active `ui:urlbar_provider` for suggestions and treats absence as an empty list. This is graceful degradation at the mechanism layer, not a fallback policy.

---

## 9. The two Phase-2-prerequisite security items

### 9.1 Glob candidate normalization before the first URL/host permission sink
Phase 2 is where the first real URL/host permission sink appears (navigation, and `http:fetch`/`net:intercept_request` resources flowing through live pages). Per `02-risks.md`/master-plan implementation finding: DESIGN's `net:intercept_request:!*.banking.com` glob matches a **normalized host** (`secure.banking.com`), not a full URL (`https://secure.banking.com/login`). **Before any operation's resource is passed to `Gatekeeper::check`, the runtime seam must normalize it to the canonical resource form the permission patterns are written against.** Phase 2 deliverable: **document the canonical resource form per permission domain** (e.g. `net:*`/`page:*` → normalized host; `http:fetch` → `scheme://host[:port]` origin; `secret:read` → secret name) and implement normalization in `mote-runtime`/`mote-dispatch` at the point each domain's operations are dispatched, with the chrome's own navigation going through the same normalization. This is a hard prerequisite — getting it wrong means permission checks silently mismatch.

### 9.2 Non-exclusive `capabilities.invoke` dispatch-shape resolution
DESIGN: how the runtime treats multiple fulfillers of a non-exclusive capability is specified **per-capability** in the registry, not by a framework taxonomy. Phase 1 wired `capabilities.invoke` to route to "the current fulfiller" (single), with the `secret:provider` case ambiguous (`02-risks.md` C-series: non-exclusive but effectively single because `password-manager:provider` is exclusive). **Phase 2 deliverable:** for every capability a Phase-2 surface invokes (the only one Phase 2 actually exercises is the urlbar-suggestion collector and, transitively, theme stacking), the registry contract must declare its dispatch shape (call-one-by-priority / aggregate / stack), and `capabilities.invoke` must honor it rather than assuming a single fulfiller. Concretely: `theme:provider` (non-exclusive in the registry but DESIGN treats one active theme — confirm exclusive-in-practice for v0.1), and the urlbar `urlbar:suggest` collector (aggregate-and-rank, owned by the exclusive provider). Document and enforce the dispatch shape at the registry contract level before the omnibox-suggestion seam ships.

---

## 10. Ordered work breakdown (dependencies + parallelization + the early interactive slice)

Notation: `‖` parallelizable (disjoint files/crates); `→` hard dependency. `[STATE]` = `mote-session` (UI-independent); `[CEF]` = `mote-cef` extension; `[CHROME]` = `mote-ui` chrome/compositor; `[GLUE]` = `mote-shell`. Blast-radius rule: smallest-surface change lands first within a serialized chain.

### Wave A — foundations (start immediately, parallel)
- **W-A0 `[PLUGIN/GLUE]` minimal bundled urlbar + workspace provider slices** ‖ : thin bundled plugins that fulfill `ui:urlbar_provider` (empty suggestion list, real protocol) and `workspace:provider` (single default workspace + `mote.workspace.define` support), embedded via `source = "bundled"`. Exercises the real runtime→plugin→capability→chrome path from the interactive slice onward; becomes the Phase-5 skeleton. Must be wired before W-C1 (omnibox seam) and W-C5 (picker). (§8.3)
- **W-A1 `[STATE]` `mote-session` core** ‖ : `Identity`/`Workspace`/`Session`/`Tab{state}`, session SQLite schema (WAL, continuous flush), crash-recovery==clean-exit, tab-state transitions, `TabPicker` ranking, `HiddenTabReaper` (30d), `Discarder` (30m), `FormDraftStore` (opt-in + sensitivity filters). Pure state; integration-tested against temp SQLite. *(master 2.1)*
- **W-A1b `[STATE/doc]` `docs/identity-isolation.md`** ‖ : the enumerated isolation surface (DISCIPLINES §5); W-A1 code references it. *(master 2.2)*
- **W-A2 `[CEF]` `ProfileHandle`** → (needs nothing new; isolated CEF work): per-identity Chromium profile (`RequestContext` + per-identity paths); `Page::with_profile`. Long pole on the CEF side — staff early.
- **W-A3 `[CEF]` input injection + chrome/content `Page` roles** ‖ W-A2 : `send_mouse_*`/`send_key`/`send_focus`/`notify_resized`; `PageRole::{Chrome,Content}` (transparent bg + bridge for chrome).
- **W-A4 `[CHROME]` design-system materialization** ‖ : `tokens.css` (dual-theme, all of spec/03/04/05), per-component CSS (spec/components/*), `chrome.html` scaffold + CSP + Lucide-static. *(this is §4; large but fully parallel — no Rust deps)*
- **W-A5 `[CHROME]` `UiHost` trait + slot/element/theme data model** ‖ : `Slot`/`ElementKind`/`Element`/`Theme`/`TokenResolver`, reading slot/kind/token sets from `mote-registry`. No backend yet; unblocks shell wiring. *(master 2.3)*

### Wave B — the compositor + bridge + the vertical slice
- **W-B1 `[CHROME]` the wgpu compositor** → W-A3 : winit-handle→surface seam, blit pipeline (from spike), `PaintFrame`→texture upload (dirty-tracked), composite (viewport page tex + full-window chrome tex). Reuses `spikes/ui-cef-html` blit.
- **W-B2 `[CEF]` `HostBridge`** → W-A3 : message-router transport, JS bindings on chrome `RenderProcessHandler` only, `on_query`/`send_to_chrome` structured ops, page-string sanitization at the boundary.
- **W-B3 `[GLUE]` window + event loop + compose/pump cycle** → W-B1, W-A3 : winit window, create chrome `Page`, wgpu surface, the pump-compose-redraw loop.

> ### ★ EARLY INTERACTIVE-SLICE MILESTONE (lands at the end of Wave B) ★
> **The earliest point a human launches a window, opens a tab, types a URL, and sees a page composited inside Mote's chrome.** Concretely: `mote-app` boots the shell → a winit window opens on `DISPLAY=:1` → the chrome (materialized dusk theme: tab strip + omnibox + sidebar shell) renders via the chrome CEF browser → typing a URL in the omnibox (`host.omnibox.submit` over the bridge → shell parses → creates/navigates a content `Page`) shows the page composited in the viewport region with chrome surrounding it → clicking inside the page routes input correctly → a second tab + tab switch is a texture swap. This is a deliberately *vertical* slice: one window, one workspace, in-memory tab list, minimal navigation, dusk theme only, no integrity panel yet. **Schedule this EARLY and iterate** — it de-risks the four hardest seams (window/surface, bridge, input routing, compose) on real running code before the breadth work (session persistence wiring, panel, dialog, dual-theme polish) layers on.
> **Requires for the slice:** W-A3, W-A4 (chrome + dusk only), W-A5, W-B1, W-B2, W-B3, and W-C1 (minimal nav). Session persistence (W-A1) is *not* required for the slice (in-memory tabs suffice) — it wires in Wave C.

### Wave C — breadth on top of the slice
- **W-C1 `[GLUE]` minimal navigation + input routing** → W-B2, W-B3 : the §1.3 routing rule (hit-test + focus owner, coord mapping), omnibox submit → navigate, back/forward/reload, command/find modes, chrome keybind interception. *(part of the slice; routing correctness iterated here)*
- **W-C2 `[GLUE]` config loader** ‖ : `init.lua` → runtime state (`mote.plugins`, `theme_overrides`, `keys.bind`, `workspace.define`, `session.configure`, `tabs.configure`). Lua-only, UI-independent.
- **W-C3 `[GLUE]` tab lifecycle ↔ session ↔ CEF ↔ UI** → W-A1, W-C1 : `TabId→Option<Page>` map, active/hidden/closed transitions wired to session persistence + bridge tab-state push; window-strip-as-view; undo-close.
- **W-C4 `[CHROME/GLUE]` theme runtime + dual-theme** → W-A4, W-A5 : `TokenResolver`, default-layout + dusk + vellum bundled token sets, theme switch (CSS-var swap), constraint enforcement. Both themes first-class.
- **W-C5 `[GLUE/CHROME]` workspace tab picker (`Mod+Space`)** → W-A1, W-C3 : the fuzzy picker overlay, ranking, reveal/focus.
- **W-C6 `[GLUE]` glob normalization + canonical resource form** → W-C1 : §9.1 — document per-domain canonical form, normalize before `Gatekeeper::check`; route navigation through it.
- **W-C7 `[REGISTRY/GLUE]` capabilities.invoke dispatch-shape** ‖ : §9.2 — registry contracts declare dispatch shape; invoke honors it; the urlbar-suggestion collector + theme stacking confirmed.

### Wave D — the transparency surfaces
- **W-D1 `[CHROME/GLUE]` integrity panel** → W-C4 : active plugins, requested→effective, audit/storage/denials/MCP reads from `mote-audit`, "data Mote is keeping" view, action wiring (revoke/adjust-scope/reload now; update/rollback stubbed to Phase 3). The load-bearing transparency surface. *(master 2.7)*
- **W-D2 `[CHROME/GLUE]` permission-approval dialog** → W-C4 : narrowing UI (3-mode + glob editor), `identity_scope` choice, dangerous-combination surfacing (`mote-registry` combinations), decision → `ApprovalPolicy`. *(master 2.8)*
- **W-D3 `[CHROME]` status line + empty-slot + sidebar shell polish** ‖ W-D1 : the `bottom-bar` status segments, dot-grid empty slots, sidebar activity bar.

### Parallelization summary
- **Immediately parallel (no deps):** W-A0, W-A1, W-A1b, W-A2, W-A3, W-A4, W-A5, W-C2.
- **Long poles to staff first:** W-A2 (`ProfileHandle`, CEF), W-B2 (`HostBridge`, CEF + new pattern), W-C1 (input routing — the trickiest correctness surface), W-D1 (integrity panel — broadest UI).
- **W-A0 gates W-C1 and W-C5:** the omnibox suggestion seam and workspace picker must wire against the real provider protocol, not a shell stub. Staff W-A0 early alongside W-A2.
- **Serialize within a crate's files; parallelize across crates.** `mote-cef` extensions (W-A2/W-A3/W-B2) touch the same crate — serialize or coordinate; `mote-session` (W-A1) and `mote-ui` chrome (W-A4) are fully disjoint and run alongside.
- **The interactive slice is the schedule's spine** — everything in Wave A/early-B exists to reach it; Wave C/D iterate on a running browser.

---

## 11. Verification strategy

"Done" = happy path proven end-to-end at the integration seam, culminating in a runnable interactive build on `DISPLAY=:1` (the global verification rule; master §3). Per piece:

- **`mote-session` (W-A1):** integration tests vs temp SQLite — namespace/identity isolation (identity A sees nothing from B), WAL durability (kill mid-write, reopen, state intact = crash-recovery==clean-exit), hidden-tab TTL reaping (30d), discard at 30m idle, form-draft sensitivity filtering (password/`autocomplete=off` never saved). **Done:** simulated crash recovers to ≤5s-old state.
- **`mote-cef` `ProfileHandle` (W-A2):** headless — two profiles, a cookie set in profile A is invisible in profile B; document in `identity-isolation.md` what the test does *not* prove (the Chromium known-leakage surfaces).
- **`mote-cef` input + roles (W-A3) + `HostBridge` (W-B2):** headless smoke — create a chrome `Page`, install the bridge, round-trip a `host.query` → Rust callback → `send_to_chrome` op applied (assert via a chrome-side echo); a content `Page` has **no** `window.mote` (assert the binding is absent — the isolation test).
- **Compositor (W-B1):** offscreen — composite a chrome texture + a page texture, read back, assert chrome-surrounds-page (the spike's `out.png` shape); steady-state blit sub-ms; texture re-upload only on new `paint_count`.
- **Design system (W-A4) + theme runtime (W-C4):** render `chrome.html` in dusk and vellum on `DISPLAY=:1`, capture both (Playwright/`browser_take_screenshot` MCP tools available), review against spec contracts; theme switch is an instant CSS-var swap; radius/gradient/filled-icon constraints rejected at token-set time.
- **★ The interactive slice (end of Wave B + W-C1):** the milestone above — **boot `mote-app`, window opens on `DISPLAY=:1`, chrome renders, type a URL → page composites inside the chrome, click into the page (input routes), open a second tab and switch (texture swap).** Captured as a screenshot. This is the Phase-2 spine proof.
- **Input routing (W-C1):** scripted — a click at a known window coord inside the viewport reaches the page at the mapped page coord; a click on a tab reaches the chrome; `⌘L`/`⌘T`/`Mod+Space` are intercepted before the page; focus owner switches keyboard target.
- **Tab lifecycle + session (W-C3):** open/hide/close/reopen a tab; close a window → tabs go hidden (not destroyed); restart → active workspace restores as placeholders, page loads on focus; hidden tab survives, discarded tab reloads on click.
- **Glob normalization (W-C6):** unit/property — a navigation to `https://secure.banking.com/login` checked against `net:intercept_request:!*.banking.com` matches the negation (normalized to host); per-domain canonical form documented and exercised.
- **capabilities.invoke dispatch shape (W-C7):** the registry contract's dispatch shape is honored (collector aggregates+ranks; theme stacking/exclusivity as declared).
- **Integrity panel (W-D1) + approval dialog (W-D2):** render on `DISPLAY=:1` with a real (tiny) loaded Lua plugin — requested→effective shown, revoke takes effect (audited), narrowing dialog produces the narrowed effective set, a dangerous combination is surfaced, "data Mote is keeping" lists session/storage categories with clear controls.
- **CI gates (unchanged, must stay green):** CEF import-isolation (no `cef`/`cef_rs` outside `mote-cef` — the new wgpu/winit/bridge code must not leak CEF types), contract-conformance plugin, clippy `-D warnings`, `missing_docs`.

**The Phase-2 exit criterion:** a human launches Mote on `DISPLAY=:1`, opens a tab, types a URL, sees the page composited inside the dual-theme chrome, opens the integrity panel and sees the loaded plugins with their effective permissions, and approves a plugin through the narrowing dialog — all on a continuously-persisted, crash-recoverable session.

---

## 12. ADR compliance note

This plan must pass the `adr-review` gate (CLAUDE.md mandatory gate after writing a plan). ADR-0003 (chrome = HTML/CSS in CEF + wgpu compositor) is honored throughout — no custom widget toolkit, no second design-system representation, tokens-as-CSS-vars, FFI only on DOM mutation, structural isolation. ADR-0001 (declarative plugin registration) and ADR-0002 (inter-plugin via capability contracts only) are respected by the §3/§5/§9.2 wiring (the urlbar provider is a capability contract, not a direct import). The "no AI UI" decision (DESIGN principle #8 / spec/00) is honored: `[ask]`/`assist` are reserved empty slots, not built.
