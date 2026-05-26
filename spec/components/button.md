# Button

## Purpose

A clickable action affordance. Used wherever the user triggers a discrete command. Mote's buttons follow the **keycap construction** — every variant has a 1px hairline + 2px bottom border, which collapses on press.

## Variants

| Variant | Use |
|---|---|
| `primary` | One per surface — the recommended action. Amber background. |
| `secondary` | Side-by-side alternatives, default actions. Surface-1 background. |
| `ghost` | Tertiary actions. Transparent background, **hairline border always visible** (this is a recent correction — a borderless ghost reads as text, not a button). |
| `danger` | Destructive actions. Transparent with an ember-tinted border. |
| `icon` | Icon-only square button. Same construction, 32×32. |

## Structure

```html
<button class="btn btn-primary">apply theme</button>
<button class="btn btn-secondary">cancel</button>
<button class="btn btn-ghost">skip</button>
<button class="btn btn-danger">remove</button>
<button class="btn btn-icon btn-secondary" aria-label="settings">
  <svg><!-- Lucide --></svg>
</button>
```

## Tokens

```css
.btn {
  height: 32px;
  padding: 0 14px;
  font: var(--text-small);
  font-weight: 500;
  letter-spacing: 0.01em;
  border: 1px solid var(--border);
  border-bottom-width: 2px;             /* keycap */
  border-radius: var(--radius-1);
  background: var(--surface-1);
  color: var(--fg);
  transition:
    background var(--dur-micro) var(--ease-out),
    transform var(--dur-micro) var(--ease-out),
    border-bottom-width var(--dur-micro) var(--ease-out);
}

.btn-primary {
  background: var(--accent);
  color: var(--accent-on);
  border-color: var(--accent-deep);
}

.btn-secondary {
  background: var(--surface-1);
  color: var(--fg);
  border-color: var(--border-strong);
}

.btn-ghost {
  background: transparent;
  color: var(--fg-1);
  border-color: var(--border-subtle);   /* always visible */
}

.btn-danger {
  background: transparent;
  color: var(--danger);
  border-color: rgba(200, 74, 44, 0.4);
}

.btn-icon { width: 32px; padding: 0; justify-content: center; }
```

## States

| State | Style |
|---|---|
| **hover** (primary) | `background: var(--accent-soft)` |
| **hover** (secondary) | `background: var(--surface-2)` |
| **hover** (ghost) | `background: var(--surface-1); color: var(--fg); border-color: var(--border)` |
| **hover** (danger) | `background: rgba(200,74,44,0.08); border-color: rgba(200,74,44,0.6)` |
| **press** (any) | `border-bottom-width: 1px; transform: translateY(1px)` |
| **focus-visible** | `outline: 2px solid var(--focus); outline-offset: 2px` |
| **disabled** | `opacity: 0.4; cursor: not-allowed; pointer-events: none` |

## Behavior

- **Click:** triggers the bound action.
- **Enter / Space:** triggers when focused (default `<button>` behavior).
- **Long press:** no special behavior.
- **No double-click handling.** Mote's UI is single-click everywhere.

## Accessibility

- Always rendered as `<button>`, never `<div role="button">`.
- Icon-only buttons require `aria-label`.
- Disabled buttons must set `disabled` attribute, not just opacity.
- Press state must not be the only press indicator if motion is reduced — pair with `:active { background: var(--surface-3) }`.

## Example

See [`preview/components-buttons.html`](../../preview/components-buttons.html) for the canonical reference and all states rendered.

## Anti-patterns

- ❌ Pills (`border-radius: 9999px`).
- ❌ Gradients of any kind.
- ❌ Drop shadows.
- ❌ Trailing kbd hints inside the button (e.g. `apply theme ⏎`) — distracting and was removed during review. Show kbd hints separately if needed.
- ❌ Borderless ghost buttons.
- ❌ Scale on hover.
