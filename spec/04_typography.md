# 04 · Typography

## Stack

| Role | Family | Notes |
|---|---|---|
| Sans | **Geist** | Vercel's open-source sans. UI, prose, button labels. |
| Mono | **JetBrains Mono** | The dotfile-culture monospace. Used heavily — labels, breadcrumbs, omnibox, palette, status line. |
| Serif | **Instrument Serif** | Marketing accents and doc display **only**. Never in browser chrome. |

CSS variables:

```css
--font-sans:  "Geist", ui-sans-serif, system-ui, sans-serif;
--font-mono:  "JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace;
--font-serif: "Instrument Serif", ui-serif, Georgia, serif;
```

Fonts are loaded via `@import` at the top of the chrome's runtime stylesheet. If Mote ships a commissioned typeface, drop `woff2` files into the chrome's `fonts/` directory and replace the `@import` line with `@font-face` blocks; the semantic vars don't need to change.

## Ramps (use these — not raw size/weight)

| Token | Spec | Use |
|---|---|---|
| `--text-display` | 600 48/1.05 sans | hero text (rare) |
| `--text-h1` | 600 32/1.15 sans | page title |
| `--text-h2` | 600 24/1.2 sans | section heading |
| `--text-h3` | 600 18/1.3 sans | subsection |
| `--text-body-lg` | 400 16/1.55 sans | long-form prose |
| `--text-body` | 400 14/1.5 sans | **default UI** |
| `--text-small` | 400 12/1.4 sans | dense UI |
| `--text-micro` | 500 11/1.3 sans | status line, badges (uppercased) |
| `--text-mono` | 400 13/1.4 mono | code, paths |
| `--text-mono-sm` | 400 11/1.3 mono | inline metadata |
| `--text-kbd` | 500 11/1 mono | key bindings |
| `--text-serif-display` | 400 56/1.05 serif | marketing only |
| `--text-serif-quote` | 400 28/1.3 serif | marketing only |

Tracking:

```css
--tracking-tight: -0.01em;   /* headings */
--tracking-normal: 0;
--tracking-wide:  0.04em;    /* uppercase micro labels */
--tracking-mono: -0.01em;    /* mono looks tight at small sizes */
```

## Apply in CSS

```css
.thing {
  font: var(--text-body);
}

h1 {
  font: var(--text-h1);
  letter-spacing: var(--tracking-tight);
}

.kbd-thing {
  font: var(--text-kbd);
}
```

## Key bindings (`<kbd>`)

`<kbd>` renders as a small mechanical key. Construction:

```css
kbd {
  font: var(--text-kbd);
  display: inline-flex;
  align-items: center; justify-content: center;
  min-width: 18px;
  padding: 2px 5px;
  border: 1px solid var(--border);
  border-bottom-width: 2px;   /* keycap */
  border-radius: var(--radius-1);
  background: var(--surface-1);
  color: var(--fg-1);
}
```

Use the **actual unicode key symbols**, never `Cmd+K`:

```
⌘ command   ⌥ option   ⇧ shift   ⌃ control
⏎ return    ⌫ delete   ⎋ escape  ⇥ tab
```

Render multi-key combos as separate `<kbd>` elements with a hairline gap:

```html
<span class="combo"><kbd>⌘</kbd><kbd>⇧</kbd><kbd>P</kbd></span>
```

```css
.combo { display: inline-flex; gap: 2px; }
```

## Mono usage policy

JetBrains Mono is used **prominently** — much more than in typical UI libraries. Use it for:

- All paths, URLs, file names
- All UI labels in dev-tooling contexts (the omnibox, status line, sidebar headers, the command palette)
- Tabs and breadcrumbs
- Metadata strings (`3 of 17 · ⏎ next`)
- Numbers in dense data displays

Use sans for:

- Headings (`h1`–`h3`)
- Body prose (readme, documentation)
- Button labels
- Form labels

When in doubt: **mono**. Mote's audience prefers it.

## Serif usage policy

Serif is reserved. Only use it in:

- The `motesh.dev` marketing site (out of scope for the browser implementation)
- Long-form documentation pages where headings benefit from contrast
- Pull quotes in changelog/release-note pages

**Never use serif in browser chrome.** It breaks the system.

## Next

[`05_motion.md`](./05_motion.md).
