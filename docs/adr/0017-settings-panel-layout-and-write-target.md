# ADR-0017 — Settings Panel: Multi-Section Layout, Deep-Link Contract, `managed.lua` Write Target, URL-Install Deferral

- **Status:** Accepted (approved by the maintainer 2026-06-02)
- **Date:** 2026-06-02

---

## Context and Problem Statement

Mote has no first-party settings UI. Users wanting to change theme, set
the default search engine, manage plugin capabilities, review the
integrity panel, or look up keybinds either edit `~/.config/mote/*.lua`
by hand (per the canonical-config-set memory) or hit a chord-only
surface. P6 introduces the settings panel. This is the first time
Mote's chrome will *write* to a user-facing config file, the first
multi-section in-app surface (general / plugins / integrity / keybinds),
and the place where the eventual URL-source plugin install flow would
live. Each of those is a recorded decision.

## Decision Drivers

- The settings UI is one rail icon, not one icon per section. Rail
  icons are precious; users don't switch between settings sections
  often enough to warrant top-level icons for each.
- Mote's config files are user-owned (per ADR-0006 / the
  canonical-config-set memory: `plugins.lua` + `secrets.lua` are
  read-only to Mote; `managed.lua` is the mutation layer Mote writes).
  Settings writes go to `managed.lua` — never `plugins.lua` or
  `secrets.lua`.
- Each settings section needs a stable URL so palette commands
  (`[cmd] open keybinds`) can jump straight in and so users can
  bookmark or share configuration paths.
- URL-source plugin install (installing a plugin from a remote URL)
  introduces real supply-chain concerns (signature verification,
  source attribution, revocation) that don't apply to local file-picker
  install. Deferring URL install — but recording *why* — keeps the
  v0.1 install path safe while documenting the future surface.

## Considered Options

- **Multiple rail icons (one per settings section).** Rejected: rail
  inflation; users don't switch between Plugins and Keybinds often
  enough to warrant the visual cost.
- **One rail icon → single-page settings.** Rejected: cramped at any
  reasonable window size; doesn't support deep-linking.
- **One rail icon → multi-section panel with tab strip + deep-link
  contract** (this ADR). One icon, multi-section panel, each section
  individually addressable via URL.

## Decision Outcome

Chosen: **one `cog` rail icon opens the Settings panel; the panel is
multi-section with four sections in v0.1; each section is deep-linkable
via `mote://chrome/settings/<section>`; user-driven writes go through
`managed.lua` (per ADR-0006 + canonical-config-set memory); URL-source
plugin install is deferred with its rationale recorded.**

### Layout

```
┌────┬──────────────────────────────────────────────────────────┐
│ ▣  │ [settings]                                                │
│ 🔖 ├───────────────────────────────────────────────────────────│
│ 🕒 │ general · plugins · integrity · keybinds                   │  section tabs
│ ⚙  ├───────────────────────────────────────────────────────────│
│ ▫  │                                                            │
│ ▫  │   active section content                                   │
│    │                                                            │
└────┴────────────────────────────────────────────────────────────┘
```

- Rail icon: `lucide:cog` (themable per ADR-0013, action name
  `rail.settings`)
- Settings panel uses the same sidebar shell as tabs/bookmarks/history
- Section tab strip uses the bracket-lockup pattern (`[settings]` +
  `general · plugins · integrity · keybinds`)
- Single active section content area below the tabs

### Deep-link contract

Each section is addressable:

| URL | Section |
|---|---|
| `mote://chrome/settings/general` | General |
| `mote://chrome/settings/plugins` | Plugins |
| `mote://chrome/settings/integrity` | Integrity |
| `mote://chrome/settings/keybinds` | Keybinds |

- Navigating to the URL via the omnibox or a palette command opens the
  settings panel with that section active
- The chrome page reads the path fragment on load and renders the
  matching section
- Adding a new section requires extending the URL whitelist; arbitrary
  `mote://chrome/settings/<anything>` is NOT permitted (defense against
  future typo-driven 404s)
- Subject to the `mote://` global-request-context constraint
  recorded in ADR-0015 — settings pages load only in the global request
  context, never per-profile

### Section scope for v0.1

**General**:
- Theme dropdown (dusk · vellum · installed customs)
- Default search engine (name + URL template, e.g.
  `https://duckduckgo.com/?q={q}`)
- Hardware acceleration toggle
- Per-origin zoom persistence toggle (links P5's session zoom to
  managed.lua persistence)
- Startup behavior (only ships if session-restore exists in v0.1;
  otherwise the row is omitted to avoid misleading toggle)

**Plugins** (read-only enumeration in v0.1; capability-revoke + disable
+ uninstall actions land in v0.1):
- Each row: plugin name + version + integrity badge + granted
  capability chips
- Capability chips clickable for "what this plugin uses this for"
  drill-down view
- Actions: `[disable]` `[revoke <cap>]` `[uninstall]`
- Install: file picker (zip/tarball) → existing integrity verification
  + approval dialog flow
- **URL install button is NOT shown in v0.1** (deferral rationale
  below)

**Integrity** (promotes the existing chrome-overlay integrity surface):
- Sortable columns: plugin / status / last-verified
- Filter by status
- Search by plugin name
- `[reverify all]` action
- Per-plugin drill-down with signature mismatch / file diff detail
- The startup-blocking integrity overlay (chrome-overlay path) is
  unchanged; this section is the day-to-day view

**Keybinds** (read-only reference in v0.1):
- Columns: action · chord · scope (global / chrome / content /
  captured-modal per ADR-0012) · source (built-in / plugin / user
  override)
- Grouped by scope; search filter at top
- Generated from the live keybind registry — stays in sync as chords
  are added
- User chord customization is **deferred** per ADR-0012's explicit
  intent; this section is the reference view that v2 customization
  will write into

### Mutation: `managed.lua` is the write target

Per ADR-0006 (User Config Read-Only) and the canonical-config-set
memory, Mote's chrome must NEVER mutate `plugins.lua` or
`secrets.lua`. User-driven writes from the settings panel go through
**`managed.lua`**:

- The General section's theme/search-engine/HW-accel/zoom-persist
  toggles all write to `managed.lua`.
- The Plugins section's revoke/disable/uninstall actions write to
  `managed.lua`.
- The Keybinds section is read-only in v0.1; v2 customization writes
  to `managed.lua` (deferred to the future user-keybind ADR per
  ADR-0012).

Reads consult the layered config: `managed.lua` overrides applied on
top of `plugins.lua` / `secrets.lua` (the existing layering
mechanism). Settings sections that display effective values show the
*merged* value plus a "modified by managed.lua" indicator when an
override applies; users can clear the override (which removes the
managed.lua entry, falling back to the user-owned value).

### URL-source plugin install deferral

The Plugins section omits a "Install from URL" button in v0.1. **The
deferral rationale is recorded so the eventual revisit has the prior
reasoning, not just the absence:**

- **Signature verification.** A plugin downloaded from a URL needs a
  trust path — author signature, distribution signature, or a trust
  registry. Mote doesn't yet have one. Local file-picker install
  inherits trust from the user's filesystem (the user manually
  downloaded the file); URL install does not.
- **Source attribution and revocation.** When a URL-installed plugin
  is later found malicious, the trust registry must support revocation
  (don't trust future downloads from that source; mark currently-
  installed copies as compromised). Building revocation in v0.1 is
  premature — we don't have the registry, the trust authority, or the
  signing infrastructure.
- **Supply-chain attack surface.** A compromised distribution URL
  (DNS hijack, registrar takeover, TLS MITM, server compromise) is a
  RCE-equivalent attack on Mote users. The mitigation isn't refusing
  to ship the feature — it's shipping the whole trust path together,
  which is its own design pass.
- **Local file-picker install is safe-by-default.** The user has
  already exercised judgement to download the file; Mote's integrity
  verification + approval dialog flow covers what we can verify
  locally.

Future URL-install work requires its own ADR scoping the trust path
(signatures, registries, revocation) and the install pipeline
(staging, integrity recheck, dependency resolution).

## Consequences

- Good: one rail icon, deep-linkable sections, no rail inflation;
  matches user mental model from every other browser.
- Good: `managed.lua` write target preserves user-owned config files
  (ADR-0006); user-edited values are never silently overwritten by
  chrome UI clicks.
- Good: URL-install deferral rationale is recorded; future work has
  the prior reasoning, not just "it wasn't there."
- Good: Keybinds reference uses the live registry — adding chords
  (per ADR-0012) automatically surfaces them; v2 customization is
  purely additive.
- Bad: 4 sections is more surface than R-waves shipped; P6 is
  inherently bigger work than P2 or P5. Mitigated by sections being
  parallel-implementable internally.
- Bad: the "modified by managed.lua" disclosure pattern is new and
  needs UX iteration. v0.1 ships a simple indicator; refinement
  happens during the next polish cycle.
- Bounded scope: this ADR is the v0.1 settings panel layout, the
  deep-link contract, the write-target rule, and the URL-install
  deferral rationale. User keybind customization, URL-source plugin
  install, theme installation flow, and any settings sections beyond
  the four named here are future work with their own ADRs.

## Relationship to existing ADRs

- **Inherits from ADR-0003** (Chrome UI as HTML/CSS in CEF). Settings
  is more chrome HTML/CSS; no new infrastructure.
- **Inherits from ADR-0006** (User Config Read-Only). The
  `managed.lua` write target enforces ADR-0006 at the chrome-UI
  boundary; settings writes never touch `plugins.lua` or
  `secrets.lua`.
- **Inherits from ADR-0015** (`mote://` global-request-context). The
  settings pages are `mote://chrome/settings/*` URLs subject to the
  same global-context constraint.
- **Surfaces ADR-0012** (Browser-Keybind Suite). The Keybinds section
  is the read-only reference view ADR-0012 forward-references;
  v2 user customization writes via this section into managed.lua.
- **Surfaces ADR-0013** (Themable Icons). The General section's theme
  dropdown selects the active theme; icon overrides made by the theme
  are visible across the chrome.
- **Surfaces ADR-0007** (Plugin Management UI). The Plugins section
  is the day-to-day surface for the privileged plugin-approval flow
  ADR-0007 established; revoke/disable/uninstall actions go through
  the same approval/integrity infrastructure.
- **Forward-references** future ADRs for URL-source plugin install,
  user keybind customization, and any expansion of the settings
  panel beyond v0.1's four sections.
