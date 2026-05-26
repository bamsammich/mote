# ADR-0001 — Declarative Plugin Registration

- **Status:** Accepted
- **Date:** 2026-05-25

---

## Context and Problem Statement

DESIGN.md's Enforcement Rules describe a declarative plugin registration model: plugins expose their hooks, events, and API surface as module-level Lua tables (`M.manifest`, `M.hooks`, `M.events`, `M.api`), and `setup()` is called only after four load-time validation steps complete. The key property this enables is **static contract conformance**: step 3 of the load sequence reads `M.events`/`M.hooks`/`M.api` without executing the plugin, verifying that the declared API surface matches the capability contract before any plugin code runs.

DESIGN.md flags this as an unresolved implementation-time question. The "Open at implementation time" callout box in §Enforcement Rules states that the declarative model is the design choice, but that whether it survives contact with real plugin authoring is open, and that the fallback is imperative `events.on(...)` inside `setup()` with conformance becoming a dynamic check. Critically, it notes the choice is **reversible only before the v1 schema locks**.

This ADR closes that open question for v1: lock the declarative model now, before the schema locks, because the alternative forecloses a security property.

---

## Decision Drivers

- Static contract conformance (load-step 3) is a security property: the runtime can validate a plugin's API surface before executing any plugin code. This is only possible if hooks/events are declared in module-level tables, not registered inside `setup()`.
- The imperative model (`events.on(...)` inside `setup()`) requires running the plugin to discover what it will do — collapsing validation into a dynamic check that happens after execution, not before.
- DESIGN.md §Enforcement Rules states: "Event handlers are declarative, not imperative … the only registration path."
- The decision is flagged as irreversible after the v1 schema locks; it must be settled before `mote-lua` (`1C.1`) and `mote-dispatch` (`1D.1`) are built.
- DISCIPLINES.md §2 (schema versioning discipline) and §9 (plugin approval boundary discipline) both depend on the runtime being able to inspect a plugin's declared surface without executing it.

---

## Considered Options

1. **Declarative tables (`M.manifest`, `M.hooks`, `M.events`, `M.api`); `setup()` runs only after validation** — Static conformance is possible; hooks/events are known before any plugin code executes.
2. **Imperative registration (`events.on(...)` inside `setup()`)** — Conformance becomes a dynamic check; validation can only happen after execution.
3. **Hybrid: declarative tables required for core hooks; imperative allowed inside `setup()` for supplemental registrations** — Partial static analysis; complex loader; fragmented mental model for plugin authors.

---

## Decision Outcome

**Chosen option: Declarative tables; `setup()` runs only after all four load-time validation steps.**

The four-step load sequence is:

1. **Schema validation.** Every entry in `permissions`, `capabilities`, and `consumes` must reference known terms from the registry. Dangling consumers (no fulfiller installed) fail here.
2. **Module load.** The plugin's Lua module is loaded — `M` table constructed, `M.api`/`M.events`/`M.hooks` declared. `setup()` is **not called**.
3. **Contract conformance.** For each claimed capability, the loaded module is checked against the capability contract: required API methods must exist in `M.api`; required event handlers must be declared in `M.events`. This step reads the module tables — it does not execute `setup()` or any handler.
4. **Permission approval.** User approves (or denies) the permission set. Cached across launches; re-approval triggered by changes to the hash of `{permissions, capabilities, consumes, identity_scope}` (see ADR-0002).

Only after all four pass does `setup()` run and bind the declared handlers.

Plugin authors register hooks and events by populating module-level tables:

```lua
local M = {}

M.manifest = { schema = "v1", name = "my-plugin", ... }

M.hooks = {
  ["net:intercept_request"] = { priority = 70 },
}

M.events = {
  ["password-manager-form-services:form-detected"] = function(form)
    -- handler logic
  end,
}

M.api = {
  my_exported_fn = function(...) ... end,
}

function M.setup()
  -- runs only after steps 1–4 complete
  -- may write storage, fetch over network, etc.
end

return M
```

There is no dynamic event-listener subscription API. Hooks and events are registered exclusively through the module-level tables.

Option 3 (hybrid) was considered and rejected. Allowing supplemental imperative registration inside `setup()` would reintroduce the "run to discover" problem for any hook registered that way, making conformance partially static and partially dynamic — two mental models, two code paths, higher maintenance cost, weaker security property.

---

## Consequences

**Good:**
- The runtime can validate any plugin's full API surface before executing a single line of plugin code. A malicious or malformed plugin cannot use `setup()` as an execution vector to side-step contract validation.
- Contract conformance (step 3) is a simple table inspection, not a sandboxed execution trace.
- Plugin authors have one registration model: declare in tables, implement in functions. The getting-started guide is simpler.
- DISCIPLINES.md §2 (contract-conformance CI tests) can be implemented as static analysis over `M.events`/`M.hooks` — no plugin execution required in CI.

**Bad:**
- Plugins with dynamically-determined event subscriptions (e.g., "subscribe to events based on config read at setup time") cannot use a dynamic approach. They must declare all possible handlers statically and branch inside the handler body.

**Neutral:**
- The DESIGN.md "Open at implementation time" note is now closed: declarative tables are the committed model for v1. Revisiting requires a schema version bump (v2) and is governed by DISCIPLINES.md §2.
- `setup()` retains full expressive power for initialization logic; it simply cannot register new hooks or events.

---

## Links / References

- DESIGN.md §Enforcement Rules — four-step load sequence and the "Open at implementation time" callout box
- DESIGN.md §Security Model — Enforcement Rules: "Event handlers are declarative, not imperative … the only registration path"
- DISCIPLINES.md §2 — Schema versioning discipline (contract-conformance CI test)
- DISCIPLINES.md §9 — Plugin approval boundary discipline
- `docs/plans/risks-and-inconsistencies.md` items A2, B5
