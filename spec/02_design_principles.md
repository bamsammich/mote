# 02 · Design Principles

These are the rules every contribution to Mote's UI must respect. Read them as constraints, not suggestions.

## The mood

**Warm ink on warm paper.** Mote rejects the cool, bluish, glassy aesthetic of mainstream browsers. It looks closer to a well-typeset technical manual run through a CRT — Solarized's bones, Gruvbox's warmth, the typographic care of a Zed or an iA Writer.

The default themes are named for times of day in low light: `dusk`, `vellum`, `embers`, `gloam`.

## Voice

| Quality | Rule |
|---|---|
| Casing | **Lowercase** UI labels by default. Acronyms keep canonical casing (`AI`, `URL`, `JSON`). |
| Person | Second person ("you") in product. First-person plural ("we") only in changelogs. Never "I". |
| Tone | Direct, technical, assumes competence. No hand-holding. No "welcome back!". |
| Punctuation | No exclamation marks. Em dashes welcome. Sentences can end without periods in dense UI. |
| Density | High. Mote users prefer information density. Don't pad with whitespace for "breathing." |
| Emoji | **Never in product UI.** Acceptable in changelogs/release notes only, sparingly, dev-cultural set: 🔧 🐛 ⚡ 📦 🔐. |
| Glyphs | Unicode glyphs are encouraged when they convey meaning: `›` `·` `⌘ ⌥ ⇧ ⌃ ⏎ ⌫` `■ □ ◐ ◯` `›_`. |

### Examples

✅ `omnibox empty. type a url, a query, or :help`
✅ `7 tabs, 2 hibernated. ⌘W to close, ⌘⇧T to undo.`
✅ `theme loaded from ~/.config/mote/themes/dusk.lua (217 lines)`

❌ "Welcome back! Ready to browse? 🚀"
❌ "Oops, looks like we couldn't load that page. Don't worry — try again!"
❌ "Awesome! Your theme has been successfully applied."

## Visual rules

### Color

- Use **`var(--accent)`** for active states, focus rings, cursors, the brand mark. The default is amber `#E0A458`; never reference the hex.
- **`var(--success)`, `var(--danger)`, `var(--info)`, `var(--special)`** for status. Never use raw hex.
- **No bluish-purple gradients.** No mesh gradients. No backdrop blur.
- **Two themes are first-class:** `[data-theme="dusk"]` (default) and `[data-theme="vellum"]`. Test both.

### Borders, shadows, elevation

- **Borders do the work.** A 1px `var(--border)` hairline separates almost everything.
- **Shadows are for floating surfaces only:** palette, completion popup, modal. Never on inline cards.
- **No inner shadows.** No glow. Glow reads "AI cliché" — Mote rejects it.

### Radius

- `--radius-0` = 0 — slots, dividers, status line
- `--radius-1` = 2px — buttons, fields, tabs, chips
- `--radius-2` = 4px — cards, dialogs
- `--radius-3` = 6px — large floating surfaces (palette, completion popup)
- `--radius-dot` = 9999px — **status indicators only**, never buttons

### The keycap construction

Every interactive element that registers a click (button, chip, kbd, segmented control) uses the same mechanical metaphor:

```
border: 1px solid var(--border);
border-bottom-width: 2px;
```

On press:

```
border-bottom-width: 1px;
transform: translateY(1px);
```

This gives the entire UI a tactile, mechanical feel — fitting for a tool aimed at people whose hands live on keyboards.

### The bracket lockup

The `[mote]` lockup — mono brackets in amber, contents in sans — is **brand DNA** that should reappear in chrome wherever there's a "slot indicator":

- The omnibox mode tag: `[url]` / `[cmd]` / `[ask]` / `[find]`
- The sidebar panel header: `[tabs]` / `[assistant]` / `[plugins]`
- Any contextual region label in dev tooling

**The pattern is:** mono open bracket (amber) → sans content (foreground color) → mono close bracket (amber).

### Cursor

The text-insertion cursor is a **vim-style block**, not a thin caret:

```css
.cursor {
  display: inline-block;
  width: 7px; height: 14px;
  background: var(--accent);
  animation: blink 1.2s steps(2, end) infinite;
}
@keyframes blink { 50% { opacity: 0.4; } }
```

In modal contexts (`[ask]`, `[find]`), the cursor color matches the mode's accent (`var(--special)`, `var(--info)`).

### Backgrounds

- **No gradients.**
- **Empty slots** wear the dot-grid motif (`background-image: var(--dots)`).
- **Vertical/horizontal hairlines** divide adjacent slots.
- **ASCII separators** (`────`, `· · · ·`) appear in dev-tooling contexts only.

## Animation

- **Default duration:** 120ms.
- **Micro feedback** (button press, tab close): 80ms.
- **Entrances** (palette open, dropdown reveal): 200ms max.
- **Easing:** `cubic-bezier(0.2, 0, 0, 1)` (`var(--ease-out)`). Sharp-out. No spring, no bounce.
- **No loading spinners.** Use an indeterminate top bar or a `·` → `··` → `···` mono ticker.

## Things to avoid

- **Pills** — `border-radius: 9999px` on anything that isn't a status dot.
- **Filled icons** — Lucide stroke icons only.
- **Glow effects** — including under elements claiming to be "AI."
- **Drop shadows on inline elements** — only on floating surfaces.
- **Bluish-purple gradients, mesh gradients, backdrop blur.**
- **Emoji in product surfaces.**
- **Decorative SVG illustrations** — Mote's product is text and chrome.
- **Loading spinners** in the chrome.
- **Welcome screens, onboarding wizards, success toasts with checkmarks.**

## Next

[`03_tokens.md`](./03_tokens.md).
