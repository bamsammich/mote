# `<kbd>` — key glyphs

## Purpose

Render a keyboard key as a small mechanical glyph. The construction Mote's whole UI inherits from — the **keycap** — originates here.

## Structure

```html
<span class="combo"><kbd>⌘</kbd><kbd>⇧</kbd><kbd>P</kbd></span>
```

Multiple keys go in separate `<kbd>` elements inside a `.combo` wrapper. Do not concatenate into one `<kbd>` (`<kbd>⌘⇧P</kbd>` — wrong).

## Tokens

```css
kbd {
  font: var(--text-kbd);
  display: inline-flex; align-items: center; justify-content: center;
  min-width: 18px;
  padding: 2px 5px;
  border: 1px solid var(--border);
  border-bottom-width: 2px;          /* keycap */
  border-radius: var(--radius-1);
  background: var(--surface-1);
  color: var(--fg-1);
}

.combo {
  display: inline-flex;
  gap: 2px;
  vertical-align: baseline;
}
```

In compact contexts (palette rows, button hints), use a tighter `kbd`:

```css
.kbd-sm {
  font-size: 9px;
  padding: 1px 3px;
  min-width: 14px;
}
```

## Unicode glyphs (mandatory)

Always use the unicode symbol, never spelled out:

| Key | Glyph |
|---|---|
| Command | ⌘ |
| Option / Alt | ⌥ |
| Shift | ⇧ |
| Control | ⌃ |
| Return | ⏎ |
| Delete / Backspace | ⌫ |
| Escape | ⎋ |
| Tab | ⇥ |
| Space | (literal word `space` in lowercase) |
| Arrow keys | ↑ ↓ ← → |

Letter keys are uppercase: `<kbd>P</kbd>`, `<kbd>K</kbd>`. Numeric and punctuation keys render as typed: `<kbd>1</kbd>`, `<kbd>,</kbd>`.

## Where they appear

- Inline in copy: "press <kbd>⌘</kbd><kbd>K</kbd> to focus the omnibox"
- Right-aligned in palette rows
- In the status line, as text — not as `<kbd>` glyphs (font-size constraints)
- In tooltips on icon-only buttons

## Accessibility

- `<kbd>` is the correct semantic element — don't replace with `<span>`.
- For screen reader output, optionally wrap a key combo in a parent with `aria-label="Command + K"` to override the glyph reading.

## Anti-patterns

- ❌ `Cmd+K` written out as text instead of glyphs.
- ❌ A single `<kbd>` containing multiple keys.
- ❌ Underline or hover state on `<kbd>` — they're not interactive.
- ❌ `box-shadow` on `<kbd>` to fake depth. The 2px bottom border *is* the depth.
