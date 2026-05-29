# ADR-0009 — `password-manager:provider` is Non-Exclusive; Secret/Provider Routing is Targeted, Not Fan-Out

- **Status:** Proposed
- **Date:** 2026-05-28

---

## Context and Problem Statement

DESIGN.md and the v1 registry declared `password-manager:provider` an **exclusive**
capability ("the user enables exactly one at a time", DESIGN `:1769`). The Secret
Management section leaned on that to justify routing for the `password-manager` secret
backend: "Because `password-manager:provider` is exclusive … there's only one password
manager plugin to ask, so routing is unambiguous" (`:1300`). Risk **B7** encoded the same
rationale into `secret:provider`'s registry description ("resolution is effectively
singular in practice because the active fulfiller is gated by the exclusive
password-manager:provider").

This premise is wrong against real use. A user routinely runs a **work** password manager
and a **personal** password manager in the same browser at the same time. Exclusivity
makes that impossible, and the "ask the one provider" routing has no answer once more than
one provider exists.

## Decision Drivers

- Multiple password managers (work + personal; or two vendors) must coexist as active fulfillers.
- Established password-manager UX: the **user chooses** which manager acts (fills a form,
  resolves a secret). Nothing races or silently guesses among providers.
- Secret resolution must be **deterministic** — a given secret resolves through exactly the
  provider the user named, every time, on every device.
- Avoid a framework-level "merge multiple provider results" behavior the user never asked for.

## Considered Options

- **Keep `password-manager:provider` exclusive.** Rejected: forbids the core work+personal case.
- **Non-exclusive + fan-out, first successful resolution wins.** Rejected: silent guessing;
  ambiguous when two providers can serve the same reference; not how any shipping password
  manager behaves.
- **Non-exclusive + targeted routing by explicit provider name** (this ADR). The user names
  the provider; the runtime invokes exactly that fulfiller.

## Decision Outcome

Chosen: **`password-manager:provider` is non-exclusive, and the secret subsystem routes to a
single explicitly-named fulfiller — never fan-out.**

- `password-manager:provider` `composability` becomes `non-exclusive` in the registry;
  multiple managers may be loaded simultaneously. `check_exclusive_claims` no longer rejects
  a second password manager.
- `secret:provider` resolution is **targeted**: a `secrets.lua` entry with
  `backend = "password-manager"` **must** name `provider = "<plugin-name>"`. The runtime
  invokes `resolve_secret(reference)` on that one fulfiller via a new targeted dispatch
  primitive `Core::invoke_capability_on(provider_name, …)`. A missing `provider`, or a named
  provider that is not loaded, is a clear error — never a fallback to another provider.
- `secret:provider` remains registered `non-exclusive`, but its description is corrected:
  resolution is unambiguous because the **caller names the provider**, not because only one
  exists. The B7 "singular in practice" rationale is superseded.
- Phase-5 autofill (`fill_credential` / `list_credentials`) across multiple active managers
  is a separate, deferred design: the **user picks** the manager per form (established
  extension UX), recorded as a follow-up — this ADR does not specify it.

### Consequences

- Good, because work + personal (and multi-vendor) password managers coexist, matching real use.
- Good, because secret resolution is deterministic and auditable — one named provider, no race.
- Good, because the model matches established password-manager UX (explicit user choice).
- Bad, because `secrets.lua` `password-manager` entries are coupled to plugin names the user
  must know; mitigated by a clear config-load error and (Phase 5) the install-dialog vault picker.
- Bad, because it adds a targeted-invocation path (`invoke_capability_on`) alongside the
  existing exclusive/fan-out shapes; bounded — it is a thin filter over the same per-fulfiller
  invocation machinery, used only by the secret route in v0.1.
- Requires reconciling DESIGN.md (6 exclusivity assertions + the routing paragraph), the
  registry (`password-manager:provider` composability; `secret:provider` description), and
  risk B7 (marked resolved).

## Relationship to ADR-0002

Refines, does not supersede, ADR-0002 (inter-plugin dependencies via capability contracts
only). Capabilities remain the sole coupling surface; this ADR records that one capability's
composability changes from exclusive to non-exclusive and that a contract may be invoked on a
**specific named fulfiller**, not only broadcast/fan-out, when the caller (here, the secret
subsystem driven by user config) names the target.
