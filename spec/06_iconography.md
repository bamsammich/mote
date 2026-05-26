# 06 · Iconography

## System

Mote uses **[Lucide](https://lucide.dev)** for all chrome and content icons. The choice is deliberate: 1.5px stroke, 24×24 grid, geometric, terminal-friendly. Matches Mote's aesthetic exactly.

**Load via CDN at runtime:**

```html
<script src="https://unpkg.com/lucide@latest/dist/umd/lucide.min.js"></script>
<script>lucide.createIcons();</script>
```

**Or as individual SVG strings** when bundling production code (use `lucide-static` for raw SVG, or `lucide-react` for React).

## Rules

| Rule | Why |
|---|---|
| **Stroke icons only.** Never filled. | Consistency with the linework aesthetic. |
| **Stroke width: 1.5px.** | Lucide's default is 2; override. |
| **Color: `currentColor`.** | Icons inherit from the surrounding text. Never hardcode. |
| **Size: 16px in chrome, 20px in dialogs, 14px in status line.** | Three sizes only. Don't invent new ones. |
| **Pair with labels in primary surfaces** (omnibox row, tab strip). Icon-only only where the meaning is conventional (close `×`, plus `+`). | Discoverability. |
| **Never use emoji as icons.** | Voice rule. |
| **Use Lucide where one exists** before reaching for a unicode glyph. | Consistency. |

## Canonical mapping

| Concept | Lucide name | Used in |
|---|---|---|
| New tab / plus | `plus` | tab strip |
| Close | `x` | tab close, dialog dismiss |
| Back / forward | `arrow-left`, `arrow-right` | omnibox row |
| Reload | `rotate-cw` | omnibox row |
| Bookmark | `bookmark` | omnibox right side |
| Lock (secure) | `lock` | omnibox left edge (HTTPS) |
| Globe (insecure) | `globe` | omnibox left edge (HTTP) |
| Search | `search` | omnibox empty state |
| AI assistant | `sparkles` | activity bar |
| Command palette | `command` | palette trigger |
| Sidebar toggle | `panel-left` / `panel-left-close` | omnibox right side |
| Settings / config | `sliders-horizontal` | settings, theme switcher |
| Lua / config code | `braces` | lua panel |
| Plugin | `puzzle` | plugin manager |
| Hibernated tab | `moon` | hibernated state |
| Tabs (activity bar) | `layers` | sidebar tabs panel |
| Bookmarks (activity bar) | `bookmark` | sidebar bookmarks panel |
| History | `history` | sidebar history panel |
| Active tab | `dot` | active state |
| Split view | `columns-2` | window split |
| Vim mode | `terminal` | mode indicator |
| User | `circle-user` | identity slot |
| Folder | `folder` | bookmarks tree |
| File | `file` | file picker |
| External link | `arrow-up-right` | inline external links |
| Check / confirm | `check` | confirmations |
| Build passing | `circle-check` | status line |
| Build failing | `circle-x` | status line |

When you need an icon **not** on this list, add it to the table above in the same commit.

## Unicode glyphs (semantic, not iconographic)

These are used directly inline, in mono type. **Do not substitute them with Lucide icons.**

| Glyph | Use |
|---|---|
| `⌘ ⌥ ⇧ ⌃ ⏎ ⌫ ⎋ ⇥` | key bindings (always inside `<kbd>`) |
| `■ □ ◐ ◯ ●` | status indicators (in mono surfaces) |
| `›` | breadcrumb separator, terminal prompt mark |
| `·` | inline metadata separator (`v0.34 · stable · 4d ago`) |
| `›_` | the assistant query / command palette prompt |
| `↑ →` | inline arrows (status line link hover) |
| `─ ╌` | ASCII separators (dev-tooling contexts only) |

The brand mark `[·]` is built from this same vocabulary: mono brackets in amber + a center dot.

## Logos & marks

- `assets/wordmark.svg` — the full `[mote]` wordmark
- `assets/mark.svg` — the `[·]` mark, suitable for favicon

Both use `currentColor` for the wordmark text. The brackets and dot are hard-coded `#E0A458` (the amber default) because the mark must stay recognizable across themes. Themes that recolor the brackets do it as a deliberate override.

## Next

[`07_themes.md`](./07_themes.md).
