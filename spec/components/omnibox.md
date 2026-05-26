# Omnibox

## Purpose

The combined URL bar, command runner, AI query input, and find-in-page input. **Multi-modal** — the mode is declared by a bracket-wrapped tag in the left of the field, reusing the `[mote]` brand lockup.

## Modes

| Mode | Prefix to enter | Tag | Use |
|---|---|---|---|
| `url` | (default) | `[url]` | navigate to a URL or search |
| `cmd` | `:` | `[cmd]` | run a command (e.g. `:theme switch dusk`) |
| `ask` | `?` | `[ask]` | send a query to the AI assistant |
| `find` | `/` | `[find]` | find text in the current page |

The mode auto-switches when the user types the entering prefix as the first character. The user can also force a mode via keybinds:

- `⌘K` — open in url mode (default)
- `⌘⇧P` — open command palette (separate component)
- `⌘L` — open in ask mode (alternative entry to AI)
- `⌘F` — open in find mode

## Structure

```html
<div class="omni mode-url is-focused">
  <span class="mode">
    <span class="br">[</span><span class="name">url</span><span class="br">]</span>
  </span>
  <div class="body">
    <!-- secure indicator, then URL or input -->
    <span class="secure">⎈</span>
    <span class="host-dim">github.com/motesh/</span>
    <span class="host">mote</span>
    <span class="path">/blob/main/init.lua</span>
    <div class="right">★ · ▣</div>
  </div>
</div>
```

When focused, the URL display is replaced with an `<input>` and a block cursor:

```html
<div class="body">
  <input class="omni-input" />
  <span class="cursor"></span>
</div>
```

## Tokens

```css
.omni {
  height: 32px;
  background: var(--surface-sunk);
  border: 1px solid var(--border);
  border-radius: var(--radius-1);
  font: var(--text-mono);
  color: var(--fg);
}
.omni.is-focused {
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(224, 164, 88, 0.18);
}
.omni.is-focused.mode-ask  { border-color: var(--special); }
.omni.is-focused.mode-find { border-color: var(--info); }

.mode {
  padding: 0 10px;
  color: var(--accent);
  border-right: 1px solid var(--border);
  background: var(--surface-1);
}
.mode .name { color: var(--fg); }

.host-dim { color: var(--fg-2); }
.host     { color: var(--fg); }
.path     { color: var(--fg-2); }
```

The block cursor:

```css
.cursor {
  display: inline-block;
  width: 7px; height: 14px;
  background: var(--accent);
  margin-left: 1px;
  animation: blink 1.2s steps(2, end) infinite;
}
.mode-ask  .cursor { background: var(--special); }
.mode-find .cursor { background: var(--info); }
```

## States

| State | Style |
|---|---|
| default | URL shown with host-dim / host / path coloring |
| focused (any mode) | input visible, cursor blinking, focus ring matching mode |
| empty + focused | placeholder text (`var(--fg-3)`), cursor at start |
| cmd / ask / find | mode-specific prefix character in accent color, mode-specific cursor color |

## Behavior

- **Tab** moves focus to the next chrome element (browser nav).
- **Esc** blurs the omnibox without committing.
- **Enter** commits the action for the current mode.
- **Backspace at the start of an empty field** does not change mode — the user must press `Esc` and re-enter.
- **Typing `:`, `?`, `/` as the first char** switches mode and consumes the character.
- Mode tag is **not clickable** — modes are entered by prefix or keybind, not mouse.

## Accessibility

- Wrap in `<form role="search">` for url/find modes; `<form>` without role for cmd/ask.
- Input gets `aria-label` matching the active mode (`"url bar"`, `"command runner"`, `"ai query"`, `"find in page"`).
- Mode tag has `aria-hidden="true"` — it's decorative; the input's `aria-label` carries the meaning.
- Right-side icons are buttons with `aria-label`.

## Example

See [`preview/components-omnibox.html`](../../preview/components-omnibox.html) for all four modes rendered with annotations.

## Anti-patterns

- ❌ Translucent / blurred backdrop.
- ❌ Animated mode switch (slide, fade) — modes swap instantly.
- ❌ Showing all four mode tags simultaneously as a segmented control. The mode is set by typing, not by clicking.
- ❌ Thin caret cursor — Mote uses the block cursor.
- ❌ Mode tag as a pill.
