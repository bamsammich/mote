# ADR-0014 — Rail as a Plugin-Declarable Slot: Isolation Boundary, Declaration Model, Collision Policy

- **Status:** Accepted (approved by the maintainer 2026-06-02)
- **Date:** 2026-06-02

---

## Context and Problem Statement

The P1 chrome anatomy redesign defines five sidebar rail slots: the first
three are first-party (tabs, bookmarks, history); the fourth and fifth are
**reserved for plugin contributions**. Today (v0.1) they render an
unbound `[+]` placeholder; the design intent is that a future plugin can
declare a rail icon and supply the panel that appears when the user clicks
it. This is the **first plugin-authored chrome UI surface** Mote will
expose — every prior plugin UI has been Mote-rendered (the integrity
overlay, the approval dialog) using plugin-supplied data, never
plugin-supplied UI. The isolation boundary, declaration model, and
collision policy need to be recorded now so v0.1's placeholder behavior
doesn't constrain the v2 implementation, and so v2 can't accidentally
violate the trust model ADR-0007 established.

## Decision Drivers

- ADR-0007 places trust-critical UI on the privileged `mote://chrome`
  origin. A plugin-supplied DOM running at `mote://chrome` would have
  full host-bridge authority — equivalent to chrome-XSS.
- Plugins must be able to render arbitrary UI in their rail panel —
  bookmarks lists, AI chat surfaces, RSS feeds, color pickers — without
  the chrome having to anticipate every possible shape via structured
  data.
- Plugin → chrome communication (e.g. "open this URL in a new tab")
  must remain mediated by the capability system; a plugin can ask the
  shell to open a URL, but only if its capability set allows tab
  creation.
- v0.1 ships the *placeholder* only. The ADR records the constraints
  the future implementation must honour, not the implementation itself.

## Considered Options

- **Plugin rail panels render at the privileged `mote://chrome`
  origin.** Rejected: plugin-supplied DOM running with chrome authority
  is equivalent to chrome-XSS by design; defeats ADR-0007's trust
  surface.
- **Plugin rail panels render at the privileged origin but inside an
  iframe sandboxed via CSP.** Rejected: iframe sandboxing in
  same-process content is fragile (sandbox escapes via DOM-mutation,
  prototype pollution, etc. have shipped before); a process-/origin-
  level boundary is stronger.
- **Plugin rail panels render at the existing sandboxed
  `mote://overlay` origin** (`PageRole::Overlay`, see
  `crates/mote-cef/src/browser.rs`), with no host-bridge access; plugin
  → host communication goes through the plugin's existing Lua host APIs
  (capability-gated), not through chrome's bridge. (This ADR.)

## Decision Outcome

Chosen: **plugin-supplied rail panels render at the existing sandboxed
`mote://overlay` origin (`PageRole::Overlay`) — never `mote://chrome` —
and have no host-bridge access. Plugin → host communication goes through
the plugin's Lua host API surface, gated by the plugin's existing
capability set. v0.1 ships the unbound `[+]` placeholder; this ADR
constrains the v2 implementation, it does not deliver it.**

### Isolation boundary

| Surface | Origin | Bridge access | Authority |
|---|---|---|---|
| First-party rail panels (tabs, bookmarks, history, settings) | `mote://chrome` | Yes (full enumerated ops) | Mote-authored, trusted |
| Plugin rail panels (slots 4–5) | `mote://overlay` | **No** (`cefQuery` binding not installed for overlay origin per ADR-0005) | Plugin-authored, sandboxed |
| Integrity / approval overlay surfaces | `mote://chrome` (legacy) → `mote://overlay` (target) | Per surface | Mote-authored |

Plugins author panel HTML/CSS/JS as static files shipped in the plugin
package. The runtime serves these from the plugin's content-addressed
asset path under `mote://overlay/plugins/<plugin>/<asset>`. CEF loads the
overlay panel as a content browser with `PageRole::Overlay`; the renderer
origin gate at `crates/mote-cef/src/bridge.rs` only installs the host
bridge for `mote://chrome`, so the overlay frame has no `cefQuery`
binding and no path to enumerated chrome ops.

### Plugin → host communication

Plugins can perform host operations from their **Lua side** (the existing
`mote-runtime` host API, capability-gated). The panel's JS communicates
with the plugin's Lua via a *plugin-scoped* message channel (not the
chrome bridge): JS posts a structured message, the Lua side handles it,
and any host op invoked is subject to the plugin's capabilities. This
channel:
- Is installed only for `mote://overlay/plugins/<plugin>/*` frames
- Is scoped to the originating plugin (a panel from plugin A cannot
  reach plugin B's Lua)
- Carries only structured data (JSON-shaped), never code
- Is the subject of its own future ADR specifying message shape, error
  semantics, and Lua-side handler registration

That channel is **not** scoped or implemented in this ADR. v0.1 does
not ship the channel because v0.1 does not ship plugin rail bindings;
the channel design lands with the binding implementation.

### Declaration model

Plugins declare rail bindings in their manifest's existing top-level
table set (per ADR-0001 declarative registration):

```lua
return {
  manifest = { name = "rss-reader", ... },
  rail = {
    {
      slot_id    = "rss-reader",         -- unique within the plugin
      label      = "RSS",                -- shown in tooltip
      icon       = "lucide:rss",         -- themable per ADR-0013
      panel_path = "panels/main.html",   -- served from mote://overlay/plugins/rss-reader/panels/main.html
      capabilities = { "fetch:any-https" },  -- required for the panel to function
    },
  },
}
```

Load-step 3 (contract conformance) validates the rail table the same way
it validates `statusline` entries. Missing capabilities → registration
fails with a clear error.

### Collision policy: more plugin rails than slots

The default theme reserves 2 plugin slots; future themes may declare
more. When more plugin rails are declared than slots available:

1. **User intent wins**: if the user has explicitly enabled rail
   visibility for a plugin (via the P6 settings UI), that plugin
   binds its slot first.
2. **Plugin-declared `priority` next** (integer, higher = preferred —
   matches the status-line schema).
3. **Plugin name alphabetical order** as a deterministic tiebreaker.

Plugins that lose the collision are not silently dropped — they appear
in the P6 settings UI's "available plugin panels" list with a "no rail
slot available" annotation. The user can swap which plugins get visible
rails from there. This is the **same disclosure pattern** as the
status-line overflow (truncation indicator with a hover for "more
elements").

### v0.1 scope: visible placeholder + manifest schema lock-in

v0.1 ships **two** pieces of the design:

1. **Visible placeholder.** P1 renders the unbound `[+]` glyph in slots
   4 and 5 with the tooltip "available — plugins can add panels here."
   Clicking the placeholder opens the palette filtered to
   plugin-discovery (`[cmd] install panel plugin`).
2. **Manifest schema + validation.** The `rail = { ... }` manifest
   table is read and validated at load-step 3 (the existing declarative
   conformance check, per ADR-0001). Plugin authors can write
   rail-binding manifests against v0.1 today and the runtime catches
   schema errors at load time — missing required fields, capabilities
   the plugin doesn't hold, malformed icon source per ADR-0013, etc.
   Plugin authors get a real head start; the project gets
   manifest-validation test coverage early; the schema is concrete and
   testable, not just hypothetical.

What v0.1 explicitly does NOT ship:

- The JS↔Lua scoped channel (no plugin panel JS runs in v0.1)
- Panel asset routing under `mote://overlay/plugins/<plugin>/*`
  (assets are not served in v0.1)
- The actual binding of a declared `rail` entry to a slot (declarations
  are accepted and validated, but the panel does not appear in the UI;
  the placeholder still shows)
- The P6 settings UI for managing which plugin-declared rails are
  visible (collision policy is *defined* in this ADR but not
  *exercised* in v0.1 because nothing actually binds)

The split: **schema is locked in, runtime binding is deferred.** A
plugin author shipping a manifest with `rail = { ... }` against v0.1
gets a successful load + a clear "rail bindings are accepted but not
yet rendered in this Mote version" log message; the same manifest will
just work when the v2 implementation ships, with no schema changes
needed.

## Consequences

- Good: plugin UI cannot reach the privileged chrome origin; ADR-0007's
  trust surface is preserved by construction.
- Good: plugins can still render arbitrary HTML/CSS/JS for their
  panels — the sandbox is at the origin level, not the content level.
- Good: declaration goes through the existing manifest table (ADR-0001),
  so no new registration model is introduced.
- Good: collision policy + the "no rail slot available" disclosure
  pattern prevents silent plugin failure when users have more eligible
  plugins than slots.
- Bad: plugin panel JS cannot directly call chrome ops (e.g. "open this
  URL"); it must round-trip through the plugin's Lua side, which adds
  latency. This is intentional — direct chrome-op access from plugin
  JS would defeat the isolation — but plugin authors will feel it.
- Bad: a future bug in the renderer origin gate would let an overlay
  frame reach chrome ops, defeating the isolation. Mitigated by the
  existing gate being a compile-time constant (ADR-0005); same risk as
  any chrome-origin-isolation regression today.
- Bounded scope: v0.1 ships the visible placeholder + the manifest
  schema (declaration is read, validated, and a clear log message
  acknowledges the binding without rendering). The runtime binding
  (JS↔Lua channel, panel asset routing under `mote://overlay/plugins/`,
  P6 settings UI for rail management) requires its own ADR and its own
  scoped implementation pass. This ADR sets the guardrails the future
  work must honour AND ships the schema concretely enough that plugin
  authors can write rail-binding manifests today.

## Relationship to existing ADRs

- **Refines ADR-0007** (Plugin Management UI — Privileged Async
  Approval). ADR-0007 establishes that trust-critical UI lives on the
  privileged origin; this ADR scopes the inverse — that plugin-authored
  UI must NOT live on the privileged origin — and names the existing
  `mote://overlay` sandboxed origin as the home for plugin panels.
- **Inherits from ADR-0001** (Declarative Plugin Registration). Rail
  bindings declare via a top-level `rail` table in the plugin manifest,
  validated at load-step 3, consistent with how `statusline` will work
  (per the P4 ADR forthcoming) and how `events`, `api`, `hooks` already
  work.
- **Depends on ADR-0005** (Two-Layer Isolation). The "no bridge access
  for overlay origin" guarantee rests on the renderer-side origin gate
  being a compile-time constant; this ADR's safety story is only as
  strong as that gate's enforcement.
- **Depends on ADR-0013** (Themable Icons). Rail icons use the
  `theme.icons.<action>` mechanism for default + theme overrides;
  plugin-supplied icons via `icon = "lucide:rss"` etc. resolve through
  the same `set_icon` source format and sanitization.
- **Forward-references a future ADR** for the v2 plugin-rail
  implementation: manifest schema enforcement, JS↔Lua scoped channel,
  panel asset routing, P6 settings UI for rail management.
