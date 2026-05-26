# 00 · Overview

## What Mote is

**Mote** is a programmable, AI-native web browser for developers who live in dotfiles. Its UI is composed of **slots**, **elements**, and **themes**:

- **Plugin authors** provide content (elements).
- **Themes** decide placement and styling (which element binds to which slot, in what visual treatment).
- **Users** override anything in Lua.

Every visible region in Mote's chrome is a **slot**. Every piece of content inside one is an **element**. The browser is held together by a Lua configuration that tells the runtime which elements live in which slots and how they look.

The product surface is **the browser window itself**. There is no separate marketing site or settings app in scope — settings happen by editing Lua.

## Glossary

The slot and element-kind names below are the runtime's canonical taxonomy (kebab-case). Where this spec's component files refer to a region informally (e.g. "the omnibox," "the status line"), that name maps onto a runtime slot/element as noted.

| Term | Definition |
|---|---|
| **Slot** | A named layout region the runtime owns. The fixed v0.1 set is `top-bar`, `left-sidebar`, `right-sidebar`, `bottom-bar`, `urlbar-inline`, `tab-row`. Themes decide which elements go in which slots; plugins do not choose placement. |
| **Element** | A renderable content unit of a known *kind* that a plugin contributes. The fixed v0.1 kinds are `urlbar`, `tabstrip`, `bookmarks-bar`, `sidebar-panel`, `action-button`, `status-indicator`, `urlbar-extension`, `widget`. Elements bind to slots by theme decision. |
| **Theme** | A plugin fulfilling `theme:provider` (exclusive). It decides element-to-slot placement, ordering, resizability, and applies design tokens. |
| **Plugin** | A Lua (optionally WASM) module that contributes elements (and optionally commands, keybinds, palette entries) via a declarative module table. Lives in `~/.config/mote/plugins/<name>/`. |
| **Token** | A design value (color, size, font) exposed as a CSS variable AND as a Lua field on `theme.tokens`. The bridge between Lua config and CSS. |
| **Omnibox** | The URL/command/find bar — the `urlbar` element, shown in `top-bar` and extended via `urlbar-inline`. Multi-modal: `[url]`, `[cmd]`, `[find]`. The `[ask]` mode name is *reserved* for a future plugin; the runtime ships no AI mode (see below). |
| **Palette** | The command palette (⌘⇧P). A `widget`-kind overlay, centered, 640px wide. |
| **Sidebar** | A `left-sidebar` or `right-sidebar` slot hosting swappable `sidebar-panel` elements (tabs, bookmarks, history, plugins, lua config). |
| **Status line** | A 22px persistent strip in `bottom-bar`, composed of `status-indicator` elements. Shows mode, connection, hovered URL, etc. |
| **Dusk / Vellum** | The default dark and light themes that ship with Mote. Both must remain first-class. |
| **The mote** | Conceptually a particle of light/dust. Visually: an amber `#E0A458` accent. The mark for the brand is `[·]`. |

> **AI is plugin-delivered, not a runtime feature.** Mote ships no built-in AI UI — no chatbot panel, no AI summaries, no AI urlbar suggestions (core principle #8). The `[ask]` omnibox mode and an `assist` sidebar panel are *reserved names* a future AI plugin may fill; a plugin reaches an LLM via `http:fetch` plus `secret:read`. Nothing in this spec implies the runtime ships AI behavior.

## The slot/element split, concretely

A theme is a plugin fulfilling `theme:provider`. Its module table places elements into slots and applies styling tokens:

```lua
-- ~/.config/mote/plugins/dusk/init.lua
local M = {}

M.manifest = {
  schema = "v1",
  name = "dusk",
  version = "1.0.0",
  capabilities = { "theme:provider" },
}

M.theme = {
  layout = {
    ["top-bar"]      = { "urlbar", "tabstrip" },
    ["left-sidebar"] = { "sidebar-panel:bookmarks", "sidebar-panel:*" },
    ["bottom-bar"]   = { "status-indicator:*" },
  },
  styling = {
    colors = { bg = "#14110F", fg = "#ECE5D8", accent = "#E0A458" },
  },
}

return M
```

A plugin contributes elements via `ui.register_element`. The plugin declares the element's *kind*; the theme decides placement:

```lua
-- ~/.config/mote/plugins/git-status/init.lua
local M = {}

M.manifest = {
  schema = "v1",
  name = "git-status",
  version = "0.2.0",
  permissions = { "ui:status_indicator" },
}

function M.setup()
  ui.register_element({
    id = "git-status",
    kind = "status-indicator",
    render = function(host) ... end,
  })
end

return M
```

A user, in their config, picks the theme, lists plugins, binds keys, and overrides anything:

```lua
mote.plugins({ "dusk", "git-status" })
mote.keys.bind("Mod+Shift+P", function() mote.palette.open() end)
mote.theme_overrides({ styling = { colors = { accent = "#FF8800" } } })
```

This three-layer system (theme / plugin / user) is the core architectural commitment. Every component spec in this repo respects it.

## Non-goals (for the design system)

- **Cross-platform native widgets.** Mote uses HTML/CSS for chrome. Don't mock native macOS/Windows widgets.
- **Multiple product surfaces.** The browser is the product. There is no companion mobile, no settings UI, no marketing site within this spec's scope. (Marketing is a future addition that should use this same token vocabulary.)
- **A "kitchen sink" component library.** Mote ships a small, sharp set: button, field, tab, omnibox, palette, status line, badge, card, kbd, sidebar shell. Don't invent more without a real need.

## Stack assumptions

Mote's chrome renders as HTML/CSS on an off-screen CEF surface composited by the shell. The design system is delivered as **CSS variables + HTML structural conventions** the chrome consumes directly. Lua-side, design tokens are surfaced as `theme.tokens.<name>` — the names mirror the CSS var names.

## Next

Continue to [`01_architecture.md`](./01_architecture.md) for the Lua API surface.
