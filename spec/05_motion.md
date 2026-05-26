# 05 · Motion

Mote does almost nothing animated. The ethos is **immediate**.

## Durations

| Token | Value | Use |
|---|---|---|
| `--dur-micro` | 80ms | button press, tab close, hover-fade |
| `--dur-base` | 120ms | default — color/background transitions |
| `--dur-entrance` | 200ms | palette open, dropdown reveal, sidebar slide |

There is no `--dur-slow`. If something needs more than 200ms, it's the wrong animation.

## Easing

| Token | Value | Use |
|---|---|---|
| `--ease-out` | `cubic-bezier(0.2, 0, 0, 1)` | **default for everything** |
| `--ease-in` | `cubic-bezier(0.6, 0, 1, 0.4)` | exits (rare) |
| `--ease-in-out` | `cubic-bezier(0.4, 0, 0.2, 1)` | bidirectional state machines |

**No spring physics.** No bounce. No overshoot. Mote rejects "playful" motion — it's a tool, not a toy.

## When to animate, when not to

| Situation | Animate? | How |
|---|---|---|
| Button hover (color shift) | Yes | `background var(--dur-micro) var(--ease-out)` |
| Button press | Yes | bottom-border collapse + 1px translate, `var(--dur-micro)` |
| Tab switch | **No** | Instant. The active tab indicator just snaps. |
| Tab close | Yes (subtle) | Element collapses width over `var(--dur-micro)` then unmounts |
| Palette open | Yes | Fade-in over `var(--dur-entrance)`. **No slide-down.** |
| Palette close | **No** | Instant unmount on `Escape` |
| Sidebar toggle | Yes | Width transitions over `var(--dur-entrance)` |
| Status-line segment update | **No** | Text swaps instantly |
| Page load | N/A | Use the inline status-line progress bar, not chrome animation |
| Theme switch | **No** | Instant CSS-var swap. The user wants to see it change, not wait. |
| Cursor blink | Yes | 1.2s `steps(2, end)` infinite — the only periodic animation in the system |

## Prohibited animations

- **Loading spinners.** Use an inline indeterminate top bar or a `·` → `··` → `···` mono ticker.
- **Spring/bounce on entrance.** No `cubic-bezier` with overshoot. No `framer-motion` springs.
- **Stagger animations on lists.** Items appear all at once.
- **Slide transitions for content swap.** Use a cross-fade if anything; usually instant is correct.
- **Hover scale.** No `transform: scale(1.05)` on hover. Background shift only.
- **Glow pulse on "AI" elements.** This is a cliché Mote rejects. Special/`plum` surfaces use the accent statically.

## The one acceptable animated indicator

A "working" state — e.g. a plugin-provided indicator that's busy — may pulse the `plum` dot in a status indicator or panel header (Mote ships no AI runtime, so any such indicator is plugin-contributed):

```css
.dot.special {
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  0%, 100% { opacity: 1; }
  50%      { opacity: 0.4; }
}
```

This is the **only** ambient animation in the chrome. Everything else is event-driven and brief.

## Reduced motion

Respect the user's preference:

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0ms !important;
    transition-duration: 0ms !important;
  }
}
```

This must be set globally — don't opt individual components in or out. Cursor blink and progress bars are the only exceptions, and they can blink/fill instantly.

## Next

[`06_iconography.md`](./06_iconography.md).
