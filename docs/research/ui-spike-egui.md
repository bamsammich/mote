# UI Framework Spike — egui

**Date:** 2026-05-25
**Branch:** main
**Spike location:** `spikes/ui-egui/`
**Output:** `spikes/ui-egui/out.png` (1280×800 PNG, dark theme)

---

## Purpose

Evaluate egui as the candidate browser chrome UI framework for Mote. A sibling spike implements the same mock in a custom wgpu layer; the orchestrator compares both to decide. The mock renders a 1280×800 dusk-theme browser chrome with tab strip, omnibox, sidebar integrity panel (plugin card, permission lines, action buttons), and a composited procedural page texture proving chrome-surrounds-content compositing.

---

## Headline Metrics

| Metric | Value |
|---|---|
| Handwritten LOC (`src/main.rs`, non-blank/non-comment) | 634 |
| Total source lines | 775 |
| Direct dependencies | egui 0.31.1, egui-wgpu 0.31.1, wgpu 24.0.5, image 0.25.10, pollster 0.4 |
| Transitive dependency count | 223 crates |
| Release binary size (stripped, LTO not applied in spike) | 14 MB |
| Avg offscreen render time (100 frames, warm) | 0.14 ms |
| VmRSS during rendering | ~300 MB |
| VmRSS delta across 100 frames | 24 kB (near-zero allocation per frame) |

The ~300 MB VmRSS is dominated by wgpu's Vulkan/Metal driver context and egui's compiled shaders — not egui data structures. The delta across 100 frames is negligible, confirming no per-frame heap growth.

---

## Dependency List

```toml
egui = "0.31"
egui-wgpu = "0.31"
wgpu = "24"
image = { version = "0.25", features = ["png"] }
pollster = "0.4"
```

The image crate pulled in ~10 additional crates (codec support). The egui/wgpu stack brought the transitive count to 223.

---

## Text Rendering Quality Assessment

**Rating: Adequate for a prototype; below bar for a shipping product.**

- egui ships with an embedded Hack-derived bitmap atlas (no system fonts without explicit loading).
- Monospace rendering is functional. The tab titles, omnibox URL, permission lines, and badge labels render legibly at 10–12px.
- Sub-pixel positioning is absent — egui rasterizes at whole-pixel boundaries, giving slightly coarser character spacing than a native system text stack or a Skia/freetype renderer.
- No font-variant support (no separate weight/width axes) — bold requires a separate font file.
- Anti-aliasing is software-only; jagged diagonals are visible at small sizes on non-HiDPI targets.
- Loading JetBrains Mono or Geist would require embedding the .ttf and registering it via `FontData` — straightforward, but the embedded font does not match Mote's typography spec (`--font-mono: JetBrains Mono`).

For a browser chrome rendering user-facing text in monospace at 10–14px, the quality is borderline. An HiDPI display (2×) would hide most of the roughness; a 1× display would feel noticeably soft compared to native OS text.

---

## Design Token Mapping to egui::Style/Visuals

egui's styling system (`egui::Style` / `egui::Visuals` / per-widget `WidgetVisuals`) covers a subset of Mote's token vocabulary.

| Mote token | egui equivalent | Fidelity |
|---|---|---|
| `--bg`, `--surface-1` | `Visuals::panel_fill`, `Visuals::window_fill` | Direct |
| `--surface-2`, `--surface-sunk` | `Visuals::faint_bg_color`, `Visuals::extreme_bg_color` | Direct |
| `--border`, `--border-strong` | `WidgetVisuals::bg_stroke` | Per-widget only; no global border token |
| `--fg`, `--fg-1`, `--fg-2` | `WidgetVisuals::fg_stroke`, `Visuals::override_text_color` | Coarse; only one override-text-color applies globally |
| `--accent` | `Visuals::selection.bg_fill`, `Visuals::hyperlink_color` | No single "accent" concept |
| `--radius-1`, `--radius-2` | `WidgetVisuals::corner_radius` (per widget) | Per-widget; no cascade |
| `--text-mono-sm`, `--text-body` | Not tokened — set per `painter.text()` call | Manual only |
| `--space-*` | `Spacing::item_spacing`, `Spacing::button_padding` | Partial |
| Shadow tokens | `Shadow` struct (ambient + spread) | Available but unused in spec (inline components) |
| `--dur-base`, `--ease-out` | No animation system | **Missing entirely** |

**Key friction points:**

1. **Global style is coarse.** egui's `Visuals` covers window/panel backgrounds and a handful of widget states (inactive/hovered/active/open). The fine-grained per-component token cascade Mote needs (different fg colors for primary/secondary/muted text, different border colors for specific components) is not expressible through `Style` — it must be applied manually in each `painter.text()` / `painter.rect_stroke()` call. This is workable but means the theme contract (plugin references `theme.tokens.fg_2`, framework resolves to the active value) cannot be implemented by setting `egui::Style` once; it requires passing token values through to every draw call.

2. **No animation primitives.** egui is stateless immediate-mode. The Mote spec's motion tokens (`--dur-base: 120ms`, `--ease-out`) and the keycap press animation (`translateY(1px)`, `border-bottom-width: 1px`) have no egui equivalent. Implementing them requires tracking animation state externally and recomputing on each frame — possible but awkward for something that's supposed to be a theme-level detail.

3. **Radius is per-widget, not per-component category.** Mote's `--radius-1` (buttons/fields), `--radius-2` (cards), `--radius-3` (palette) are distinct semantic categories. egui's `corner_radius` in `WidgetVisuals` applies to all widgets in a given interaction state — no way to say "buttons use radius 2, cards use radius 4" without overriding per draw call.

4. **Token cascade does not exist.** The spec's CSS-var cascade (set `--border` once, every component picks it up) is not how egui works. The framework reads Style once at frame start; per-component overrides require either painting entirely with raw `Painter` (bypassing egui widgets) or cloning and mutating `Style` before each component. The spike chose the raw-painter approach for this reason — it's more flexible but means no egui widgets at all.

---

## Lua Plugin Model Ergonomics (the load-bearing assessment)

Mote's plugin render model is:

```lua
ui.register_element({
  id = "my-panel",
  kind = "sidebar-panel",
  render = function(host)
    -- host exposes theme.tokens, layout helpers, draw primitives
  end,
})
```

The `render(host)` function is called each time the element needs to draw. It receives the host object (which provides token values and drawing context) and produces a frame worth of UI.

**Mapping to egui via mlua:**

egui is immediate-mode. Every frame, Lua code would be called to run the `render` function, which calls into egui's layout/painter API through an mlua-exposed Rust wrapper. This maps naturally in theory — egui was designed for exactly this "call per frame" pattern.

**The friction:**

1. **egui's type system and Lua's dynamic types are at odds.** egui's `Painter` methods take strongly-typed Rust types (`FontId`, `Rect`, `Color32`, `Stroke`). Exposing these through mlua requires either:
   - Wrapping each type as a Lua userdata (correct but verbose — `painter.text(pos, align, str, font_id, color)` requires `font_id` to be a constructed FontId userdata, `color` to be a Color32 userdata, etc.), or
   - Accepting plain tables/strings and converting at the FFI boundary (ergonomic in Lua, but adds marshal overhead per call and loses type safety).
   
   Either way, every `painter.text()` / `painter.rect_filled()` / etc. is an mlua call crossing the Rust/Lua boundary. At 100+ calls per sidebar panel render, this adds up — the FFI budget per frame would need to be benchmarked but 10k+ calls/frame for a complex UI is plausible.

2. **egui's global Style conflicts with per-element theming.** The spec calls for per-element theming: `theme:style("tab.active", { border_top = {2, theme.tokens.accent} })`. In egui, this cannot be expressed through Style alone. It requires each Lua `render` function to receive the active token values and apply them manually to each draw call. This is not wrong (it's how the spike is implemented), but it means the "Lua plugin author reads from `theme.tokens.fg_2` and the framework does the rest" ergonomic ideal breaks down — the author must thread token values through every primitive call.

3. **No retained element identity.** egui is stateless immediate mode — there is no concept of "this widget is `tab.active`". Theming hooks that work on CSS-style selectors (the `theme:style` contract) have no implementation path in egui. The theme contract would need to be redesigned as token-passing rather than selector-overriding.

4. **Positional layout vs. mlua.** egui's layout (Areas, Panels, horizontal/vertical groups) is somewhat ergonomic from Rust. Exposing it through Lua adds verbosity: every `ui.horizontal(function() ... end)` requires a Lua closure that mlua wraps. The closure call pattern works in mlua but each crossing adds latency. For a real-time browser chrome updating at 60fps with multiple plugin-contributed panels, the accumulated FFI cost is a real concern.

**The verdict on Lua ergonomics:** egui's API surface is not hostile to Lua binding, but the combination of stateless immediate mode + global Style + strongly typed draw primitives creates an impedance mismatch with Mote's token-cascade + per-element theming + Lua-authored render functions model. The Lua author would end up writing more boilerplate than expected (thread tokens through every call, convert types at the boundary, manage their own animation state), and the theme contract's selector-based styling would need significant redesign or abandonment.

---

## Pros of egui for Mote

- **Pure Rust, no C deps (aside from wgpu/native GPU drivers).** Memory safety throughout; no Skia FFI, no cairo, no pango.
- **Offscreen rendering works cleanly.** The wgpu backend renders to any `wgpu::TextureView`. The spike proved this in ~100 lines of setup code.
- **Near-zero per-frame allocation.** The 24 kB RSS delta across 100 frames is excellent. egui reuses internal buffers aggressively.
- **Fast tessellation.** 0.14 ms avg frame time for this chrome mock is fast. Even a 10× increase for a full production UI would be 1.4 ms, well within a 60 fps budget.
- **Actively maintained.** egui 0.31 is current; the API surface is stable and well-documented.
- **No hidden layout engine.** Immediate mode means the rendering is predictable; no layout thrashing, no style recalculation.
- **Easy texture compositing.** `register_native_texture` + `painter.image()` proved that an external RGBA texture (CEF OSR frame) composites cleanly into egui's render pass.

## Cons of egui for Mote

- **Visual fidelity ceiling is low.** No sub-pixel text, no system font stack, no path-level vector rendering. The chrome will look like an egui app, not like a native OS application. This is acceptable for developer tooling but Mote's design system expects a more refined output.
- **Theming model is mismatched.** The CSS-var cascade / selector-based theme contract cannot be implemented through egui's Style API. The theme would need to become "pass tokens to every draw call" rather than "set theme once, components pick up tokens."
- **No accessibility layer.** egui has no screen reader, no ARIA semantics, no keyboard focus that integrates with the OS accessibility tree. The Mote design spec has explicit accessibility requirements (ARIA roles, focus rings). egui can draw focus rings visually but provides no semantic accessibility plumbing.
- **Immediate mode is awkward for conditional animations.** The keycap press animation (1px border collapse, translateY), tab close transitions, sidebar panel swap — all require external state tracking and are not expressible declaratively.
- **Large transitive footprint.** 223 crates is substantial. The spike's image crate alone pulled 10+ crates; egui's wgpu backend is another 100+. Build times will be slow.
- **The Lua plugin API surface for egui is complex.** Exposing egui's painter API to Lua requires wrapping many types and crossing the FFI boundary frequently. The ergonomics for plugin authors would be poor without a significant abstraction layer.

---

## Summary Verdict

egui is **a poor fit for Mote** as the primary chrome UI framework, despite its technical strengths in performance and pure-Rust lineage.

The immediate blockers are the accessibility gap (Mote has explicit accessibility requirements; egui provides none), the theming model mismatch (egui's global Style cannot express Mote's per-component token cascade or selector-based theme overrides), and the Lua FFI ergonomics (the immediate-mode + strongly-typed API creates significant friction for `render(host)` plugin functions authored in Lua).

The custom wgpu layer spike should be evaluated to see whether it allows a better-matched API surface for Mote's theming contract and plugin model, even at the cost of more implementation work.

If egui were chosen anyway, it would require: building a full Lua abstraction layer on top (hiding egui's types behind simpler table-passing APIs), redesigning the theme contract to be token-passing rather than selector-based, and accepting that visual fidelity, animations, and accessibility will all be hand-implemented rather than framework-provided.
