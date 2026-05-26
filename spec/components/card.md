# Card

## Purpose

A grouped, lightly-bordered container for related content. Mote's cards are deliberately quiet — hairline border, no shadow, no header band, no left-border accent stripe.

## Structure

```html
<div class="card">
  <div class="title">dusk.lua</div>
  <div class="desc">theme loaded from <code>~/.config/mote/themes/dusk.lua</code></div>
  <div class="meta">
    <span><b>217</b> lines</span>
    <span><b>4m</b> ago</span>
    <span><b>1</b> override</span>
  </div>
</div>
```

## Tokens

```css
.card {
  background: var(--surface-1);
  border: 1px solid var(--border);
  border-radius: var(--radius-2);     /* 4px */
  padding: var(--space-4);            /* 16px */
  display: flex; flex-direction: column;
  gap: var(--space-2);
}
.card .title { font: var(--text-h3); color: var(--fg); }
.card .desc  { font: var(--text-small); color: var(--fg-1); }
.card .meta {
  display: flex; gap: var(--space-3);
  font: var(--text-mono-sm);
  color: var(--fg-2);
  margin-top: var(--space-1);
}
.card .meta b { color: var(--fg-1); font-weight: 500; }
```

## Variants

| Variant | Difference |
|---|---|
| default | hairline border, surface-1 |
| **assist** | border becomes `border-left: 2px solid var(--special)`, slot indicator at top: `[· assistant]` |
| **danger** | border becomes `border-color: var(--danger)`; otherwise unchanged |
| **inert** (data summary) | no padding tweaks; just a title + key/value rows in mono |

```css
.card.assist {
  border-color: rgba(142, 111, 160, 0.35);
}
.card.assist .title::before {
  content: "";
  display: inline-block; width: 6px; height: 6px;
  border-radius: 50%;
  background: var(--special);
  margin-right: 8px;
  vertical-align: middle;
}
```

## States

Cards are **not interactive by default**. If they need to be clickable:

```css
.card.is-clickable {
  cursor: pointer;
  transition: background var(--dur-micro) var(--ease-out);
}
.card.is-clickable:hover {
  background: var(--surface-2);
}
```

Hover lift (`box-shadow: var(--shadow-1)`) is allowed but should be **rare** — borders carry the affordance.

## Accessibility

- A clickable card is wrapped in `<a>` or `<button>` — don't put `onClick` on a `<div>`.
- Decorative dots (`assist`) are `aria-hidden="true"`.

## Anti-patterns

- ❌ Headers with a different background color than the body.
- ❌ Left-border accent stripes as decoration.
- ❌ Drop shadow by default.
- ❌ Border-radius larger than `--radius-2`.
- ❌ Cards inside cards. Use a divider hairline instead.
