# ADR-0015 — `mote://newtab` Slot Architecture + `mote://` Global-Request-Context Constraint

- **Status:** Accepted (approved by the maintainer 2026-06-02)
- **Date:** 2026-06-02

---

## Context and Problem Statement

Today a freshly-opened tab loads a `data:text/html,<html>...mote — composited
web content</body></html>` placeholder. The tab title literally shows the
data URL until R2's `OnTitleChange` mirror catches up (and even then, the
default title for that data URL is uninformative). The placeholder page
also offers no extension surface — there's nowhere for themes or future
plugins to put a quick-launch UI, AI prompt, recent-pages list, etc. P3
introduces a proper `mote://newtab` page; this ADR records the *slot
architecture* that page exposes and the **`mote://` global-request-context
constraint** that every `mote://` chrome page (newtab, future settings,
etc.) inherits.

## Decision Drivers

- The new-tab surface needs an extension point — every browser worth using
  lets themes/plugins customise what shows when the user opens a fresh
  tab. Themes and AI plugins are the obvious early consumers.
- The slot pattern already exists for sidebar/right-panel/status (ADR-0014
  forward-references the rail). `mote://newtab` should reuse the same
  pattern, not invent a new one.
- CEF custom-scheme handlers are registered against a CEF *request
  context*; Mote registers the `mote://` handler only against the global
  request context (`crates/mote-cef/src/engine.rs`,
  `Engine::register_chrome_resources` path). Per-identity profile contexts
  (used for untrusted web content per the identity-isolation work) do NOT
  have the handler installed — a `mote://` URL loaded in a profile context
  fails with `ERR_UNKNOWN_URL_SCHEME`. This is documented in memory's
  `running-and-cef-notes` and bit us during the chrome-overlay routing
  work. Future `mote://` surfaces must inherit this constraint or they
  silently break.
- v0.1 ships the *minimal* new-tab: centered `[·]` brand mark, faint hint
  line. Anything richer (bookmarks shortcuts, AI chat surface, recent
  tabs) lands in later phases by binding the declared slot.

## Considered Options

- **Static HTML, no slot.** Rejected: works for v0.1 but commits us to a
  full redesign every time we want to extend the new-tab surface.
- **One large `newtab.body` slot covering the whole page.** Rejected: too
  coarse — themes/plugins binding to it replace the brand mark and hint
  line, with no way to compose.
- **One named `newtab.center` slot, brand mark + hint render outside the
  slot** (this ADR). Default theme renders only the brand mark and hint;
  the `newtab.center` slot is empty in v0.1 default theme, available for
  binding by themes/plugins/AI surfaces in later phases.

## Decision Outcome

Chosen: **`mote://newtab` is a proper chrome page (served via the
existing `mote://chrome` registration path, since the asset routing is
the same), declaring exactly one slot — `newtab.center` — that themes
and future plugins can bind. The default theme leaves the slot empty in
v0.1, rendering only the centered brand mark and a faint hint line.**

### Page structure

```
┌──────────────────────────────────────────┐
│                                          │
│                                          │
│              ┌─────────┐                 │
│              │   [·]   │                 │  brand mark, 96px
│              └─────────┘                 │
│                                          │
│      press ⌘L to navigate                │  hint line, var(--fg-mute)
│                                          │
│            <newtab.center>               │  declared slot, empty in v0.1
│                                          │
│                                          │
└──────────────────────────────────────────┘
        full-bleed dot-grid motif at 12% opacity
```

- Brand mark: `assets/mark.svg`, ~96px, centered
- Hint line: `press ⌘L to navigate` in `var(--fg-mute)` (the only string
  content the default page ships; localizable later)
- Background: the canonical empty-slot dot-grid motif (per
  `spec/components/empty-slot.md`) at 12% opacity, full bleed
- Page title: `new tab` (static string; R2's `OnTitleChange` mirror picks
  it up for the sidebar tab title and window title automatically)
- Served from `mote://chrome/newtab.html` — same scheme/origin as the
  rest of chrome; the *content* is a separate document but the *origin*
  is unchanged

### The slot

```html
<!-- inside newtab.html, between the hint line and the page footer -->
<div data-slot="newtab.center" class="slot-empty">
  <!-- empty in v0.1 default theme; bound by themes/plugins -->
</div>
```

- Slot discovery happens at page load (same mechanism as existing
  sidebar/right-panel slots — see `crates/mote-ui/chrome/host.js` for the
  current resolver pattern)
- Default theme does not bind it; rendering shows the `slot-empty`
  treatment (per P1's empty-slot motif)
- Themes can bind via `theme:bind_slot("newtab.center", <element>)` (the
  existing theme API extends to this slot; no new API added by this ADR)
- Plugin bindings to this slot are subject to the future plugin-rail
  work scoped in ADR-0014 — a manifest `slots = { ... }` table is
  *not* introduced here, deferred to the same future ADR

### The `mote://` global-request-context constraint

**Every `mote://` page MUST be loaded into CEF's global request context,
not into any per-identity profile context.** The custom-scheme handler is
registered against the global context only; a `mote://` URL navigated to
in a profile context fails with `ERR_UNKNOWN_URL_SCHEME`. Concrete
implications:

- `Page::new(url, ...)` (global context) — OK for `mote://*` URLs
- `Page::with_profile(url, opts, profile)` (profile context) — **must
  reject `mote://` URLs** and the request handler enforces this via the
  S1 navigation guard (`PageRole::Content` cannot navigate to `mote://`).
  R3's `is_popup_url_allowed` pre-filter is a parallel defense at the
  popup intercept.
- Future `mote://` surfaces (settings via ADR-0016, any plugin-authored
  chrome-side pages if ever introduced) must be created via `Page::new`
  on the global context. The existing chrome page, the integrity panel,
  the picker overlay, the approval dialog, and the new newtab page all
  honour this.
- A future change introducing a `mote://`-served page in a profile
  context would silently break it; the constraint is encoded in this
  ADR to make that breakage a recorded-decision violation, not an
  accident.

A consequence worth naming: `mote://newtab` has no per-profile state.
Future surfaces that *need* per-profile state (e.g. a per-profile "recent
tabs" list on newtab) must not try to access it via cookie/localStorage
on the newtab page — those are global-context. Instead, surface the state
via host APIs the slot's bound element reads at render time.

## Consequences

- Good: `mote://newtab` is extensible without redesigning the page —
  themes and plugins bind one slot, the rest of the page stays stable.
- Good: the `mote://` global-context constraint is now a recorded
  decision; any future `mote://` page lands knowing the rule.
- Good: v0.1 default-theme behavior is minimal and clean — the user sees
  a calm empty page, not a Chrome-style "frequently visited" grid that
  Mote hasn't earned the right to render.
- Bad: themes/plugins wanting per-profile state on newtab can't use web
  storage on the newtab page (it's global). Mitigated by routing state
  through host APIs; called out so the failure mode is documented.
- Bounded scope: this ADR adds one slot and records one pre-existing
  constraint. The plugin-binding mechanism for the slot (manifest schema,
  asset routing, JS↔Lua channel) is the same future work ADR-0014
  scopes for the rail; this ADR does not duplicate it.

## Relationship to existing ADRs

- **Inherits from ADR-0003** (Chrome UI as HTML/CSS in CEF). The newtab
  page is another HTML document served via the same chrome scheme
  handler; no new infrastructure.
- **Inherits from ADR-0005** (Host Bridge — Two-Layer Isolation). The
  newtab page is on the privileged `mote://chrome` origin and would have
  bridge access if any code on the page called `cefQuery` — v0.1 doesn't,
  but the trust model is unchanged.
- **Parallels ADR-0014** (Rail as Plugin-Declarable Slot). Same slot
  pattern, same future-binding deferral, same isolation story for any
  plugin-supplied panel content (sandboxed `mote://overlay`, never
  `mote://chrome`).
- **Forward-references the same v2 plugin-binding ADR** as ADR-0014 for
  the actual plugin-binding mechanism.
