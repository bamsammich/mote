# Mote — Phase 2 (Browser Shell): Risks, Unknowns, and Decisions

Companion to `docs/plans/02-browser-shell.md`. Each item: what it is, why it risks, a proposed resolution, and who decides. Tags:
- `[DECISION]` — needs a maintainer/user call before the affected unit is built (building either way risks rework).
- `[RISK]` — implementing engineer can proceed with the proposed default; confirm where flagged.
- `[UNKNOWN]` — needs a spike or measurement to resolve.

**Surface these to the maintainer first:** D1 (windowing — load-bearing, but already leaning), D2 (urlbar/workspace provider sequencing — shapes the omnibox seam), D7 (DESIGN "fully isolated" wording — an active doc contradiction), D8 (the 30d-vs-30min TTL reading).

---

## D1. `[DECISION]` Windowing: winit vs CEF native window

**What.** ADR-0003 puts the chrome in an *off-screen* CEF browser composited by wgpu. That means Mote — not CEF — owns the OS window and the GPU surface. The window toolkit is unspecified.

**Why it risks.** Project-lifetime choice; the event loop, input source, DPI handling, and the `raw-window-handle`→`wgpu::Surface` seam all hang off it. CEF *can* host its own native window, but that contradicts the OSR-compositor architecture (CEF would own the surface and we'd lose the chrome-surrounds-content composite + sub-ms texture-swap tab switch).

**Proposed resolution.** **`winit`** for the OS window + event loop; wgpu surface from its `raw-window-handle`. The spike rendered offscreen (no window) and proved the compositor; winit is the standard, maintained Rust windowing crate, integrates with wgpu directly, and supports X11/XWayland-first (ADR-0003: dev targets `--ozone-platform=x11`). CEF never opens a top-level window. **Maintainer: confirm winit** (the alternative — embedding a CEF windowed browser for chrome and a separate OSR path for pages — is architecturally inconsistent with ADR-0003 and rejected here). Add `winit` + `wgpu` + `raw-window-handle` to `[workspace.dependencies]`.

**Knock-on `[RISK]`:** wgpu's `create_surface` is `unsafe` on some versions (the window-handle lifetime). The workspace is `unsafe_code = "deny"` outside `mote-cef`. Pin a wgpu version where `create_surface` is safe (recent wgpu took a safe `SurfaceTarget`), or isolate the one call in `mote-ui` with a justifying `#[allow(unsafe_code)]` and a comment — a documented, single-site exception, not a general relaxation. Decide at W-B1.

---

## D2. `[DECISION]` urlbar / workspace provider sequencing (orchestrator brief #8)

**What.** `ui:urlbar_provider` (Phase-5 `history` plugin, critical) and `workspace:provider` (Phase-5 `workspace-manager`, critical) are plugins. Phase 2 must ship a usable browser *before* Phase 5.

**Why it risks.** If the shell assumes a provider plugin exists, the browser can't navigate until Phase 5 — unacceptable. If the shell duplicates everything the plugin will do, Phase 5 has to unpick it. The split must be drawn deliberately.

**Proposed resolution (the plan §8 split):**
- **Shell provides the navigation + workspace-mechanics *floor* in Phase 2, plugin-independent:** type-URL→navigate, back/forward/reload, `:`/`​/` command/find modes, a single default workspace + whatever `mote.workspace.define` declares, the tab picker mechanics. Navigation never depends on a plugin.
- **Provider plugins *enrich* in Phase 5:** `ui:urlbar_provider` adds the suggestion/completion dropdown (via its `urlbar:suggest` collector); `workspace:provider` adds Lua-driven workspace management. When no provider is loaded, the omnibox has no suggestion dropdown but navigation works; the shell queries the active provider (if any) for suggestions and treats absence as an empty list, not a failure.

**Decide.** Confirm the floor/enrichment split before building the omnibox-suggestion seam (W-C1/W-C5) and the `urlbar:suggest` collector contract (W-C7). The alternative — making the urlbar fully plugin-owned in Phase 2 — would block usability on Phase 5 and is rejected. **Maintainer call** because it determines how much navigation logic lives in `mote-shell` vs the `history` plugin permanently.

---

## D3. `[RISK]` Accelerated vs CPU OSR for chrome and pages

**What.** v0.1 baseline is CPU `on_paint` (BGRA), already in `mote-cef::Page`. Accelerated zero-copy shared-texture OSR (`on_accelerated_paint` + `SharedTextureHandle`, the `cef` crate's `accelerated_osr` feature, pulls wgpu 29) is the perf path.

**Why it risks.** Accelerated OSR on Linux needs the ANGLE/Ozone path (`--use-angle=gl-egl`, `--ozone-platform`), validated as *available* but *not exercised* by the spike. CPU `on_paint` means a per-frame BGRA copy + texture upload for every dirty surface — fine for chrome (small, rarely dirty) and a single focused page, but a heavy page at 60fps is a full-frame upload each paint.

**Proposed resolution.** **CPU `on_paint` for all of Phase 2** (the guaranteed baseline; dirty-tracked uploads — only re-upload on a new `paint_count`; only the *focused* page paints at full rate). Keep `Page::frame_rate` modest for unfocused-but-visible surfaces. Accelerated OSR is a `mote-cef` feature flag, proven on target hardware in a later phase — both paths behind `mote-cef` per DISCIPLINES §1. **Confirm**: CPU path is acceptable for the interactive slice; revisit if the slice shows upload-bound frames.

---

## D4. `[RISK]` Input-routing edge cases

**What.** §1.3 routes mouse by viewport hit-test and keyboard by focus owner. Several edges are not obvious.

**Why it risks.** Getting any of these wrong is a "feels broken" bug a user hits in the first minute.

**Edges + proposed handling:**
- **Drag across the chrome/page boundary** (mousedown in page, mouseup over chrome, or vice versa): capture the routing target at mousedown and hold it for the duration of the drag (don't re-hit-test mid-drag).
- **Wheel/scroll momentum** crossing the boundary: route by the position at scroll start.
- **IME / composition events**: route to the focus owner; Phase 2 supports basic IME into the focused surface (full IME is a `[UNKNOWN]` for later — flag if it surfaces).
- **Fast resize**: re-read the viewport rect from the chrome on every resize; debounce the CEF `WasResized` if it floods. Stale viewport rect during resize = mismapped clicks; the re-read must complete before routing resumes.
- **Focus stealing**: only one CEF browser is told it has focus (`send_focus`); the shell is the single authority; the chrome reports focus changes via the bridge.
- **DPI / scale factor**: pass the same device-scale-factor to chrome and page browsers and map logical↔physical consistently, or hit-testing drifts.

**Resolution.** Implement the capture-at-mousedown rule and the resize re-read as hard requirements in W-C1; cover them in the input-routing scripted test (§11). IME beyond basic input is deferred and flagged.

---

## D5. `[RISK]` Multi-CEF-browser memory

**What.** Each window has 1 chrome browser + N content browsers (one per active, non-discarded tab). The spike measured ~10–33 MB PSS per CEF renderer + ~230 MB GPU-device baseline + ~305 MB full-shell PSS.

**Why it risks.** DESIGN's old ~50–100 MB target is unachievable (ADR-0003 restates this); naive "keep every tab's renderer alive" multiplies the per-renderer cost.

**Resolution.** The tab-state model *is* the mitigation: hidden tabs have **no** renderer (SQLite rows only); active tabs unfocused >30min are **discarded** (renderer killed). So live content browsers ≈ focused tab + recently-focused-and-not-yet-discarded tabs, not "every tab." The ~230 MB GPU baseline is a shared cost both architectures pay (spike). **Confirm** the discard/hidden lifecycle (W-A1/W-C3) is wired *before* the shell can open many tabs, so memory stays bounded. Measure full-shell PSS at the interactive slice and again with 10+ tabs; flag if it diverges from the spike's ~305 MB.

---

## D6. `[DECISION]` Serving the chrome document to CEF

**What.** The chrome HTML/CSS/JS must load into the chrome CEF browser.

**Why it risks.** A `file://` to a temp dir works but leaves an on-disk dependency and complicates CSP/relative-import resolution; a custom scheme is cleaner but is more `mote-cef` plumbing.

**Proposed resolution.** First slice: `file://` to extracted bundled assets (fastest to running code). Production: a **`mote://chrome/…` custom scheme** served by a `ResourceInterceptor` over assets embedded in the binary (`include_dir`), so the chrome boots with no network and no on-disk dependency, CSP applies cleanly, and relative imports resolve. **Decide** when to switch (recommend during W-A4→W-B3); the `mote://` scheme is the target, `file://` the stopgap. Lucide icons bundled as `lucide-static` SVGs (not the spec/06 CDN) to honor the no-implicit-network posture.

---

## D7. `[DECISION]` DESIGN "fully isolated" wording vs DISCIPLINES §5

**What.** DESIGN §Identity / Glossary say identities are "fully isolated" / "effectively different browser instances." DISCIPLINES §5 explicitly forbids "fully isolated" because Chromium has known cross-profile leakage (HTTP cache key, service-worker storage, certain network state).

**Why it risks.** Phase 2 builds the identity axis (`ProfileHandle`) and authors `docs/identity-isolation.md`. Any code comment or doc copying DESIGN's glossary would violate the discipline the same phase is meant to honor.

**Proposed resolution.** Author `docs/identity-isolation.md` (W-A1b) as the enumerated truth ("isolated across: cookies, localStorage/IndexedDB, history, cache directory; NOT fully isolated: HTTP cache key construction, service-worker storage, certain network state"). **Amend DESIGN's glossary/§Identity "fully isolated" wording to match** — this is a source-doc edit, so **maintainer approves the DESIGN change** (CLAUDE.md: specs/DESIGN change only by stakeholder decision). Code never claims "fully isolated."

---

## D8. `[RISK]` Hidden-tab TTL (30 days) vs active-tab discard (30 minutes)

**What.** The orchestrator brief lists "active-tab discarding (30min)" and "hidden-tab TTL" together. DESIGN is explicit: active-tab **discarding** = 30 *minutes* idle (renderer killed, tab stays in strip); hidden-tab **TTL** = 30 *days* (row deleted). These are different mechanisms with different units.

**Why it risks.** Conflating them (e.g. reaping hidden tabs at 30 min, or discarding renderers at 30 days) would either lose user tabs or never reclaim memory.

**Resolution.** Implement as two distinct mechanisms in `mote-session` (W-A1): `Discarder` (30m idle, renderer kill, tab persists) and `HiddenTabReaper` (30d, row delete; `never` disables). Both configurable (`mote.tabs.configure` / `mote.session.configure`). **Confirm** the 30d/30m reading is correct (it follows DESIGN §Tab Persistence verbatim).

---

## D9. `[DECISION]` Workspace runtime-state home (resized slots, last-active tab)

**What.** Workspace *definitions* are dotfile Lua (`mote.workspace.define`). But resized-slot state "persists per workspace" (spec/07) and last-active-tab-per-workspace is runtime — these cross the config/session line.

**Why it risks.** `mote-session` (W-A1) needs to know where workspace runtime UI state lives; getting it in dotfiles would pollute config with machine-local state.

**Proposed resolution.** Workspace *runtime UI state* (resized slot sizes, last-active tab) → **session SQLite, keyed by workspace id** (machine-local, not dotfile). Definitions stay dotfile Lua. (Resolves master-plan risk E2.) **Confirm** the split before W-A1's schema is fixed.

---

## D10. `[RISK]` `capabilities.invoke` non-exclusive dispatch shape (orchestrator brief #9.2)

**What.** Phase 1 wired `capabilities.invoke` to a single fulfiller. DESIGN says non-exclusive dispatch shape is per-capability in the registry. Phase 2 exercises the urlbar `urlbar:suggest` collector and theme stacking.

**Why it risks.** Assuming single-fulfiller breaks `theme:provider` stacking and the urlbar suggestion collector if/when multiple fulfillers exist.

**Proposed resolution.** Registry capability contracts declare a `dispatch_shape` (call-one-by-priority / aggregate / stack / collector); `capabilities.invoke` and the runtime honor it (W-C7). For v0.1, confirm `theme:provider` is treated as **exclusive-in-practice** (one active theme per DESIGN, despite the "non-exclusive, stacks" registry note — `02`/master C-series ambiguity) and the urlbar `urlbar:suggest` is a **collector** owned by the exclusive provider. **Confirm** the v0.1 treatment of `theme:provider` (exclusive vs stacking) — it affects W-C4 and the registry contract.

---

## D11. `[RISK]` Glob candidate normalization / canonical resource form (orchestrator brief #9.1)

**What.** Permission globs match a *normalized resource form* (e.g. host `secure.banking.com`), not a full URL. Phase 2 is the first place real URL/host sinks appear (navigation, page `http:fetch`/`net:intercept_request`).

**Why it risks.** If the runtime passes a full URL to `Gatekeeper::check` where the pattern expects a host, checks silently mismatch — a security correctness bug, not a cosmetic one.

**Proposed resolution.** **Document the canonical resource form per permission domain** (`net:*`/`page:*` → normalized host; `http:fetch` → `scheme://host[:port]` origin; `secret:read` → secret name; etc.) and implement normalization in `mote-runtime`/`mote-dispatch` *before* `Gatekeeper::check`, with the chrome's navigation routed through the same normalization (W-C6). This is a hard Phase-2 prerequisite, flagged because the contract is currently undocumented in DESIGN (master-plan implementation finding).

---

## D12. `[UNKNOWN]` CEF message-router availability/shape in `cef` 148

**What.** The host bridge (§3) assumes `CefMessageRouterBrowserSide`/`RendererSide` and JS-binding registration on a `RenderProcessHandler` are exposed by `cef` 148 (tauri-apps/cef-rs).

**Why it risks.** The spike validated OSR + on_paint + the subprocess split, but used a simple `innerHTML` string for the `render(host)` probe — it did **not** exercise the message router. The bindings-on-chrome-only isolation depends on a working `RenderProcessHandler` + router.

**Proposed resolution.** **Spike the message router early** (part of W-B2, before committing the bridge API) — confirm `cef` 148 exposes the router and per-browser JS binding registration, and that bindings can be scoped to the chrome browser only. If the router isn't cleanly exposed, fall back to a constrained, escaped `innerHTML`/structured-op channel over CEF process messages (still structured, still chrome-only) — but this needs the spike to confirm before W-B2's API is fixed. Highest-uncertainty new external surface in Phase 2; staff it first within the CEF stream.

---

## D13. `[RISK]` `embers`/`gloam` themes

**What.** spec/07 lists four standard themes (`dusk`, `vellum`, `embers`, `gloam`) the runtime "must include."

**Why it risks.** The orchestrator brief and DESIGN emphasize `dusk` (default) + `vellum` as the dual-theme requirement; `embers`/`gloam` are dark variants.

**Proposed resolution.** Phase 2 ships `dusk` + `vellum` as first-class (the hard requirement). `embers`/`gloam` ship as additional bundled token sets — trivial once the token-resolution path exists (W-C4), low-risk, can land in Phase 2 polish or slip to a follow-up. **Confirm** dusk+vellum is the Phase-2 bar and embers/gloam are not blockers.

---

## Decision summary table

| # | Item | Tag | Who decides | Gates |
|---|---|---|---|---|
| D1 | Windowing: winit vs CEF native | DECISION | maintainer | W-B1/W-B3 + workspace deps |
| D2 | urlbar/workspace provider sequencing | DECISION | maintainer | W-C1/W-C5/W-C7 |
| D3 | CPU vs accelerated OSR | RISK | engineer (confirm) | W-B1 |
| D4 | Input-routing edge cases | RISK | engineer (confirm) | W-C1 |
| D5 | Multi-CEF-browser memory | RISK | engineer (confirm) | W-A1/W-C3 |
| D6 | Serving chrome (`mote://` vs `file://`) | DECISION | engineer→maintainer | W-A4/W-B3 |
| D7 | DESIGN "fully isolated" wording | DECISION | maintainer (DESIGN edit) | W-A1b |
| D8 | 30d TTL vs 30min discard | RISK | engineer (confirm reading) | W-A1 |
| D9 | Workspace runtime-state home | DECISION | engineer→maintainer | W-A1 schema |
| D10 | capabilities.invoke dispatch shape | RISK | engineer (confirm theme treatment) | W-C4/W-C7 |
| D11 | Glob normalization / canonical resource form | RISK | engineer (hard prereq) | W-C6 |
| D12 | CEF message-router in cef 148 | UNKNOWN | spike first | W-B2 |
| D13 | embers/gloam themes | RISK | engineer (confirm bar) | W-C4 polish |
