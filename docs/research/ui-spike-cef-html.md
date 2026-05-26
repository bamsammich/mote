# UI Spike #3 — Chrome as HTML/CSS in CEF (OSR) + thin wgpu compositor

- **Date:** 2026-05-25
- **Project:** Mote (programmable AI-native browser). The THIRD and decisive throwaway
  prototype to settle the chrome UI framework (a project-lifetime lock-in).
- **Crate:** `spikes/ui-cef-html/` (standalone — empty `[workspace]`). mise toolchain,
  rust 1.95, edition 2024.
- **Prior spikes:** `ui-spike-wgpu.md` (custom-wgpu toolkit) and an egui sibling. Both
  concluded the right architecture is HTML/CSS chrome in CEF + a thin wgpu compositor,
  NOT a hand-built native widget toolkit. This spike VALIDATES that conclusion against
  real, running code.
- **Status:** **Built, linked, ran off-screen, produced `spikes/ui-cef-html/out.png`.**
  CEF bring-up SUCCEEDED on this machine. Verdict at the bottom: **GO.**

---

## 1. CEF bring-up (the highest risk) — SUCCEEDED

**The `cef` crate (tauri-apps/cef-rs) `148.2.0+148.0.8` built, linked, initialized, and
off-screen-rendered on this Linux x86_64 machine with zero manual intervention.** This
is the load-bearing de-risk and it passed cleanly.

What it took, concretely:

| Item | Reality |
|---|---|
| Crate version | `cef = "148"` resolved to `148.2.0+148.0.8` (Chromium 148.0.7778.96, CEF 148.0.8) |
| CEF binary acquisition | **Fully automatic.** `cef-dll-sys`'s build script invokes `download-cef` (v2.3.2), streams the `linux64` prebuilt from `cef-builds.spotifycdn.com`, verifies, and extracts into `OUT_DIR`. No `export-cef-dir` / `CEF_PATH` needed for a dev build. |
| Distribution | The **`minimal`** CEF dist. Both `Release/` (binaries) and `Resources/` (`.pak`, `icudtl.dat`, locales) are present and complete. |
| Footprint (unpacked) | **~596 MB** for the CEF dist. `libcef.so` is **557 MB unstripped** (this dominates; it is strippable for shipping). Plus `libGLESv2.so` 19 MB (ANGLE), `libEGL.so` 772 KB, `chrome-sandbox` 31 KB, `v8_context_snapshot.bin`, `icudtl.dat`, ~700 locale `.pak`s. |
| Runtime colocation | **Solved automatically.** The build script's `copy_cef_runtime_files` copies `libcef.so`, all `.pak`s, `icudtl.dat`, `locales/`, `v8_context_snapshot.bin`, and ANGLE libs into `target/release/` next to the binary. At runtime I set `LD_LIBRARY_PATH=target/release` (no rpath was emitted; this is the one packaging wart — `mote-cef`'s build should emit `$ORIGIN` rpath). |
| Link flags | `cargo::rustc-link-lib=dylib=cef` + `rustc-link-search` into the extracted dir. No static `libcef_dll_wrapper` on Linux (that path is macOS/Windows). |
| Sandbox | Set `no_sandbox = 1` for the spike. On Linux the sandbox is the SUID/userns model governed by CEF settings + `chrome-sandbox`, not a separate static link (unlike macOS/Windows). For production, keep the sandbox; it needs the helper + `chrome-sandbox` SUID setup. |
| `panic=abort` / `catch_unwind` | **Not needed for this spike.** CEF callbacks (`on_paint`, `view_rect`) ran without unwinding issues. For production, FFI callbacks crossing into Rust SHOULD `catch_unwind` (unwinding across the C ABI is UB) — a `mote-cef` hardening item, not a blocker. |
| Subprocess split | Implemented. Single `execute_process` early-return pattern + a second `[[bin]]` helper (`ui-cef-html-spike_helper`) that only calls `execute_process` and exits. The browser process asserts `ret == -1`; subprocesses return `0` without initializing CEF. Worked first try. |
| API ergonomics | The `wrap_render_handler!` / `wrap_client!` macros are the sane path. My first attempt hand-wrote the `RcImpl` refcounting (`WrapRenderHandler::wrap_rc`) and hit 5 trait-signature errors (E0053 — `wrap_rc` takes `*mut RcImpl<T,Self>`, not `*mut T`). Switching to the macros fixed it. **Lesson for `mote-cef`: always use the macros; never hand-roll the refcount glue.** |
| Build time | First build (incl. CEF download + extract + full wgpu/cef tree): a few minutes. `target/` reaches ~1.3 GB (CEF dist counted twice: OUT_DIR + copied to target/release). |

**Blockers encountered:** none fatal. The two real warts: (1) no rpath emitted, so
`LD_LIBRARY_PATH` is required until `mote-cef` adds `$ORIGIN`; (2) a startup warning
about `root_cache_path` (silenced by setting it). Both are `mote-cef`-internal.

**GPU path used:** CPU `on_paint` (BGRA buffer), NOT accelerated shared-texture OSR.
This was deliberate — it is the deterministic, ANGLE-independent Linux v0.1 path the
prior research recommended as the safe baseline. The `cef` crate DOES ship an
`accelerated_osr` feature (pulls in `wgpu 29`, matching our compositor) with an
`on_accelerated_paint` + `SharedTextureHandle::import_texture` path for zero-copy; that
is the optimization, validated as available but not exercised here.

---

## 2. Chrome slice rendered as HTML/CSS in CEF OSR — DONE

`chrome/chrome.html` is a single HTML document with the spec's **design tokens as CSS
custom properties** (`--accent: var(--amber)`, etc., `[data-theme="dusk"]`). It renders
the same slice as the prior spikes:

- **Tab strip** (40px): 3 tabs, active tab "motesh.dev — themes" with the 2px amber
  top-border + `--bg` fill + accent favicon dot + close ×; inactive tabs in `--fg-2`;
  `+` new-tab button. (`spec/components/tabs.md`)
- **Omnibox row** (36px): sunk-well field with `--accent` focused border + the focus-ring
  `box-shadow`, `[url]` mode tag on `--surface-1`, secure glyph, and the host-dim/host/
  path URL coloring **as three styled spans the CSS cascade colors** — no manual advance-
  width math (the exact brittleness the wgpu spike flagged). Two keycap icon buttons.
  (`spec/components/omnibox.md`)
- **Left "Browser Integrity" sidebar** (280px): bracketed panel header, one card —
  `password-manager-1password` (mono), `v1.0.0`, a border-only `success` "verified"
  badge with status dot, three monospace permission lines with check glyphs, and a
  `Revoke` (danger) + `Update` (secondary) keycap button row. (`sidebar/badge/button/card.md`)
- **Viewport**: `background: transparent` so the page OSR texture composites through.

The frame is captured as a CPU BGRA buffer in `on_paint` and handed to the compositor.
Rendering is pixel-faithful to the dusk theme (see `out.png`) — produced by Chromium's
own engine, so ARIA roles, focus rings, `box-shadow`, and the cascade are **native, with
zero translation layer**.

---

## 3. Composite chrome + page — DONE → `out.png`

A second OSR `Browser` renders `chrome/page.html` (a warm gradient page) at 1000×724.
A thin wgpu blit compositor (adapted from `ui-wgpu`'s `blit.wgsl`) draws:

1. the **page** texture into the `[viewport]` rect (x=280, y=76, 1000×724), then
2. the **chrome** texture over the full 1280×800 (its transparent viewport region lets
   the page show through),

reads back, and writes `spikes/ui-cef-html/out.png` (1280×800). This is the
chrome-surrounds-content thesis proven end-to-end through **two real CEF OSR textures**.
A tab switch in production = bind a different page texture; the chrome texture is reused.

| Metric | Value |
|---|---|
| First-frame latency (CEF init → both browsers painted) | **~500 ms** (cold; dominated by Chromium bring-up, one-time) |
| Composite + readback (wgpu, two textures → PNG) | **~240 ms** first call (includes wgpu device init + readback); steady-state blit is sub-ms like the wgpu spike (0.52ms there) |
| Output | `out.png`, 1280×800, 679 KB, visually correct |
| GPU | Vulkan, NVIDIA RTX 2080 (same as wgpu spike) |

(The ~240ms composite figure is one-shot: it stands up a fresh wgpu instance/device and
does a synchronous mapped readback. In a real shell the device is persistent and there is
no readback — the wgpu spike already measured steady-state blit at ~0.5ms.)

---

## 4. Chrome-renderer memory footprint

Measured with **PSS** (proportional set size), which is the honest number because every
CEF process maps the shared 557 MB `libcef.so` — naive RSS counts it N times and reports
a misleading ~725 MB.

| Process | PSS (unique) | RSS (incl. shared libcef) |
|---|---|---|
| Browser/main (hosts compositor + **wgpu device**) | **~230 MB** | ~386 MB |
| CEF renderer / GPU subprocesses (zygote-forked) | **~10–33 MB each** | ~46–74 MB each |
| **Full shell, summed PSS (browser + GPU + renderers)** | **~305 MB** | (725 MB naive RSS — ignore) |

**Reading this against the prior spikes' ~230–300 MB:**

- The **chrome-renderer process specifically** (the CEF renderer hosting the chrome HTML
  doc) costs only **~10–33 MB incremental PSS** — cheap. HTML/CSS chrome is a few hundred
  KB of document; the renderer overhead is small.
- The **~230 MB** in the browser/main process is **the same GPU-device baseline the wgpu
  spike measured (231 MB)** — it is the cost of holding a Vulkan device + wgpu open, NOT
  CEF. Both architectures pay this; it is not a CEF tax.
- So CEF's *net* addition over a wgpu-only shell is roughly the renderer/GPU
  subprocesses: **~75 MB unique**. Full shell ~305 MB PSS sits **at the top of the prior
  spikes' range, not above it.** The 50–100 MB design target is missed by both
  architectures equally; that target needs revisiting regardless of UI choice (it is a
  Chromium-embedding reality).

---

## 5. Lua → DOM `render(host)` — ergonomics verdict: **STRONG FIT**

Prototyped in `src/bin/lua_host.rs` with mlua 0.10 (lua54, vendored). `host` is an mlua
`UserData` that builds an HTML/DOM subtree string the chrome document consumes (via
`innerHTML` of the target slot, or — in production — a CEF message-router DOM patch).

Code sketch of the host API (the actual, running probe):

```lua
-- A Mote element, as a plugin author writes it. Builds ONE integrity-panel perm row.
return function(host, perm)
  host:el("div", { class = "perm" })
    host:el("span", { class = "glyph", style = "color:" .. host:token("success") })
      host:text("✓")
    host:close()
    host:el("code", { style = "color:" .. host:token("fg.1") })
      host:text(perm)               -- HTML-escaped by the host
    host:close()
  host:close()
end
-- => <div class="perm"><span class="glyph" style="color:var(--success)">✓</span>
--      <code style="color:var(--fg-1)">http:fetch:https://*.1password.com/*</code></div>
```

Host methods: `host:el(tag, attrs?)` (opens element), `host:text(s)` (escaped text node),
`host:token(name)` → **`var(--name)`**, `host:close()`. Tags balance correctly.

**Why this beats the wgpu spike's immediate-mode painter — concretely:**

1. **Tokens map to `var(--...)`, so the CSS cascade owns the value, not Rust.** The wgpu
   spike froze tokens into Rust consts (a second copy of the design system) with "no
   cascade, no `var()`, no `[data-theme]` stacking." Here a theme switch is a CSS-var
   rewrite under `[data-theme]` — exactly what `spec/07_themes.md` mandates — for free.
2. **FFI volume is a non-issue.** Measured **200 ns per host call**; a ~50-call chrome
   element subtree = ~10 µs. Critically, this runs **only on DOM mutation (dirty
   elements), NOT per-frame at 60 Hz** — because CEF retains the DOM. The wgpu spike's
   central worry ("thousands of Lua→Rust calls per frame, push toward command-batching
   that erodes the API") **does not exist** in the retained-DOM model.
3. **Text measurement / layout is Chromium's job, not the plugin's.** The wgpu spike had
   to hand-place glyphs and guess advance widths (faking inline multi-color URL text).
   Here the browser lays out text; `host` never exposes a `measure()` round-trip.
4. **No reimplementation of focus rings / animation / a11y.** All native to the document.

**Residual friction (honest):** building DOM via imperative `el/text/close` is slightly
more verbose than writing the HTML literally; a real `mote-cef` would likely also support
a template/HTML-fragment path alongside the builder. And the string-injection approach
needs escaping discipline (the probe escapes text nodes; attribute-value escaping must be
equally rigorous to avoid chrome-side injection). Via the CEF V8 message router this
becomes structured DOM ops rather than string concat — cleaner, and the production target.

**Verdict:** The `render(host)` model maps onto HTML/DOM **more ergonomically than onto
the immediate-mode painter**, because tokens-as-CSS-vars eliminates the second design-
system copy and the retained DOM eliminates per-frame FFI pressure.

---

## 6. Chrome/content security isolation

The architecture keeps privileged chrome JS isolated from untrusted web content by
construction:

1. **Separate CEF `Browser` instances.** The chrome doc and each web page are distinct
   browsers with distinct renderer processes (Chromium's site isolation / process-per-
   site applies). In the spike, chrome and page were two separate OSR browsers with
   separate render handlers — they share no DOM, no V8 context, no renderer process.
2. **Host/V8 bindings registered ONLY on the chrome browser.** The privileged bridge
   (the Lua `host` API, message router, `window.mote` bindings) is installed exclusively
   on the chrome `Client`/`RenderProcessHandler`. Web-page browsers get a `Client` with
   no such bindings — a page cannot name or reach `host`/`window.mote`.
3. **Process isolation as the hard boundary.** Even a fully compromised web renderer
   cannot script the chrome renderer: they are different OS processes with no shared
   address space and no IPC channel to each other (only each to the browser process,
   which mediates).
4. **Isolated worlds for plugin injection into PAGES** (`DESIGN.md §Script Injection`).
   When a plugin does `page:inject_script`, it runs in a per-plugin isolated V8 world —
   pristine prototypes, private JS state, shared DOM only. This is orthogonal to the
   chrome boundary: it isolates plugins from the page AND from each other, on the page
   side. The chrome doc is a different boundary entirely (different browser/process).

**Residual risk (state it plainly):**
- The chrome↔Lua bridge is the crown-jewel attack surface: any web content that can
  cause the chrome document to render attacker-controlled markup (e.g. an unescaped page
  title rendered into a tab) could attempt DOM/script injection INTO the privileged
  chrome world. Mitigation: treat ALL page-derived strings (titles, URLs, favicons,
  plugin-supplied content) as untrusted and escape/sanitize at the host boundary — the
  probe escapes text nodes; attribute-value and URL contexts need the same rigor. A CSP
  on the chrome document and structured (non-string) DOM ops via the message router
  reduce this further.
- The chrome rides Chromium's monthly upgrade treadmill (the cef-rs API reshapes with
  Chromium); must live behind `mote-cef` per `DISCIPLINES §1`.

---

## 7. LOC + dependencies

| | |
|---|---|
| Hand-written Rust + WGSL | **712 lines** (main.rs 530, lua_host.rs 129, blit.wgsl 39, helper.rs 14) |
| Hand-written HTML/CSS (chrome + page) | **261 lines** (chrome.html 244, page.html 17) |
| Direct deps | **6** (`cef`, `wgpu`, `pollster`, `bytemuck`, `image`, `mlua`) |
| Transitive crates | **154** |
| Main binary (release) | 7.3 MB |
| Helper binary (release) | 492 KB |
| CEF dist on disk | ~596 MB (libcef.so 557 MB unstripped, strippable) |

Compare: the wgpu toolkit spike was 1,116 hand-written LOC for a *static, single-theme,
no-input, no-a11y, no-animation* mock. This spike is fewer Rust LOC **and** the chrome is
real HTML/CSS that already has the cascade, ARIA, focus rings, and runtime theming the
wgpu spike explicitly did NOT implement. The LOC that exists here is mostly CEF plumbing
(which `mote-cef` amortizes once for the whole project), not per-component work.

---

## 8. FINAL: GO / NO-GO

**GO.** Lock the ADR on: **Mote chrome is an HTML/CSS document rendered by CEF off-screen,
composited with the page's OSR texture by a thin wgpu compositor.**

The decisive evidence: CEF built/linked/OSR-rendered here with no fatal blockers; the
spec's exact chrome slice rendered pixel-faithfully from real CSS-variable tokens with
zero translation layer; chrome+page composited to `out.png` through two real OSR textures;
the `render(host)` Lua model is *more* ergonomic on retained DOM (tokens = `var()`, FFI
only on mutation) than on the immediate painter; security isolation is structural
(separate browsers/processes + bindings only on chrome + isolated worlds for page
injection). Memory sits at the top of — not above — the prior spikes' range, and the
~230 MB is a shared GPU-device cost both architectures pay.

**Top 2 risks if GO:**

1. **Chromium upgrade treadmill + Linux GPU path.** The chrome now inherits CEF's monthly
   Chromium bumps (the cef-rs API reshapes with each), and accelerated zero-copy OSR on
   Linux needs the ANGLE/Ozone path (`--use-angle=gl-egl`, `--ozone-platform`) — this
   spike used the safe CPU `on_paint` fallback. Both MUST live behind `mote-cef` per
   `DISCIPLINES §1`, with the CPU path as the guaranteed v0.1 baseline and accelerated OSR
   as an opt-in once the ANGLE path is proven on target hardware.
2. **The chrome↔privileged-bridge security boundary.** Driving privileged chrome over a
   CEF JS/host bridge means any path that renders page-derived strings into the chrome
   document is an injection vector into the privileged world. Mitigate with mandatory
   escaping/sanitization at the host boundary, structured (non-string) DOM ops via the
   message router, a chrome-document CSP, and keeping ALL bindings off the page browsers.

---

## Files

- `spikes/ui-cef-html/src/main.rs` — process split, two OSR browsers, CPU on_paint
  capture, wgpu two-texture compositor, RSS/PSS + timing instrumentation
- `spikes/ui-cef-html/src/bin/helper.rs` — CEF subprocess helper (`execute_process`)
- `spikes/ui-cef-html/src/bin/lua_host.rs` — Lua→DOM `render(host)` ergonomics probe
- `spikes/ui-cef-html/src/blit.wgsl` — BGRA OSR-texture blit/composite shader
- `spikes/ui-cef-html/chrome/chrome.html` — the chrome slice, tokens as CSS variables
- `spikes/ui-cef-html/chrome/page.html` — stand-in page (gradient) for the viewport
- `spikes/ui-cef-html/out.png` — the rendered evidence frame (chrome surrounds page)

## Relationship to prior research

`cef-and-ui-framework.md` RECOMMENDED this architecture; `ui-spike-wgpu.md` measured the
custom-toolkit alternative it argued against. This spike CONFIRMS the recommendation with
running code: CEF brings up cleanly, the compositor half (which the wgpu spike proved
excellent) is reused verbatim, and the toolkit half the wgpu spike found costly
(tokens/cascade/text-layout/a11y/theming reimplementation) is entirely avoided by letting
CEF be the HTML/CSS renderer. The open decisions in that doc resolve: spec's HTML/CSS
chrome wins (Q1: GO); chrome is a second OSR surface composited in wgpu (Q2); CPU on_paint
is the accepted v0.1 fallback (Q3); X11/XWayland first (Q4) — ran under `DISPLAY=:1`,
`--ozone-platform=x11`.
