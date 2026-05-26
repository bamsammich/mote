# Command palette

## Purpose

The keyboard-driven action surface. Invoked by `⌘⇧P`. Floats centered, anchored 80px from the top of the viewport. Fuzzy-matches against a flat list of commands contributed by core + plugins.

## Structure

```html
<div class="palette-backdrop" role="dialog" aria-modal="true">
  <div class="palette">
    <div class="palette-input">
      <span class="prompt">›_</span>
      <input value="theme" placeholder="search commands" autoFocus />
      <span class="count">12</span>
    </div>
    <div class="palette-list">
      <div class="palette-row is-sel">
        <span class="cat">theme</span>
        <span class="name"><b>theme</b> switch — dusk</span>
        <span class="keys"><kbd>⏎</kbd></span>
      </div>
      <!-- more rows... -->
    </div>
  </div>
</div>
```

## Tokens

```css
.palette-backdrop {
  position: fixed; inset: 0;
  background: rgba(14, 12, 10, 0.4);    /* dusk default */
  padding-top: 80px;
  display: flex; justify-content: center;
}
[data-theme="vellum"] .palette-backdrop {
  background: rgba(20, 17, 15, 0.18);
}
.palette {
  width: var(--palette-w);              /* 640px */
  background: var(--surface-1);
  border: 1px solid var(--border);
  border-radius: var(--radius-3);
  box-shadow: var(--shadow-2);
}
.palette-input {
  padding: 12px 14px;
  border-bottom: 1px solid var(--border);
  font: var(--text-mono);
}
.palette-input .prompt { color: var(--accent); }
.palette-row {
  padding: 8px 14px;
  font: var(--text-mono);
  color: var(--fg-1);
}
.palette-row.is-sel {
  background: var(--surface-2);
  color: var(--fg);
}
.palette-row .cat {
  color: var(--fg-2);
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
}
.palette-row .name b { color: var(--accent); }
```

## States

| State | Style |
|---|---|
| open | backdrop dims; palette fades in over `var(--dur-entrance)` |
| input focused | input always autofocuses on open |
| row selected | row gets `is-sel` class — `background: var(--surface-2)` |
| empty result | shows "no commands match" message in mono fg-2 |

## Behavior

| Key | Action |
|---|---|
| `⌘⇧P` | open / close |
| `Esc` | close |
| typing | filters list incrementally |
| `↑` / `↓` | move selection |
| `Enter` | invoke selected command, close palette |
| `Ctrl+J/K` | alternative ↓/↑ (vim-style) |
| clicking row | invoke command |
| clicking backdrop | close |

The match highlights the matched substring in `var(--accent)` (the `<b>` in the `.name` span).

## Behavior — list source

The palette's flat list comes from:

1. Core commands (theme, tab, view, omnibox, sidebar)
2. Plugin-contributed commands (declared in the plugin's module table)
3. User-bound aliases (via `mote.palette.add` in user config)

Each row is `{ cat: string, name: string, keys?: string[] }`. The `cat` is the prefix users type to filter by category (e.g. typing "theme" narrows to all theme commands).

## Accessibility

- `<div role="dialog" aria-modal="true" aria-label="command palette">`.
- Input is the initial focus.
- Selected row gets `aria-selected="true"`; list has `role="listbox"`, rows `role="option"`.
- Esc closes regardless of focus location inside the palette.
- Reduced motion: fade-in becomes instant.

## Anti-patterns

- ❌ Wider than 640px.
- ❌ Slide-down entrance animation.
- ❌ Translucent palette body (the backdrop dims; the palette itself is opaque).
- ❌ Showing more than 8 rows without scroll. The list is dense; users scan, they don't scroll-browse.
- ❌ Icons next to row names — this is the mono surface, text only.
