# Spec — Mote design system

This is the design-system handoff for Mote. Everything here is scoped to **how the frontend should look, behave, and be built** — not the runtime, plugin model, or product architecture. (Those belong in your own docs.)

The spec is multi-file so an agent can jump straight to the slice it needs.

## Append this to your `CLAUDE.md`

```md
## Design system

When implementing any frontend surface (chrome, components, themed views), read `spec/README.md`
first. It indexes the design tokens, typography, motion rules, and per-component contracts.

Hard rules:
- Use design tokens (`var(--accent)`, `var(--surface-1)`, etc) — never raw hex or px values
  that aren't tokenized. The token vocabulary is defined in `spec/03_tokens.md`.
- Both `[data-theme="dusk"]` (default dark) and `[data-theme="vellum"]` (light) must work.
- Borders carry the visual weight; shadows are for floating surfaces only.
- The `[mote]` bracket lockup (mono amber brackets + sans contents) is the brand DNA — reuse it
  for any "slot indicator" in chrome (omnibox mode, sidebar panel header, etc.).
- Interactive elements use the keycap construction: 1px border + 2px bottom border, collapsing
  on press. See `spec/02_design_principles.md`.

For specific components, point at `spec/components/<name>.md` directly.
```

That snippet is enough to wire CC into the spec. Everything else lives in this folder.

## Index

| File | What's in it | Read when |
|---|---|---|
| [`00_overview.md`](./00_overview.md) | Glossary + the slot/element/theme model | First read — establishes vocabulary the rest assumes |
| [`02_design_principles.md`](./02_design_principles.md) | Voice, density, do's and don'ts | Writing any copy or UI |
| [`03_tokens.md`](./03_tokens.md) | Every design token by name + meaning | Picking colors, spacing, radius |
| [`04_typography.md`](./04_typography.md) | Type ramps, fonts, key bindings | Anything with text |
| [`05_motion.md`](./05_motion.md) | Animation rules | Anything that moves |
| [`06_iconography.md`](./06_iconography.md) | Lucide usage + unicode glyphs | Adding icons |
| [`07_themes.md`](./07_themes.md) | The Lua theming contract | Implementing the theme runtime / writing themes |
| [`components/`](./components/) | Per-component implementation contracts | Building specific UI |

> **Note:** `01_architecture.md` describes the Mote runtime's slot/element/theme architecture *as the design system depends on it*. It's here because the design system *bakes the architecture in* — the bracket lockup, the empty-slot motif, the sidebar's swappable-panel pattern, the omnibox's mode tag are all visual expressions of that architecture. The authoritative runtime/plugin/security model lives in the project's `DESIGN.md`; treat `01_architecture.md` as the visual-system view that points back to it.

## Source of truth

The textual spec in this folder is the source of truth. Design tokens are defined in [`03_tokens.md`](./03_tokens.md) and typography in [`04_typography.md`](./04_typography.md); the per-component contracts in [`components/`](./components/) define structure, tokens, states, and behavior for each component. (Earlier drafts referenced standalone `ui_kits/`, `colors_and_type.css`, `*.jsx`, and `preview/` assets that do not exist in this repo; the spec docs themselves are canonical.)

## What's NOT in this spec

- Project setup, build tooling, package management
- Runtime implementation (Lua bridge, slot host, plugin loading) — only the design-system implications
- Routing, state management, networking
- Anything outside the visible chrome
