---
name: mote-design
description: Design system + UI implementation guide for Mote — a programmable, AI-native browser composed of slots, elements, and themes. Use this skill when implementing or modifying any frontend surface in Mote (chrome, components, theme files), or when generating visual artifacts (mocks, prototypes, slides) that need to look like Mote.
user-invocable: true
---

# Mote design system

You're working on **Mote** — a programmable, AI-native browser whose UI is composed of **slots** (regions a theme declares), **elements** (content a plugin contributes), and **themes** (Lua files that wire it all together and apply styling).

This skill is your reference for the visual language. Read it linearly the first time; jump to the relevant file thereafter.

---

## Hard rules — never break these

1. **Use design tokens, not raw values.** Write `var(--accent)`, never `#E0A458`. Write `var(--space-4)`, never `16px` for spacing-related dimensions. Tokens are in [`colors_and_type.css`](colors_and_type.css). If you need a value that isn't tokenized, add it to that file first.

2. **Both themes are first-class.** `[data-theme="dusk"]` (warm-ink dark, default) and `[data-theme="vellum"]` (warm-paper light). Test every component in both.

3. **The `[mote]` lockup is brand DNA.** Mono brackets in `var(--accent)` + sans contents in `var(--fg)`. Reuse this pattern for any "slot indicator" in chrome — omnibox mode (`[url]`/`[cmd]`/`[ask]`/`[find]`), sidebar panel header (`[tabs]`/`[assistant]`), section headers in dev surfaces.

4. **Keycap construction for every interactive element.** Buttons, chips, `<kbd>`: `border: 1px solid var(--border); border-bottom-width: 2px;`. On press: bottom collapses to 1px, transform `translateY(1px)`. This is what makes Mote feel mechanical.

5. **Borders carry the visual weight.** A 1px `var(--border)` hairline separates almost everything. **Shadows only on floating surfaces** — palette, completion popup, modal. Never on inline cards or buttons.

6. **Sharp corners.** Max `var(--radius-3)` (6px). `var(--radius-dot)` (9999px) is **only** for status dots.

7. **Vim-style block cursor in `var(--accent)`** wherever there's text input. Not a thin caret. Color matches the active mode for `[ask]` and `[find]`.

8. **Lowercase UI labels** by default. No emoji in product surfaces. No exclamation marks.

9. **Animation is restrained.** 120ms default (`var(--dur-base)`), sharp-out easing (`var(--ease-out)`). No spring/bounce, no spinners, no glow. The pulsing plum dot on the AI status indicator is the *only* ambient animation in the entire chrome.

10. **Tokens cascade through Lua.** Every CSS var is mirrored on `theme.tokens` (`--surface-1` ↔ `theme.tokens.surface_1`). Themes set tokens, never hard-code colors.

---

## File map

```
spec/                            ← full design specification
  README.md                      ← navigate from here
  00_overview.md                 ← slot/element/theme model + glossary
  01_architecture.md             ← Lua API surface (skip if your project docs cover this)
  02_design_principles.md        ← voice, density, do's and don'ts
  03_tokens.md                   ← every token by name + meaning
  04_typography.md               ← type ramps + key bindings
  05_motion.md                   ← animation rules
  06_iconography.md              ← Lucide usage + unicode glyphs
  07_themes.md                   ← Lua theming contract
  components/                    ← per-component implementation contracts
    button.md
    omnibox.md
    tabs.md
    status-line.md
    palette.md
    sidebar.md
    field.md
    card.md
    badge.md
    kbd.md
    empty-slot.md

colors_and_type.css              ← canonical tokens — import directly
assets/
  wordmark.svg                   ← [mote] logo
  mark.svg                       ← [·] mark, favicon
  lucide-usage.md                ← canonical icon mapping
fonts/README.md                  ← font substitution notes

ui_kits/browser/                 ← WORKING reference implementation
  index.html                     ← all component CSS lives here (canonical)
  *.jsx                          ← composition patterns

preview/                         ← design-review snapshots (visual reference, not implementation)
```

---

## Common tasks

### "Build component X"

1. Read `spec/components/<x>.md` end-to-end. It includes structure, tokens, states, behavior, accessibility, and the anti-patterns to avoid.
2. Check `ui_kits/browser/index.html` for the working CSS — that's the canonical reference. Lift from there rather than re-deriving.
3. Test in both themes by toggling `[data-theme]` on the root.

### "Theme an existing component"

1. Read `spec/07_themes.md` for the Lua theming contract.
2. Identify which tokens to override (`theme:set_token`) versus which element selectors to restyle (`theme:style`).
3. Never modify the component's CSS directly to achieve theming — it breaks the theme contract.

### "Add a new token"

1. Add the CSS variable to `colors_and_type.css` under `:root`, and override under `[data-theme="vellum"]` if it needs to differ.
2. Add a row to the relevant table in `spec/03_tokens.md` with name + meaning + when to use.
3. The Lua bridge picks it up automatically by mapping `--name-here` → `theme.tokens.name_here`.

### "I need an icon"

1. Check `assets/lucide-usage.md` — the canonical icon mapping. If the concept is already mapped, use that name.
2. If not, pick the closest Lucide icon (`https://lucide.dev`) matching 1.5px stroke / geometric / terminal-friendly. Add a row to `lucide-usage.md` and `spec/06_iconography.md` in the same commit.
3. Render at 16px (chrome), 20px (dialogs/palettes), or 14px (status line). Never invent new sizes.
4. Never use emoji. Never use raw unicode where a Lucide icon exists (but DO use unicode for keys, separators, and the bracket lockup — see `spec/06_iconography.md`).

### "Implementing a slot"

1. The slot is just a `<div data-slot="<name>">` in HTML. The runtime owns the slot lifecycle; you own the structure inside.
2. If a slot is declared but unbound, the runtime fills it with the empty-slot motif (dot grid + `[ ] <name>`). See `spec/components/empty-slot.md`.
3. Standard slots are listed in `spec/01_architecture.md`. Custom slots are allowed but use the same pattern.

### "Writing copy"

1. Lowercase, second person, direct, no emoji, no exclamation marks. See `spec/02_design_principles.md` for examples.
2. Use unicode glyphs (`·` `›` `⌘ ⌥ ⇧ ⌃ ⏎`) where they convey meaning more tightly than words.

---

## What to do if you can't find what you need

- If a value isn't tokenized → check `colors_and_type.css` carefully, then add the token if it'll be reused.
- If a component isn't specced → check `ui_kits/browser/` for an in-progress version, or extrapolate from the closest specced component (most patterns repeat).
- If a behavior isn't documented → ask the user. Don't invent product behavior.
- If a rule seems to conflict with what you're building → flag it, don't silently break it.

---

## Anti-patterns (a partial list — full one in `spec/02_design_principles.md`)

- ❌ Bluish-purple gradients, mesh gradients, any gradients.
- ❌ `border-radius: 9999px` (pills) on anything except status dots.
- ❌ Drop shadows on inline elements.
- ❌ Filled Lucide icons for static use. Stroke only. (Carve-out: **state-indicator toggles** like bookmark/favorite/pin may fill on the active state — see `spec/06_iconography.md`.)
- ❌ Spring/bounce easing, slide transitions on content swap.
- ❌ Loading spinners.
- ❌ Glow effects, especially on AI surfaces.
- ❌ Emoji in product surfaces.
- ❌ `Cmd+K` spelled out — use `<kbd>⌘</kbd><kbd>K</kbd>`.
- ❌ Welcome screens, onboarding wizards, success toasts.
- ❌ Decorative SVG illustrations. Mote's product is text and chrome.
