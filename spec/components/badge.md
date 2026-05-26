# Badge

## Purpose

A small inline label that carries status, count, or a non-actionable category. **Not actually pill-shaped** — Mote rejects pills. Badges use `--radius-1` (2px) for a slab feel.

## Variants

| Variant | Use |
|---|---|
| default | neutral, surface-1 fill |
| `success` | online, build-passing |
| `danger` | error |
| `info` | informational, link-related |
| `special` | AI surfaces |
| `accent` | "active" status |
| `count` | numeric (`v0.34`, `7 tabs`) — same as default |

Optional leading status dot.

## Structure

```html
<span class="badge success">
  <span class="dot"></span>online
</span>

<span class="badge">stable</span>

<span class="badge accent">
  <span class="dot"></span>active
</span>

<span class="badge">v0.34</span>
```

## Tokens

```css
.badge {
  display: inline-flex; align-items: center; gap: 6px;
  height: 20px; padding: 0 8px;
  font: var(--text-mono-sm);
  font-size: 10px;
  text-transform: lowercase;
  letter-spacing: 0.02em;
  border: 1px solid var(--border);
  border-radius: var(--radius-1);
  background: var(--surface-1);
  color: var(--fg-1);
}

.badge.success { color: var(--success); border-color: rgba(107, 142, 78, 0.3); }
.badge.danger  { color: var(--danger);  border-color: rgba(200, 74, 44, 0.35); }
.badge.info    { color: var(--info);    border-color: rgba(91, 124, 163, 0.35); }
.badge.special { color: var(--special); border-color: rgba(142, 111, 160, 0.35); }
.badge.accent  { color: var(--accent);  border-color: rgba(224, 164, 88, 0.4); }

.badge .dot {
  width: 6px; height: 6px;
  border-radius: 50%;
  background: currentColor;
}
```

## Behavior

Badges are **not interactive**. They never have click handlers. If you need a clickable variant, use a button styled to look like a badge.

## Accessibility

- A badge with only a dot and a single word reads fine for screen readers — no `aria-label` needed.
- For numeric badges in context ("3 new"), prefer the surrounding label to carry meaning.

## Example

See [`preview/components-badges.html`](../../preview/components-badges.html) for status / label / count groupings.

## Anti-patterns

- ❌ `border-radius: 9999px` (pills).
- ❌ Filled backgrounds in saturated colors (use the desaturated border-only approach).
- ❌ Badges with icons. Mono text with optional leading dot is the vocabulary.
- ❌ Decorative badges next to product names ("✨ NEW ✨").
