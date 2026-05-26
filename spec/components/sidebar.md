# Sidebar

## Purpose

A generic side-slot host. Holds a swappable **element panel** chosen via a thin **activity bar** of icons on the inside edge. Blends Zen browser's sidebar-first density with VS Code/Obsidian's activity-bar pattern.

A theme picks the default panel; the user can override. Any element bound to the `sidebar` slot can be a panel.

## Structure

```html
<aside class="sidebar" data-slot="sidebar">
  <nav class="activitybar">
    <button class="activity-btn is-active" aria-label="tabs">
      <svg><!-- Lucide: layers --></svg>
    </button>
    <button class="activity-btn" aria-label="bookmarks">
      <svg><!-- Lucide: bookmark --></svg>
    </button>
    <button class="activity-btn" aria-label="history">
      <svg><!-- Lucide: history --></svg>
    </button>
    <button class="activity-btn" aria-label="assistant">
      <svg><!-- Lucide: sparkles --></svg>
    </button>
    <button class="activity-btn" aria-label="plugins">
      <svg><!-- Lucide: puzzle --></svg>
    </button>
    <button class="activity-btn" aria-label="config">
      <svg><!-- Lucide: braces --></svg>
    </button>
    <div class="activitybar-spacer"></div>
    <button class="activity-btn" aria-label="close sidebar">
      <svg><!-- Lucide: panel-left-close --></svg>
    </button>
  </nav>
  <div class="sidepanel">
    <header class="sidepanel-head">
      <span class="sidepanel-slot">
        <span class="br">[</span><span class="name">tabs</span><span class="br">]</span>
      </span>
      <span class="sidepanel-meta">4 open · 1 hibernated</span>
    </header>
    <div class="sidepanel-body">
      <!-- panel content (TabsPanel / BookmarksPanel / etc.) -->
    </div>
  </div>
</aside>
```

## Tokens

```css
.sidebar {
  display: grid;
  grid-template-columns: 36px 280px;   /* activity bar + panel */
  border-right: 1px solid var(--border);
  background: var(--surface-1);
}

.activitybar {
  display: flex; flex-direction: column;
  align-items: center;
  padding: 6px 0;
  gap: 2px;
  background: var(--bg);
  border-right: 1px solid var(--border);
}

.activity-btn {
  width: 28px; height: 28px;
  background: transparent;
  border: 1px solid transparent;
  border-radius: var(--radius-1);
  color: var(--fg-2);
  position: relative;
  transition: background var(--dur-micro) var(--ease-out);
}
.activity-btn:hover { background: var(--surface-2); color: var(--fg); }
.activity-btn.is-active { color: var(--accent); }
.activity-btn.is-active::before {
  content: "";
  position: absolute;
  left: -4px; top: 4px; bottom: 4px;
  width: 2px;
  background: var(--accent);
}

.sidepanel-head {
  height: 30px;
  padding: 0 12px;
  border-bottom: 1px solid var(--border);
  font: var(--text-mono-sm);
  color: var(--fg-2);
}
.sidepanel-slot .br   { color: var(--accent); }
.sidepanel-slot .name { color: var(--fg); }
```

## Panels (canonical)

Mote ships these built-in. Plugin-contributed panels register the same way.

| Panel | Activity bar icon | Default content |
|---|---|---|
| `tabs` | `layers` | vertical tab list |
| `bookmarks` | `bookmark` | grouped tree |
| `history` | `history` | reverse-chrono rows |
| `assist` | `sparkles` | AI chat thread + composer |
| `plugins` | `puzzle` | installed plugins list with on/off |
| `lua` | `braces` | live init.lua editor |

Per-panel structure and styling is documented inline in the reference implementation ([`ui_kits/browser/Sidebar.jsx`](../../ui_kits/browser/Sidebar.jsx)). A panel is just an element bound to the `sidebar` slot:

```lua
theme:bind("sidebar", {
  mote.elements.sidebar_tabs,
  mote.elements.sidebar_bookmarks,
  mote.elements.sidebar_history,
  mote.elements.sidebar_assist,
  mote.elements.sidebar_plugins,
  mote.elements.sidebar_lua,
})
mote.sidebar.default("tabs")
```

## Behavior

| Action | Result |
|---|---|
| click activity-bar icon | switch active panel |
| `⌘B` | toggle sidebar open / closed |
| `⌘⇧E` | focus the panel body (vim-style; "explorer") |
| drag right edge | resize (snaps to `--gutter-xs/sm/md/lg`) |
| click "close sidebar" button | hide sidebar (state persists per-theme) |

## Customization

- **Side:** `mote.sidebar.side("left" | "right")`. Default is left.
- **Activity bar visibility:** `mote.sidebar.activitybar(false)` hides the icon strip; switching panels then happens via keybinds only (pure dotfile-purist mode).
- **Default panel:** `mote.sidebar.default("assist")`.

When the sidebar's `tabs` panel is the chosen default, themes typically also `theme:unbind("tab_bar")` to drop the redundant top tab strip. The runtime accepts both: a theme can keep both, drop the top strip, or drop the sidebar tabs panel.

## Accessibility

- The sidebar is `<aside aria-label="sidebar">`.
- The activity bar is `<nav aria-label="sidebar panels">` with each button having an `aria-label` and `aria-pressed` matching its active state.
- The active panel name is announced via `aria-live="polite"` in the panel header.

## Example

See the running implementation in [`ui_kits/browser/index.html`](../../ui_kits/browser/index.html) — clicking any activity-bar icon switches the bound panel.

## Anti-patterns

- ❌ Activity bar icons larger than 28px.
- ❌ Labels next to activity bar icons by default. Tooltip on hover is fine.
- ❌ Panel headers with their own background color (use `var(--surface-1)`, same as the panel body).
- ❌ Drop shadow on the sidebar — it's docked, not floating.
