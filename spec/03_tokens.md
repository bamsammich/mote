# 03 · Tokens

Every value in Mote's visual system is a token. Tokens are exposed two ways:

1. **CSS custom properties** on `:root` (and overridden under `[data-theme="..."]`). Used directly in stylesheets.
2. **Lua fields** on `theme.tokens.<name>`. Used in theme files.

The names mirror each other: CSS `--surface-1` ↔ Lua `theme.tokens.surface_1`.

This file is the canonical token reference. The chrome's runtime stylesheet declares these CSS variables and the Lua bridge surfaces them on `theme.tokens`; the names and values here are ground truth.

---

## Color — palette (raw)

These are the underlying color scales. **Do not use them directly in components.** Reference the semantic tokens below.

| Token | Hex (dusk default) | Description |
|---|---|---|
| `--ink-900` | `#0E0C0A` | deepest |
| `--ink-800` | `#14110F` | bg (dusk) |
| `--ink-700` | `#1C1815` | surface-1 (dusk) |
| `--ink-600` | `#241F1B` | surface-2 |
| `--ink-500` | `#2E2823` | border |
| `--ink-400` | `#3A332D` | border-strong |
| `--ink-300` | `#5C544B` | fg-3 (disabled) |
| `--ink-200` | `#8A8278` | fg-2 (muted) |
| `--ink-100` | `#B5AEA3` | fg-1 (secondary) |
| `--paper-100` | `#FBF8F1` | surface-1 (vellum) |
| `--paper-200` | `#F4EFE6` | bg (vellum) |
| `--paper-300` | `#EAE3D5` | surface-2 |
| `--paper-400` | `#DDD3BF` | border |
| `--paper-500` | `#C7B89C` | border-strong |
| `--paper-600` | `#A89B82` | fg-3 |
| `--paper-700` | `#6B6359` | fg-2 |

## Color — accents

Desaturated, warm-leaning. Used semantically.

| Token | Hex | Role |
|---|---|---|
| `--amber` | `#E0A458` | primary accent — the mote |
| `--amber-soft` | `#F1C893` | hover state of amber surfaces |
| `--amber-deep` | `#B47C36` | border under amber buttons (keycap depth) |
| `--ember` | `#C84A2C` | error / destructive only |
| `--ember-soft` | `#E07B5F` | |
| `--moss` | `#6B8E4E` | success, build-passing, online |
| `--moss-soft` | `#93AE76` | |
| `--dusk-blue` | `#5B7CA3` | info, links, keyword syntax |
| `--dusk-blue-soft` | `#88A3C3` | |
| `--plum` | `#8E6FA0` | special, AI surfaces, "different" |
| `--plum-soft` | `#B398C0` | |
| `--bone` | `#C7B89C` | secondary mark color, dividers in marketing |

## Color — semantic (USE THESE in components)

| Token | Meaning | Dusk default | Vellum default |
|---|---|---|---|
| `--bg` | page background | `--ink-800` | `--paper-200` |
| `--surface-1` | cards, panels, sidebar | `--ink-700` | `--paper-100` |
| `--surface-2` | hover background | `--ink-600` | `--paper-300` |
| `--surface-3` | press background | `--ink-500` | `--paper-400` |
| `--surface-sunk` | inset wells (omnibox field) | `--ink-900` | `--paper-300` |
| `--border` | hairlines | `--ink-500` | `--paper-400` |
| `--border-strong` | stronger borders | `--ink-400` | `--paper-500` |
| `--border-subtle` | rare divider | `--ink-600` | `--paper-300` |
| `--fg` | primary text | `#ECE5D8` | `--ink-800` |
| `--fg-1` | secondary text | `#C9C0B0` | `--ink-400` |
| `--fg-2` | muted text | `--ink-200` | `--paper-700` |
| `--fg-3` | disabled/placeholder | `--ink-300` | `--paper-600` |
| `--fg-inverse` | text on accent surfaces | `--ink-900` | `--paper-100` |
| `--accent` | THE brand accent | `--amber` | `#B47C36` |
| `--accent-soft` | hover on accent | `--amber-soft` | `--amber-soft` |
| `--accent-deep` | accent bottom border | `--amber-deep` | `#8B5A1F` |
| `--accent-on` | text on accent bg | `--ink-900` | `--paper-100` |
| `--success` | green | `--moss` | (recomputed) |
| `--danger` | red | `--ember` | (recomputed) |
| `--info` | blue | `--dusk-blue` | (recomputed) |
| `--special` | plum — AI surfaces | `--plum` | (recomputed) |
| `--focus` | focus ring color | `--amber` | `--amber-deep` |

## Color — syntax (used by code blocks and Lua editing)

| Token | Default | Role |
|---|---|---|
| `--syn-keyword` | `--dusk-blue-soft` | `local`, `function`, `if`, `return` |
| `--syn-string` | `--moss-soft` | quoted strings |
| `--syn-number` | `--amber-soft` | numeric literals |
| `--syn-comment` | `--ink-200` (italic) | comments |
| `--syn-fn` | `--plum-soft` | function calls |
| `--syn-punct` | `--ink-100` | brackets, commas |

## Spacing — 4px base grid

| Token | px |
|---|---|
| `--space-0` | 0 |
| `--space-px` | 1 |
| `--space-1` | 4 |
| `--space-2` | 8 |
| `--space-3` | 12 |
| `--space-4` | 16 |
| `--space-5` | 20 |
| `--space-6` | 24 |
| `--space-7` | 32 |
| `--space-8` | 40 |
| `--space-9` | 48 |
| `--space-10` | 64 |
| `--space-11` | 80 |
| `--space-12` | 96 |

Mote uses the small end of the scale far more than the large end. Density is the goal.

## Radius

| Token | px | Use |
|---|---|---|
| `--radius-0` | 0 | slots, status line |
| `--radius-1` | 2 | buttons, fields, tabs, chips, kbd |
| `--radius-2` | 4 | cards, dialogs |
| `--radius-3` | 6 | palette, completion popup |
| `--radius-dot` | 9999 | status dots only |

## Shadow

| Token | Use |
|---|---|
| `--shadow-1` | hover lift on interactive cards (rare) |
| `--shadow-2` | palette, dropdown, completion popup |
| `--shadow-3` | full-screen modal overlay (rare) |

Shadow values are theme-tuned — dusk shadows are darker, vellum lighter.

## Motion

| Token | Value | Use |
|---|---|---|
| `--ease-out` | `cubic-bezier(0.2, 0, 0, 1)` | default |
| `--ease-in` | `cubic-bezier(0.6, 0, 1, 0.4)` | exits |
| `--ease-in-out` | `cubic-bezier(0.4, 0, 0.2, 1)` | bidirectional |
| `--dur-micro` | 80ms | button press, tab close |
| `--dur-base` | 120ms | default |
| `--dur-entrance` | 200ms | palette open, dropdown reveal |

## Layout (Mote-specific)

| Token | px | What |
|---|---|---|
| `--chrome-tabbar` | 40 | top tab strip height |
| `--chrome-omnibox` | 36 | omnibox row height |
| `--chrome-statusline` | 22 | status line height |
| `--gutter-xs` | 240 | sidebar snap point |
| `--gutter-sm` | 320 | sidebar snap point |
| `--gutter-md` | 400 | sidebar snap point |
| `--gutter-lg` | 480 | sidebar snap point |
| `--palette-w` | 640 | command palette width |
| `--dots` | (gradient) | empty-slot dot-grid texture |

## Lua-side access

A plugin's `render` function receives a `host` that exposes the active theme's resolved tokens (mirrors each CSS var without the leading `--`):

```lua
render = function(host)
  host.tokens.accent      -- "#E0A458"
  host.tokens.surface_1   -- string color
  host.tokens.space_4     -- 16 (number, px)
  host.tokens.radius_2    -- 4
end
```

A theme sets token values declaratively in its `M.theme.styling` block (see `07_themes.md`); a user overrides them via `mote.theme_overrides({ styling = { colors = { accent = "#FF8800" } } })`.

## Adding a token

If you genuinely need a new token, add it in **two places at once**:

1. `spec/03_tokens.md` — add a row to the right table here (the canonical reference)
2. The chrome's runtime stylesheet — declare the CSS variable; the Lua bridge maps CSS-var-name → Lua-snake-case automatically, so verify `theme.tokens` picks up your new var

If a value isn't reused by more than one component, **don't tokenize it** — it's a local detail.
