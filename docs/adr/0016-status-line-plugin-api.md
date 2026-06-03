# ADR-0016 — Status-Line Plugin API: Declarative Registration, Read-Only v1, Clickable v2 Planned

- **Status:** Accepted (approved by the maintainer 2026-06-02)
- **Date:** 2026-06-02

---

## Context and Problem Statement

The chrome's status line currently shows three built-in elements (mode,
security, tab-count) wired into the chrome JS via ad-hoc message types
the shell pushes. Plugins have no way to publish to it. The user has
been explicit that the status line is one of the surfaces plugin authors
will want to extend (word count, sync status, AI status, weather,
whatever). P4 introduces the plugin API. **The user has also explicitly
flagged that v2 click-handler support must not be precluded by the v1
shape.** Locking the schema, registration model, capability gates, and
forward-compatibility hooks needs a recorded decision before any plugin
in the wild registers against this API.

## Decision Drivers

- The registration model must satisfy ADR-0001 — plugins declare via
  module-level tables, not via imperative `register()` calls at runtime.
- The schema must be forward-compatible with v2 click handlers; adding
  the `action` field later cannot break v0.1 registrations.
- Built-ins and plugin-registered elements should share the schema, not
  be separate paths. Same shape means same rendering code, same tooltip
  primitive, same theme overrides.
- Color is a token name (per ADR-0003 + ADR-0013) — never a raw value;
  themes own the palette.
- The capability gate for v2 click handlers must be named now so
  user-facing capability grant UI is forward-compatible; enforcement
  ships with v2.

## Considered Options

- **Imperative `mote.statusline.register(...)` from plugin code.**
  Rejected: violates ADR-0001's declarative-tables-only model.
- **Declarative `statusline = { ... }` top-level manifest table; one
  host API for updates (`mote.statusline.set(id, payload)`)** (this
  ADR). Mirrors ADR-0014's rail declaration pattern; updates flow from
  declared event handlers, not from imperative registrations.
- **Defer v2 click-handler reservation; ship v1 with no forward
  hooks.** Rejected: the user explicitly raised click-handler
  forward-compatibility as a hard constraint; the cost of reserving the
  fields now is one schema line, the cost of NOT reserving them is
  breaking every v0.1 plugin when v2 ships.

## Decision Outcome

Chosen: **Status-line elements are declared in a top-level `statusline =
{ ... }` plugin manifest table (per ADR-0001), updated via the one host
API `mote.statusline.set(id, payload)` from declared event handlers. The
v1 schema reserves `action` and `disabled` fields for v2 click
handlers; the capability name `statusline.publish-clickable` is named
now (not enforced in v1).**

### Plugin manifest declaration

```lua
return {
  manifest = { name = "wordcount", version = "0.1.0", ... },

  -- Declared at load time; load-step 3 validates conformance
  statusline = {
    {
      id        = "wordcount",
      zone      = "right",
      priority  = 50,
      kind      = "text",
      text      = "0 words",
      tooltip   = "current page word count",
    },
  },

  events = {
    -- Declared event handlers can update declared element state
    ["tab:loaded"] = function(tab)
      local words = count_words(tab)
      mote.statusline.set("wordcount", {
        text = string.format("%d words", words)
      })
    end,
  },
}
```

### Schema (v1)

| Field | Type | Required | Notes |
|---|---|---|---|
| `id` | string | yes | Unique within the plugin's `statusline` table; namespaced runtime-side as `<plugin>.<id>` to avoid cross-plugin collision |
| `zone` | enum `"left"`, `"center"`, `"right"` | yes | Layout zone |
| `priority` | integer | yes | Higher = closer to the zone's outer edge; ties broken by id alphabetical |
| `kind` | enum `"text"`, `"icon"`, `"icon-text"` | yes | v2 will add `"button"` |
| `text` | string | only if `kind ≠ "icon"` | Rendered as plain text; HTML escaped at render time |
| `icon` | string | only if `kind ≠ "text"` | Format per ADR-0013: `"lucide:<name>"` in v0.1; rejected at registration if unknown |
| `color` | enum `"fg"`, `"accent"`, `"warn"`, `"mute"` | no, defaults to `"fg"` | Token name only; raw color values rejected |
| `tooltip` | string | no | Plain text; HTML escaped; rendered via P1's tooltip primitive on hover |
| **`action`** | function | **NO — reserved for v2** | v0.1 rejects this field with a warning log + ignores the registration's `action` value; element still publishes |
| **`disabled`** | boolean | **NO — reserved for v2** | v0.1 ignores; element rendered as enabled regardless |

### Host API (v1)

```lua
mote.statusline.set(id, { text = "...", icon = "...", color = "...", tooltip = "..." })
```

- Updates state for an already-declared element. Looks up by the
  plugin-scoped id (`<plugin>.<id>` after namespacing).
- Idempotent. Setting the same value twice is a no-op.
- Called from declared event handlers, not at module load time.
- Rejects updates to fields not in the v1 schema (including `action`,
  `disabled`).
- Rejects updates to undeclared element ids (typo protection).
- **No `register`, `unregister`, or `mote.events.on` API surface.**
  Those would violate ADR-0001.

### Built-ins use the same schema

Built-in elements live in the chrome page (not in a plugin manifest)
but use the identical schema. The chrome's bootstrap registers them
once at startup. v0.1 ships three:

| id | zone | priority | kind | source |
|---|---|---|---|---|
| `mote.mode` | `left` | 100 | `text` | shell mode (NORMAL / INSERT / etc.); pushed via existing bridge message; color `accent` |
| `mote.security` | `left` | 50 | `icon-text` | active tab security state; icon `lucide:lock` / `lucide:unlock`; text `https · tls 1.3` / `http · insecure`; color `accent` / `warn` |
| `mote.tabcount` | `right` | 50 | `text` | current workspace's tab count (matches sidebar, not global) |

P5's hover-URL feature ships a fourth built-in (`mote.hoverurl`, center
zone) — that lands with P5, not P4.

Removed: the existing dev-noise `theme: dusk` and `142mb` memory
readout. Plugins can re-add the memory readout if a user wants it.

### Capability gates

| Action | v0.1 | v2 (clickable) |
|---|---|---|
| Declare element (publish text/icon) | no capability required | no capability required |
| Update element state from a declared event handler | no capability required (event handler is already capability-gated for what it can READ) | unchanged |
| Declare element with `action = function` | rejected with warning log | requires `statusline.publish-clickable` capability |
| Click handler fires in plugin's Lua sandbox | n/a | runs with the plugin's existing capability set |

The capability name `statusline.publish-clickable` is **reserved now,
not enforced in v1**. Plugin manifests listing it in v1 succeed in
load-step 3 conformance (the capability registry must include the name
or load-step 3 rejects it — registry update lands in this wave). v0.1
plugin code that requests this capability *and* includes an `action`
field gets the warning-log + ignore behavior; no enforcement until v2
ships the click-handler runtime.

### Layout + rendering

- Status line: 24px tall, 1px top hairline, `surface-1` bg
- Three zones; within a zone, elements separated by `·` in `var(--fg-mute)`
- Within a zone: priority order — high → outer edge of the zone
- Overflow: truncate the lowest-priority element first with ellipsis;
  the truncated element shows a "more" tooltip on hover that lists the
  full text
- Never wrap; never resize the status line vertically
- Hover any element with `tooltip` set → P1 tooltip primitive after
  200ms

### Theme overrides

Standard theme-contract overrides apply via existing APIs:

```lua
-- Override the built-in security element's icon for a custom lock glyph
theme:set_icon("statusline.mote.security_https", "lucide:lock-keyhole")

-- Override styling of any element via a CSS selector
theme:style(".sl-element[data-id='mote.mode']", { font_weight = 700 })
```

### Forward compatibility (v2 click handlers)

When click handlers ship in a future ADR:

- Schema gains `action` and `disabled` semantics — additively, no v1
  registration breaks
- Routing path is already correct: status line lives in the chrome
  page, future clicks route through chrome's click handler (no new
  chrome-overlay-input-routing seam — same chrome page as today)
- `statusline.publish-clickable` capability is activated; plugins not
  holding it have their `action` fields ignored (same as v1 behavior)
- v0.1 plugins running on v2 keep their read-only behavior (no `action`
  declared → no clicks); v2 plugins running on v1 degrade gracefully
  (action ignored with warning log) — both directions work

## Consequences

- Good: declarative model satisfies ADR-0001; one host API
  (`mote.statusline.set`) is the minimum surface.
- Good: built-ins use the same schema as plugin registrations — same
  rendering code, no special-case branches.
- Good: forward-compatibility for click handlers is locked in now; v1
  plugin manifests will keep working when v2 ships.
- Good: capability name reserved now means the user-facing grant UI
  doesn't need a name change when v2 lands.
- Bad: plugins lose the ergonomics of imperative `mote.events.on`
  subscriptions; the declared-event-handler-only model is stricter.
  Mitigated by ADR-0001 making this consistent across every plugin
  registration surface.
- Bad: per-update field validation (rejecting `action` in v1) is
  defensive code that runs on every `set` call. Negligible cost for
  the safety it provides.
- Bounded scope: this ADR is the v1 element shape and the host-API
  shape only. v2 click handlers, additional `kind` values, additional
  zones, animation/transition support, and any per-element interaction
  modes (drag, drop, focus) are all future ADRs.

## Relationship to existing ADRs

- **Inherits from ADR-0001** (Declarative Plugin Registration). Plugin
  declarations go through the same module-level `M.<feature>` table
  pattern as everything else; load-step 3 validates conformance.
- **Inherits from ADR-0003** (Chrome UI as HTML/CSS in CEF). The status
  line is part of the chrome HTML; rendering uses existing CSS-token
  and theme-override mechanisms.
- **Inherits from ADR-0013** (Themable Icons). Element `icon` field
  uses the `"<pack>:<name>"` format; default theme provides defaults
  for built-in elements.
- **Parallels ADR-0014** (Rail-as-Plugin-Slot). Same declarative-table
  pattern, same load-step 3 conformance check, same future-ADR pattern
  for the runtime extension surface.
- **Forward-references a future ADR** for v2 click handlers, scoping
  the capability enforcement, the click-routing path through the chrome
  page, and the per-element interaction modes.
