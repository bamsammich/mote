# Lucide icon usage — authoritative mapping

Per ADR-0013, every chrome icon routes through `theme.icons.<action>`. This
file is the canonical record of default mappings. Changes here are reviewed
under the same gate as ADRs.

The chrome bundles `crates/mote-ui/chrome/assets/lucide-sprite.svg`; every
icon listed below must appear as a `<symbol>` in that file.

## Action → lucide name mapping

| Action name | Lucide name | Sprite ID | Context |
|---|---|---|---|
| `chrome.close` | `x` | `icon-x` | Window close keycap (top-right header) |
| `chrome.bookmark` | `bookmark` | `icon-bookmark` | Bookmark current page (top-right header) |
| `chrome.new_tab` | `plus` | `icon-plus` | New tab keycap (top-right header) |
| `chrome.back` | `arrow-left` | `icon-arrow-left` | Navigate back (disabled in P1) |
| `chrome.forward` | `arrow-right` | `icon-arrow-right` | Navigate forward (disabled in P1) |
| `chrome.reload` | `rotate-cw` | `icon-rotate-cw` | Reload page (disabled in P1) |
| `tab.close` | `x` | `icon-x` | Per-tab close button (hover-only) |
| `tab.favicon_placeholder` | *(dot-grid SVG)* | — | Pre-favicon slot; inline CSS only |
| `rail.tabs` | `layers` | `icon-layers` | Rail: tabs panel icon |
| `rail.bookmarks` | `bookmark` | `icon-bookmark` | Rail: bookmarks panel icon |
| `rail.history` | `clock` | `icon-clock` | Rail: history panel icon |
| `rail.settings` | `settings` | `icon-settings` | Rail: settings cog (P6, forward-declared) |
| `rail.plugin_unbound` | `circle-plus` | `icon-circle-plus` | Rail: unbound plugin placeholder |
| `collapse.sidebar` | `panel-left-close` | `icon-panel-left-close` | Collapse sidebar button (rail bottom) |
| `statusline.security_https` | `lock` | `icon-lock` | Status line: https indicator (P4) |
| `statusline.security_http` | `triangle-alert` | `icon-triangle-alert` | Status line: insecure indicator (P4) |

## Notes

- v0.1 accepts ONLY the `lucide:` pack. Unknown pack names or unknown lucide
  names are rejected with a clear error at `theme:set_icon` registration time.
- The `tab.favicon_placeholder` action is special: the dot-grid uses CSS only;
  `set_icon` for this action is ignored in v0.1 (reserved for future icon-pack
  support where a real glyph could substitute).
- Adding a new action requires: (1) adding the `<symbol>` to the sprite, (2)
  adding a row to this table, (3) adding the default to the icon registry in
  `crates/mote-ui/src/icon_registry.rs`.
