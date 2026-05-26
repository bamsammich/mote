# 01 · Architecture

> This file describes the Mote runtime's slot/element/theme architecture as the *visual* design system depends on it. The canonical architecture, plugin model, and security model live in the project's `DESIGN.md`; where this file once described a different plugin API it has been aligned to DESIGN. Treat the design-token, `[data-slot]`, and CSS-variable mechanics below as authoritative for the chrome; treat the Lua surface as a pointer to DESIGN's plugin model, not a competing definition.

## Layers

```
┌─────────────────────────────────────────────┐
│  user init.lua             (top — overrides)│
├─────────────────────────────────────────────┤
│  plugins                   (contribute elements, commands)
├─────────────────────────────────────────────┤
│  theme                     (places elements into slots, styles)
├─────────────────────────────────────────────┤
│  mote runtime              (owns slots + element kinds, lua bridge, web chrome)
└─────────────────────────────────────────────┘
```

Load order is **bottom up, then top wins on conflict**:

1. Runtime boots. Default theme `dusk` is loaded.
2. User config runs. It lists plugins, switches themes, binds keys, and applies overrides.
3. Plugins register their elements (by kind). Plugins may offer placement *hints*; the active theme decides actual placement.
4. The user's overrides (`mote.theme_overrides`, key binds) win unconditionally.

## Lua API (target surface)

Plugins are **declarative module tables**, not imperative registrations. A plugin returns an `M` table with `M.manifest`, and optionally `M.theme`, `M.hooks`, `M.events`, `M.api`, and a `M.setup()` function. The runtime reads these tables to validate a plugin *without running it* (static contract conformance); `setup()` runs only after validation and permission approval. There is no `mote.plugin.register(...)` / `events.on(...)` imperative path — the declarative tables are the only registration surface. (Full plugin/security model: `DESIGN.md`.)

This file lists only the surface the *visual* design system leans on. The authoritative, complete API reference is in `DESIGN.md`.

### Theme module table

A theme is a plugin fulfilling `theme:provider`. Styling and placement live in `M.theme`:

```lua
M.theme = {
  inherits = "default-layout",       -- optional; inherit slot placement
  layout = {
    ["top-bar"]      = { "urlbar", "tabstrip" },
    ["left-sidebar"] = {
      elements    = { "sidebar-panel:bookmarks", "sidebar-panel:*" },
      resizable   = true,
      default_size = 280, min_size = 200, max_size = 500,
    },
    ["right-sidebar"] = {},           -- explicitly empty (renders the empty-slot motif)
  },
  styling = {
    colors  = { bg = "...", fg = "...", accent = "..." },
    fonts   = { ui = "...", content = "...", mono = "..." },
    spacing = { ... },
  },
}
```

Placement targets slot names (kebab-case) and element references of the form `<kind>` or `<kind>:<id>`; the `:*` wildcard catches any element of that kind not placed elsewhere. The `default-layout` theme ships with Mote.

### Element registration

A plugin registers a UI element in `M.setup()`; it declares the element *kind*, not its placement (the active theme decides where it goes):

```lua
ui.register_element({
  id = "bookmarks",
  kind = "sidebar-panel",            -- one of the 8 fixed element kinds
  title = "Bookmarks",
  icon = "bookmark",
  render = function(host)
    -- host exposes the token vocabulary (theme tokens) and layout helpers
  end,
})
```

### User config helpers

Users program against `mote.*` helpers in `~/.config/mote/`:

```lua
mote.plugins({ "dusk", "git-status" })        -- list of plugins to load
mote.theme_overrides({ styling = { ... }, layout = { ... } })  -- deep-merged onto active theme
mote.keys.bind("Mod+Shift+P", function() mote.palette.open() end)
mote.dispatch.order("net:intercept_request", { "privacy-headers", "adblock" })
```

User overrides win on conflict (deep-merge); a theme switch preserves them.

### Palette / omnibox / sidebar / tabs

These are runtime surfaces the chrome exposes. The names below map onto the slot/element taxonomy:

```lua
mote.palette.open()                 -- the command-palette widget overlay
mote.palette.add({ name, cmd, cat, keys })

mote.omnibox.open(mode?)            -- mode = "url" | "cmd" | "find"  (the urlbar element)
mote.omnibox.set(text)
mote.omnibox.mode()

mote.tabs.current()                 -- tabstrip element / session
mote.tabs.list()
mote.tabs.create(url?)
mote.tabs.close(id?)
mote.tabs.focus(id)
mote.tabs.move(id, { workspace = "..." })   -- includes pin-to-workspace
```

Tab lifecycle uses DESIGN's vocabulary: a tab is *active*, *hidden in workspace* (renderer destroyed — DESIGN's "discard"), or *closed*. There is no `hibernate` verb in the runtime; where this spec's component files say "hibernated," read "hidden in workspace."

> **No `mote.ai`.** Mote ships no AI host API. AI is plugin-delivered: a plugin calls an LLM via `http:fetch` and reads credentials via `secret:read` (DESIGN principle #8, "LLM access lives in plugins"). The `[ask]` omnibox mode and an `assist` sidebar panel are reserved element names a future AI plugin may fill.

### Events

Plugins declare handlers in the `M.events` (broadcast/inter-plugin) and `M.hooks` (filter-chain) module tables — never via an imperative `mote.on(...)` inside `setup()`. Event names use DESIGN's `domain:action` vocabulary:

```lua
M.hooks = {
  ["net:intercept_request"] = { priority = 70, handler = function(req) ... end },
  ["page:on_load"]          = function(p) ... end,
}

M.events = {
  ["tabs:on_change"]        = function(t) ... end,
  ["workspaces:on_change"]  = function(w) ... end,
}
```

Canonical event/hook names include `net:intercept_request`, `page:on_load`, `tabs:on_change`, `workspaces:on_change`. (Spec-era names like `tab.opened` / `page.loaded` are superseded by these.)

## Slots and element kinds (fixed in v0.1)

The runtime owns a fixed set of layout **slots** and a fixed set of element **kinds**. Themes place elements into slots; plugins provide elements of a kind without choosing placement.

**Slots:**

| Slot | Position | Typical content |
|---|---|---|
| `top-bar` | top | `urlbar`, `tabstrip`, `action-button`s |
| `left-sidebar` | left | `sidebar-panel`s, `widget`s |
| `right-sidebar` | right | `sidebar-panel`s, `widget`s |
| `bottom-bar` | bottom | `status-indicator`s (the status line) |
| `urlbar-inline` | within the urlbar | `urlbar-extension`s |
| `tab-row` | within the tab strip | `tabstrip`, per-tab pieces |

**Element kinds:** `urlbar` (one, always present), `tabstrip` (one, always present), `bookmarks-bar`, `sidebar-panel`, `action-button`, `status-indicator`, `urlbar-extension`, `widget` (catch-all for non-standard plugin UI). The page content itself (the CEF web view) is not a plugin element — it is the runtime's render surface, filling the area not claimed by slots.

The command palette is a `widget`-kind overlay, shown floating/centered and hidden until invoked.

## CSS / HTML conventions

The chrome renders these slots as HTML with `data-slot="<name>"` attributes (kebab-case, matching the slot names above). Themes target slots via CSS variables on `[data-slot="..."]`.

```html
<div class="mote-root" data-theme="dusk">
  <div data-slot="top-bar">...</div>
  <div class="mote-body">
    <aside data-slot="left-sidebar">...</aside>
    <main class="mote-page"><!-- CEF web view --></main>
    <aside data-slot="right-sidebar">...</aside>
  </div>
  <div data-slot="bottom-bar">...</div>
</div>
```

Empty slots render the dot-grid motif — see `spec/components/empty-slot.md`.

## Next

Continue to [`02_design_principles.md`](./02_design_principles.md).
