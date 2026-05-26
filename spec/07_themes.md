# 07 · Themes (the Lua contract)

Themes are the central programmable surface in Mote. **A theme is a plugin that fulfills the `theme:provider` capability** (exclusive — one theme active at a time). It declares element-to-slot placement (`layout`) and token-level styling (`styling`) as a **declarative module table**, `M.theme`. (See `01_architecture.md` and the project `DESIGN.md` for the full model.)

## File location

```
~/.config/mote/plugins/<name>/init.lua
```

A theme is loaded like any plugin (e.g. `mote.plugins({ "dusk" })`); the runtime activates the one fulfilling `theme:provider`.

## Minimum theme

A styling-only theme inherits placement from `default-layout` and provides only `styling`:

```lua
local M = {}

M.manifest = {
  schema = "v1",
  name = "my-theme",
  version = "1.0.0",
  capabilities = { "theme:provider" },
}

M.theme = {
  inherits = "default-layout",
  styling = {
    colors = { bg = "#191724", fg = "#e0def4", accent = "#E0A458" },
  },
}

return M
```

The `default-layout` theme ships with the browser, so most themes only recolor. A theme need not place any slot itself; placement falls back to the inherited layout.

## Placement (the `layout` block)

A more ambitious theme places elements into slots itself. Slots and element kinds are the fixed runtime set (kebab-case) from `01_architecture.md`:

```lua
M.theme = {
  layout = {
    ["top-bar"]      = { "tabstrip", "urlbar", "action-button:*" },
    ["left-sidebar"] = {
      elements     = { "sidebar-panel:bookmarks", "widget:*", "sidebar-panel:*" },
      resizable    = true,
      default_size = 280, min_size = 200, max_size = 500,
    },
    ["right-sidebar"] = {},   -- explicitly empty; renders the dot-grid motif
  },
  styling = { colors = { ... } },
}
```

Element references are `<kind>` or `<kind>:<id>`. The `:*` wildcard catches any element of that kind not placed elsewhere — this is how a theme handles plugins it wasn't written to know about. A slot listed as `{}` is explicitly empty. The runtime decides which slots are required for a functional chrome; the urlbar and tabstrip elements always exist.

## Resize and persistence

Slots opt into user resizing via the long form shown above (`resizable`, `default_size`, `min_size`, `max_size`). The user can drag the slot edge within bounds; resized state persists per workspace. A theme switch resets to the new theme's defaults.

## Styling (the `styling` block)

The most common customization is recoloring. **Always work through the token vocabulary.**

```lua
M.theme = {
  styling = {
    colors  = { bg = "#1A1A1A", fg = "#FFFFFF", accent = "#FF8800" },
    fonts   = { ui = "Geist, sans-serif", mono = "JetBrains Mono, monospace" },
    spacing = { tab_height = 28, sidebar_padding = 8 },
  },
}
```

The runtime resolves the active theme's `styling` into the CSS variables under `[data-theme="<name>"]` and onto `theme.tokens` for Lua.

## Mode-specific themes (light/dark)

Mote does not branch a single theme on system color scheme. Ship two themes — a dark one and a light one — and let the user pick (the `dusk`/`vellum` pair is the canonical example). A user override can swap the active theme on a system color-scheme change in their own config.

## Standard themes

Mote ships these by default. Implementing the runtime, you must include them.

| Theme | Mode | Vibe |
|---|---|---|
| `dusk` | dark | warm ink (default) |
| `vellum` | light | warm paper |
| `embers` | dark | redder accent, hotter |
| `gloam` | dark | bluer cool variant |

The exact token values for each live in the bundled theme plugins shipped with the runtime — they're not duplicated here. The `dusk` and `vellum` values are the defaults tabulated in `spec/03_tokens.md`.

## What themes CAN'T do

Mote's design system establishes constraints themes must respect. The runtime enforces these:

- Themes cannot disable the focus ring (accessibility).
- Themes cannot set radius higher than `--radius-3` (6px). The constraint is enforced at token-set time.
- Themes cannot reintroduce filled icons — the icon component renders strokes regardless.
- Themes cannot add bluish-purple gradients or backdrop blur — there's no API for either.

These limits exist so a community theme can't easily produce something that doesn't feel like Mote. The token vocabulary is intentionally **constrained**.

## User overrides (in user config)

The user's config runs after the theme and overrides it surgically via `mote.theme_overrides`, deep-merged onto the active theme. User overrides always win on conflict, and they survive a theme switch (re-applied on top of the new theme's defaults).

```lua
mote.plugins({ "dusk" })

mote.theme_overrides({
  styling = {
    colors = { accent = "#FF8800" },
  },
  layout = {
    ["right-sidebar"] = { "sidebar-panel:bookmarks" },
  },
})
```

This is the same deep-merge pattern Neovim users expect from `vim.opt`-style overrides.

## Next

[`components/`](./components/) — implementation contracts for each component.
