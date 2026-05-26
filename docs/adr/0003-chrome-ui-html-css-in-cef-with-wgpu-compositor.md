# ADR-0003 — Chrome UI as HTML/CSS in CEF (OSR) with a Thin wgpu Compositor

- **Status:** Accepted
- **Date:** 2026-05-25

---

## Context and Problem Statement

DESIGN.md §Dependency Stack lists the UI framework as TBD and §Open Decisions leaves the rendering layer unresolved, with a stated lean toward "a thin custom UI layer over `wgpu` or Skia rather than adopting an opinionated framework like `iced` or `egui`." This is a project-lifetime lock-in: the chrome rendering technology determines how the entire UI composition model (slots, elements, themes), the Lua `render(host)` plugin API, accessibility, theming, and CEF page compositing are all implemented.

To settle it, we ran three throwaway prototype spikes against real, running code on the target Linux x86_64 machine: a custom immediate-mode widget toolkit over wgpu, an egui implementation, and the convergent alternative — authoring the chrome as an HTML/CSS document rendered by CEF off-screen and composited with a thin wgpu layer. Each spike rendered the same dusk-theme chrome slice (tab strip, omnibox, integrity-panel sidebar, composited page texture) and measured LOC, frame time, memory, text quality, theming fidelity, and Lua-plugin ergonomics.

This ADR records the decision that lock-in produced.

---

## Decision Drivers

- The frontend spec (`spec/`) is authoritatively HTML/CSS/ARIA: tokens are CSS custom properties, slots are `[data-slot]` elements, components are specified as HTML structure + CSS classes + `@keyframes` + ARIA roles. Any toolkit that is not an HTML/CSS renderer must reimplement this vocabulary as a hand-maintained second representation.
- DESIGN.md's UI composition model (slots/elements/themes), the token cascade, and runtime theme switching are spec requirements that map directly onto a CSS cascade.
- The Lua `render(host)` plugin model must bind ergonomically and without per-frame FFI pressure.
- DESIGN.md mandates the chrome surrounds the page (chrome-as-compositor, page-as-texture), with a sub-millisecond tab-switch target.
- Accessibility is an explicit spec requirement; Mote ships Linux x86_64 first.
- The chrome rendering technology is a project-lifetime decision and must be de-risked with running code, not chosen on paper.

---

## Considered Options

**(a) Custom immediate-mode widget toolkit over wgpu.**
Spike: ~1,116 hand-written LOC, ~0.52 ms/frame offscreen, 5 direct deps, 8 MB stripped binary, ~231 MB RSS (GPU-device baseline). Best-in-class CEF compositing (the blit pipeline proves chrome-surrounds-content directly) and the immediate `render(host)` model is a natural structural fit. But as a *toolkit* it means rebuilding an entire UI platform: hit-testing, input, focus, accessibility, animation, the CSS cascade, runtime theming, and text layout are all unimplemented and on us. Tokens were frozen into Rust consts — a second hand-maintained copy of the design system with no `var()`, no `[data-theme]` stacking, structurally hostile to programmable theming. Per-frame FFI volume (thousands of Lua→Rust calls) pushes toward command-batching that erodes the API ergonomics. Inline multi-color text required hand-placed glyphs and guessed advance widths.

**(b) egui.**
Spike: fastest at ~0.14 ms/frame, leanest at ~634 LOC, near-zero per-frame allocation. But egui's global `Style`/`Visuals` cannot express Mote's per-element token cascade or selector-based theme overrides — the theme contract would have to be redesigned as token-passing threaded through every draw call. Every `render(host)` paint primitive is an mlua FFI crossing (10k+ calls/frame plausible for complex chrome). No accessibility on Linux (AccessKit covers Windows/macOS only). Visual fidelity ceiling is low (no sub-pixel text, no system font stack). Rejected as a poor fit for this spec.

**(c) CHOSEN: chrome authored as an HTML/CSS document rendered by CEF off-screen, composited with a thin wgpu layer.**
Spike: built, linked, and off-screen-rendered cleanly on the target machine. Fewer Rust LOC (~712) than the wgpu toolkit, and the chrome is real HTML/CSS that *already* has the cascade, ARIA, focus rings, and runtime theming the toolkit spike explicitly did not implement.

---

## Decision Outcome

**Chosen option: (c) — Mote chrome is an HTML/CSS document rendered by CEF off-screen (OSR), composited with the page's OSR texture by a thin wgpu compositor.**

The spike returned an unambiguous **GO**. The decisive evidence:

- **CEF brings up on the target hardware.** The `cef` crate (tauri-apps/cef-rs) `148.2.0+148.0.8` (Chromium 148) built, linked, initialized, and off-screen-rendered on the target Linux x86_64 machine with no fatal blockers. CEF binary acquisition and runtime colocation were automatic; the subprocess split worked first try via the `wrap_*` macros.
- **The chrome reuses Chromium's in-process engine.** Layout, text shaping, the CSS cascade, `box-shadow`, focus rings, ARIA accessibility, animations, and input are all native to the document — no translation layer. The spec's exact chrome slice rendered pixel-faithfully from real CSS-variable tokens.
- **The Lua `render(host)` model maps onto the DOM more ergonomically than onto an immediate painter.** `host:token("accent")` resolves to `var(--accent)`, so the CSS cascade owns theming — a theme switch is a CSS-var rewrite under `[data-theme]`, exactly what the spec mandates, with no duplicate design system. FFI was measured at ~200 ns/call and occurs **only on DOM mutation, not per frame** (CEF retains the DOM), eliminating the per-frame FFI pressure that burdened the toolkit option. Text measurement and layout are Chromium's job, not the plugin author's.
- **The compositor half is the thin wgpu layer the design doc actually wanted.** The wgpu compositor (reused from the wgpu spike's blit pipeline) composites the chrome surface over the page OSR texture (chrome-surrounds-content), and a tab switch is a texture swap — keeping the sub-millisecond tab-switch target reachable.
- **Security isolation is structural.** Chrome and each web page are distinct CEF browsers in distinct renderer processes; the privileged host/`window.mote` bindings are installed only on the chrome browser; per-plugin isolated worlds handle page-side injection.

The custom layer Mote builds is a thin wgpu **compositor**, not a custom widget **toolkit** — it sits *under* CEF-rendered chrome rather than replacing it. This is the genuinely thin custom layer the design doc's lean was reaching for.

---

## Consequences

**Good:**
- The spec's HTML/CSS chrome is validated as directly implementable — no hand-translated widget tree, no reinvented CSS engine.
- A single rendering engine (Chromium/CEF) renders both chrome and page; no second UI runtime.
- Strong plugin-UI ergonomics: tokens-as-CSS-vars, retained DOM, FFI only on mutation.
- Accessibility, focus rings, animations, and the cascade come for free from Chromium — including on Linux, where egui's AccessKit backend does not reach.

**Bad / risks:**
- The chrome now inherits Chromium's monthly upgrade treadmill (the cef-rs API reshapes with each Chromium bump) **and** the Linux accelerated-OSR ANGLE/Ozone GPU path. The spike used the safe CPU `on_paint` fallback; the accelerated zero-copy path is validated as available but not exercised. **Both must live behind the `mote-cef` wrapper crate (DISCIPLINES §1)**, with the CPU path as the guaranteed v0.1 baseline and accelerated OSR as an opt-in once the ANGLE path is proven on target hardware.
- The chrome↔privileged-bridge boundary is the crown-jewel attack surface: any path that renders page-derived strings (titles, URLs, favicons, plugin content) into the privileged chrome document is an injection vector into the privileged world. **Mandatory escaping/sanitization of all page-derived strings at the host boundary is required**, alongside structured (non-string) DOM ops via the CEF message router, a chrome-document CSP, and keeping all privileged bindings off the page browsers.
- The ~50–100 MB shell memory target is unachievable. Full-shell PSS measured ~305 MB — at the top of, not above, the prior spikes' range; the ~230 MB GPU-device baseline is a cost both architectures pay, and the target is a Chromium-embedding reality that DESIGN.md is being updated to restate.

**Neutral:**
- `mote-cef`'s build must emit a `$ORIGIN` rpath so `libcef.so` resolves next to the binary (the spike used `LD_LIBRARY_PATH` as a stopgap).
- Development targets X11/XWayland first (`--ozone-platform=x11`); native Wayland hardening is a later task.
- The unstripped `libcef.so` is ~557 MB on disk (strippable for shipping); the CEF distribution footprint (~150–600 MB unpacked) is the accepted cost of embedding Chromium.

---

## Links / References

- DESIGN.md §Engine — CEF (off-screen rendering as a listed capability; extensions subsystem off)
- DESIGN.md §UI Composition — Slots, Elements, and Themes
- DESIGN.md §Open Decisions — "UI framework / rendering layer" (resolved by this ADR)
- DISCIPLINES.md §1 — CEF upgrade discipline (all CEF interaction behind `mote-cef`)
- `docs/research/ui-spike-wgpu.md` — custom-wgpu toolkit spike (option a)
- `docs/research/ui-spike-egui.md` — egui spike (option b)
- `docs/research/ui-spike-cef-html.md` — HTML/CSS-in-CEF spike (option c, GO verdict)
- `docs/research/cef-and-ui-framework.md` — CEF/cef-rs integration and UI framework recommendation
