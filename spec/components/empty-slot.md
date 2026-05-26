# Empty slot

## Purpose

The visual fallback when a theme **declares** a slot but no element is **bound** to it. Mote's signature dot-grid texture lives here. This is the *only* situation in which the dot grid appears.

## Structure

The runtime renders an empty slot automatically when `theme:slot(name)` is called without a subsequent `theme:bind(name, ...)`, or when `theme:unbind(name)` is called explicitly.

```html
<div data-slot="sidebar" class="slot-empty">
  <div class="empty-card">
    <span class="glyph">[ ]</span>
    <span class="name">sidebar</span>
    <span class="hint">no element bound</span>
  </div>
</div>
```

## Tokens

```css
.slot-empty {
  background-image: var(--dots);
  display: flex; align-items: center; justify-content: center;
}

.empty-card {
  text-align: center;
  display: flex; flex-direction: column; gap: 4px;
}
.empty-card .glyph {
  font: var(--text-mono);
  font-size: 22px;
  color: var(--accent);
  letter-spacing: -0.04em;
}
.empty-card .name {
  font: var(--text-mono-sm);
  color: var(--fg-1);
}
.empty-card .hint {
  font: var(--text-mono-sm);
  font-size: 9px;
  color: var(--fg-3);
  text-transform: uppercase;
  letter-spacing: 0.1em;
}
```

## The dot pattern

The `--dots` token is defined in `colors_and_type.css`:

```css
--dots: radial-gradient(rgba(236, 229, 216, 0.06) 1px, transparent 1px) 0 0 / 4px 4px;
```

The vellum theme overrides this to use ink-tinted dots on cream:

```css
[data-theme="vellum"] {
  --dots: radial-gradient(rgba(20, 17, 15, 0.08) 1px, transparent 1px) 0 0 / 4px 4px;
}
```

The 4px grid matches the spacing scale. **Don't change the spacing** — the dot rhythm is part of the brand.

## States

| State | Appearance |
|---|---|
| **declared but unbound** | dot grid + empty-card with slot name and `[ ]` glyph (default) |
| **loading** (plugin about to bind) | dot grid alone, no card. Lasts up to 200ms then content replaces |
| **hidden** | not rendered at all; the surrounding layout collapses |

## When NOT to use the dot grid

This motif appears **only** for empty slots. Do not use the dot-grid texture as:

- A decorative page background in marketing
- A loading skeleton
- A divider band
- A hover state for any element

Restricting it gives it semantic weight: when a user sees dots, they know there's a slot they could fill.

## Programmatic API

In production code, the empty-slot renderer is the runtime's responsibility, not a component to instantiate manually. The runtime walks the theme's declared slots, finds any without bindings, and inserts an empty-slot element with the slot's name.

```lua
-- nothing to do; the runtime handles this. but observe:
theme:slot("custom_dock", { position = "right", width = 320 })
-- no theme:bind("custom_dock", ...) follows
-- → result: a 320px right dock filled with the dot grid and "[ ] custom_dock"
```

## Accessibility

- The empty slot's container has `aria-label="empty slot: <name>"`.
- The decorative glyph (`[ ]`) is `aria-hidden="true"`.
- A keyboard user can still tab past or into the slot's region.

## Anti-patterns

- ❌ Showing instructional text inside the empty slot ("drag an element here"). Mote doesn't onboard.
- ❌ Animating the dots.
- ❌ Tinting the dot color away from `var(--fg)` tones. Stay in the warm-ink/warm-paper grayscale.
- ❌ Putting the dot grid behind content that *is* bound.
