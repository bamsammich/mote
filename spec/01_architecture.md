# 01 · Architecture

## Layers

```
┌─────────────────────────────────────────────┐
│  user init.lua             (top — overrides)│
├─────────────────────────────────────────────┤
│  plugins                   (contribute elements, commands)
├─────────────────────────────────────────────┤
│  theme                     (declares slots, binds defaults, styles)
├─────────────────────────────────────────────┤
│  mote runtime              (slot host, lua bridge, web chrome)
└─────────────────────────────────────────────┘
```

Load order is **bottom up, then top wins on conflict**:

1. Runtime boots. Default theme `dusk` is loaded.
2. User's `init.lua` runs. It can switch themes, install plugins, and apply overrides.
3. Plugins register their elements. Each plugin can suggest default slot bindings; the active theme decides whether to honor them.
4. The user's overrides (the last block of `init.lua`) win unconditionally.

## Lua API (target surface)

This is the API plugin authors, theme authors, and users program against. Implement what's listed; don't expand the surface without a reason.

### `mote.theme`

```lua
mote.theme.load(name)              -- switch to a theme by name (e.g. "dusk")
mote.theme.reload()                -- re-evaluate the active theme file from disk
mote.theme.new(name)               -- create a new theme (returns a Theme handle)
mote.theme.current()               -- get the active theme handle
```

### `Theme` (handle returned by `theme.new`)

```lua
theme:slot(name, attrs)            -- declare a slot
theme:bind(slot_name, element)     -- bind an element into a slot
theme:unbind(slot_name)            -- empty a slot (renders the empty-slot motif)
theme:style(selector, props)       -- apply style overrides to an element selector
theme.tokens                       -- table of design tokens (see spec/03_tokens.md)
```

`attrs` for `slot()` accepts: `width`, `height`, `bg`, `fg`, `border`, `accent`, `position` (`top|bottom|left|right|float|inline`), `resizable` (bool), `min`, `max`.

### `mote.plugin`

```lua
mote.plugin.register(id, manifest) -- called by plugin's init.lua
mote.plugin.use(id, config)        -- user enables a plugin in their init.lua
mote.plugin.list()                 -- enumerate installed plugins
```

Manifest shape:

```lua
{
  name = "git-status",
  version = "0.2.0",
  elements = { { id = "...", slot = "...", render = function() end } },
  commands = { { id = "git.commit", run = function() end, keys = "cmd-shift-g" } },
  palette  = { { name = "git: commit", cmd = "git.commit" } },
}
```

### `mote.bind`

```lua
mote.bind(keys, fn)                -- global keybinding ("cmd-k", "ctrl-shift-p", etc.)
mote.bind(keys, ":command")        -- bind to a registered command id
mote.unbind(keys)
```

### `mote.palette`

```lua
mote.palette.open()
mote.palette.close()
mote.palette.add({ name, cmd, cat, keys })
```

### `mote.omnibox`

```lua
mote.omnibox.open(mode?)           -- mode = "url" | "cmd" | "ask" | "find"
mote.omnibox.set(text)
mote.omnibox.mode()
```

### `mote.sidebar`

```lua
mote.sidebar.open()
mote.sidebar.close()
mote.sidebar.toggle()
mote.sidebar.default(panel_id)     -- set the panel that opens by default
mote.sidebar.show(panel_id)
mote.sidebar.side("left" | "right") -- where the sidebar docks
```

### `mote.tabs`

```lua
mote.tabs.new(url?)
mote.tabs.close(id?)
mote.tabs.list()
mote.tabs.switch(id)
mote.tabs.hibernate(id)
mote.tabs.pin(id)
```

### `mote.ai`

```lua
mote.ai.ask(prompt, opts)          -- one-shot
mote.ai.conversation()             -- handle for an ongoing chat
mote.ai.context.add(refs)          -- attach pages/files/selections as context
```

### Events

Plugins subscribe via `mote.on(event, fn)`:

- `tab.opened`, `tab.closed`, `tab.activated`, `tab.hibernated`
- `page.loading`, `page.loaded`, `page.error`
- `theme.changed`, `theme.reloaded`
- `palette.opened`, `palette.closed`
- `omnibox.mode_changed`
- `ai.message`, `ai.complete`

## Standard slots

Themes are free to declare custom slots, but **these are reserved names** the runtime expects:

| Slot | Position | Default content | Notes |
|---|---|---|---|
| `tab_bar` | top | tabs list | May be empty if theme uses sidebar tabs only |
| `omnibox` | top | omnibox element | Required |
| `sidebar` | left or right | swappable panel (default: tabs) | Toggleable |
| `viewport` | center, fill | the active page | Required, fills remaining space |
| `status_line` | bottom | status indicators | Required, 22px |
| `palette` | float, centered | command palette | Hidden until invoked |

## CSS / HTML conventions

The chrome runtime renders these slots as standard HTML with `data-slot="<name>"` attributes. Themes can target slots via CSS variables on `[data-slot="..."]`.

```html
<div class="mote-root" data-theme="dusk">
  <div data-slot="tab_bar">...</div>
  <div data-slot="omnibox">...</div>
  <div class="mote-viewport">
    <aside data-slot="sidebar">...</aside>
    <main data-slot="viewport">...</main>
  </div>
  <div data-slot="status_line">...</div>
</div>
```

Empty slots render the dot-grid motif — see `spec/components/empty-slot.md`.

## Next

Continue to [`02_design_principles.md`](./02_design_principles.md).
