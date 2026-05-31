# Omnibox suggestions

## Purpose

The completion popup for the url-mode omnibox. Renders `{url, title, source}`
rows pushed via `applyOp('urlbar_suggestions', [...])` — the shell-side
`urlbar_query` op (commit `1776cfa`) supplies them by invoking
`Runtime::invoke_capability("ui:urlbar_provider", "query", ...)`, which merges
history and bookmark contributions. The popup is keyboard-driven and appears
anchored immediately below `.omni`, sharing its bottom edge to form a unified
mechanical surface.

## Structure

```html
<!-- input (inside .omni) gains ARIA combobox wiring -->
<input
  id="omnibox-input"
  role="combobox"
  aria-autocomplete="list"
  aria-controls="omnibox-completions"
  aria-expanded="false"
/>

<!-- dropdown: sibling of <form class="omnibar-row">, inside [data-slot="top-bar"] -->
<div
  id="omnibox-completions"
  class="omni-completions"
  role="listbox"
  aria-label="url suggestions"
>
  <!-- rows appended by host.js via DOM-build (never innerHTML) -->
  <div
    class="omni-completion-row"
    role="option"
    aria-selected="false"
    id="omnibox-completion-row-0"
    data-url="https://example.com/page"
  >
    <span class="url">https://example.com/<b>page</b></span>
    <span class="title">example page title</span>
    <span class="source">
      <span class="br">[</span><span class="name">history</span><span class="br">]</span>
    </span>
  </div>
</div>
```

Row layout: `url (primary, 1fr) | title (dim, auto) | [source] (dim, right-aligned,
fixed-width)`. Three columns, 16px gap, 32px height. The `<b>` in `.url` wraps the
matched substring (built via `createElement + textContent`, never `innerHTML`).

## Tokens

| Property | Token | Notes |
|---|---|---|
| `background` | `var(--surface-1)` | same as palette |
| `border` | `1px solid var(--border)` | top: 0 (shared edge) |
| `border-radius` | `0 0 var(--radius-3) var(--radius-3)` | bottom corners only |
| `box-shadow` | `var(--shadow-2)` | floating surface rule |
| `font` | `var(--text-mono)` | mono surface throughout |
| `max-height` | `calc(10 * 32px)` | matches 10-result merge cap |
| row `.url` color | `var(--fg)` | primary |
| matched `<b>` color | `var(--accent)` | same as palette match highlight |
| row `.title` color | `var(--fg-2)` | dim |
| row `.source` color | `var(--fg-2)` | metadata, not mode indicator |
| row `.is-sel` bg | `var(--surface-2)` | matches palette row selection |
| `.omni.has-completions` | removes bottom border-radius | unification rule |

## States

| State | Description |
|---|---|
| closed | `display: none`; `.omni-completions` lacks `.is-open`; input `aria-expanded="false"` |
| open | `.is-open` present; `display: block`; 1–10 rows rendered; input `aria-expanded="true"` |
| selected | one row has `.is-sel`; that row has `aria-selected="true"`; input carries `aria-activedescendant` pointing to row id |

## Behavior

| Key | Action |
|---|---|
| typing | shell op `urlbar_query` fires; shell pushes `urlbar_suggestions` back via `applyOp` |
| `↓` | move selection down (wraps to first); `preventDefault` |
| `↑` | move selection up (wraps to last); `preventDefault` |
| `Enter` (row selected) | invoke `navigate {url}`, close dropdown, blur; `preventDefault` |
| `Enter` (no selection) | fall through to form submit (existing behavior) |
| `Escape` (row selected) | clear selection only; leave dropdown open; `preventDefault` |
| `Escape` (no selection) | fall through to existing omnibox blur behavior |
| `Tab` | clear selection; fall through (no `preventDefault`) |
| click row | invoke `navigate {url}`, close dropdown (mousedown fires before blur) |
| input blur | close dropdown after 150ms (allows row click to register first) |
| empty input | close dropdown locally, no round-trip to shell |

v0.1 cap: the shell's history merge policy returns at most 10 results;
`max-height: calc(10 * 32px)` is the visual enforcement.

## Behavior — data source

Suggestions are pushed by the shell via `applyOp('urlbar_suggestions', records)`
where `records` is an array of `{url: string, title: string, source: string}`.
Source values today: `"history"` and `"bookmark"`. The shell side is the
`urlbar_query` op (commit `1776cfa`) which calls
`Runtime::invoke_capability("ui:urlbar_provider", "query", ...)` and merges
contributions from registered providers.

Future contributors slot in by subscribing to `urlbar:suggest` (see ADR-0010)
— the chrome rendering layer is agnostic to source count; it renders whatever
the shell pushes.

## Accessibility

- Container: `role="listbox"`, `aria-label="url suggestions"`.
- Each row: `role="option"`, `aria-selected` toggled by host.js selection logic.
  Each row carries a stable `id="omnibox-completion-row-N"` for
  `aria-activedescendant`.
- Input: `role="combobox"`, `aria-autocomplete="list"`,
  `aria-controls="omnibox-completions"`, `aria-expanded` toggled by host.js,
  `aria-activedescendant` set to the selected row id (removed when no row is
  selected).

## Anti-patterns

- ❌ `innerHTML` with payload content — build all DOM nodes with `createElement`
  and `textContent`; the `<b>` highlight is no exception.
- ❌ Icons, emoji, or favicons in rows — mono surface, text only.
- ❌ Hover background on non-selected rows — keyboard-first; mouse is quiet.
- ❌ Slide or fade animations on open/close — instant per motion rule.
- ❌ `border-radius` larger than `var(--radius-3)` on any new surface.
- ❌ Rounded-pill source tags — the `[source]` lockup uses the standard bracket
  pattern; no pills.
- ❌ Shadow on individual rows — shadow on the container only.
- ❌ Overriding the max-width of the dropdown (must match the omnibox width).
- ❌ New tokens for one-off values — all values above map to existing tokens.
