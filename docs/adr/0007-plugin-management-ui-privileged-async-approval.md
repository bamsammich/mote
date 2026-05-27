# ADR-0007 — Plugin Management UI: Privileged-Chrome Surfaces with Async Approval

- **Status:** Accepted
- **Date:** 2026-05-27

---

## Context and Problem Statement

Phase 3's install→approval flow and integrity-panel actions must (a) render an
approval dialog and (b) receive the user's grant/narrow/deny and panel actions
back in the shell. Two current-architecture facts constrain this: plugins load
synchronously *before* the winit event loop starts (so no dialog can take input
during boot), and the bridge (ADR-0005) installs `window.cefQuery` only on the
privileged `mote://chrome` origin — Phase 2 deliberately moved overlays to the
unprivileged `mote://overlay` origin, which has no bridge and cannot call back.

## Decision Drivers

- An approval dialog needs a running event loop + input; it cannot run during the
  pre-loop synchronous boot.
- A nested CEF message-pump loop inside `Runtime::load` risks re-entrancy/deadlock.
- Only `mote://chrome` can call back into the shell; the approval dialog and
  integrity panel are trust-critical UI (the security boundary itself), not
  web-adjacent overlays.
- The runtime already models async re-approval (`reload(require_reapproval=false)`
  keeps the old instance and returns `ApprovalDenied`).

## Considered Options

- **Async awaiting-approval, privileged-chrome surfaces** (this ADR).
- **Modal nested-pump approval** on either origin (re-entrancy risk; can't run at boot).
- **Keep panel/dialog unprivileged + a bespoke callback relay** through the chrome page.

## Decision Outcome

Chosen: **(1)** Plugin loading moves to *after* the event loop is live; a plugin
requiring approval enters an **async "awaiting approval"** state and resolves via
an `approve_plugin` bridge op (no nested pump). **(2)** The approval dialog and
integrity panel render on the **privileged `mote://chrome`** origin — they are
Mote's own trusted chrome, so they belong with tabs/urlbar, not on the
unprivileged `mote://overlay` origin reserved for web-adjacent surfaces.

### Consequences

- Good, because it avoids CEF re-entrancy, fits the runtime's existing async
  re-approval model, and keeps trust-critical UI on the tamper-resistant origin.
- Good, because panel actions (revoke/update/rollback) get a real bridge path.
- Bad, because it reverses Phase-2's "overlays are unprivileged" hardening *for
  these two specific surfaces*; the boundary is now "trusted Mote chrome vs
  web-adjacent overlay," which must be applied deliberately per surface.
- Bad, because the shell's boot sequence must be restructured (load plugins on a
  post-loop tick rather than synchronously in `PluginHost::boot`).

## Relationship to ADR-0005

Complements, does not supersede, ADR-0005 (two-layer isolation stands). It
classifies the approval dialog + integrity panel as **privileged-chrome**
surfaces and narrows the unprivileged `mote://overlay` origin to web-adjacent
overlays. The `approve_plugin` and panel-action ops are new privileged-origin
ops subject to ADR-0005's existing origin gate.
