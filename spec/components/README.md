# Components

Implementation contracts for every component in Mote's UI. Each file follows the same shape:

- **Purpose** — what the component does
- **Structure** — HTML/JSX skeleton
- **Tokens** — which design tokens are used and where
- **States** — default, hover, focus, press, disabled, plus component-specific states
- **Behavior** — keyboard, mouse, programmatic
- **Accessibility** — ARIA, focus management, reduced motion
- **Example** — a complete working snippet

## Index

| File | Component | Notes |
|---|---|---|
| [`button.md`](./button.md) | `<button>` | Keycap construction. primary / secondary / ghost / danger / icon |
| [`omnibox.md`](./omnibox.md) | Omnibox | Multi-modal: `[url]` / `[cmd]` / `[ask]` / `[find]` |
| [`tabs.md`](./tabs.md) | Tab strip | Horizontal default; theme variants: underline, keycap, vertical |
| [`status-line.md`](./status-line.md) | Status line | Bottom strip; segments switch contextually |
| [`palette.md`](./palette.md) | Command palette | Centered overlay, 640px |
| [`sidebar.md`](./sidebar.md) | Sidebar | Activity bar + swappable panel |
| [`field.md`](./field.md) | Form fields | Inputs, selects, toggles |
| [`card.md`](./card.md) | Card | Hairline-bordered container |
| [`badge.md`](./badge.md) | Badge | Status pills (not actual pills) |
| [`kbd.md`](./kbd.md) | `<kbd>` | Mechanical key glyph |
| [`empty-slot.md`](./empty-slot.md) | Empty slot | Dot-grid motif for unbound slots |

## Reference implementation

The canonical CSS for every component lives in `ui_kits/browser/index.html`. The JSX components in `ui_kits/browser/*.jsx` show composition. **When in doubt, lift from there** — those files are kept in sync with this spec.
