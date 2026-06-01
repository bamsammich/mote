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

## Panel header — new-tab button

The tabs panel header contains a `+` button (`.new-tab-btn`) as its rightmost
child. It is always visible regardless of which activity-bar panel is active
(v0.1 design decision; per-panel hiding is a polish-phase task).

```html
<button
  type="button"
  class="new-tab-btn"
  data-action="new-tab"
  aria-label="new tab"
  title="new tab (⌘T)"
>
  <svg ...><!-- Lucide plus: two intersecting paths --></svg>
</button>
```

- `data-action="new-tab"` — picked up by `wireNewTab()` in `host.js`.
- `type="button"` — guards against accidental form submission.
- `title` — surfaces the `⌘T` keybind in the tooltip.
- Keycap construction (`border-bottom-width: 2px`; collapses to `1px` + `translateY(1px)` on `:active`) matches mote-design rule 4.

### `⌘T` / `Ctrl+T` shortcut

`wireNewTabShortcut()` in `host.js` binds a document-level `keydown` listener.
`ev.metaKey || ev.ctrlKey` + `ev.key === "t"` invokes `new_tab` and calls
`ev.preventDefault()`. Wired from `boot()`.

## Behavior

| Action | Result |
|---|---|
| click activity-bar icon | switch active panel |
| click `+` button in panel header | open new tab (`new_tab` op) |
| `⌘T` / `Ctrl+T` | open new tab (chrome-side keydown listener) |
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

## Workspace strip

### Purpose

A persistent, always-visible workspace context indicator at the very top of the
left sidebar, spanning its full width. Shows the active workspace's display name
as a brand lockup (`[name]`) and a `›` chevron. Clicking the strip toggles a
popover dropdown that lists all workspaces; clicking a row switches to it.

The strip is the primary affordance for workspace switching in the chrome — it
is always visible regardless of which sidebar panel is active.

### Structure

```html
<aside data-slot="left-sidebar" class="sidebar" aria-label="sidebar">
  <div
    class="workspace-strip"
    role="button"
    aria-haspopup="listbox"
    aria-expanded="false"
    tabindex="0"
  >
    <span class="label" aria-hidden="true">workspace</span>
    <span class="sep" aria-hidden="true">·</span>
    <span class="name">default</span>
    <span class="spacer"></span>
    <span class="chevron" aria-hidden="true">›</span>
  </div>
  <div class="sidebar-body">
    <!-- existing <nav class="activitybar"> and <div class="sidepanel"> unchanged -->
  </div>
  <div
    id="workspace-popover"
    class="workspace-popover"
    role="listbox"
    aria-label="workspaces"
    hidden
  >
    <!-- rows appended by host.js via DOM-build (never innerHTML) -->
  </div>
</aside>
```

The `<nav class="activitybar">` and `<div class="sidepanel">` move into
`.sidebar-body` UNCHANGED — just wrapped. The `.sidebar` element changes from a
2-column grid to a flex column so the strip can sit above the grid.

### Tokens

```css
/* .sidebar changed from grid to flex column; grid moved to .sidebar-body */
.sidebar {
  display: flex;
  flex-direction: column;
  position: relative;                    /* popover anchor */
  border-right: 1px solid var(--border);
  background: var(--surface-1);
}
.sidebar-body {
  display: grid;
  grid-template-columns: 36px 280px;     /* activitybar + panel — unchanged */
  flex: 1;
  min-height: 0;
}

.workspace-strip {
  height: 30px;                          /* matches .sidepanel-head rhythm */
  padding: 0 12px;
  border-bottom: 1px solid var(--border);
  background: var(--surface-1);
  font: var(--text-mono-sm);
  color: var(--fg);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: space-between;
  user-select: none;
}
.workspace-strip:hover { background: var(--surface-2); }
.workspace-strip:focus-visible {
  outline: 1px solid var(--accent);
  outline-offset: -1px;
}
.workspace-strip .lockup .br   { color: var(--accent); }
.workspace-strip .lockup .name { color: var(--fg); }
.workspace-strip .chevron       { color: var(--fg-2); }

/* Floating dropdown below the strip */
.workspace-popover {
  position: absolute;
  top: 30px;
  left: 0;
  right: 0;
  z-index: 700;
  background: var(--surface-1);
  border: 1px solid var(--border);
  border-top: 0;
  box-shadow: var(--shadow-2);
  font: var(--text-mono);
}
.workspace-popover[hidden] { display: none; }
.workspace-popover .row {
  display: grid;
  grid-template-columns: 20px 1fr;
  gap: 8px;
  align-items: center;
  padding: 6px 12px;
  color: var(--fg);
  cursor: pointer;
}
.workspace-popover .row:hover           { background: var(--surface-2); }
.workspace-popover .row.is-current .check { color: var(--accent); }
.workspace-popover .row .check          { color: transparent; }
```

### States

| Element | State | Effect |
|---|---|---|
| `.workspace-strip` | default | `background: var(--surface-1)`, lockup in accent brackets |
| `.workspace-strip` | hover | `background: var(--surface-2)` |
| `.workspace-strip` | focus-visible | `outline: 1px solid var(--accent)`, offset -1px |
| `.workspace-strip` | popover-open | `aria-expanded="true"` |
| `.workspace-popover .row` | default | transparent background |
| `.workspace-popover .row` | hover | `background: var(--surface-2)` |
| `.workspace-popover .row` | is-current | `.check` in `var(--accent)` |

### Behavior

- Click `.workspace-strip` → toggle popover (open ↔ closed).
- Click a popover row → `mote.invoke("set_active_workspace", { id })` +
  close popover. Strip text updates when the next `workspace_list` push arrives.
- Click outside the strip and popover → close popover.
- `Esc` while popover is open → close popover + return focus to strip.
- `Enter` / `Space` on the strip → toggle popover (keyboard accessibility).

### Data flow

The shell pushes workspace state via:
```
applyOp("workspace_list", { rows: [{id, name, active}, …] })
```

Push happens:
1. On chrome-ready (first paint), so the strip is populated on initial render.
2. After every `switch_workspace` call, so the strip reflects the new active workspace.

`host.js` processes the payload:
- Sets `.workspace-strip .lockup .name` to the active row's `name`.
- Clears and rebuilds the popover rows via `createElement + textContent +
  appendChild` (NEVER innerHTML on payload content, ADR-0005).
- Adds `.is-current` + "✓" to the active row.

### Accessibility

- `.workspace-strip`: `role="button"`, `aria-haspopup="listbox"`,
  `aria-expanded` toggled, `tabindex="0"`.
- `#workspace-popover`: `role="listbox"`, `aria-label="workspaces"`, `hidden`
  when closed.
- Each popover row: `role="option"`, `data-id` for the workspace id.

### Anti-patterns

- ❌ `innerHTML` on payload content — `createElement + textContent` only.
- ❌ Shadow on `.workspace-strip` (it is docked, not floating; shadow is only on
  the popover).
- ❌ Optimistic strip-name update on row click — wait for `workspace_list` push.
- ❌ Labels or icons inside the lockup beyond `[name]`.

---

## Panel rows (bookmarks / history)

Bookmarks and history panels render their data as a mono-row vertical list.
The shell builds the DOM; plugin authors return only data.

History uses a **chronological visit log** model: every navigation produces a
separate event entry, so the same URL appears multiple times (once per visit) in
the history panel. Titles are stored at the URL level — when `update_title` is
called after page load, the new title propagates to all historical visit rows for
that URL via the join in `query_history(sort=recent)`. Bookmarks use a distinct
model (one entry per URL, no event log) and do not carry a `time_ms` field.

### Row structure

Title is the **primary** identifier (matches every shipping browser's bookmarks /
history panel); URL is the dim **secondary** context. When a row has no title
(e.g., a bookmark added before title-capture lands, or a visit whose page never
resolved a title), the URL becomes the primary text and the secondary cell is
left empty so column alignment stays consistent across rows.

History rows also carry a `.row-time` span with a human-readable relative
timestamp (e.g., "2 minutes ago"). Bookmark rows do not carry `time_ms` and
therefore never render a `.row-time` span.

```html
<!-- bookmark row (has title — both cells populated) -->
<button class="sidepanel-row" data-url="https://example.com">
  <span class="row-title">Example Page</span>
  <span class="row-url">https://example.com</span>
  <button class="row-remove" aria-label="remove bookmark">×</button>
</button>

<!-- bookmark row (no title — URL fills primary, secondary cell empty) -->
<button class="sidepanel-row" data-url="https://example.com">
  <span class="row-title">https://example.com</span>
  <span class="row-url"></span>
  <button class="row-remove" aria-label="remove bookmark">×</button>
</button>

<!-- history row (no remove control; time_ms present → .row-time rendered) -->
<button class="sidepanel-row" data-url="https://example.com">
  <span class="row-title">Example Page</span>
  <span class="row-url">https://example.com</span>
  <span class="row-time">2 minutes ago</span>
</button>

<!-- footer shown only when truncated == true -->
<div class="sidepanel-footer">showing 200 most recent</div>
```

> **Favicons are deliberately deferred to v0.2.** Real browsers show site icons next to bookmark/history rows, but doing favicons properly means per-URL fetch on add, on-disk cache with eviction, fallback rendering, and a security review for cross-origin fetches from the chrome origin. v0.1 ships title-primary text rows; favicons are a separate body of work.

### Token table

| Class | Property | Token | Notes |
|---|---|---|---|
| `.sidepanel-list` | `font` | `var(--text-mono)` | mono surface throughout |
| `.sidepanel-list` | `padding` | `4px 0` | tight vertical rhythm |
| `.sidepanel-row` | `grid-template-columns` | `1fr auto auto` | title \| dim url \| optional control |
| `.sidepanel-row` | `gap` | `12px` | column spacing |
| `.sidepanel-row` | `padding` | `6px 12px` | row inset |
| `.sidepanel-row` | `background` | `transparent` (default) / `var(--surface-2)` (hover) | |
| `.row-title` | `color` | `var(--fg)` | primary (title or URL-as-fallback) |
| `.row-title` | `text-overflow` | `ellipsis` | overflow handling |
| `.row-url` | `color` | `var(--fg-2)` | dim secondary context |
| `.row-url` | `display` (default) | `none` | hidden when scanning; reveal on hover/focus |
| `.row-url` | `display` (row hover / `:focus-within`) | `inline` | confirms the URL before click |
| `.row-url` | `max-width` | `240px` | cap so long URLs don't squeeze the title when revealed |
| `.row-time` | `color` | `var(--fg-2)` | dim relative timestamp (history only) |
| `.row-time` | `font` | `var(--text-mono-sm)` | smaller than the title |
| `.row-time` | `white-space` | `nowrap` | never wraps |
| `.row-time` | `margin-left` | `4px` | small gap from `.row-url` |
| `.row-remove` | `color` (default) | `var(--fg-3)` | very dim |
| `.row-remove` | `color` (row hover) | `var(--fg-2)` | surfaces on row hover |
| `.row-remove` | `color` (button hover) | `var(--accent)` | destructive signal |
| `.row-remove` | `border` (button hover) | `1px solid var(--border)` | |
| `.row-remove` | `background` (button hover) | `var(--surface-1)` | |
| `.row-remove` | `border-radius` | `var(--radius-1)` | sharp |
| `.sidepanel-footer` | `font` | `var(--text-mono-sm)` | smaller than rows |
| `.sidepanel-footer` | `color` | `var(--fg-2)` | dim |
| `.sidepanel-footer` | `border-top` | `1px solid var(--border)` | hairline separator |

### States

| Element | State | Effect |
|---|---|---|
| `.sidepanel-row` | hover | `background: var(--surface-2)` |
| `.row-remove` | row-hover | color `var(--fg-2)` |
| `.row-remove` | button-hover | `color: var(--accent)` + border + background |

### Behavior

- Click a row → `mote.invoke("navigate", {url})`.
- Click `×` (bookmarks only) → `event.stopPropagation()` then `mote.invoke("bookmark_remove", {url})`. The shell re-pushes `bookmark_list` after the removal.
- History rows have no remove control.
- History panel shows a `.sidepanel-footer` footer only when `truncated == true` in the payload.
- History shows each visit as a separate row (chronological log); the same URL appears multiple times at different timestamps. The title is URL-level: `update_title` propagates to all historical rows for that URL.
- Bookmark rows do not carry `time_ms`; `.row-time` is never rendered for bookmark rows.

### Behavior — data source

Data is pushed by the shell via:
- `applyOp("bookmark_list", { rows: [{url, title}, ...], count: N })`
- `applyOp("history_list", { rows: [{url, title, time_ms}, ...], count: N, truncated: bool })`

History rows include `time_ms` (Unix epoch milliseconds, wall-clock stamped by
the shell at navigate time). Bookmark rows do not include `time_ms`.

The shell calls the relevant capability when:
1. The panel becomes active (`set_active_panel` op).
2. After a bookmark mutation (`bookmark_remove` op → re-push).

Plugin authors return data only via the `ui:bookmarks_provider` and
`ui:history_provider` capabilities. They never touch CSS or HTML — this is the
key insulation property (same data-only discipline as `bookmarks_contribution.rs`).

### Anti-patterns

- ❌ `innerHTML` with payload content — `createElement` + `textContent` only.
- ❌ Icons or emoji in rows — mono text surface.
- ❌ Keycap on rows — rows are list items, not press-buttons (matches `.palette-row` precedent in `palette.md`).
- ❌ Animation on panel switch — instant, no transitions.
- ❌ Shadows on rows — shadow only on floating surfaces.
- ❌ `border-radius` larger than `var(--radius-1)` on `.row-remove`.

## Anti-patterns

- ❌ Activity bar icons larger than 28px.
- ❌ Labels next to activity bar icons by default. Tooltip on hover is fine.
- ❌ Panel headers with their own background color (use `var(--surface-1)`, same as the panel body).
- ❌ Drop shadow on the sidebar — it's docked, not floating.
