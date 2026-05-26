# UI Spike — Custom Immediate-Mode UI over wgpu

- **Date:** 2026-05-25
- **Project:** Mote (programmable AI-native browser). Throwaway evaluation prototype
  to inform the chrome UI framework decision (project-lifetime lock-in).
- **Crate:** `spikes/ui-wgpu/` (standalone — empty `[workspace]` keeps it out of the
  repo workspace). Built with the mise toolchain (rust 1.95, edition 2024).
- **Sibling spike:** the same chrome mock built in egui. Orchestrator compares.
- **Status:** Built, ran, produced `spikes/ui-wgpu/out.png`. See verdict at bottom.

## What it does

Renders a 1280×800 dark-theme (dusk) Mote chrome mock **fully offscreen** (no
window/winit) to an `Rgba8UnormSrgb` texture, reads it back, and saves a PNG. Layout:

- **Tab strip (40px):** 3 tabs, active tab has the 2px amber top-border + `--bg`
  fill, each with a favicon dot, mono title, the active tab shows a close `×`, plus
  a `+` new-tab button.
- **Omnibox row (36px):** sunk-well field with accent (focused) border, `[url]` mode
  tag on `--surface-1`, secure glyph, and host-dim/host/path URL coloring per the
  omnibox spec; two icon buttons (★, ▣) on the right.
- **Left sidebar (280px):** "Browser Integrity" header, one plugin card —
  `password-manager-1password` (mono title), `v1.0.0` + a border-only `verified`
  badge, three permission bullets in monospace
  (`http:fetch:https://*.1password.com/*`, etc.), and a `Revoke` (danger) + `Update`
  (secondary) keycap button row.
- **Viewport (remaining area):** a procedural warm gradient written into an RGBA
  texture and **composited via a blit pipeline** — the stand-in for a CEF
  off-screen-render frame. This proves chrome-surrounds-content compositing.

## Headline metrics

| Metric | Value |
|---|---|
| Hand-written LOC | **1,116** (main.rs 916 *post-rustfmt expansion*, tokens.rs 76, rect.wgsl 85, blit.wgsl 39) |
| Direct deps | **5** (wgpu, glyphon, image, bytemuck, pollster) |
| Transitive crates | **107** unique |
| Release binary | 11 MB (8.2 MB stripped) |
| Process RSS while rendering | **~231 MB** VmRSS |
| Avg frame time (100 offscreen renders) | **~0.52 ms** |
| GPU path | Vulkan, NVIDIA RTX 2080, driver 595.71.05 |

Caveats on the numbers:
- **RSS (~231 MB)** is dominated by the Vulkan/NVIDIA driver + wgpu device, not the
  app. This is *adapter+device* baseline, not per-frame growth; a real shell adds the
  CEF process on top. It is **not** comparable to a "pure logic" RSS — it's the cost of
  holding a GPU device open.
- **0.52 ms/frame** is offscreen render-to-texture *excluding* PNG readback, on a
  discrete GPU, rebuilding the whole scene each frame (immediate mode). 27 rect
  instances + 23 glyphon text runs. This is the optimistic ceiling; a contended
  laptop iGPU under Wayland will be slower, and steady-state idle redraw is wasted
  work in immediate mode (see cons).

## Dependencies

`wgpu 29` (GPU), `glyphon 0.11` (cosmic-text + wgpu text), `image 0.25` (png only),
`bytemuck 1` (POD casts for instance/uniform buffers), `pollster 0.4` (block on async
device requests). Versions were aligned deliberately: glyphon 0.11 → wgpu 29; glyphon
0.10 → wgpu 28. Mismatching these is the first thing that breaks the build.

## Text rendering approach & candid quality assessment

**glyphon (cosmic-text shaping + swash rasterization + a wgpu glyph-atlas renderer).**
Each text run becomes a `cosmic_text::Buffer`, shaped with `Shaping::Advanced`, fed to
a `TextRenderer.prepare()` then drawn last in the render pass (on top of rects + page).
Mono runs request `JetBrainsMono Nerd Font` by family name (system-installed via
fontdb's font discovery); sans runs use `Family::SansSerif`, which falls back to Noto
Sans here because **Geist is not installed** on this machine.

Quality: **very good.** Glyphs are crisp, correctly hinted/antialiased, advances are
right, and colored per-run. This is real text shaping, not a bitmap-font hack — it is
the same stack iced uses, and the rendered PNG looks production-grade at the chrome's
small sizes (10–18px). The honest gap is that I had to **hand-place every run with x/y
pixel math** and **guess advance widths** for inline URL-segment coloring
(`host-dim`/`host`/`path` are three separate runs positioned by `chars * ~0.6em`),
which is brittle — cosmic-text can lay out a single mixed-attribute buffer properly,
but I didn't wire per-span attributes, so multi-color inline text is currently faked.
A real implementation would build one `Buffer` with attribute spans.

## Spec design-token mapping (clean? verbose?)

Tokens live in `src/tokens.rs` as `const` values transcribed from `spec/03_tokens.md`:
a `const fn hex()` parses `#RRGGBB` at compile time into `[f32;4]`, and semantic colors
/ spacing / radius / type-size tokens are plain consts (`BG`, `SURFACE_1`, `ACCENT`,
`SPACE_4`, `RADIUS_2`, `TEXT_MONO`, …). **Mapping was clean and direct** —
one-token-one-const, readable, and the `with_a()` helper covers the spec's
`rgba(..., 0.x)` border tints (badge/danger borders).

But this is the crux of the lock-in concern: **it is a second, hand-maintained copy of
the design system.** The spec declares `colors_and_type.css` the ground truth and Lua
`theme.tokens.*` the runtime surface. Here those values are frozen into Rust consts at
compile time. There is **no cascade, no `var()`, no `[data-theme]` stacking, no
`theme:set_token()` at runtime** — a theme switch would mean re-resolving every const
through a runtime token table I'd have to build by hand. Faithful for one static theme;
structurally hostile to the spec's *programmable* theming.

## KEY: how would Mote's Lua plugin model map onto this?

Mote elements declare `render = function(host) ... end`, referencing tokens by name and
laying out content; themes decide placement; the host exposes the styling vocabulary.

**Immediate vs retained.** Custom-wgpu here is immediate by construction — I rebuild the
entire `Scene` (rect instances) + `TextLayer` (runs) every frame from scratch. That maps
*naturally* onto `render(host)`: each frame, the runtime calls every bound element's
`render` and the element emits draw commands into the host. No retained widget tree to
diff or reconcile. This is the one place custom-wgpu fits the Lua model **better** than a
retained toolkit — `render(host)` *is* an immediate-mode API.

**The `host` API.** `host` would be an mlua `UserData` wrapping my `Scene` + `TextLayer`
+ a layout cursor. Methods map cleanly to what I already wrote:
`host:rect(x,y,w,h, host.tokens.surface_1)`, `host:text("v1.0.0", x, y, host.tokens.text_mono_sm, host.tokens.fg_2)`,
`host:rounded(rect, radius, fill, border)`. Tokens become a table on `host.tokens`
returning the same const values. Layout helpers (`cut_top`, `cut_left`, `inset`) become
host methods returning sub-regions. This is genuinely **ergonomic** — the API surface is
small and the immediate model means no lifecycle for Lua to manage.

**FFI friction — the real cost.** Three frictions surface:
1. **Crossing the mlua boundary per draw call is expensive.** A chrome frame here is
   ~50 host calls (27 rects + 23 texts). Done naively (one `mlua` call → one Rust method
   → push to Vec) that's fine at 50, but real chrome with many elements + per-frame
   immediate redraw means thousands of Lua→Rust calls *per frame* at 60fps. mlua calls
   are cheap but not free; you'd want to batch (Lua builds a command table, Rust drains
   it once) — which dilutes the clean `host:rect(...)` ergonomics.
2. **Text layout can't live in Lua.** The brittle hand-placement I hit (advance-width
   guessing for inline colored URL segments) becomes the plugin author's problem unless
   `host` exposes a real text-measurement/layout API back *into* Lua — i.e., the host
   must offer `host:measure(text, font)` and ideally a span-based text builder, or
   plugins produce misaligned text. That measurement call is a synchronous Rust→shaping
   round-trip mid-`render`, more FFI churn.
3. **No HTML/CSS, so the spec's vocabulary is reimplemented in the host.** Focus rings,
   keycap borders, the block cursor, dot-grid empty slots, `@keyframes` motion — every
   one becomes a host primitive I hand-build (I built keycap borders and dots; I did
   *not* build focus rings, animation, or the cascade). Plugin authors target *my*
   primitive set, not the documented CSS one, so the spec and the runtime API drift.

**Verdict on Lua mapping:** the immediate `render(host)` shape fits custom-wgpu's
immediate model elegantly, and a small mlua `host` userdata is pleasant to use. But the
host has to re-expose the entire spec vocabulary (tokens ✓ easy; layout ✓ easy; text
measurement ⚠ needed; cascade/theming/animation ✗ all hand-built), and per-frame FFI
call volume pushes you toward command-batching that erodes the ergonomic API. It works;
it is not free.

## Honest pros / cons of custom-wgpu for Mote

**Pros**
- **Best-in-class CEF compositing.** The blit pipeline proves it: Mote owns the
  compositor, the page is just a texture bound into the `viewport` slot. Tab switch =
  swap the bound texture. This is exactly the sub-ms-tab-switch / chrome-surrounds-page
  thesis, and custom-wgpu does it most directly of any option.
- **Immediate `render(host)` is a natural fit** for Mote's Lua element model (above).
- **Lean and fast** for what it draws: 5 direct deps, 8 MB stripped, sub-ms frames,
  full control over every pixel. Text via glyphon is genuinely high quality.
- **No framework opinions to fight** — the spec's exact pixel treatments (keycap depth,
  block cursor, 2px accent tab border) are trivially expressible because you own the
  shader.

**Cons**
- **You rebuild a UI platform.** This 1,116-line mock has *no* hit-testing, input,
  focus, accessibility, animation, cascade, runtime theming, or text-layout API — all of
  which the spec mandates and all of which are on you. The spec is written in HTML/CSS/
  ARIA; custom-wgpu means reimplementing that vocabulary or diverging from it.
- **The design system becomes two representations.** `colors_and_type.css` is declared
  ground truth; here tokens are frozen Rust consts. Runtime `theme:set_token()` /
  `[data-theme]` stacking would need a hand-built token-resolution layer. This is the
  exact "two representations drift" tech-debt the disciplines warn about.
- **No accessibility for free.** Would need AccessKit wired by hand (Linux a11y support
  is the constraint that also sinks egui).
- **Immediate-mode idle cost.** Rebuilding + redrawing the whole chrome every frame is
  wasted CPU/GPU when the UI is static — wrong default for a long-lived laptop browser
  shell unless you add dirty-tracking (which fights the immediate simplicity).
- **RSS baseline (~231 MB)** is the GPU device/driver cost before CEF; the combined
  shell would need budget scrutiny against the 50–100 MB target.

## Files

- `spikes/ui-wgpu/src/main.rs` — renderer, layout, scene build, offscreen render + readback + bench
- `spikes/ui-wgpu/src/tokens.rs` — design tokens (dusk) as Rust consts
- `spikes/ui-wgpu/src/rect.wgsl` — rounded-rect SDF pipeline (fill + border + keycap bottom-border)
- `spikes/ui-wgpu/src/blit.wgsl` — external-RGBA-texture composite (CEF OSR stand-in)
- `spikes/ui-wgpu/out.png` — the rendered evidence frame

## Relationship to prior research

`docs/research/cef-and-ui-framework.md` recommends rendering chrome as an HTML/CSS
document inside a CEF surface with a thin wgpu *compositor*, explicitly **not** a custom
wgpu *toolkit*, because the spec is authoritatively HTML/CSS. This spike measures the
"custom wgpu toolkit" option that recommendation argues against. Findings here
**corroborate** it: the compositing half (blit page texture) is excellent and is the
part that recommendation keeps; the toolkit half (reimplementing tokens/cascade/text-
layout/a11y/theming) is exactly the cost that recommendation avoids by keeping CEF as
the HTML/CSS renderer. Custom-wgpu shines as the compositor under the chrome, not as the
chrome's widget layer.
