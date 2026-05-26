# Tab strip

## Purpose

Display the open tabs. Mote's tab strip is **themable** — the default ships as a horizontal strip across the top, but themes can override to an underline-only treatment, keycap-style tabs, or move tabs into the sidebar entirely (the `tabs` panel).

## Structure (default horizontal strip)

```html
<div class="tabbar">
  <div class="tab is-pinned"><span class="favicon" /></div>
  <div class="tab is-active">
    <span class="favicon" />
    <span class="title">motesh.dev — themes</span>
    <button class="close" aria-label="close tab">×</button>
  </div>
  <div class="tab">
    <span class="load">···</span>
    <span class="favicon" />
    <span class="title">build #482 — running</span>
    <button class="close">×</button>
  </div>
  <div class="tab"><span class="audio">♪</span> ... </div>
  <div class="tab is-hidden">...</div>
  <button class="new" aria-label="new tab">+</button>
</div>
```

The `is-hidden` state is the visual treatment for a tab that's been *discarded* (renderer destroyed but still shown in the strip) — see DESIGN's tab lifecycle. Earlier drafts called this "hibernated."

## Per-tab states

| State | Style |
|---|---|
| default | mono 11px, fg-2 text, hairline right border, close button hidden |
| **hover** | `background: var(--surface-2)`, close button reveals |
| **is-active** | `background: var(--bg)`, `border-top: 2px solid var(--accent)`, fg text, close visible |
| **is-hidden** | `opacity: 0.55`, favicon becomes hollow ring (discarded renderer) |
| **is-pinned** | width collapses to 28px, title and close hidden |
| **loading** | mono `···` ticker before the favicon, in accent color |
| **audio** | unicode `♪` glyph before the favicon, in accent color |

## Tokens

```css
.tabbar {
  height: var(--chrome-tabbar);  /* 40px */
  background: var(--surface-1);
  border-bottom: 1px solid var(--border);
}
.tab {
  font: var(--text-mono-sm);
  color: var(--fg-2);
  border-right: 1px solid var(--border);
}
.tab.is-active {
  background: var(--bg);
  color: var(--fg);
  border-top: 2px solid var(--accent);
}
```

## Behavior

| Action | Result |
|---|---|
| click | activate tab |
| click ×  | close tab |
| middle-click | close tab |
| ⌘W | close active tab |
| ⌘⇧T | reopen last closed tab |
| ⌘T | new tab |
| ⌘1 … ⌘9 | switch to tab N |
| drag | reorder; off-strip = open in new window |
| ⌘⇧H | hide active tab (move to hidden-in-workspace) |

## Theme variants

These are example user/theme overrides. Themes implement them by re-styling the `.tab` selector via `theme:style`.

### `underline` — chrome stripped

```css
.tabbar.theme-underline {
  background: transparent;
  border: 0;
  border-bottom: 1px solid var(--border);
}
.theme-underline .tab.is-active {
  background: transparent;
  border-top: 0;
  box-shadow: inset 0 -2px 0 0 var(--accent);
}
```

### `keycap` — tabs as discrete keys

```css
.tabbar.theme-keycap { gap: 4px; padding: 4px 4px 0; background: transparent; }
.theme-keycap .tab {
  background: var(--surface-1);
  border: 1px solid var(--border);
  border-bottom-width: 2px;
  border-radius: var(--radius-1);
}
.theme-keycap .tab.is-active {
  background: var(--bg);
  border-top: 1px solid var(--accent);
  border-bottom-color: var(--accent-deep);
}
```

### `vertical` — tabs in the sidebar

When the user runs `mote.sidebar.default("tabs")` and a theme implements vertical tabs, the theme leaves the `tabstrip` out of `top-bar` and the tabs render in the sidebar's `[tabs]` panel. See [`sidebar.md`](./sidebar.md).

## Accessibility

- Each tab is `role="tab"`, the strip is `role="tablist"`.
- The active tab carries `aria-selected="true"`.
- Close buttons have `aria-label="close tab"`.
- Hidden (discarded) tabs add `aria-label="<title> (hidden)"`.

## Anti-patterns

- ❌ Pill-shaped tabs.
- ❌ Tabs that grow taller than 40px in the horizontal strip.
- ❌ Tab close as an always-visible × on every tab. Reveal on hover/active.
- ❌ "+" button as a primary-colored CTA. It's chrome.
