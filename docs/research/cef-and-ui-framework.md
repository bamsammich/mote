# Research: CEF/cef-rs Integration & UI Framework Decision

- **Date:** 2026-05-25
- **Project context:** Mote — a programmable, AI-native browser in Rust embedding Chromium via CEF. This document resolves two coupled, project-defining decisions: how `mote-cef` wraps CEF, and what renders the browser chrome. Read alongside `DESIGN.md` (Engine — CEF, Performance Architecture, UI Composition, Open Decisions), `spec/` (the authoritative frontend spec), and `DISCIPLINES.md §1` (CEF behind a single wrapper crate).
- **Status:** Recommendation. Section B's recommendation contradicts the design doc's stated lean; the contradiction is load-bearing and explained below. Spec is treated as immutable; this doc conforms to it.

---

## Section A — CEF / cef-rs

### A.1 State of the `cef` crate (tauri-apps/cef-rs)

| Fact | Value (as of 2026-05-25) |
|---|---|
| Crate name | `cef` (workspace at `tauri-apps/cef-rs`) |
| Latest published version | **`148.2.0+148.0.8`** (published 2026-05-25) |
| Chromium / CEF tracked | **Chromium 148** (CEF 148.0.8); version major == Chromium major |
| Earlier reference points | 146.0.0 (Chromium 146), 140.3.0+140.1.14 (the docs.rs default a few months back) |
| Maintainer | Tauri working group; automated releases via `release-plz`, `tauri-bot` publishing |
| API doc coverage | ~70% (1986 / 2841 items) — the binding wraps the *entire* CEF C API |
| Open issues | **7**, recent activity (most recent #409, 2026-05-13). None are showstoppers. |

**Versioning model.** The crate version's major number tracks Chromium's major, and the `+x.y.z` build metadata pins the exact CEF build. A new `cef` release lands within days of each CEF/Chromium bump (148 was published *today*). This directly enables `DESIGN.md`'s "monthly upgrade cadence" and `DISCIPLINES.md §1`'s CEF-upgrade discipline — and makes that discipline mandatory, because the binding surface moves with Chromium.

**API shape.** The crate is a generated 1:1 safe wrapper over the CEF C API (`cef-dll-sys` is the raw bindgen layer; `cef` is the safe layer with refcounting + conversion traits). Because it wraps the whole capi, the handler traits Mote depends on are all present (confirmed on docs.rs): `App`, `Client`, `BrowserProcessHandler`, `RenderProcessHandler`, **`RequestHandler`**, **`ResourceRequestHandler`**, **`RenderHandler`**, plus the `window_info` module, `Sandbox`, and `LibraryLoader`.

**Stability caveat.** This is *not* a hand-curated stable API. It is a thin generated wrapper that reshapes whenever CEF reshapes, and the maintainers' own open issues acknowledge ergonomic rough edges (#297 "`wrap_*` macros are not good DX", #364 stderr spam on startup). Treat the cef-rs *API* as unstable and Chromium-versioned — which is exactly why `mote-cef` exists.

**Open issues worth knowing (not blockers):**
- #208 — "Implement WRY interface on top of off-screen rendering samples" (OSR exists as an example and is being built up).
- #192 — tracking integration with Tauri as an alternate web renderer (signals the crate is heading toward production webview use, good for longevity).
- #29 — link/bundle deps are partly hardcoded rather than parsed from CMake output (a build-robustness wart, relevant to packaging).
- #409 — Windows app_id; #364 — UTF-16 stderr spam during normal startup (cosmetic, noisy logs).

### A.2 Binary distribution & linking on Linux x86_64

- **Acquisition.** The `download-cef` crate streams the CEF prebuilt `.tar.bz2` for the `linux64` target from `https://cef-builds.spotifycdn.com/index.json`, verifies a SHA-1 against the index, and extracts. `cef-dll-sys`'s build script does this automatically into `OUT_DIR` if you haven't pre-provisioned. Two utilities matter:
  - `export-cef-dir` (`cargo run -p export-cef-dir -- --force`) — provisions a shared CEF dir once so subsequent builds don't re-download. Strongly recommended for CI and dev.
  - `bundle-cef-app` — produces the platform layout (the helper/subprocess executable, resources, `.app` on macOS). Bundle behavior (helper name, resources) is configured via `Cargo.toml` `[package.metadata]`.
- **Footprint.** CEF distribution is the ~100–200 MB on-disk reality `DESIGN.md` already accepts (Chromium binary + `icudtl.dat` + `*.pak` resource bundles + `v8_context_snapshot.bin`). The `download-cef` page doesn't state a number; budget ~150 MB unpacked for linux64.
- **Linking.** Link against `libcef.so`; CEF resources must sit alongside the executable at runtime (the Linux extraction "flattens Resources/ into the CEF root"). The hardcoded-link-deps issue (#29) means odd CMake/link layouts can need manual nudging — wrap that knowledge in `mote-cef`'s build script, not scattered.
- **Sandbox.** `cef_sandbox` is primarily a macOS/Windows concern in the crate (macOS renames `cef_sandbox.a` → `libcef_sandbox.a`). On Linux, Chromium's sandbox uses the SUID-helper / user-namespace model and is governed by CEF settings + the subprocess executable rather than a separate static lib link. A `Sandbox` type exists in the crate for the platforms that need explicit init/destroy.

### A.3 The subprocess (multi-process) model in Rust

CEF is multi-process: one browser process plus renderer/GPU/utility subprocesses. The `cef` crate's idiomatic pattern is a **single binary that re-execs itself** as its own helper:

1. `main()` constructs the `App` and (macOS) loads the framework via `LibraryLoader`.
2. Call `execute_process` early. In a subprocess invocation this runs the CEF subprocess loop and **never returns** — `main` exits there.
3. In the browser-process invocation, `execute_process` returns; then `initialize(settings, app)` runs, the browser host is created, and `run_message_loop()` drives the UI until shutdown.
4. `bundle-cef-app` wires the `helper_name` so the same binary is invoked as the helper. There is no separate hand-written helper crate.

`mote-cef` should own this entire split so the rest of Mote never sees a raw `execute_process` call.

### A.4 OSR (CefRenderHandler) vs native windowed — the central integration choice

| Axis | OSR (windowless, `RenderHandler`) | Native windowed |
|---|---|---|
| Output | CEF renders into a buffer Mote owns; `on_paint` (CPU BGRA) or `on_accelerated_paint` (GPU shared texture) delivers frames | CEF owns a real OS child window/surface |
| Compositing with custom chrome | Mote composites the page texture into its own scene — page can sit under rounded corners, overlays, animated chrome | Page is a separate native surface; chrome must be *around* it, not over it. Overlays/transparency over the page are painful |
| Input routing | Mote captures all input and forwards via `send_*_event` — full control (needed for vim-mode, omnibox interception, keybinds) | OS routes input to the CEF window directly; intercepting is fragile |
| Perf | Extra compositor hop; **on Linux, accelerated shared-texture OSR is not the default path** (see footguns) — expect CPU `on_paint` + a per-frame texture upload unless ANGLE/EGL is configured | Native GPU path, lowest latency, but you give up the unified compositor |
| Fit for Mote | **Strong** — Mote's whole UI thesis (slots/elements over and around the page, sub-ms tab switch via texture swap, theme-driven chrome) presumes the chrome composites the page, not the reverse | Weak for the chrome model; only attractive if OSR perf proves unacceptable |

**Recommendation: OSR.** Mote's slots/elements/themes model and the integrity-panel/omnibox/sidebar overlays require the chrome to be the compositor and the page to be a texture within it. A tab switch is then "bind a different page texture into the `viewport` slot" — which is how the sub-millisecond target is actually achievable. Windowed mode cannot deliver that composition.

The cost is paid in input plumbing (Mote forwards every mouse/key/IME event into CEF) and in the Linux GPU path. Keep windowed mode reachable behind a `mote-cef` flag as an escape hatch if OSR latency disappoints on weak GPUs, but design for OSR.

### A.5 `CefResourceRequestHandler` — the network hook

**Confirmed exposed.** `RequestHandler` and `ResourceRequestHandler` are both present in the `cef` crate API. The CEF flow Mote's `net:intercept_request` filter chain rides on:

`Client::get_request_handler` → `RequestHandler::get_resource_request_handler` → `ResourceRequestHandler::{on_before_resource_load, get_resource_handler, on_resource_response, get_resource_response_filter}`.

This is the single hardest plugin API in the design and it maps directly onto CEF primitives the crate wraps. `on_before_resource_load` is where block/modify/allow/defer decisions land; `get_resource_response_filter` backs `net:read_response_body` / `net:modify_response`. `mote-cef` exposes a Rust-native trait here; the permission-dispatch layer (not plugins) implements it, then fans out to Lua/WASM handlers.

### A.6 Footguns building a CEF app in Rust today

- **Linux accelerated OSR needs ANGLE.** Shared-texture (zero-copy GPU) OSR is mature on Windows (D3D11) and is the *exception, not the rule* on Linux. On Linux you must pass `--use-angle=gl-egl` (and an `--ozone-platform`) or `GetGLOzone()` returns null and the Skia/shared-image pipeline breaks. Without that, you fall back to CPU `on_paint` + an upload-per-frame. **Bake these switches into `mote-cef`'s default `CefSettings`/command-line.** (chromiumembedded/cef #3953, #3263.)
- **Wayland vs X11 / GTK assertions.** Mixing a GTK host with Wayland objects triggers `GDK_SCREEN_XDISPLAY`-style assertion crashes. CEF's Aura/Views path is required under Wayland. For OSR specifically Mote isn't using CEF's own windowing, which *reduces* exposure — but the GPU/Ozone selection still matters. The available `DISPLAY=:1` (X11/XWayland) here is a workable dev target; force `ozone-platform=x11` for predictability during development. Treat native Wayland as a later hardening task.
- **Binary is huge and runtime-colocated.** CEF can't be a pure-Cargo dependency; resources (`*.pak`, `icudtl.dat`, snapshots) must ship next to the binary. Bundling/packaging is real work (this is also why `wew` exists as an alternative wrapper — noted, but cef-rs's tauri backing makes it the better bet).
- **Generated-API churn + ergonomic warts.** `wrap_*` macros are clumsy (#297) and startup logs spam UTF-16 warnings (#364). None block, but they reinforce: keep all of it behind `mote-cef`.
- **Build robustness.** Hardcoded link/bundle deps (#29) mean non-standard environments (NixOS — which `DESIGN.md` explicitly supports for source builds) may need build-script overrides.

### A.7 Recommended `mote-cef` wrapper shape

`mote-cef` should expose a small, Mote-shaped, **OSR-first** surface and hide every `cef::` type:

- **Process entry:** `mote_cef::bootstrap()` owns the `execute_process` re-exec split; subprocess path never returns. Mote's `main` calls this before anything else.
- **Settings:** internal default `CefSettings` with Linux GPU/Ozone switches (`use-angle=gl-egl`, `ozone-platform`), `windowless_rendering_enabled = true`, extensions subsystem **off** (per design).
- **Browser handle:** `mote_cef::Page` wrapping `BrowserHost`, with `resize`, `send_input(...)`, and a frame callback delivering the current texture (GPU handle when accelerated OSR is live, CPU BGRA buffer otherwise) for the renderer to composite.
- **Network hook:** a `mote_cef::ResourceInterceptor` trait mapped onto `ResourceRequestHandler`; the permission-dispatch layer implements it. No plugin or other crate ever names a `cef::` type.
- **CI guard (DISCIPLINES §1):** fail any `use cef::` / `use cef_rs::` outside `mote-cef`.

---

## Section B — UI framework

### B.0 The decision is reframed by the spec

`DESIGN.md` leans toward "a thin custom UI layer over `wgpu` or Skia." **The frontend spec overrides that lean**, and the spec is authoritative for what the UI must render:

- `spec/00_overview.md`: *"Mote uses HTML/CSS for chrome."* Non-goal: cross-platform native widgets. Stack: *"chrome renders via a web technology … delivered as CSS variables + HTML structural conventions."*
- `spec/01_architecture.md`: the runtime *"renders these slots as standard HTML with `data-slot` attributes"*; themes target slots *"via CSS variables on `[data-slot=...]`."*
- `spec/03_tokens.md`: tokens are **CSS custom properties** first (`--surface-1`), Lua fields second; the canonical source is a `.css` file.
- Every component (`omnibox.md`, `tabs.md`, `palette.md`, `sidebar.md`, …) is specified as HTML structure + CSS classes + `@keyframes` animations + ARIA roles (`role="search"`, `aria-label`, focus rings, `box-shadow`).

This means the real question is **not** "iced vs egui vs custom-wgpu as a widget toolkit." It's **"what renders an HTML/CSS document for the chrome, and how does the page (CEF OSR texture) composite into it."** A token-keyed CSS cascade, theme stacking via `[data-theme]`, ARIA, and CSS animations are the spec's vocabulary — reimplementing that vocabulary on a retained/immediate widget tree is rebuilding a browser engine to host a browser.

The three options must therefore be read as: **(1) custom wgpu/Skia = build/own an HTML/CSS renderer; (2) iced = retained widget tree, no HTML/CSS; (3) egui = immediate widget tree, no HTML/CSS.** None of the three *as-stated* is "render the spec's HTML/CSS." The recommendation resolves this.

### B.1 Comparison matrix

| Criterion | (1) Custom over wgpu/Skia | (2) iced 0.14 | (3) egui 0.34 |
|---|---|---|---|
| Renders the spec's HTML/CSS | Only if you *build* a CSS engine on top — enormous | No (widget tree; CSS is a translation layer you hand-write) | No (immediate widgets; CSS → manual) |
| Slots/elements/themes model | Must hand-build layout + cascade | Map slots→containers, elements→widgets, theme→`Theme` struct; tokens→Rust consts. Theme *stacking* is manual | Map slots→panels, elements→immediate calls; theme via `Style`. Stacking very manual |
| Token vocabulary (`--surface-1`, etc.) | Implement a token→style resolver | Translate every CSS token to a Rust field; no cascade/`var()` | Same; tokens become `Style` mutation |
| Theme stacking (non-exclusive `theme:provider`) | Hand-build cascade/merge | No cascade model; merge in Rust | No cascade; merge in Rust |
| CSS animations / motion tokens | Hand-build a timeline system | Re-express as iced animations (0.14 improved) | Per-frame manual easing |
| Composite CEF OSR texture | First-class — you own the compositor; bind page texture into `viewport` slot. **Best** | Possible via `iced::widget::shader` / custom wgpu primitive; integrating an external texture is off the beaten path | Possible via `egui_wgpu` `Callback` / `register_native_texture`; **most direct of the three** |
| Text rendering quality | You own it (cosmic-text/swash); top ceiling, high effort | Good (cosmic-text) | Good (now skrifa + vello_cpu; hinting/variations landed) |
| Retained vs immediate fit for chrome | N/A (you choose) | Retained — fits browser chrome state (tabs, panels) well | Immediate — re-emit whole UI each frame; awkward for deep, persistent, themed chrome and per-frame CPU cost |
| Accessibility | You implement AccessKit yourself | AccessKit hooks present (0.14) | AccessKit present but **Windows/macOS only — no Linux a11y**; Mote ships Linux x86_64 |
| Dev velocity | Lowest (you build the platform) | Medium | Highest for simple UIs, drops for spec-fidelity theming |
| Control / fidelity to spec | Highest ceiling, highest cost | Medium — pixel-faithful theming fights the widget model | Low — immediate-mode look resists the spec's precise CSS |
| Memory / perf vs 50–100 MB target | Excellent if lean | Good; wgpu retained scene | Good, but per-frame redraw burns CPU/GPU continuously (battery, idle cost) |
| Ecosystem maturity @ Rust 1.95 / ed. 2024 | wgpu mature; *no* CSS engine to lean on | iced 0.14 mature, edition-2024 ok | egui 0.34 very mature, edition-2024 ok |

### B.2 Prose

**egui** is the fastest way to get *a* UI on screen and the most direct CEF-texture compositing (`egui_wgpu` native-texture registration). But it is the worst fit for *this* spec: immediate mode re-emits the entire chrome every frame (continuous CPU/GPU even when idle — wrong for a long-lived browser shell on a laptop), its visual idiom resists the spec's exact CSS treatment (block cursor, focus rings, keycap-depth borders, dot-grid empty slots), and its AccessKit backend doesn't cover Linux — Mote's launch platform. Rejected.

**iced** is a better structural fit than egui — retained, Elm-style state matches tabs/panels/sidebars, AccessKit hooks are wired, 0.14 added reactive rendering and better animations, and CEF OSR can composite through a custom `shader` primitive. But every spec token, cascade rule, theme-stack merge, ARIA mapping, and `@keyframes` becomes hand-maintained Rust that must be kept in lockstep with `.css` files that are declared the *ground truth*. You'd be maintaining two representations of the design system and a translation layer between them, forever. That is the exact "two representations drift" tech-debt the disciplines doc warns about, structurally guaranteed.

**Custom over wgpu/Skia** as a *widget toolkit* is the design doc's lean, but taken literally against this spec it means **writing an HTML/CSS layout+cascade engine** — Mote would be building a second browser engine to host the first. That is not "thin."

**The resolution the spec is actually pointing at:** render the chrome as a real HTML/CSS document, because the spec *is* HTML/CSS, and Mote already ships a world-class HTML/CSS engine in-process — **CEF itself**. The chrome becomes a privileged CEF view (a windowed/native CEF browser, or a second OSR surface composited on top) loading Mote's internal `mote://chrome` document; the Lua runtime drives it over a CEF message-router/JS bridge; web pages are OSR textures composited into the `viewport` slot. This:
- renders the spec's HTML/CSS/ARIA/animations exactly, with zero translation layer;
- makes tokens-as-CSS-variables and `[data-theme]` stacking *native*, not reimplemented;
- inherits accessibility from Chromium (covers Linux, unlike egui);
- keeps the shell lean (no second UI runtime — `tabs.md`-style chrome is a few hundred KB of HTML/CSS);
- and the sub-ms tab switch is a texture/host swap, not a re-layout.

The trade is a thin custom **compositor** in wgpu (page-texture + chrome surface), not a custom **toolkit**. That is the genuinely thin custom layer the design doc wanted — it's just thinner and lower than "a UI framework," sitting *under* CEF-rendered chrome rather than replacing it.

### B.3 Recommendation

**Render Mote's chrome as an HTML/CSS document inside a dedicated CEF surface, with a thin custom wgpu compositor combining the chrome surface and page OSR textures. Do not adopt iced or egui, and do not build a from-scratch CSS engine.**

- **Why (one sentence):** the frontend spec is authoritatively HTML/CSS/ARIA, Mote already embeds the best HTML/CSS engine in existence (CEF) in-process, so the chrome should *be* a web document rather than a hand-translated widget tree (iced/egui) or a reinvented CSS engine (raw wgpu/Skia).
- **Top risk:** coupling the chrome to CEF means the chrome rides Chromium's upgrade treadmill and an interactive-chrome-over-OSR-page compositor on Linux must thread the ANGLE/Ozone GPU path (A.6) — i.e., the UI layer inherits the CEF-upgrade discipline and the Linux accelerated-OSR footguns, both of which must live inside `mote-cef`. Secondary risk: driving privileged chrome via a CEF JS bridge needs a hard security boundary so the chrome document can't be reached or scripted by web content (separate isolated context, distinct from the per-plugin isolated worlds).

This recommendation *changes a design-doc Open Decision*; flag for the user (below).

---

## Open questions for the user

1. **Chrome-as-CEF-document vs hand-translated widget toolkit — confirm the reframing.** The frontend spec mandates HTML/CSS chrome, which contradicts `DESIGN.md`'s "thin layer over wgpu/Skia" lean. The recommendation reconciles them (custom layer = compositor, not toolkit; chrome = CEF-rendered HTML/CSS). **Does the spec's "Mote uses HTML/CSS for chrome" override the design doc's lean, or is the spec itself open to revision?** This is the single decision everything else hangs on.
2. **Chrome surface: native windowed CEF view vs a second OSR surface composited in wgpu?** Windowed chrome is simpler and gets native input/a11y for free but constrains overlay/compositing freedom; OSR chrome unifies the compositor but doubles the OSR plumbing. Both are viable; pick based on how much the chrome must visually overlap/animate over the page.
3. **Linux GPU baseline.** Accelerated zero-copy OSR is not Linux's default path. Is CPU `on_paint` + texture upload an acceptable v0.1 fallback if ANGLE/EGL shared-texture proves flaky on target hardware, or is GPU-accelerated OSR a hard v0.1 requirement?
4. **Wayland vs X11 for v0.1.** Target X11/XWayland first (matches the available `DISPLAY=:1`) and defer native Wayland hardening? Recommended, but confirm.

---

## Key sources

- cef crate — https://crates.io/crates/cef , https://docs.rs/crate/cef/latest (148.2.0+148.0.8, 2026-05-25)
- cef-rs repo / README / issues — https://github.com/tauri-apps/cef-rs , https://github.com/tauri-apps/cef-rs/issues
- cef-rs architecture (DeepWiki) — https://deepwiki.com/tauri-apps/cef-rs , https://deepwiki.com/tauri-apps/cef-rs/5.1-cef-download-and-setup
- CEF Linux OSR / ANGLE / Ozone — https://github.com/chromiumembedded/cef/issues/3953 , https://github.com/chromiumembedded/cef/issues/3263
- CEF shared-texture OSR (Windows D3D11) — https://github.com/chromiumembedded/cef/issues/3730 , https://github.com/chromiumembedded/cef/issues/4057
- iced 0.14 — https://github.com/iced-rs/iced/releases , https://iced.rs/
- egui 0.34 + AccessKit (Win/macOS only) — https://github.com/emilk/egui/releases , https://github.com/emilk/egui/pull/7850
- Mote spec (authoritative): `spec/00_overview.md`, `spec/01_architecture.md`, `spec/03_tokens.md`, `spec/components/*.md`
