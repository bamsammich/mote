# ADR-0010 — Collector Dispatch: Runtime Semantics and Deadline Budgeting for Provider Contribution Surfaces

- **Status:** Accepted (approved by the maintainer 2026-05-30)
- **Date:** 2026-05-30

---

## Context and Problem Statement

DESIGN.md establishes that an exclusive provider may expose an *internal event surface* for
other plugins to contribute to. The urlbar is the canonical example: `history` holds
`ui:urlbar_provider` exclusively and emits `urlbar:suggest`, which "bookmarks, tab-search, and
other plugins contribute to" (DESIGN.md:349). And: "**Collectors** — only used inside an
exclusive capability's internal event surface (e.g., history's `urlbar:suggest`). Each
subscriber returns contributions; the provider plugin merges and ranks. The provider owns the
merge policy." (DESIGN.md:862).

The schema for this already exists. The event registry declares `urlbar:suggest` with
`dispatch = "collector"` (`crates/mote-registry/data/events/v1.toml:63-66`: "Collector surface
emitted by the urlbar provider; contributors return suggestions the provider merges/ranks"), and
`ui:urlbar_provider` is documented as "May emit urlbar:suggest for contributors"
(`capabilities/v1.toml:48`).

**But the runtime does not implement collector dispatch.** `Core::emit` iterates listeners and
discards every return value (`crates/mote-runtime/src/core.rs:182`, `let _ =
call_hook_with_deadline(...)`); `hook_type_for` collapses `EventDispatch::Collector` →
`HookType::Broadcast` (`crates/mote-runtime/src/runtime.rs:647-651`). There is no path for a
subscriber's return to reach the provider, so a provider cannot synchronously gather
contributions.

The architectural decision (the urlbar *is* a collector surface) and the schema are already
made. What is **undecided** — and what this ADR records — is the **runtime contract**: how a
provider invokes collection, how subscriber returns are captured, and, most importantly, **how
total latency is bounded when N untrusted subscribers feed an interactive surface under the
inter-plugin deadline** (DISCIPLINES §3).

## Decision Drivers

- **DESIGN-faithful and open.** Contribution must be an open surface any enabled plugin can join
  (`urlbar:suggest` → bookmarks now, tab-search and others later), **not** a provider→provider
  call hard-wired by capability name. This explicitly rejects an earlier "history directly
  invokes `ui:bookmarks_provider`" shortcut as both throwaway and non-extensible.
- **Provider owns the merge.** The runtime only *gathers* contributions; ranking/merge policy
  stays in the provider (DESIGN.md:862).
- **Bounded interactive latency.** The urlbar is hit on every keystroke. A slow or malicious
  subscriber must not be able to stall the address bar, regardless of subscriber count.
- **Per-subscriber failure isolation.** One bad contributor cannot break the suggestion list or
  the provider's `query`.
- **Determinism and testability.**
- **No schema churn.** The collector event row and the `events:emit`/`events:on` permissions
  already exist.

## Considered Options

- **Direct invocation** (provider calls each contributor by capability name). *Rejected:*
  hard-wires the provider to a closed contributor set; contradicts DESIGN.md:349 ("and other
  plugins contribute"); the contribution code would be thrown away the moment a second
  contributor type appears.
- **Fire-and-forget emit + asynchronous back-channel** (contributors push suggestions later).
  *Rejected:* suggestions are synchronous per-keystroke; an async channel complicates ranking,
  ordering, and adds cross-keystroke state.
- **Synchronous collecting dispatch under a shared deadline** (this ADR). The provider calls a
  collect primitive; the runtime invokes each subscriber, captures returns, bounds total time,
  isolates failures.

## Decision Outcome

Implement collector dispatch as a **synchronous, deadline-bounded gather**:

- **Host API.** `mote.events.collect(event, payload) -> { contribution, ... }`, valid **only**
  for events whose registry dispatch shape is `Collector`. Gated by the **`events:emit`**
  permission (the caller owns/emits the collector surface). Returns a Lua array, one entry per
  *contributing* subscriber, each the subscriber's return marshalled to a `HostValue`.
  **Default-deny is the uniform failure mode:** missing permission, unknown event, non-collector
  event, or a payload-marshalling failure all return an **empty result** — the host API never
  raises into the plugin sandbox. This matches the codebase's universal default-deny idiom
  (e.g. `storage.get` returns `nil` on deny). The distinct rejection conditions remain
  observable to tests and audit via the runtime's `CollectError` typed return inside
  `Core::collect`; they just don't surface as Lua errors to the calling plugin.
- **Subscriber contract.** A contributor declares an `events[<collector-event>]` handler that
  **returns** its contributions. (Broadcast handlers' returns remain discarded; only the
  collecting path captures them.) Subscribing requires **`events:on`**.
- **Runtime path.** `Core::collect(event, payload) -> Vec<HostValue>` mirrors `Core::emit`'s
  listener-gather but captures each `Ok` return (via `call_hook_with_deadline`), marshalled to
  `HostValue`. Subscribers are iterated in deterministic **name-sorted** order.
- **Deadline budgeting (the load-bearing safety contract).** The entire collection runs under a
  **single shared deadline** equal to the caller's *remaining* inter-plugin budget. Each
  subscriber is invoked under `min(remaining, PER_SUBSCRIBER_CAP)`. When the shared deadline is
  exhausted, remaining subscribers are **not invoked** and contribute nothing that round. Because
  the provider re-ranks, subscriber order affects only *which contributors may be dropped* under
  deadline pressure — never the correctness of the merged result. Total surface latency is
  therefore bounded by the caller's budget **irrespective of subscriber count**.
- **Failure isolation.** A subscriber that errors or times out is dropped from results and
  audit-logged under the *subscriber* (mirrors `emit`'s broadcast isolation). It never aborts the
  collection or the provider's `query`.

### Scope and non-goals

- This ADR pins **runtime dispatch + deadline semantics** for an **already-declared** registry
  shape. It adds **no** registry, capability, or permission schema (`urlbar:suggest` already
  exists as a collector event; `events:emit`/`events:on` already exist).
- It does **not** change the provider→consumer capability model (ADR-0002): contribution remains
  an internal event surface of an exclusive provider, not a new coupling channel.
- It is **distinct from ADR-0009's** targeted `invoke_capability_on` (one *named* fulfiller):
  `collect` fans out to **all** subscribers of a collector event and returns their contributions;
  it is not a capability invocation and names no fulfiller.
- `collect` is gated by `events:emit` only (not "must own the capability that owns the event") —
  the runtime does not map events to an owning capability. Acceptable: collector events carry
  best-effort contribution data (suggestion lists), not secrets, and any caller able to `collect`
  could already `emit` the same event.
- `PER_SUBSCRIBER_CAP` and the urlbar's interactive budget value are implementation constants
  chosen to keep keystroke latency imperceptible; they are tuning, not contract.

### Consequences

- Good, because it matches DESIGN exactly: the urlbar is open to any contributor (bookmarks now;
  tab-search and others later with **zero** changes to history).
- Good, because urlbar latency is bounded and predictable even under slow/adversarial subscribers,
  and a failing contributor is isolated.
- Good, because it adds no schema and reuses the existing deadline, marshalling, and audit
  machinery — a thin addition over `emit`.
- Bad, because it introduces a second event-dispatch return-handling path (collecting vs
  discarding) in the runtime; bounded strictly to `Collector`-dispatch events.
- Bad, because under deadline pressure with many subscribers, late-ordered contributors may be
  silently dropped from a given keystroke's suggestions; acceptable for best-effort suggestions
  and documented as such.
- Requires a **DISCIPLINES §3** note that collector dispatch runs N subscribers under **one
  shared deadline**, not N independent budgets.

## Relationship to ADR-0002

Refines, does not supersede. Capabilities remain the sole coupling surface between plugins. A
collector event is an *internal surface of an exclusive capability's provider*; contributors
couple to the provider only through that declared event contract, consistent with ADR-0002.
