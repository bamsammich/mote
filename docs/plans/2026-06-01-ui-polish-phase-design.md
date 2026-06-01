# Mote UI Polish Phase — Design

**Date:** 2026-06-01
**Status:** Approved
**Authors:** Travis Huddleston, Claude (brainstorming pair)
**Phase context:** Follows Phases 1–5a (all closed). Sits alongside / before the
deferred 5b (password-manager stack) and Phase 6 (AI surface). Aims to bring
the v0.1 chrome from "functional with rough edges" to "comfortably usable as a
daily-driver browser for reading," while locking in the status-line plugin
contract that future first-party and third-party plugins will depend on.

## Overview

This phase splits cleanly into two halves:

1. **Repair pass (R1–R4)** — four small focused commits fixing functional
   defects that make the chrome feel broken: resize doesn't repaint, address
   bar doesn't mirror navigation, popups open chromeless OS windows, no window
   close affordance and incomplete browser-keybind suite.
2. **Polish pass (P1–P6)** — six waves of visual + UX work, each touching one
   surface end-to-end. P1 (chrome anatomy + tooltip primitive) must land
   before P2–P6, which then dispatch in parallel.

Total: 10 waves, sequenced so the smallest-blast-radius work lands first.

## Cross-cutting principles

Five rules every wave honors. Derived from `mote-design`, `frontend-design`,
the user's seven initial polish items, and modern-browser table-stakes.

1. **Every visible element does something.** If drawn, it responds to hover
   (cursor + tooltip) and click (or carries `aria-disabled` with a reason).
   Inert UI gets wired up or removed in its wave.
2. **Borders carry visual weight; shadows are reserved for floating
   surfaces.** 1px `var(--border)` hairlines separate all slots.
3. **Accent shows up where focus or activity lives.** Active tab left-stripe,
   focused input ring, active rail icon, current workspace label, hover
   states, omnibox mode prefix.
4. **Lockup pattern reused.** `[mote]`-style bracket lockup repeats wherever
   a "slot indicator" lives: `[url]` `[cmd]` `[find]` for omnibox modes,
   `[tabs]` `[bookmarks]` `[history]` for sidebar panels, `[ws]` for the
   workspace chip, `[mode]` `[sec]` for status-line elements. Brackets in
   `var(--accent)`, contents in `var(--fg)`.
5. **Both themes verified side-by-side every wave.** Vellum (light) hasn't
   been eyeballed on the new surfaces. Each wave's commit ships with dusk +
   vellum screenshots in `docs/screenshots/<wave>/`.

**Live verification gate.** Every wave's commit must include before/after
screenshots and a recorded interaction sequence, captured with `hyprctl` +
`ydotool` + `grim` per the mechanics in
`~/.claude/projects/.../memory/running-and-cef-notes.md`.

**Icons are themable.** Default theme keeps the current bookmark mark and
other choices. Every chrome icon routes through a `theme.icons.<action>`
mapping themes can override (lucide name or inline SVG). `assets/lucide-usage.md`
documents defaults, not constants.

## Wave inventory

| Wave | Surface | Touches | Depends on |
|---|---|---|---|
| R1 | Resize repaint cascade (chrome + content + overlays) | shell event loop, CEF host wrappers | — |
| R2 | Address-bar truth (URL mirror, page title, copy URL) | chrome bridge, CEF display handler | R1 |
| R3 | Popup intercept → in-window tab | CEF lifespan handler, tab manager | R1 |
| R4 | Window lifecycle + browser-keybind suite | keybind table, chrome HTML, shell event routing | R1 |
| P1 | Chrome anatomy + tooltip primitive | `mote://chrome` HTML/CSS, theme tokens | R1–R4 |
| P2 | Address bar redesign | omnibox component, security popover | P1 |
| P3 | Tabs + `mote://newtab` | tabs component, newtab page, tab events | P1 |
| P4 | Status line + read-only plugin contract | status-line component, host API, ADR | P1 |
| P5 | Table-stakes (find, context menu, hover-URL, zoom, reopen) | find UI, context menu, CEF handlers, keybinds | P1, P4 (hover-URL only) |
| P6 | First-party settings (general / plugins / integrity / keybinds) | new settings panel, plugin-manager API | P1, P4 |

P2 / P3 / P4 / P5 / P6 dispatch in parallel after P1 — file overlap planned
to support this (see "File overlap + dispatch" below).

## Repair pass (R1–R4)

### R1 — Resize repaint cascade

**Problem.** Triggered cleanly by opening a neighbor window in the same
Hyprland workspace (e.g. `hyprctl dispatch exec`): Mote tiles smaller,
then when the neighbor closes Mote expands back — but rendering stays at
the old (small) dimensions while the surface is padded black to the new
dimensions. An explicit subsequent `hyprctl resizeactive` immediately
fixes it.

**Corrected diagnosis** (initial design assumption was wrong). Existing
`handle_resize` (`lib.rs:2419`) DOES fan out to chrome + integrity overlay
+ picker overlay + active content page. The defect is that **winit is not
delivering `WindowEvent::Resized` for every window-state transition
Hyprland produces** — particularly the close-of-tiled-neighbor → expand
sequence. `handle_resize` works correctly when called; the question is
why it's not always called.

Three subordinate gaps that compound the user-visible symptom:
1. **Inactive content pages** (other tabs in the current workspace) are
   not part of the fanout — switching to a non-active tab post-resize
   shows it at the old size.
2. **Pages in other workspaces** are not in the fanout — switching
   workspace post-resize requires materialization but also a correct size.
3. **The size-zero guard** (`size.width == 0 || size.height == 0` early
   return at line 2420) could swallow transient zero-size events during
   compositor reconfigure; needs verification, not blind removal.
4. **`on_scale_factor_changed`** still only notifies the active content
   page (`lib.rs:2406`) — the original latent HiDPI gap from memory.

**Fix shape** (refined from initial design):
- Diagnose which winit events fire on the open-neighbor-close-neighbor
  sequence (add temporary `tracing::info!` logs in `handle_resize`,
  `on_scale_factor_changed`, and the main event-loop match — see what
  arrives vs. what's missing).
- Likely supplement: catch additional event(s) (`WindowEvent::Moved`,
  `Focused`, `Occluded`, or a periodic `RedrawRequested`-driven
  size-reconciliation) to detect and recover when window dimensions
  changed without a `Resized` delivery. Choice depends on what the
  diagnostic logs show.
- Extend fanout to every alive page (active + inactive content,
  every workspace), not just the active one. Introduce
  `notify_all_pages_of_size_change(width, height, scale)` and route both
  `Resized` and `ScaleFactorChanged` through it.
- Investigate the size-zero guard: log when it triggers; if it's swallowing
  legitimate transient sizes during compositor reconfigure, replace with
  smarter handling.

**Verification.**
- Repro the original bug (open alacritty in same workspace, close it);
  confirm Mote redraws cleanly without manual intervention.
- Drag Mote between monitors of different scales via `hyprctl`.
- Tile-grid changes (cycle through tile layouts).
- Switch to a previously-inactive tab post-resize; verify it materializes
  at the correct size on activation.

### R2 — Address-bar truth

**Problem.** Omnibox doesn't update on navigation. Copy-URL impossible. Tab
titles in sidebar literally show the source URL (e.g.
`data:text/html,<html><body sty…`).

**Fix.** Wire CEF `CefDisplayHandler::OnAddressChange` and `OnTitleChange`
through the chrome bridge:
- Omnibox text mirrors active tab's current URL
- Window title tracks active tab's page title
- Sidebar tab row title tracks page title

Add `mote.tabs.copy_active_url` host API gated behind `read-tabs` capability.

**ADR-0005 compliance.** `copy_active_url` must be implemented as a
**structured op in the host bridge's enumerated op set** (per ADR-0005's
two-layer isolation), not as a JS-only function. Capability gate enforcement
lives in the bridge layer, not in Lua.

**Verification.** Navigate inside Google search results via `ydotool`; watch
omnibox + window title + sidebar tab title update. Right-click omnibox → copy
URL → paste externally matches.

### R3 — Popup intercept → in-window tab

**Problem.** Clicking a target=_blank link opens a new chromeless OS window.
CEF's default popup behavior leaks through.

**Fix.** `CefLifeSpanHandler::OnBeforePopup` returns `true` (suppresses OS
popup), enqueues `TabCreate{url, target_workspace=current}` on the shell
event bus. New tab becomes active per CEF's `user_gesture` flag.

**ADR required:** Popup behavior policy. Records (a) in-window-tab
interception as the default, (b) the `user_gesture`-driven activation rule
(gesture → foreground, non-gesture → background), (c) the plugin/theme
opt-out path (OAuth flows etc.) as a future extension point.

**Verification.** Middle-click a Hacker News story → opens in a new Mote tab
in current workspace, NOT a new chromeless OS window.

### R4 — Window lifecycle + browser-keybind suite

**Problem.** No window close button. `⌘W` / `⌘T` / `⌘L` / `⌘R` / `⌘[` /
`⌘]` / `⌘1`–`⌘9` either don't exist or don't work from content focus.

**Fix.** Three pieces sharing the shell event-routing seam:

- **Visible close button** keycap top-right of header, paired with bookmark
  icon position. `1px var(--border)`, `border-bottom-width: 2px`. Glyph =
  lucide `x` (themable).
- **Keybind suite** wired through `intercept_keybind` so they work from any
  focus owner:
  - `⌘T` new tab · `⌘W` close tab · `⌘⇧W` close window · `⌘Q` quit
  - `⌘L` focus omnibox · `⌘[` back · `⌘]` forward · `⌘R` reload
  - `⌘1`–`⌘9` switch workspace by index
  - (`⌘N` new window deferred to follow-up if multi-window adds real scope)
- **Workspace keybinds** as above.

**ADR required:** Browser-keybind suite + scope rules. Records (a) the
chord-to-action table, (b) scope semantics (global / chrome / content /
omnibox / palette-open), (c) the **`⌘W` contextual rule** (closes the tab
normally; closes the window when only one tab remains), (d) **whether
plugins can register global keybinds in v0.1** — current answer: **no**,
the keybind table is closed in v0.1 and plugin-registered keybinds are a
future ADR. User customization deferred to a later phase.

**Verification.** From each focus state (chrome, content, omnibox, palette
open), the full keybind suite fires the right action. Close button click
closes the window. `⌘W` with one tab open closes the window.

**Commit order within R:** R1 → R3 → R2 → R4 by ascending blast radius.

## Polish pass (P1–P6)

### P1 — Chrome anatomy + tooltip primitive

**Goal.** Restructure the header (3 stacked bars → 1 row), fix tab-row
visuals, identify rail icons, drop the right slot from default theme, build
the tooltip primitive used by every subsequent wave.

**Layout target.**

```
┌─[ws] Default ›─┬─[url] · https://google.com [‹][›][↻]  ⊕  🔖  ✕─┐  52px header
├────────────────┴───────────────────────────────────────────────────│  1px hairline
│ ▣ [tabs]                                    3 │     viewport       │
├────────────────────────────────────────────────│                    │  1px hairline
│ ▣ ●─ Amazon.com…                              │                    │  active: 2px accent stripe + surface-2
│ ▣ ◯  Example Domain                           │                    │
│ ▣                                              │                    │
│                                                │                    │  empty zone: dot motif 20%
│ ▣                                              │                    │
│ ⊟                                              │                    │  collapse btn moves into rail
├────────────────────────────────────────────────┴────────────────────│  1px hairline
│ [mode] NORMAL · [sec] 🔒 https · tls 1.3              3 tabs        │  24px status line
└──────────────────────────────────────────────────────────────────────┘
```

**Header changes.**
- Workspace strip + omnibox merge into one 52px row separated by 1px hairline
- Left chunk = `[ws] Default ›` bracket-chip (keycap construction, opens
  existing workspace popover on click)
- Right of omnibox: three minimal essential keycap buttons `[‹] [›] [↻]`
  (16px lucide icons in 28x28 keycaps); then `[⊕]` new-tab, `[🔖]`
  bookmark-current-page, `[✕]` close-window
- Every button: tooltip, keycap pressed state, `aria-label`

**Tab strip header.**
- `[tabs]` lockup + count chip `3` (no more "3 open" wording)
- `+` button moves up into header as global `[⊕]`

**Tab rows.**
- Empty checkbox squares → 14px faint dot-grid favicon slot
  (`var(--accent-mute)` 30% opacity); `[·]` mark for newtab tabs
- Active tab: 2px `var(--accent)` left-stripe + `surface-2` bg lift +
  `var(--fg)` text color
- Close X on hover only (right side, 16px lucide, keycap construction)

**Rail icons.**
- `layers` → tabs (existing)
- `bookmark` → bookmarks (existing)
- `clock` → history (existing)
- 4th + 5th slots: **unbound plugin-icon placeholders**. Render `[+]` glyph
  with tooltip "available — plugins can add panels here." Click opens palette
  filtered to plugin-discovery. Rail itself is a documented declarable slot
  for plugin authors.
- Collapse icon moves into the rail proper (not floating below)
- Every rail icon: tooltip, active-state = 2px accent stripe + surface-2 bg +
  accent stroke fill, hover-state = surface-2 lift

**ADR required:** Rail-as-plugin-declarable-slot. The rail is the **first
plugin-authored chrome-UI surface** — distinct from page-adjacent slots
covered by DESIGN.md. The ADR records: (a) how plugins declare a rail panel
in their manifest, (b) icon contribution model, (c) collision policy when
more plugins want rail icons than slots available, (d) the
provenance/isolation boundary — plugin panel content rendered in privileged
chrome vs. sandboxed overlay context, given ADR-0007's ruling that
trust-critical UI belongs on the privileged origin.

**ADR required:** Themable icon contract. The `theme.icons.<action>`
mapping and `theme:set_icon` API are a new extension on the theme contract
not covered by ADR-0003 (which addresses tokens but not icon overrides).
Spans P1 (rail + chrome icons), P2 (nav buttons + bookmark + close), P4
(status-line icons). The ADR records the API shape, override scope (per
chrome surface and per status-line element id), and forward-compatibility
guarantees.

**Right slot.** Default theme **drops** the `right-side` slot. 94px reclaimed
by the viewport. Plugins/themes can re-declare it.

**Empty zone in sidebar.** Below tab list, render canonical dot-grid
empty-slot motif at 20% opacity — signals "intentional space" not "broken."

**Tooltip primitive.**
- 200ms delay
- `surface-2` bg, 1px `var(--border)`, sharp corners, `var(--radius-2)` max
- Caption + optional `<kbd>` chord on right
- Positions below trigger; flips to above when clipped
- Exposed to Lua themes via `theme.tokens.tooltip_*`

### P2 — Address bar

**Modal omnibox** per `spec/components/omnibox.md`:
- `[url]` default · `[cmd]` palette · `[find]` find-in-page
- `[ask]` mode deferred to AI phase

**Focus state.** 2px `var(--accent)` outset (implemented as sharp double
border, no glow per design rules).

**Vim block cursor** in `var(--accent)` when text input is active.

**Security indicator** (green dot) becomes clickable. Popover anchored below
the omnibox:
- Cert subject + issuer + valid-through
- TLS version + cipher
- Cookies count (capability-gated)
- Permissions granted to origin (slots ready for future permission UI)
- "Site settings" action → opens P6 settings panel filtered to this origin

HTTP / mixed-content: `[!]` in warn color, clickable for reason.

**Copy URL** — three paths, all call `mote.tabs.copy_active_url` from R2:
- Right-click omnibox → "copy URL" / "copy as markdown link"
- `⌘C` while omnibox focused (no text selection) copies full URL
- `[cmd] copy url` palette command

**Three keycap nav buttons** `[‹] [›] [↻]`:
- Bound to CEF `GoBack` / `GoForward` / `Reload`
- Disabled state when no history (greyed via `var(--fg-mute)`,
  `aria-disabled`, tooltip explains)
- Long-press `[‹]` opens history popover (table-stakes muscle memory)
- Tooltips show keybind chords (`<kbd>` primitive)

**`[⊕] [🔖] [✕]`** to the right of the omnibox — new-tab, bookmark current
page (toggles filled vs outline per state-indicator carve-out), close
window. All with tooltips + keybind chords.

**Workspace chip touch.** Small `var(--accent)` dot to right of `Default ›`
if other workspaces exist.

### P3 — Tabs + new-tab page

**`mote://newtab`** — minimal + slot-driven:
- Centered `[·]` mark from `assets/mark.svg` at ~96px
- Single faint hint line below: `press ⌘L to navigate` in `var(--fg-mute)`
- Background: dot-grid empty-slot motif at full bleed, 12% opacity
- Page title fixed to `new tab` (kills the `data:text/html` literal)
- Page declares one slot `newtab.center` for future bindings (AI prompt,
  bookmarks shortcuts, recent tabs)
- Served from `mote://chrome/newtab` — global request context per
  CEF constraint in memory

**ADR required:** `mote://newtab` slot architecture. Records (a) the
declarable-slot pattern for `mote://` chrome pages (first non-overlay
example, sets the pattern for future `mote://` surfaces), (b) the **global
request context constraint** — `mote://` pages run in CEF's global request
context, NOT in any per-identity profile/cookie context, with implications
for any future `mote://` surface that might otherwise rely on per-origin
storage or session state, (c) the load-step at which declared slots are
discovered and the empty-slot-motif default.

**Favicon slot stops looking like a checkbox.** 14px faint dot-grid square
in `var(--accent-mute)` 30% opacity until real favicons land. `[·]` mark
glyph for newtab tabs.

**Tab interaction.**
- Active / hover states per P1
- Middle-click closes tab (chrome page mousedown `event.button === 1`)
- `⌘`-click on content links → background tab in current workspace, routed
  **only** through CEF callbacks (preferentially `OnBeforeContextMenu`
  short-circuit or `CefRequestHandler::OnBeforeBrowse` with the modifier
  flags). **JS injection into content pages is explicitly prohibited** —
  it conflicts with ADR-0005's "never arbitrary eval" principle and the CSP
  constraint. If no CEF callback fits cleanly, escalate as a separate design
  question rather than reach for JS injection.
- `⌘⇧`-click → foreground tab (deferred unless requested)
- Tab tooltip: 200ms hover → full page title + full URL, two lines

**Tab title flow.** R2's `OnTitleChange` wires sidebar tab row updates
alongside omnibox + window title.

### P4 — Status line + read-only plugin contract

**Most consequential design surface.** Locks the schema future plugins
depend on.

**ADR required:** "Status-line plugin API — read-only v1; clickable v2
planned." Records (a) the **declarative-registration model** (plugins
declare elements in a top-level `statusline` table at load time, reconciling
with ADR-0001's declarative-tables-only mandate — no imperative `register`
or dynamic event subscription), (b) the element schema and capability
gates, (c) the **`statusline.publish-clickable` reserved capability name**
and v2 forward-compatibility commitment (clickable elements will be
additive; existing read-only registrations are unaffected; the capability
is named now so user-facing capability grant UI is forward-compatible),
(d) layout and overflow semantics, (e) theme-override surfaces.

**Element schema** (same for built-ins and plugins):

```lua
{
  id        = string,                              -- unique
  zone      = "left" | "center" | "right",
  priority  = int,                                 -- higher = closer to zone outer edge
  kind      = "text" | "icon" | "icon-text",       -- future: "button"
  text      = string?,
  icon      = string?,                             -- "lucide:type" or "inline:<svg>"; themable
  color     = "fg" | "accent" | "warn" | "mute",   -- token name only, no raw values
  tooltip   = string?,                             -- P1 tooltip primitive

  -- RESERVED for a later phase; v0.1 logs warning + ignores
  action    = function?,                           -- click handler
  disabled  = boolean?,
}
```

**Plugin API — declarative registration (ADR-0001 compliant):**

```lua
return {
  manifest = { name = "wordcount", version = "0.1.0", ... },

  -- Declared at load time; load-step 3 verifies contract conformance
  statusline = {
    {
      id        = "wordcount",
      zone      = "right",
      priority  = 50,
      kind      = "text",
      text      = "0 words",
    },
  },

  events = {
    -- Declared event handlers can update element state
    ["tab:loaded"] = function(tab)
      local words = count_words(tab)
      mote.statusline.set("wordcount", {
        text = string.format("%d words", words)
      })
    end,
  },
}
```

**One host API:** `mote.statusline.set(id, payload)` — updates an
already-declared element. Idempotent. Called from declared event handlers
only.

**No imperative API surface.** No `register`, no `unregister`, no dynamic
`mote.events.on()` subscriptions — those would violate ADR-0001's
declarative-tables-only model. Elements appear when the plugin is loaded
(declared in the `statusline` table) and disappear when the plugin is
disabled (declared elements are removed by the runtime).

**Capability gates.**
- Publishing read-only elements: no capability required (just publishing a
  label)
- Populating with data: existing capabilities apply (read-tabs, etc.)
- Future click handlers: will require new `statusline.publish-clickable`
  capability (named now, enforcement code lands later) — kept separate so
  users can grant click handlers only to plugins they specifically trust

**Built-ins via the same schema** (live in chrome page, not as a plugin):

| id | zone | priority | kind | content |
|---|---|---|---|---|
| `mote.mode` | left | 100 | text | `NORMAL` in accent |
| `mote.security` | left | 50 | icon-text | `🔒 https · tls 1.3` (or `⚠ http · insecure`) |
| `mote.tabcount` | right | 50 | text | `3 tabs` — current workspace, matches sidebar |

Removed: `theme: dusk` (dev noise), `142mb` (not actionable; plugins can
re-add). `7 tabs` vs `3 open` mismatch closes by spec.

**Layout.**
- 24px tall, 1px top hairline, `surface-1` bg
- Three zones; within a zone, `·` separator in `var(--fg-mute)`
- Within a zone: priority order (high → outer edge)
- Overflow: truncate lowest-priority element first with ellipsis; never wrap
- Hover any element with `tooltip` → P1 tooltip after 200ms

**Forward compatibility for click handlers (v2).**
- Schema reserves `action` + `disabled` fields; v0.1 ignores with warning log
- Routing path is already correct (status line lives in chrome page; future
  clicks route through chrome page click router; no chrome-overlay-input-routing
  ambiguity per existing seam)
- v0.1 implementations tolerate `action`/`disabled` gracefully — future
  plugins running on v0.1 degrade to read-only

**Theme overrides.** Standard theming contract:
- `theme:set_icon("statusline.mote.security", "lucide:lock-keyhole")`
- `theme:style(".sl-element[data-id='mote.mode']", { font_weight = 700 })`

### P5 — Table-stakes additions

Five interactive features. Each small individually; together they make Mote
a credible daily-driver browser for reading.

**`⌘F` find-in-page.**
- `[find]` mode in omnibox, accent color
- Typing → CEF `Find` API on active content page
- Match count rendered right of field: `3 / 17`
- `Enter` / `⌘G` next · `⌘⇧G` previous · `Escape` close
- Matches highlighted by CEF's built-in highlight

**Right-click context menu** on links / pages (CEF `OnBeforeContextMenu`
intercepted; render Mote-styled popover, not CEF's native gray menu):
- On link: open in new tab · open in new window (deferred if `⌘N` not done)
  · copy link · copy as markdown
- On page: reload · back / forward (disabled if no history) · view page
  source (`view-source:<url>`)
- On selected text: copy · search Google · ask Mote (`[ask]` deferred,
  hidden in v0.1)
- On image: copy image URL (save image deferred to downloads phase)
- Visual: same construction as workspace popover from P1

**Hover-URL preview in status line.** CEF `OnStatusMessage` →
`mote.statusline.set("mote.hoverurl", { text = url })`. Center zone gets its
first real built-in occupant. Disappears on `mouseleave` or after 3s of no
movement.

**Zoom** `⌘+` `⌘-` `⌘0`. CEF `SetZoomLevel` per browser host. Per-tab in
v0.1. Status-line transient `mote.zoom` element shows `zoom 125%` for 1.5s.
Per-origin persistence → P6 settings panel.

**Reopen closed tab** `⌘⇧T`. Per-window stack of
`ClosedTab{url, title, workspace, position, closed_at}`, capped at 25.
Recreates in original workspace + position. No persistence across app
restart (session-restore is its own feature).

### P6 — First-party settings panels

One new rail icon (`cog`), opens multi-section panel. Rail icons are
precious; one settings icon with sections matches Chrome/Safari/Firefox
convention.

**ADR required:** Settings panel layout + deep-link contract + write
target. Records (a) the multi-section panel layout (one rail icon → tabbed
sections), (b) the `mote://chrome/settings/<section>` deep-link scheme, (c)
the **`managed.lua` mutation layer as the write target** (per
canonical-config-set memory: `plugins.lua` + `secrets.lua` remain user-owned
read-only), (d) **why URL-source plugin install is deferred** — remote
sources introduce supply-chain and trust-establishment concerns that
warrant their own design pass (signature verification, source-of-truth
attribution, revocation), distinct from local file-picker install which
inherits trust from the user's filesystem.

Each section deep-linkable: `mote://chrome/settings/{general,plugins,integrity,keybinds}`.
Palette commands jump straight in.

**General.**
- Theme dropdown (dusk · vellum · installed customs)
- Default search engine (name + URL template)
- Hardware acceleration toggle
- Per-origin zoom persistence toggle (links P5 zoom into permanent)
- Startup behavior (only ships if session-restore exists; otherwise omitted)

Backed by `managed.lua` mutation layer. `plugins.lua` and `secrets.lua`
remain user-owned read-only.

**Plugins.** Each row: name + version + integrity badge + capability chips
(clickable for "what this plugin uses this for"). Actions: disable /
revoke specific cap / uninstall. Click row → details (manifest, source,
loaded scripts). Install via file picker (zip/tarball) → existing integrity
verification + approval dialog flow. URL install deferred (supply-chain
concern).

**Integrity.** Promotes current chrome-overlay integrity surface into
first-class view. Sortable columns (plugin / status / last-verified),
filter, search, `[reverify all]`, per-plugin drill-down. Chrome-overlay
stays for startup-blocking case; two surfaces, one data model.

**Keybinds.** Read-only reference in v0.1. Columns: action · chord (`<kbd>`
primitive) · scope (global/chrome/content) · source (built-in/plugin/user
override). Grouped by scope; search filter. Generated from live keybind
registry so it stays in sync. User customization deferred — additive later.

## ADR coverage

Per project CLAUDE.md, ADRs land for surfaces that lock public contracts.
Waves requiring new ADRs:

| Wave | ADR title | Why |
|---|---|---|
| R3 | Popup behavior: in-window tab + user-gesture activation + opt-out path | Plugins/themes may want opt-out later (OAuth, etc.) |
| R4 | Browser-keybind suite + scope rules + `⌘W` contextual rule + plugin-keybind closure | Keybind table is now a public surface |
| P1 | Themable icon contract (`theme.icons.<action>` + `theme:set_icon`) | New theme API extension spanning P1/P2/P4; not covered by ADR-0003 |
| P1 | Rail-as-plugin-declarable-slot | First plugin-authored chrome-UI surface; isolation boundary per ADR-0007 needs explicit ruling |
| P3 | `mote://newtab` slot architecture + `mote://` global-request-context constraint | Sets pattern for future `mote://` surfaces |
| P4 | Status-line plugin API — declarative-registration, read-only v1, clickable v2 reserved capability | Most consequential public API in this phase; reconciles with ADR-0001 |
| P6 | Settings panel layout + deep-link contract + `managed.lua` write target + URL-install deferral rationale | Sets pattern every later settings-shaped feature follows |

Each ADR lands as the first commit in its wave's series. Per
`adr-approval-required` memory: no auto-finalization without user sign-off.

adr-review skill dispatched (a) after this plan, before implementation; and
(b) after each wave's implementation, before the wave is claimed complete.

## Testing strategy

`feedback-always-write-tests`: every feature and every bug gets a test.
Closest-seam coverage when CEF/timing makes integration hard.

**Unit tests per wave.**
- R1: page-fanout function with mock pages
- R2: address/title sync — mock chrome bridge, verify event payload shape
- R4: keybind table + scope rules (pure-Rust)
- P1: tooltip primitive (DOM test in chrome test harness)
- P3: tab-row state machine (active / hover / closing)
- P4: status-line element schema (validation, ordering, truncation,
  ignore-reserved-fields warning logging)
- P5: per-table-stake handlers (find-mode state, zoom clamp,
  closed-tab stack eviction)
- P6: settings-section routing, capability-revoke action, integrity
  sort/filter

**Integration tests** where unit can't cover the seam.
- R1: chrome resize fanout — drive actual `Resized` through shell loop
- R3: `OnBeforePopup` interception — closest-seam mock of CEF lifespan
- P4: capability gate end-to-end — fixture plugin tries clickable element,
  assert warning + click ignored

**Live verification as the acceptance gate** for every commit:
- `hyprctl` for window manipulation
- `ydotool` for click / hover / scroll / key injection
- `grim` for screenshots (both themes, side-by-side)
- Every commit body embeds: before/after screenshot paths, recorded
  interaction sequence, both-themes confirmation
- For repair-pass commits: three bug-fix questions (which gate / why missed /
  what changed)

**Bonus closures.**
- `materialize_active_if_placeholder` regression test (existing backlog
  item) lands as part of P3's tab-page-lifecycle test setup
- Theme parity verification becomes a per-commit checklist item (closes the
  "vellum not eyeballed" backlog item)

## File overlap + dispatch strategy

Per CLAUDE.md "plan PR topology by file overlap, not topic" — same rule
applies to commit topology in this session.

| Wave | Primary file zone |
|---|---|
| R1 | `crates/mote-shell/src/{app,window,event}.rs` + CEF host wrappers |
| R2 | `crates/mote-shell/src/chrome_bridge.rs` + `crates/mote-cef/src/handlers/display.rs` |
| R3 | `crates/mote-cef/src/handlers/life_span.rs` + shell tab create |
| R4 | `crates/mote-shell/src/keybind.rs` + close button in `mote://chrome/index.html` |
| P1 | `mote://chrome/*.{html,css}` + `crates/mote-themes/src/tokens.rs` |
| P2 | `mote://chrome/omnibox.*` + shell omnibox events |
| P3 | `mote://chrome/tabs.*` + `mote://chrome/newtab.html` + shell tab events |
| P4 | `mote://chrome/statusline.*` + `crates/mote-plugins/src/host_api/statusline.rs` + ADR |
| P5 | `mote://chrome/find.*` + `mote://chrome/contextmenu.*` + `crates/mote-cef/src/handlers/{context_menu,display}.rs` + keybind table |
| P6 | `mote://chrome/settings/**` + `crates/mote-plugins/src/host_api/manager.rs` |

**Parallelization plan after R lands.**
- R1 first; R3 → R2 → R4 by ascending blast radius
- P1 must land before P2–P6 (tooltip primitive + token/icon-theming + rail
  tooltip system dependencies)
- After P1: P2, P3, P4, P5, P6 dispatch in parallel via `code-implementer`
  subagents (per `implementation-directive` memory: agent-driven, commit to
  main, briefed with `mote-design` + `frontend-design`)
- One serial dependency: P5's hover-URL piece serializes after P4 lands; the
  rest of P5 doesn't

## Branch + commit strategy

Per user direction in this session: commit directly to main; no PRs. The
existing `implementation-directive` memory already names this posture for
Mote development work.

**Commit message template** (conventional + body):

```
<type>(<scope>): <subject 50ch max>

<body — what changed and why, 72ch wrap>

Closes: ui-polish-backlog.md L<line> (<item>)
Screenshots: docs/screenshots/<wave>/{before,after}-{dusk,vellum}.png
Interaction: docs/screenshots/<wave>/interaction.gif (or screenshot sequence)

# For repair-pass commits — also answer:
Gate that should have caught: <which test / process>
Why it didn't: <reason>
What changed so the next one does: <process / test addition>

# If ADR landed in this commit or wave:
ADR: docs/adr/NNNN-<topic>.md
```

ADRs land as separate commits ahead of implementation commits in each wave.

Commit order across waves: by ascending blast radius within each phase; R
before P; P1 before parallel P2–P6.

## Acceptance criteria — "polish phase done"

The phase is complete when all of:

1. All 10 wave commit series merged to main
2. `~/.claude/projects/.../memory/ui-polish-backlog.md` has every
   starting-checklist item resolved (closed or explicitly deferred with
   rationale)
3. Handoff screenshot pair (dusk + vellum) of polished chrome lives in
   `docs/screenshots/2026-06-polish-phase/`
4. Both themes verified end-to-end by manual walk-through (same kind of
   walk that opened this conversation, but on polished chrome)
5. `handoff.md` memory updates with new resume point

After phase done: open the polish backlog for the next round of items
collected during the work — UX survey expansion is expected per
`feedback-ui-polish-expansive`.

## Polish backlog mapping

Items from `ui-polish-backlog.md` closed by this design:

- "New-window popups open without Mote chrome" → R3
- "New-tab content is raw data:text/html placeholder" → R2 (title flow) + P3 (newtab page)
- "Workspace switch UX gaps" (empty workspace placeholder, viewport refresh) → R2 + P3
- "Workspace switcher strip" → P1 (becomes bracket-chip)
- "Bookmark/history sidebar header + button" → P1 (becomes global header `[⊕]`)
- "⌘T doesn't work from content-page focus" + full browser-shortcut suite → R4
- "Workspace switching has no keybinds" → R4 (`⌘1`–`⌘9`)
- "Favicons in bookmark/history rows" — deferred (still requires per-URL
  fetch + cache + security review); P1's favicon slot redesign at least
  stops the checkbox misread
- "Vellum not visually proven on new surfaces" → per-wave verification gate
- "HiDPI gaps from #7" → R1 (all-pages fanout)
- "Bookmarks plugin still uses pipe codec" → P4 (when statusline schema
  lands, bookmarks migrates to mote.json in the same wave; trivial)
- "os.time host API decision" — deferred; not in scope
- "History max_entries LRU trim" — deferred; not in scope
- "materialize_active_if_placeholder has no unit test" → P3 setup
- "mote-app lacks $ORIGIN rpath" — deferred (Phase 9 packaging)
- "serde_json workspace-deps consolidation" — deferred (chore, not polish)

## Memory updates after phase

- `ui-polish-backlog.md` — items resolved or moved to deferred
- `phase-progress.md` — polish phase marked closed
- `handoff.md` — new resume point
- New backlog file or fresh section for items collected during the polish
  work (expansion expected)

## Open follow-ups beyond this phase

- `⌘`-click foreground tab variant (`⌘⇧`-click) if user requests
- `⌘N` new window if multi-window state proves needed
- Devtools/`inspect` chrome surface (deferred from P5 context menu)
- Downloads UI (deferred from P5)
- Per-origin permission UI (slots ready in P2 security popover; behavior not
  yet implemented)
- URL-source plugin install (deferred from P6 plugins section)
- User keybind customization (deferred from P6 keybinds section)
- Session restore (deferred from P6 general section)
- `[ask]` mode wiring (deferred to AI phase)
- AI-forward newtab content (deferred to AI phase; slot ready)
- Status-line click handlers v2 (forward-compatible schema; reserved
  capability name)
- Bookmarks-bar quick-links (deferred; could bind to newtab.center slot)
