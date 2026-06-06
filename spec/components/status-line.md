# Status line

## Purpose

A 22px-tall persistent strip at the bottom of the browser (the `bottom-bar` slot), composed of `status-indicator` elements. **The status line replaces** the floating hover-URL toast, the download chip, and the connection icon that mainstream browsers scatter across the chrome. Every signal lives here.

## Structure

```html
<div class="sl">
  <div class="seg mode">NORMAL</div>
  <div class="seg"><span class="dot ok"></span>https · tls 1.3</div>
  <div class="seg spacer"></div>
  <div class="seg" style="color:var(--fg-2)">7 tabs · 142mb</div>
  <div class="seg theme-btn">theme: dusk</div>
</div>
```

## Segments (canonical)

Left-aligned (the loudest signals):

| Segment | Content | Source |
|---|---|---|
| `mode` | `NORMAL` / `INSERT` / `COMMAND` chip (if vim binds active) | omnibox / keymap state |
| `connection` | TLS info + ok/fail dot | page loader |

Spacer (`flex: 1`) fills the middle.

Right-aligned (ambient):

| Segment | Content | Source |
|---|---|---|
| `resources` | tab count, RAM usage | runtime |
| `theme` | active theme name (clickable to cycle) | active theme |

### Contextual segments

The status line **morphs** based on what's happening. The fixed segments above are joined or replaced contextually:

| Situation | Behavior |
|---|---|
| Mouse hovers a link in the page | a `link-hover` segment expands across the spacer showing the full URL, replacing the resources/theme segments temporarily |
| A page is loading | the connection segment becomes an inline progress bar; a `loading 64%` label appears next to it |

> **No built-in AI status segment.** Mote ships no AI runtime, so there is no `assist` segment in the chrome (core principle #8). A plugin providing AI features may register its own `status-indicator` element; the `dot.special` (plum, pulsing) treatment in the tokens below is documented so such a plugin stays on-brand. A `download` segment is likewise plugin-contributed where a download manager registers one.

## Tokens

```css
.sl {
  height: var(--chrome-statusline);  /* 22px */
  background: var(--surface-1);
  border-top: 1px solid var(--border);
  font: var(--text-mono-sm);
  color: var(--fg-1);
}
.seg {
  padding: 0 10px;
  border-right: 1px solid var(--border);
}
.seg:last-child { border-right: 0; }
.spacer { flex: 1; border-right: 0; padding: 0; }

.mode {
  background: var(--accent); color: var(--accent-on);
  font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase;
}
.mode.insert { background: var(--moss); color: var(--ink-900); }
.mode.cmd    { background: var(--dusk-blue); color: var(--paper-100); }

.dot.ok      { background: var(--success); }
.dot.work    { background: var(--accent); animation: pulse 1.2s ease-in-out infinite; }
.dot.special { background: var(--special); animation: pulse 1.2s ease-in-out infinite; }
.dot.off     { background: var(--fg-3); }
```

## Behavior

- Most segments are **read-only** — they reflect runtime state.
- The `theme` segment is **clickable** — click to cycle to the next theme.
- The `mode` chip is **clickable** — click to open a dropdown of available modes.
- Hovering the status line itself does nothing — segments are not buttons unless documented above.

## Customization (Lua)

Plugins contribute segments by registering `status-indicator` elements (see `01_architecture.md`):

```lua
function M.setup()
  ui.register_element({
    id = "git-status-line",
    kind = "status-indicator",
    render = function(host)
      return { text = "⎇ main · clean", color = host.tokens.fg_2 }
    end,
  })
end
```

The active theme decides segment order by placing `status-indicator` elements into `bottom-bar`:

```lua
-- in a theme's M.theme.layout
["bottom-bar"] = {
  "status-indicator:vim-mode",
  "status-indicator:connection",
  "status-indicator:*",          -- any other status indicators
},
```

## Accessibility

- The status line itself is `role="status"` with `aria-live="polite"` for state changes (mode switch, etc.).
- Per-segment text uses `aria-label` when the visible text is a glyph (`⎇` → `aria-label="branch main, clean"`).
- The mode chip has `aria-label="vim mode: NORMAL"`.
- Reduced motion: the pulsing dot becomes static.

## Anti-patterns

- ❌ Status line taller than 22px.
- ❌ Emoji or multi-color icons in segments. A segment **may** carry a single
  leading **Lucide stroke icon** at the 14px status-line size (per
  `06_iconography.md` — e.g. `lock`/`circle-check`/`circle-x`); the surface is
  mono, so the icon is single-color (`currentColor`) and text still carries the
  meaning. (Reconciles a former "no icons" rule that contradicted
  `06_iconography.md` and the shipped `mote.security` segment — CL-SPECDRIFT B2.)
- ❌ Animated segment swaps. State changes are instant.
- ❌ Toasts above the status line for messages that belong in it.
