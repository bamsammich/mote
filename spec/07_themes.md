# 07 · Themes (the Lua contract)

Themes are the central programmable surface in Mote. A theme is a Lua file that declares slots, binds default elements, and applies token-level styling.

## File location

```
~/.config/mote/themes/<name>.lua
```

A theme is loaded by name:

```lua
mote.theme.load("dusk")
```

## Minimum theme

```lua
local mote = require("mote")
local theme = mote.theme.new("my_theme")

theme:slot("omnibox", { height = 36 })
theme:slot("tab_bar", { height = 40 })
theme:slot("status_line", { height = 22, position = "bottom" })
theme:slot("sidebar", { width = 280, position = "left", resizable = true })
theme:slot("viewport", {})

return theme
```

A theme that declares no slots is invalid; the runtime rejects it. A theme must always declare at least `omnibox`, `viewport`, and `status_line`.

## Declaring slots

```lua
theme:slot(name, attrs)
```

Attrs accepted:

| Attr | Type | Default | Notes |
|---|---|---|---|
| `width` | number / "auto" / "fill" | "auto" | px or one of the gutter snap tokens |
| `height` | number / "auto" / "fill" | "auto" | |
| `position` | "top" / "bottom" / "left" / "right" / "float" / "inline" | depends on slot name | |
| `bg` | color | `theme.tokens.bg` | use tokens |
| `fg` | color | `theme.tokens.fg` | |
| `border` | color | `theme.tokens.border` | |
| `accent` | color | `theme.tokens.accent` | |
| `radius` | radius token | `theme.tokens.radius_1` | |
| `resizable` | bool | false | |
| `min`, `max` | number | nil | for resizable slots |

## Binding elements

A slot is empty until an element is bound to it. Plugins contribute elements; themes pick which goes where.

```lua
theme:bind("omnibox", mote.elements.omnibox_default)
theme:bind("sidebar", mote.elements.sidebar_tabs)
theme:bind("status_line", { mote.elements.vim_mode, mote.elements.connection, mote.elements.assist_status })
```

Bind accepts a single element or an ordered list. Multiple elements in one slot render in declaration order.

Themes that want to leave a slot empty intentionally:

```lua
theme:unbind("tab_bar")  -- explicitly empty; renders the dot-grid motif
```

## Token overrides

The most common theme customization is recoloring. **Always work through tokens.**

```lua
-- Recolor the brand accent for this theme
theme:set_token("accent", "#FF8800")

-- Or, swap many at once
theme:set_tokens({
  accent     = "#FF8800",
  surface_1  = "#1A1A1A",
  fg         = "#FFFFFF",
})
```

Setting a token rewrites the corresponding CSS variable under `[data-theme="<name>"]`.

## Element style overrides

For element-specific styling that doesn't fit a token, use `theme:style`:

```lua
theme:style("tab.active", {
  border_top = { 2, theme.tokens.accent },
})

theme:style("button.primary", {
  background = theme.tokens.moss,
  border     = { 1, theme.tokens.moss },
})

theme:style("kbd", {
  background = theme.tokens.surface_sunk,
})
```

`theme:style(selector, props)` accepts:

- **Selector:** an element class name or pseudo (`tab.active`, `button.primary`, `omnibox:focused`).
- **Props:** a flat table of CSS property names (snake_case). Compound values like `border` accept `{ width, color }` or `{ width, style, color }`.

## Mode-specific overrides (light/dark)

Themes that want to provide both dusk-ish and vellum-ish modes can branch:

```lua
local theme = mote.theme.new("rosé-pine")

if mote.system.color_scheme() == "light" then
  theme:set_tokens({ bg = "#FAF4ED", ... })
else
  theme:set_tokens({ bg = "#191724", ... })
end
```

Or simpler — define two themes and let the user pick.

## Standard themes

Mote ships these by default. Implementing the runtime, you must include them.

| Theme | Mode | Vibe |
|---|---|---|
| `dusk` | dark | warm ink (default) |
| `vellum` | light | warm paper |
| `embers` | dark | redder accent, hotter |
| `gloam` | dark | bluer cool variant |

The exact token values for each live in `themes/<name>.lua` in the runtime — they're not duplicated here. The values for `dusk` and `vellum` are what's encoded in `colors_and_type.css`.

## What themes CAN'T do

Mote's design system establishes constraints themes must respect. The runtime enforces these:

- Themes cannot disable the focus ring (accessibility).
- Themes cannot set radius higher than `--radius-3` (6px). The constraint is enforced at token-set time.
- Themes cannot reintroduce filled icons — the icon component renders strokes regardless.
- Themes cannot add bluish-purple gradients or backdrop blur — there's no API for either.

These limits exist so a community theme can't easily produce something that doesn't feel like Mote. The token vocabulary is intentionally **constrained**.

## User overrides (in `init.lua`)

The user's `init.lua` runs after the theme. It can do anything a theme can do, plus install plugins and bind keys:

```lua
local mote = require("mote")
mote.theme.load("dusk")

-- override the active theme's accent
mote.theme.current():set_token("accent", "#FF8800")

-- bind an element override at the user level
mote.theme.current():bind("sidebar", mote.elements.sidebar_assistant)
```

User overrides always win.

## Next

[`components/`](./components/) — implementation contracts for each component.
