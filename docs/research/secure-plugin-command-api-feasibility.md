# Can a sandboxed plugin securely provide the command API? (vim-as-plugin)

- **Date:** 2026-06-06
- **Project:** Mote (Apache-2.0). Gating question for the editing-paradigm-as-plugin ADR (ADR-0019, proposed).
- **Question:** can a sandboxed first-party plugin securely intercept keys, own editing modes (vim NORMAL/INSERT…), own the `:` command-line, and dispatch browser commands — so vim is a *swappable* paradigm plugin (emacs could replace it) — without breaking Mote's sandbox / capability / transparency guarantees?

## Verdict: **feasible-with-new-primitives**, via a DECLARATIVE keymap model — NOT an imperative per-key callback.

The design already anticipated this: `vim-mode` is named a Tier-2 first-party plugin (DESIGN.md:1776); the integrity-panel mockup shows it holding `keys:bind` + `keys:intercept_input` (DESIGN.md:1697); the permission registry already defines both (`mote-registry/data/permissions/v1.toml:296-310`); `HookType::Keybind` with input-coalescing, no-auto-disable dispatch is built and tested (`mote-dispatch/src/keybind.rs`, `engine.rs:451-509`); the runtime already maps `keys:*` hooks (`mote-runtime/src/runtime.rs:840`). ADR-0012 reserved `keybind:register-global` "for a future ADR" — that ADR is gated by this study.

## Grounded facts
- **Sandbox is tight** (`mote-lua/src/sandbox.rs`): no `io`/`os`/`package`/`debug`/`ffi`, no `load`/`require`. The only host effect is the declared-permission `mote.*` API → **observation ≠ exfiltration** (a key-observing plugin has no ambient network/fs/clock channel). First-party plugins are Lua; WASM (`mote-wasm`) has no input/command surface.
- **Permissions** are `domain:action:resource` globs, gated synchronously + audited per call (`mote-runtime/src/hostapi.rs:105-126`). Latency budget <100µs/Lua call (DESIGN.md:133); keybind dispatch is coalescing + deadline-bounded + exempt from auto-disable.
- **Input chokepoint:** every key (incl. content-destined, e.g. passwords) passes `mote-shell/src/lib.rs` `intercept_keybind`:3988 → `route_key`:4492 (to `FocusOwner` Chrome|Page). Keybinds today are a hardcoded Rust chord table (`classify_chord`:403) — **no path to the plugin runtime exists yet** (`Runtime::dispatch_keybind`:199 is plumbed but unfed).
- **Command surface exists** as chrome-bridge ops (navigate/new_tab/close_tab/select_tab/go_back/forward/reload/stop/set_active_workspace/set_theme/zoom/find…, `lib.rs:1196-1567`), reachable only from the `mote://chrome` origin. The plugin Lua `mote.*` API exposes **none** of these today.
- **Mode indicator:** `mote.mode` (NORMAL) is a chrome built-in (ADR-0016:128); a plugin can publish its own statusline element via `mote.statusline.set` but cannot drive the built-in.

## The security crux (and the mitigation)
A naive global key-interceptor would sit upstream of content → **would see passwords typed into web forms**. The defenses: (1) sandbox = no ambient capability; (2) exfil needs a *second, loud, independently-audited* grant (`http:fetch`/`mcp:client`/`sys:native_message`/`clipboard:write`) — and `keys:intercept_input` + any of those is a textbook keylogger that **should be added to `combinations/v1.toml` as `severity="danger"`** (the registry exists, the entry doesn't); (3) bounded command host-fns, not raw ops; (4) loud special-class grant + Integrity-Panel disclosure. **Strongest mitigation — withhold content keystrokes by construction:** when focus is content + an editable field is active (or mode==INSERT), keys bypass the plugin and go straight to CEF. Signals exist (`FocusOwner` lib.rs:490, CEF `focus_on_editable_field` input.rs:173). Converts "sees passwords" → "sees only nav/command keys outside text entry."

## Declarative keymap > imperative callback (both security AND latency)
An imperative "shell calls Lua on every keydown" model *must* receive every key (incl. content keys) to decide, and adds a synchronous Lua hop on the content input path. The **declarative model** — plugin declares modes + chord→command bindings + a small motion grammar at load (ADR-0001 pattern); shell evaluates chords in Rust and calls Lua only for *fired actions* (and only when not in content text-entry) — is faster, keeps content keys out of Lua, and is statically inspectable.

## Primitives: exist vs need-building
- **Exist:** `HookType::Keybind` + coalescing dispatch; `keys:bind`/`keys:intercept_input` perms; `keys:*`→hook mapping; per-keystroke bounded dispatch precedent (ADR-0010 `events.collect`); plugin statusline element (ADR-0016); the browser-command ops; the input chokepoint + chrome/content focus signal.
- **Need building:** input→runtime wiring; an exclusive `editing-mode:provider` (or `command:provider`) capability (makes vim swappable for emacs); a declarative keymap/grammar schema; a bounded `mote.command.dispatch(name,args)` host-fn over a registry allowlist; an omnibox-mode primitive (open the `:` runner); delegate the `mote.mode` element to the provider; the content-keystroke-withholding policy; the `combinations/v1.toml` keylogger entry; conflict-resolution precedence (plugin vs core built-ins vs user overrides — ADR-0012 deferred this).

## Recommended secure design (→ ADR-0019)
Exclusive capability contract + declarative keymap (shell-evaluated) + bounded command host-fns + content-keystroke withholding + loud `keys:intercept_input` grant + the danger combination entry. Move `[cmd]`/`:` and the `mote.mode` chip OUT of core to the paradigm plugin; keep find-in-page capability + the browser-command ops in core (exposed to the plugin via the bounded host-fns).

Key files: `mote-shell/src/lib.rs` (intercept_keybind:3988, route_key:4492, classify_chord:403, FocusOwner:490, command ops:1196-1567), `mote-dispatch/src/{keybind,engine,hook}.rs`, `mote-runtime/src/{runtime.rs:840,hostapi.rs}`, `mote-lua/src/sandbox.rs`, `mote-cef/src/{bridge.rs,input.rs:173}`, `mote-registry/data/permissions/v1.toml:296-310`, `combinations/v1.toml`; ADRs 0001/0002/0010/0012/0016.
