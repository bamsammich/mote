# Mote — Disciplines

The companion to `mote-design-decisions.md`. Where the design doc captures what's right in principle, this doc captures what will be hard to maintain in practice — and the operational mechanisms that make compromise harder.

Most failures in software projects don't come from bad design. They come from good design decisions that didn't get reinforced by operational discipline. This doc lists the disciplines Mote depends on, the temptations that will erode each one, and the mechanism that enforces it.

## How to use this document

When about to make a tradeoff under pressure — a deadline, a clever shortcut, a "we can fix this later" — check whether the tradeoff violates a discipline here. If it does, the answer is usually "find another way." The disciplines are listed because compromising them has predictable, expensive consequences six months downstream.

This is not a policy document. It's a memory aid for future-you.

---

## 1. CEF upgrade discipline

**Design intent.** Track Chromium via CEF so web compat and security updates are inherited rather than maintained.

**Temptation.** When a CEF upgrade lands and breaks the build, the path of least resistance is to scatter compatibility shims across the codebase to "just make it compile." Six upgrades later, shim debt lives in a dozen files and the next upgrade is harder, not easier.

**Discipline.**
- All CEF interaction goes through a single internal wrapper crate (`mote-cef`). The rest of the codebase imports from there, not from `cef-rs` directly.
- When CEF ships a breaking change, the breakage lives in `mote-cef` only. The rest of the codebase shouldn't need to know.
- Budget at least 20% of engineering time as CEF upgrade overhead. Plan around it; don't get surprised by it.
- Target a monthly upgrade cadence with explicit holds when an upgrade is broken. Don't try to upgrade on every CEF release.

**Mechanism.** CI fails any `use cef::` or `use cef_rs::` import outside the `mote-cef` crate. Code review checklist explicitly asks: "does this PR add CEF-direct usage outside the wrapper?"

---

## 2. Schema versioning discipline

**Design intent.** Permissions and capabilities are versioned registries. Plugins targeting v1 keep working under v1 semantics indefinitely, even after v2 ships.

**Temptation.** A feature request needs a permission that's "almost" an existing one. Easier to broaden the existing permission than to define a new one. Two months later, plugins relying on the old behavior break.

**Discipline.**
- Within a schema version, additions are allowed; modifications are not. A permission's enforcement surface, default behavior, and security implications must not change once shipped.
- New behavior gets a new permission name, even if it's "obviously the right extension of the old one."
- New schema versions are infrequent and documented. A v1 → v2 transition is a major release event, not a minor change.

**Mechanism.**
- A `tests/contract-conformance/` directory contains one minimal plugin per schema version that exercises every permission and capability in that version's registry.
- CI runs these contract-test plugins on every commit. Any drift fails the build.
- New permissions can be added to v1; tests for the existing behavior of any existing permission must continue to pass.

---

## 3. Differentiated dispatch discipline

**Design intent.** The dispatch contract varies by hook type (see design doc, *Plugin Dispatch and Composition — Runtime guarantees*). Filter chains are tight and sync; broadcasts get a more generous async budget; keybind handlers use input-coalescing.

**Temptation.** Collapse the differentiation under pressure — apply one timeout/auto-disable rule across all hook types because it's simpler to reason about. Or quietly add a fourth dispatch model for a specific plugin's needs.

**Discipline.**
- The hook types and their dispatch contracts are exactly what's in the design doc. Don't add new hook types or new dispatch models without updating the design doc first.
- When a plugin auto-disables, surface it as a system notification, not just an integrity-panel entry. Treat "your plugin stopped working and you don't know why" as a P0 UX failure.

**Mechanism.**
- The hook registration API requires specifying which hook type applies. The runtime enforces the corresponding dispatch model.
- An end-to-end test simulates bursty keybind input and verifies vim-mode doesn't auto-disable under realistic load.
- An end-to-end test confirms filter-chain handlers respect the 10ms budget and timeouts produce `defer`, not other values.

---

## 4. Capability combination discipline

**Design intent.** Permissions are granular so plugins ask for only what they need; the user approves with full visibility.

**Temptation.** Show permissions in a flat list and let the user evaluate each independently.

**Reality.** Some permission *combinations* create capabilities neither permission has alone. `page:read_dom` + `mcp:server` together let external agents read page content from any tab. Neither permission alone implies that; together they do.

**Discipline.**
- The install dialog surfaces dangerous combinations explicitly, above the per-permission list.
- The integrity panel shows "what this plugin makes possible from outside" alongside "what this plugin can do."
- New permissions added to the registry are reviewed for combination risks with existing permissions, and the review is captured in the permission's registry entry.

**Mechanism.**
- A `combinations.yaml` registry alongside `permissions/v1.yaml` lists known-dangerous combinations and their warning text.
- The install dialog code reads this registry. Missing entries don't block install but are added when discovered.
- The integrity panel renders capability *consequences*, not just capability declarations.

---

## 5. Identity isolation honesty

**Design intent.** Identities provide fully isolated user-state containers.

**Temptation.** Claim "fully isolated" because Chromium profiles are the underlying mechanism, and Chromium profiles are mostly isolated.

**Reality.** Chromium has known shared-state surfaces (HTTP cache key construction, service worker storage, certain network state) that leak across profiles in subtle ways. Marketing "fully isolated" sets users up for unpleasant surprises when an edge case bites.

**Discipline.**
- Design doc and any marketing claim match exactly what's currently true. "Isolated across [enumerated list]" rather than "fully isolated."
- Newly discovered leakage surfaces are added to a tracked list, each with either a fix or an explicit "known limitation, mitigated by [X]" note.
- Closing identity-leakage surfaces is P1 work, not "nice to have."

**Mechanism.**
- A `docs/identity-isolation.md` lives in the repo enumerating exactly what's isolated and what isn't.
- That file is referenced from the README's privacy/security section.
- PR review checklist for identity-relevant code: "does this affect what's listed in identity-isolation.md? If so, update it in the same PR."

---

## 6. Default-on transparency discipline

**Design intent.** Users can see what their browser is doing.

**Temptation.** Ship data-persisting features (form drafts, history, plugin storage) with sensible defaults the user never thinks about. This is what every other browser does.

**Reality.** Mote's audience is specifically people who want to know what their browser keeps. Form drafts saving silently violates the trust that makes the project worth using over the alternatives.

**Discipline.**
- Any data-persisting feature is either opt-in by default, or surfaced visibly in the integrity panel with one-click clear/disable.
- Form drafts ship opt-in. Users who want them enable per workspace or globally.
- The integrity panel has a "Data Mote is keeping" view showing every category — history entries, form drafts, plugin storage volumes, cached items per identity — with clear/disable controls.

**Mechanism.**
- Any new feature that writes user data to disk includes a "data persistence" section in the PR description: what's saved, where, default opt-in/opt-out, how the user discovers and clears it. The PR template enforces this.

---

## 7. Settings surface discipline

**Design intent.** Configuration is dotfile-driven; settings live in TOML files.

**Temptation.** Don't build a settings UI at all. Pure config-file approach is cleaner and matches the primary audience.

**Reality.** Even the primary audience hits situations where a quick toggle is faster than editing a file. Adjacent audiences (privacy-curious, security-research) bounce off entirely without one.

**Discipline.**
- A first-party plugin `mote-settings-ui` ships as Tier 3 (off by default, one config line to enable) at v0.2.
- The settings UI is a plugin, not a core feature. It reads and writes the same TOML config the user edits manually. No separate state.
- The Open Decisions item "Settings UI vs. config-file-only" is resolved as "both, with the UI being a plugin."

**Mechanism.** This is a roadmap commitment, not a CI rule. Putting it in this doc is the mechanism — it makes future-you's "should we build a settings UI?" question already-answered.

---

## 8. Honest positioning discipline

**Design intent.** Mote serves two pillars: programmable by users, introspectable by agents.

**Temptation.** Lead marketing with the more exciting pillar (LLM frontend validation) even when the underlying plugin (`frontend-introspection-mcp`) is months away from shipping.

**Reality.** Users showing up because of the second pillar and finding only the first will write disappointed posts. The project gets associated with overpromise.

**Discipline.**
- README and any project description state what exists today, with explicit "coming in vX.Y" for what doesn't.
- The first MCP server plugin in v0.1 — even a minimal one — exists so the second pillar isn't aspirational, just minimal.
- No marketing claim is made about capabilities that don't ship in a tagged release.

**Mechanism.**
- A `STATUS.md` enumerates the current state of each major capability — implemented, in-progress, planned, deferred — and is updated on every release.
- README feature list is reviewed against the latest tagged release at release time. Anything not in the release goes under "Planned" or is removed.

---

## 9. Plugin approval boundary discipline

**Design intent.** The install dialog is the security boundary for plugins, not the file system. Plugins surface for approval before they load; permission changes trigger re-approval; code changes within the same permission set don't (see design doc, *Plugin Management*).

**Temptation.** Soften the boundary under any of:
- "Trust dotfile edits implicitly" — auto-approve plugins because the user committed them.
- "Skip the re-approval prompt for small permission additions" — bundle a new permission into an update without forcing review.
- "Add a global dev mode" — make auto-approval easy to toggle for everything at once.

Each looks like a small UX win and erodes the substrate's transparency guarantees.

**Discipline.**
- The install dialog is invoked on first detection of any plugin, declared or implicit local.
- Re-approval is triggered by any change to `permissions`, `capabilities`, `requires`, or `identity_scope`. Code-only changes don't trigger it.
- Dev mode is per-plugin or per-directory only. There is no global "auto-approve everything" toggle. Dev-mode plugins are visually marked everywhere they appear.
- Permission changes in `mote plugin update` output are visually distinct from code-only updates. Users autopiloting on update notifications never lose visibility on permission expansion.

**Mechanism.**
- Permission/capability/requires/identity_scope hashes are stored per plugin; load-time compares against the last-approved hash. Mismatch surfaces in the integrity panel with `[Review and approve] [Decline]`.
- `mote plugin diff <name>` produces the same diff the approval dialog would show, callable from the CLI before any approval is needed.
- The Tier 3 `mote-plugin-devtools` plugin marks dev-mode plugins distinctly in its UI, mirroring the integrity panel's visual treatment.

---

## 10. Governance clarity discipline

**Design intent.** Mote is personal-first; accept help if it comes.

**Temptation.** Stay vague about governance because the project is small. Defer the conversation until the first real ask appears.

**Reality.** When the first real contributor asks for something more than a merged PR (recurring maintainer status, formalized area ownership, etc.), having no answer ready means hedging — and losing the interest.

**Discipline.**
- A `CONTRIBUTING.md` ships in the repo from day one. Short, clear, addresses:
  - What contributions are accepted (PRs under the project license).
  - What's negotiable (recurring maintainer status, area ownership).
  - What's not (CLA, joint copyright, paid governance arrangements).
- A `GOVERNANCE.md` is added when the first non-trivial conversation arrives, not before. But the answer to "do you have governance?" is already "yes, see CONTRIBUTING.md."

**Mechanism.** The repository template includes `CONTRIBUTING.md` from day one. When governance questions arise, the response is "let me think and update GOVERNANCE.md, then we can discuss" — never "I haven't thought about this."

---

## The common thread

Most of these disciplines share a shape: turning a design principle into a mechanism that survives time pressure and future-self pragmatism.

- The design doc says "permissions are versioned." This doc says "and CI runs a contract test that fails if you drift the v1 contract."
- The design doc says "checksum pinning protects against supply-chain attacks." This doc says "and the default UX pins on first install, not on user opt-in."
- The design doc says "personal-first, accept help if it comes." This doc says "and write CONTRIBUTING.md on day one so 'accept help' has an actual answer."

The disciplines are how the design survives.

## Reviewing this document

This doc is valuable only if it's read at the right moments. Add a quarterly calendar reminder titled "Mote disciplines review." Read the doc. Ask: which disciplines have been compromised since last review? What mechanism was supposed to prevent that, and why didn't it? Update mechanisms; don't lower the discipline bar.

When new failure modes emerge from real usage, add them here as new sections following the same template (design intent / temptation / discipline / mechanism). This doc is meant to grow.
