# Status line

## Purpose

A 22px-tall persistent strip at the bottom of the browser. **The status line replaces** the floating hover-URL toast, the download chip, the connection icon, and the AI status indicator that mainstream browsers scatter across the chrome. Every signal lives here.

## Structure

```html
<div class="sl">
  <div class="seg mode">NORMAL</div>
  <div class="seg"><span class="dot ok"></span>https · tls 1.3</div>
  <div class="seg"><span class="dot ok"></span>assist idle</div>
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
| `assist` | "idle" / "thinking" / "3 files" with status dot | AI runtime |

Spacer (`flex: 1`) fills the middle.

Right-aligned (ambient):

| Segment | Content | Source |
|---|---|---|
| `resources` | tab count, RAM usage | runtime |
| `theme` | active theme name (clickable to cycle) | theme.current() |

### Contextual segments

The status line **morphs** based on what's happening. The fixed segments above are joined or replaced contextually:

| Situation | Behavior |
|---|---|
| Mouse hovers a link in the page | a `link-hover` segment expands across the spacer showing the full URL, replacing the resources/theme segments temporarily |
| A page is loading | the connection segment becomes an inline progress bar; a `loading 64%` label appears next to it |
| AI is thinking | the assist segment shows a pulsing plum dot and the current context count |
| A download starts | a `download` segment slots in next to assist showing filename + progress |

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
- The `assist` segment is **clickable** — click to open the assistant panel in the sidebar.
- Hovering the status line itself does nothing — segments are not buttons unless documented above.

## Customization (Lua)

Plugins contribute segments by registering elements in the `status_line` slot:

```lua
mote.plugin.register("git-status-line", {
  elements = {
    {
      id = "git_indicator",
      slot = "status_line",
      render = function()
        return { text = "⎇ main · clean", color = mote.theme.tokens.fg_2 }
      end,
    },
  }
})
```

The theme decides segment order via the bind list:

```lua
theme:bind("status_line", {
  mote.elements.vim_mode,
  mote.elements.connection,
  mote.elements.assist_status,
  mote.elements.spacer,
  mote.elements.resources,
  mote.elements.theme_switcher,
})
```

## Accessibility

- The status line itself is `role="status"` with `aria-live="polite"` for state changes (mode switch, assist starts thinking, etc.).
- Per-segment text uses `aria-label` when the visible text is a glyph (`⎇` → `aria-label="branch main, clean"`).
- The mode chip has `aria-label="vim mode: NORMAL"`.
- Reduced motion: the pulsing dot becomes static.

## Example

See [`preview/components-statusline.html`](../../preview/components-statusline.html) for idle, link-hover, and working states rendered with annotations.

## Anti-patterns

- ❌ Status line taller than 22px.
- ❌ Icons in segments. Use text — this is the mono surface.
- ❌ Animated segment swaps. State changes are instant.
- ❌ Toasts above the status line for messages that belong in it.
