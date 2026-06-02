# ADR-0013 — Themable Icon Contract: `theme.icons.<action>` Mapping + `theme:set_icon` API

- **Status:** Accepted (approved by the maintainer 2026-06-02)
- **Date:** 2026-06-02

---

## Context and Problem Statement

ADR-0003 makes chrome themes a CSS-tokens-and-stylesheet-overrides contract:
themes adjust `--accent`, `--surface-1`, font choices, and per-selector CSS
properties. It says nothing about icons. Today the chrome page hardcodes
lucide icon names (`x`, `bookmark`, `layers`, `clock`) inline in HTML — a
theme that wanted to swap the close glyph for `xmark`, or use an inline SVG
mark, would have to fork the chrome HTML. The polish phase introduces
several new chrome icons (close button, navigation keycaps, status-line
elements, rail icons) and the user has affirmed they want default-theme
choices kept while remaining themable for users who want something else.
The icon-override mechanism needs to be a recorded surface so plugin and
theme authors can rely on it across versions.

## Decision Drivers

- Themes should control every visual choice end-users see, including
  icons; chrome HTML hardcoding lucide names contradicts ADR-0003's
  spirit.
- An override mechanism that ships arbitrary SVG must be safe against XSS
  / SVG-script injection — the chrome runs at the privileged
  `mote://chrome` origin (ADR-0007), so any inline content that reaches
  the DOM has full chrome authority.
- The override surface should be small and declarative — themes are Lua
  files, not React components.
- Default-theme stability matters: changing the default icon for an
  established action (e.g. bookmark) would surprise existing users.

## Considered Options

- **No theme icons API; themes that need different glyphs fork the chrome
  HTML.** Rejected: defeats theming as a user-facing extension surface
  and makes theme authoring fragile.
- **`theme:set_icon(action, source)` with `source ∈ {"lucide:name",
  "inline:<svg>"}` and strict SVG sanitization.** Rejected: the
  inline-SVG sanitizer is a recurring maintenance cost (deny-list
  updates each time Blink adds an SVG feature) and a sanitizer bug is
  chrome-origin XSS. The eventual icon-pack model (below) covers the
  legitimate "I want non-lucide glyphs" cases without arbitrary
  caller-supplied DOM.
- **`theme:set_icon(action, "<pack>:<name>")` with a registered-pack
  format, lucide as the only v0.1-registered pack** (this ADR). Source
  format `"<pack>:<name>"` is the stable shape; v0.1 ships lucide
  bundled; future ADRs add icon-pack registration so themes/plugins can
  bring in other icon families (Phosphor, Heroicons, custom sets)
  without ever needing to ship arbitrary inline SVG.

## Decision Outcome

Chosen: **chrome icons are theme tokens accessed via `theme.icons.<action>`
and overridable from Lua via `theme:set_icon(action, "<pack>:<name>")`.
v0.1 registers `lucide` as the only icon pack; the dot-namespaced
`<pack>:<name>` format is the stable shape from day one so adding more
packs in v2 is purely additive.**

### API surface

```lua
-- Lua, in a theme's setup() — v0.1 supports the lucide pack only
theme:set_icon("chrome.close", "lucide:x")
theme:set_icon("rail.tabs",    "lucide:layers")
theme:set_icon("rail.bookmarks", "lucide:bookmark")
```

In v0.1, `set_icon` accepts only `lucide:<name>`. Unknown packs and
unknown lucide names are rejected at registration time with a clear
error — fail closed, never silently substitute.

### Action names

A flat `category.action` namespace, declared by the chrome at build time.
The polish-phase additions:

- `chrome.close` — window close keycap (R4 close button, default `x`)
- `chrome.bookmark` — bookmark-current-page (default `bookmark`)
- `chrome.new_tab` — new tab keycap (default `plus`)
- `chrome.back` / `chrome.forward` / `chrome.reload` — navigation keycaps
- `tab.close` — per-tab close X (default `x`)
- `tab.favicon_placeholder` — pre-favicon dot-grid (default inline SVG)
- `rail.tabs` / `rail.bookmarks` / `rail.history` — sidebar rail
- `rail.settings` — P6's settings cog (forward-declared)
- `rail.plugin_unbound` — placeholder for unbound rail plugin slots (P1)
- `statusline.security_https` / `statusline.security_http` — security
  indicators (P4)
- `statusline.zoom` / `statusline.tabs_count` — status-line built-ins (P5)

The chrome side reads `theme.icons.chrome_close` (CSS-var convention
mirrors the dot-namespaced Lua name) at render time and falls back to the
default if the theme has not overridden.

### Source format

One source format, one registered pack in v0.1:

- **`"<pack>:<name>"`** — `<pack>` names a registered icon pack; `<name>`
  names an icon within that pack.
- **v0.1 registers `lucide` as the only pack.** Names come from the
  bundled lucide sprite set (`assets/lucide-usage.md` is the authoritative
  list of names Mote ships). Rendering injects an inline `<svg>` from
  the embedded sprite — no fetch, no DOM-string interpolation, no
  caller-supplied SVG.
- Unknown pack names and unknown lucide names are rejected at `set_icon`
  time with a clear error — **fail closed**, never silently substitute
  a default. Errors include the action name, the bad source, and the
  list of registered packs.
- Other source kinds (`inline:<svg>`, file paths, `data:` URLs, network
  URLs) are NOT supported in v0.1 and are not reserved syntax. A future
  ADR introducing arbitrary inline SVG would need to add the sanitizer
  surface; this ADR explicitly punts that work.

### Future icon-pack extensibility

Additional icon packs (Phosphor, Heroicons, Material Symbols, custom
plugin/theme-shipped sets) are an intended future extension. The
`"<pack>:<name>"` format is the stable shape from day one; v2 work
adds:

- The pack-registration mechanism (manifest field on themes / plugins
  to declare a pack, asset routing for the sprite set, integrity
  verification for shipped icon files)
- Validation of pack-shipped SVG assets at install time (the install
  pipeline does the sanitization once, not the chrome at render time)
- Conflict resolution when two packs ship the same name

None of that is decided here. v0.1's `lucide-only` constraint keeps the
runtime surface tiny while making sure plugin/theme authors writing
`theme:set_icon("chrome.close", "lucide:x")` today don't need to change
their code when new packs land.

### Default-theme stability

The default theme MUST NOT change an established `theme.icons.<action>`
mapping without a recorded decision (an ADR amendment or follow-up).
`assets/lucide-usage.md` is the authoritative list of default mappings;
changes to that file are reviewed under the same gate as ADRs.

## Consequences

- Good: theme authors gain a real customisation surface for chrome
  iconography; the user's "I like the default bookmark mark but want
  themes to be able to change it" requirement is met.
- Good: zero security surface in v0.1 — bundled lucide sprite set is
  the only content source, no string interpolation, no caller-supplied
  DOM, no sanitizer to author or maintain.
- Good: the `"<pack>:<name>"` format is stable from day one; v2 work
  adding more icon packs is purely additive — no manifests written
  against v0.1 need to change.
- Bad: theme authors limited to ~1500 lucide names in v0.1; a designer
  wanting a custom glyph must wait for the future icon-pack ADR.
  Mitigated by lucide's breadth and the documented future direction.
- Bounded scope: `set_icon` is the only theme API added by this ADR,
  and only the lucide pack is registered. Themes still use existing
  CSS-token + selector APIs for everything else (colors, fonts,
  spacing, layout); no broadening of the theme surface beyond icons.

## Relationship to existing ADRs

- **Extends ADR-0003** (Chrome UI as HTML/CSS in CEF with wgpu
  compositor). ADR-0003 establishes the CSS-token contract; this ADR
  adds a parallel `theme.icons.<action>` contract for iconography that
  can't be expressed as CSS variables. No supersession; orthogonal
  extension.
- **Adjacent to ADR-0007** (Plugin Management UI — Privileged Async
  Approval). ADR-0007 places trust-critical UI on the privileged
  origin; in v0.1 this ADR adds no caller-supplied content to that
  origin (lucide sprites are Mote-bundled), so there is no new trust
  surface to audit. A future inline-SVG or icon-pack ADR will need to
  re-evaluate against ADR-0007's trust model.
- **Forward-references a future icon-pack ADR** scoping the
  registration mechanism, pack asset format, install-time
  sanitization, and inter-pack name-conflict policy.
