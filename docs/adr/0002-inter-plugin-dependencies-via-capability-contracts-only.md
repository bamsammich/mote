# ADR-0002 — Inter-Plugin Dependencies via Capability Contracts Only

- **Status:** Accepted
- **Date:** 2026-05-25

---

## Context and Problem Statement

The design documents are internally inconsistent about whether plugins may declare direct dependencies on other plugins. The contradiction spans DESIGN.md, DISCIPLINES.md, and the ROADMAP:

**`requires` vs `consumes`.**
DESIGN.md §Glossary defines both `consumes` ("capabilities this plugin needs *some* other plugin to fulfill") and `requires` ("dependencies on other plugins, with semver constraints … imports the dependency's exported API"). However, DESIGN.md §Security Model and §Inter-plugin communication are unambiguous: "Plugins do not import each other directly. There is no `require('other-plugin')` … no version constraint on specific plugin names." The glossary's `requires` definition directly contradicts the body's prohibition. Additionally, DISCIPLINES.md §9 lists re-approval triggers as `{permissions, capabilities, requires, identity_scope}` while DESIGN.md §Hot Reload lists `{permissions, capabilities, consumes, identity_scope}` — two different field sets for the same mechanism.

**ROADMAP semver language vs DESIGN capability-only model.**
The ROADMAP uses language like "Plugin dependency resolution (semver constraints, library vs leaf plugins)" and "Dependency graph resolution (library plugins, transitive fetches)." DESIGN.md §Per-plugin storage states: "There is no longer a notion of multiple versions of the same plugin loaded concurrently." The ROADMAP's semver-dependency framing implies a package-manager-style resolver; DESIGN.md's considered body model needs only capability-contract resolution (dangling-consumer checks).

These inconsistencies must be resolved before the manifest schema, the re-approval hash mechanism, and `mote-pluginmgr`'s dependency resolver are built.

---

## Decision Drivers

- DESIGN.md §Inter-plugin communication is the considered, architecturally-motivated position: "All inter-plugin interaction is mediated by capability contracts." Direct plugin imports create coupling that undermines the substitutability model (swapping one password manager for another must not require the consuming plugin to know which one it is talking to).
- A semver-dependency resolver between named plugins is significant implementation scope. Capability-contract resolution (dangling-consumer check) is substantially simpler and is already fully specified in DESIGN.md §Resolution at load time.
- The `requires` field is a glossary leftover from an earlier design iteration; its presence in the glossary contradicts the security model's explicit prohibition.
- The approval-hash inconsistency (DISCIPLINES §9 vs DESIGN §Hot Reload) creates a concrete implementation ambiguity: the manifest schema either has a `requires` field to hash or it does not.

---

## Considered Options

1. **`consumes` only; `requires` removed; re-approval hash is `{permissions, capabilities, consumes, identity_scope}`** — Pure capability-contract model. "Library plugins" are plugins that fulfill capabilities others consume; no named direct dependency mechanism exists.
2. **Keep both `requires` and `consumes`; both trigger re-approval** — Preserve semver-dependency capability alongside capability contracts.
3. **`requires` as a separate, non-security-relevant metadata field** — Declare named dependencies for documentation/tooling purposes only, without security-model coupling.

---

## Decision Outcome

**Chosen option: `consumes` only; `requires` removed from v1 manifest schema; re-approval hash covers `{permissions, capabilities, consumes, identity_scope}`.**

Concretely:

- **`requires` does not exist in the v1 manifest schema.** There is no `require("other-plugin")`, no named plugin dependency with semver constraints, and no transitive dependency resolver for named plugins.
- **`consumes` is the only inter-plugin dependency mechanism.** A plugin that needs functionality from another plugin declares the capability it needs, not the plugin name. The runtime resolves the currently-installed fulfiller at load time.
- **Re-approval is triggered by changes to the hash of `{permissions, capabilities, consumes, identity_scope}`.** DISCIPLINES.md §9's reference to `requires` in the re-approval trigger set is a stale term for `consumes`; this ADR corrects that.
- **"Library plugin" means a plugin that fulfills a capability others consume but provides no leaf UI behavior** — for example, `password-manager-form-services-plugin`. This is not a distinct plugin type; it is just a fulfiller with no user-visible element. No special manifest field is needed.
- **"Dependency graph resolution" (ROADMAP) means:** when installing a consumer plugin, if a declared `consumes` entry has no current fulfiller, the install flow surfaces the gap and prompts the user to install a fulfilling plugin. Transitive resolution is capability-chain resolution, not semver-tree resolution.
- **"Version-naive code" (ROADMAP Phase 1)** means plugins do not pin or reference specific versions of other plugins — consistent with this model.

Option 2 was rejected. Adding `requires` alongside `consumes` would introduce two inter-plugin interaction models with different security properties, two resolver implementations, and ambiguity about which to use in which situation.

Option 3 was rejected. A metadata-only `requires` field with no security coupling would be unused by the runtime and create confusion about its purpose.

---

## Consequences

**Good:**
- The manifest schema is simpler: one inter-plugin relationship field (`consumes`), one re-approval hash, one resolver algorithm.
- Plugin substitutability is guaranteed by the model: a consumer plugin cannot name or couple to a specific fulfiller.
- `mote-pluginmgr`'s dependency logic is dangling-consumer detection, not a semver resolver — substantially less scope.
- The approval hash is unambiguous: `blake3(permissions ++ capabilities ++ consumes ++ identity_scope)` for each plugin.
- The DISCIPLINES.md §9 mechanism description is now consistent with DESIGN.md §Hot Reload.

**Bad:**
- Plugins that genuinely want to share code between two related plugins (e.g., shared utility functions) cannot use `require("sibling-plugin")`. They must either inline the shared code, publish it as a standalone fulfiller plugin, or wait for a future shared-library mechanism (which, if it ships, should go through its own ADR).
- The ROADMAP's semver-dependency language will mislead engineers who read it without this ADR. The ROADMAP should be updated to use capability-contract terminology.

**Neutral:**
- `password-manager-core` described in ROADMAP as a "library plugin" is correctly understood as a plugin fulfilling the `password-manager-form-services` capability — no new mechanism required.
- Future schema versions (v2+) may introduce a `requires` field if the ecosystem demonstrates a need that capability contracts cannot fulfill. That is a schema-version event governed by DISCIPLINES.md §2, not a v1 concern.

---

## Links / References

- DESIGN.md §Security Model §Inter-plugin communication — "Plugins do not import each other directly"
- DESIGN.md §Glossary — `consumes` and `requires` definitions (the `requires` definition is superseded by this ADR)
- DESIGN.md §Hot Reload — re-approval trigger set: `{permissions, capabilities, consumes, identity_scope}`
- DESIGN.md §Resolution at load time — dangling-consumer error and resolution flow
- DISCIPLINES.md §9 — Plugin approval boundary discipline (its reference to `requires` in the re-approval trigger set means `consumes` per this ADR)
- `docs/plans/risks-and-inconsistencies.md` items B1, B2
