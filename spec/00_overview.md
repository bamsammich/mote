# 00 · Overview

## What Mote is

**Mote** is a programmable, AI-native web browser for developers who live in dotfiles. Its UI is composed of **slots**, **elements**, and **themes**:

- **Plugin authors** provide content (elements).
- **Themes** decide placement and styling (which element binds to which slot, in what visual treatment).
- **Users** override anything in Lua.

Every visible region in Mote's chrome is a **slot**. Every piece of content inside one is an **element**. The browser is held together by a Lua configuration that tells the runtime which elements live in which slots and how they look.

The product surface is **the browser window itself**. There is no separate marketing site or settings app in scope — settings happen by editing Lua.

## Glossary

| Term | Definition |
|---|---|
| **Slot** | A named region declared by a theme. Examples: `tab_bar`, `omnibox`, `sidebar`, `status_line`, `viewport`. Slots are structural; they always exist when their theme is loaded. |
| **Element** | A renderable content unit a plugin contributes. Examples: a tab, an AI message, a bookmark row, a vim-mode indicator. Elements bind to slots. |
| **Theme** | A Lua file that declares slots, binds default elements, and applies design tokens. Themes ship as `~/.config/mote/themes/<name>.lua`. |
| **Plugin** | A Lua + JS package that contributes elements (and optionally commands, keybinds, palette entries). Lives in `~/.config/mote/plugins/<name>/`. |
| **Token** | A design value (color, size, font) exposed as a CSS variable AND as a Lua field on `theme.tokens`. The bridge between Lua config and CSS. |
| **Omnibox** | The URL/command/query bar. Multi-modal: `[url]`, `[cmd]`, `[ask]`, `[find]`. |
| **Palette** | The command palette (⌘⇧P). Centered floating overlay, 640px wide. |
| **Sidebar** | A generic left-side slot hosting a swappable element panel (tabs, bookmarks, history, assistant, plugins, lua config). |
| **Status line** | A 22px persistent strip at the bottom of the browser. Shows mode, connection, AI state, hovered URL, etc. |
| **Dusk / Vellum** | The default dark and light themes that ship with Mote. Both must remain first-class. |
| **The mote** | Conceptually a particle of light/dust. Visually: an amber `#E0A458` accent. The mark for the brand is `[·]`. |

## The slot/element split, concretely

A theme written in Lua declares slots and binds elements:

```lua
-- ~/.config/mote/themes/dusk.lua
local mote = require("mote")
local theme = mote.theme.new("dusk")

-- Declare a slot
theme:slot("omnibox", {
  height = 36,
  bg     = theme.tokens.surface_sunk,
  border = theme.tokens.border,
  accent = theme.tokens.amber,
})

-- Bind an element into that slot
theme:bind("omnibox", mote.elements.omnibox_default)

-- Override an element's style
theme:style("tab.active", {
  border_top = { 2, theme.tokens.accent }
})

return theme
```

A plugin contributes elements:

```lua
-- ~/.config/mote/plugins/git-status/init.lua
local mote = require("mote")
mote.plugin.register("git-status", {
  elements = {
    { id = "git_indicator", slot = "status_line", render = function() ... end }
  }
})
```

A user, in their `init.lua`, picks the theme, installs plugins, and overrides anything:

```lua
local mote = require("mote")
mote.theme.load("dusk")
mote.plugin.use("git-status")
mote.bind("cmd-shift-p", mote.palette.open)
mote.sidebar.default("tabs")
```

This three-layer system (theme / plugin / user) is the core architectural commitment. Every component spec in this repo respects it.

## Non-goals (for the design system)

- **Cross-platform native widgets.** Mote uses HTML/CSS for chrome. Don't mock native macOS/Windows widgets.
- **Multiple product surfaces.** The browser is the product. There is no companion mobile, no settings UI, no marketing site within this spec's scope. (Marketing is a future addition that should use this same token vocabulary.)
- **A "kitchen sink" component library.** Mote ships a small, sharp set: button, field, tab, omnibox, palette, status line, badge, card, kbd, sidebar shell, message. Don't invent more without a real need.

## Stack assumptions

Mote's chrome renders via a web technology (the actual implementation language is up to you — Tauri+Web, Servo+Web, custom). The design system is delivered as **CSS variables + HTML structural conventions**, which any chrome runtime can consume. Lua-side, design tokens are surfaced as `theme.tokens.<name>` — the names mirror the CSS var names.

## Next

Continue to [`01_architecture.md`](./01_architecture.md) for the Lua API surface.
