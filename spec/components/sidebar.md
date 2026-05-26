# Sidebar

## Purpose

A generic side-slot host. Holds a swappable **element panel** chosen via a thin **activity bar** of icons on the inside edge. Blends Zen browser's sidebar-first density with VS Code/Obsidian's activity-bar pattern.

A theme picks the default panel; the user can override. Any element bound to the `sidebar` slot can be a panel.

## Structure

```html
<aside class="sidebar" data-slot="left-sidebar">
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
    <!-- "assistant" is shown only when an AI plugin registers an `assist` panel; reserved, not shipped -->
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
      <span class="sidepanel-meta">4 open · 1 hidden</span>
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

Mote ships these built-in panels. Each is a `sidebar-panel`-kind element. Plugin-contributed panels register the same way.

| Panel | Activity bar icon | Default content |
|---|---|---|
| `tabs` | `layers` | vertical tab list |
| `bookmarks` | `bookmark` | grouped tree |
| `history` | `history` | reverse-chrono rows |
| `plugins` | `puzzle` | installed plugins list with on/off |
| `lua` | `braces` | live config editor |

> **`assist` is a reserved panel name, not a shipped feature.** Mote ships no built-in AI chat panel (core principle #8). A future AI plugin may register a `sidebar-panel` element named `assist` (the `sparkles` activity-bar icon and the `[· assistant]` header lockup are documented so such a plugin stays on-brand), reaching its LLM via `http:fetch` + `secret:read`. The runtime ships the activity bar without an AI panel bound.

A panel is a `sidebar-panel` element a plugin registers via `ui.register_element` (see `01_architecture.md`); the active theme places these into a sidebar slot and a user chooses the default panel:

```lua
-- in a theme's M.theme.layout
["left-sidebar"] = { "sidebar-panel:tabs", "sidebar-panel:*" },
```

```lua
-- in user config
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

- **Side:** a theme places the sidebar in `left-sidebar` or `right-sidebar`; the user can override via `mote.theme_overrides`. Default is left.
- **Activity bar visibility:** `mote.sidebar.activitybar(false)` hides the icon strip; switching panels then happens via keybinds only (pure dotfile-purist mode).
- **Default panel:** `mote.sidebar.default("bookmarks")`.

When the `tabs` panel is the chosen default, a theme typically leaves the `top-bar` `tabstrip` out of its layout (or maps `tab-row` empty) to drop the redundant top tab strip. The runtime accepts both: a theme can keep both, drop the top strip, or drop the sidebar tabs panel.

## Accessibility

- The sidebar is `<aside aria-label="sidebar">`.
- The activity bar is `<nav aria-label="sidebar panels">` with each button having an `aria-label` and `aria-pressed` matching its active state.
- The active panel name is announced via `aria-live="polite"` in the panel header.

## Anti-patterns

- ❌ Activity bar icons larger than 28px.
- ❌ Labels next to activity bar icons by default. Tooltip on hover is fine.
- ❌ Panel headers with their own background color (use `var(--surface-1)`, same as the panel body).
- ❌ Drop shadow on the sidebar — it's docked, not floating.
