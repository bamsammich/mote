# ADR-0011 — Popup Behavior: In-Window Tab + User-Gesture Activation + Opt-Out Path

- **Status:** Accepted (approved by the maintainer 2026-06-01)
- **Date:** 2026-06-01

---

## Context and Problem Statement

CEF's default popup behavior — when content invokes `window.open(...)`, when a
link with `target=_blank` is clicked, or when a middle-click opens a link —
creates a new chromeless OS window managed by CEF directly. Without an
explicit `CefLifeSpanHandler::OnBeforePopup` interceptor, that default leaks
through Mote: users see a stripped-down Chromium window with no Mote chrome,
no sidebar, no workspace context, no status line. The window also bypasses
ADR-0007's privileged-origin trust surface entirely (the popup is not running
under `mote://chrome`'s isolation rules).

This is the user-visible bug behind the polish-phase R3 wave.

## Decision Drivers

- Users expect content links to open as tabs in the same browser window, with
  the same chrome — not as detached chromeless windows.
- ADR-0007 establishes that trust-critical UI lives on the privileged origin;
  a chromeless content window has no chrome at all, defeating the rule.
- A small set of legitimate flows (OAuth popups, payment redirects) have
  historically relied on the popup-window pattern and may need an opt-out
  path later.
- CEF's `OnBeforePopup` callback exposes a `user_gesture` flag distinguishing
  click-driven popups from JS-driven ones; this is the established Chrome
  signal for foreground/background activation.

## Considered Options

- **Keep CEF default (chromeless OS windows).** Rejected: defeats ADR-0007's
  trust surface and breaks expected browser UX.
- **Intercept all popups and suppress entirely (drop the URL).** Rejected:
  silently breaks `window.open(...)`-driven flows; users see "nothing
  happened."
- **Intercept and route to in-window tab in current workspace (this ADR).**
  Accepted: matches established browser behavior; preserves Mote chrome and
  trust boundary; leaves room for a future plugin-driven opt-out for OAuth-
  shaped flows.

## Decision Outcome

Chosen: **`OnBeforePopup` is intercepted; the OS popup is suppressed and the
target URL is enqueued as an in-window tab in the current workspace.**

- `CefLifeSpanHandler::OnBeforePopup` returns `true` (CEF abandons its popup
  pipeline) and sends a `PopupTabRequest { url, user_gesture, workspace }`
  through the shell event bus.
- The shell creates a new tab in the current workspace at the URL.
- **`user_gesture`-driven activation rule.** When `user_gesture == true`
  (the popup originated from a user-driven click), the new tab opens in the
  foreground (becomes active). When `user_gesture == false` (JS-initiated
  popup with no preceding click), the new tab opens in the background. This
  mirrors Chrome's convention and minimises focus-stealing from JS-driven
  popups (ad windows, etc.).
- **Plugin opt-out path is future work.** A reserved capability name
  `popup:redirect-out` is named now (not enforced in v0.1, no plugin can be
  granted it yet); when a future ADR scopes its enforcement, a plugin holding
  that capability will receive `OnBeforePopup` notifications and may yield
  the popup to a different routing (e.g. an OS window for an OAuth flow that
  requires `window.opener` semantics). Documenting the name now keeps the
  future grant UI forward-compatible.
- The popup's `windowInfo` / `client` / `settings` out-parameters that CEF
  exposes on `OnBeforePopup` are not used by Mote in v0.1 — returning `true`
  before any of those are read instructs CEF to abandon the popup entirely,
  and Mote then creates a fresh tab using its own browser-creation path.

### Consequences

- Good: every popup-triggering interaction stays within Mote's chrome and
  workspace context; the UX matches what browser users expect.
- Good: ADR-0007's privileged-origin isolation holds — no popup window can
  appear without the Mote chrome around it.
- Good: `user_gesture`-driven foreground/background activation reduces
  focus-stealing from JS-driven popups while preserving click-driven flow.
- Bad: OAuth and other flows that require `window.opener` cross-window
  scripting will land in an unrelated tab and may break their close-popup
  logic. Mitigated by the named (not yet enforced) `popup:redirect-out`
  opt-out capability; until that lands, affected users must use a workaround
  (open the OAuth URL in a fresh tab directly).
- Bad: introduces a new shell event type (`PopupTabRequest`) and a new CEF
  lifespan handler in `mote-cef` — bounded; both are thin layers over
  existing tab-creation and CEF-handler machinery.
- Bounded scope: the `popup:redirect-out` name is reserved in this ADR
  but not enforced. Any future ADR that activates it must define its full
  grant + invocation semantics; this ADR does not authorise plugins to
  influence popup routing in v0.1.

## Relationship to existing ADRs

- **Refines ADR-0007** (Plugin Management UI — Privileged Async Approval).
  ADR-0007 places trust-critical UI on the privileged origin; this ADR
  closes a gap where CEF's default popup behavior would have allowed
  unprivileged content windows to appear without Mote chrome. No
  supersession.
- **Does not affect ADR-0005** (Host Bridge — Two-Layer Isolation). The
  intercepted popup flows through Mote's existing structured event path
  (shell event bus → tab creation), not through the host bridge.
