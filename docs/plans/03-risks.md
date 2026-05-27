# Mote — Phase 3 (Plugin Management): Risks, Ambiguities & Decisions

Companion to `docs/plans/03-plugin-management.md`. Read before starting any work unit. Each entry: **what & where**, **why it blocks/risks clean implementation**, **proposed resolution / recommendation**. Severity tags mirror `risks-and-inconsistencies.md`:

- `[BLOCKER]` — resolve before the affected unit is built; guessing risks rework.
- `[DECISION]` — a genuine open choice the design leaves unbound.
- `[INCONSISTENCY]` — two docs disagree; pick the canonical one.
- `[GAP]` — referenced but never specified.
- `[RISK]` — not contradictory, but likely to bite during implementation.

The phase-level inconsistencies B1/B2/B4 from `risks-and-inconsistencies.md` are **already resolved** by ADR-0002 (no `requires`/semver; `consumes` only; re-approval hash over `{permissions,capabilities,consumes,identity_scope}`) and by `mote-types::Checksum` (BLAKE3 `blake3:<hex>`, sha256 is stale). Phase 3 inherits those resolutions; they are not re-opened here.

---

## Decisions needing maintainer / orchestrator input (surface first)

### R1. `[DECISION]` gix confirmation — fetch-at-commit capability
- **Where.** `risks-and-inconsistencies.md` F2 recommends **gix** (pure Rust) over `git2` (libgit2 FFI) for `mote-pluginmgr`'s Git fetching, to match `unsafe_code = "deny"` and DESIGN §Implementation Language. The plan adopts gix.
- **Why it risks.** gix is the right *posture* choice, but its high-level API for "fetch a single commit and check out its working tree into directory X" is less turnkey than libgit2's. The exact uncertainty: does the pinned gix version support a blobless/shallow fetch-at-commit + worktree checkout into an arbitrary path, without a full history clone? If not, the fallback (full clone into temp, then checkout the commit) is correct but slower, and a `<10s sync` ROADMAP polish target (Phase 10) may pressure it.
- **Recommendation.** **Confirm gix.** Spike the fetch-at-commit path in unit **3.1c** *before* building `update`/`sync` on top. Pin the gix version in `[workspace.dependencies]`. If gix genuinely cannot do fetch-at-commit cleanly in v0.1, the documented last-resort fallback is shelling to system `git` via `std::process::Command` — but that reintroduces a system-git runtime dependency and should be a **maintainer decision**, not a silent engineer choice. **Needs sign-off:** gix is the choice; confirm, and confirm the system-git fallback is acceptable *only* if the spike fails.

### R2. `[DECISION]` `plugins.lua` config-Lua context vs the deferred settings/config-is-Lua system
- **Where.** Plan §2.1. `plugins.lua` is Lua calling `mote.plugins({...})`; DESIGN §Plugin Management. DESIGN core principle #5 ("config-is-plugin") + §UI Composition + risks A2 imply a much larger config-Lua surface (`mote.theme_overrides`, `mote.workspace.define`, `mote.keys.bind`, `mote.dispatch.order`, `mote.on`, …). ROADMAP Phase 2 marks "Settings model — deferred"; DISCIPLINES §7 says the settings UI is a plugin.
- **Why it blocks.** Two questions must be answered before building the evaluator: (1) Where does the config-Lua evaluator live, and (2) how much of the surface does Phase 3 implement? Building it wrong means either over-building (the whole settings system, out of scope) or building a throwaway evaluator the deferred system will replace.
- **Recommendation.** Add a `config` module to **`mote-lua`** (the only crate allowed to touch `mlua`) exposing `eval_config(source) -> ConfigCapture`. Phase 3 implements **only** `mote.plugins`, `mote.dev_mode`, `mote.updates.configure`. Design the capture so new `mote.*` config functions register additively — the deferred settings system *grows into* this module rather than replacing it. `mote-pluginmgr` owns interpreting `plugins`/`dev_mode`/`updates`; the future `mote-shell` config loader (`init.lua`) will own the rest, sharing `mote-lua::config`. **Needs sign-off:** confirm (a) `eval_config` lives in `mote-lua`, (b) Phase 3 scope is exactly those three functions, (c) the config-Lua context is a *separate* restricted sandbox from the plugin host sandbox (no `io`/`os`/`require`, no plugin `mote.*` host API). The plan assumes all three.

### R3. `[DECISION]` How `mote plugin add` rewrites the `plugins.lua` call — ✅ RESOLVED (ADR-0006)
> **RESOLVED 2026-05-27 by ADR-0006:** the CLI **never** rewrites `plugins.lua`.
> Both candidate approaches below are obsolete — `plugins.lua` is a program, not
> data, and cannot be reliably rewritten. `add`/`remove`/`source` write a
> Mote-owned, committable `managed.lua` (generated wholesale, atomic write,
> loaded last to compose over user config). `import` migrates an entry into the
> user's own config by printing (default) or opt-in append-only `--write`. See
> `docs/plans/2026-05-27-config-mutation-model-design.md`. Original analysis kept
> below for history.
- **Where.** Plan §5.1. DESIGN: "the CLI can mutate it programmatically by rewriting the call." `plugins.lua` is Lua, so the rewrite is non-trivial.
- **Why it risks.** (A) Evaluate-then-regenerate is robust but **loses user comments/formatting** inside the `mote.plugins({...})` call — a real cost for a hand-edited, Git-tracked dotfile. (B) Targeted source-span edit preserves comments but needs light Lua-table-literal parsing and is brittle for unusual formatting / dynamically-built tables.
- **Recommendation.** **(B) with a documented (A) fallback:** span-edit the affected key for the common literal case (preserves comments/order); regenerate the whole call only when the call can't be span-located (e.g. dynamically constructed table). **Needs sign-off** because it trades comment-preservation against implementation complexity; if the maintainer values simplicity over comment-preservation, choose (A) outright.

### R4. `[INCONSISTENCY]` `plugins.lua` Lua keys (underscores) vs `PluginName` (hyphens) — ✅ RESOLVED (ADR-0006)
> **RESOLVED 2026-05-27 by ADR-0006 → Option 2:** `plugins.lua` keys **must be
> valid `PluginName`s**, written quoted (`["vim-mode"] = {...}`), validated
> against the resolved manifest name at sync. DESIGN's underscore examples were
> corrected. (The maintainer chose Option 2 over the plan's Option-1 recommendation
> — the key is authoritative, not cosmetic, so no key→name resolution step.)
- **Where.** DESIGN §Manifest and lock file writes `vim_mode`, `my_local_plugin`, `cool_plugin` as the `plugins.lua` keys, but `plugins.lock` uses `cool-plugin` (hyphen), and `mote_types::PluginName` **requires** lowercase + hyphens, **no underscores** (validated; `_` is `PluginNameError::InvalidChar`). The DESIGN example keys are not valid `PluginName`s.
- **Why it blocks.** The lock + cache + storage namespaces are keyed by `PluginName` (hyphenated). The `plugins.lua` key is a Lua identifier (underscores are idiomatic, hyphens are illegal in bare Lua keys). So the `plugins.lua` key and the `PluginName` **cannot be the same string**. Three options:
  1. The `plugins.lua` key is *cosmetic* (a label); the real `PluginName` comes from the plugin's **manifest** (`M.manifest.name`, which is a validated `PluginName`). The key→name link is established at first resolve.
  2. The `plugins.lua` key **must** be a valid `PluginName` written quoted (`["vim-mode"] = {...}`) — rejects the DESIGN underscore examples.
  3. Normalize underscores→hyphens on the key.
- **Recommendation.** **Option 1.** The `plugins.lua` key is a user-facing label only; the authoritative `PluginName` is the manifest name (already a validated `PluginName`, already what the runtime/cache/storage use). `add`/`remove` match by manifest name; the key is preserved verbatim for the user. This makes the DESIGN underscore examples valid (they're just labels) and avoids a normalization footgun. **Needs confirmation** — it changes how `add`/`remove` locate entries (by manifest name, resolving the key→name map on first fetch).

### R5. `[DECISION]` Network / offline behavior
- **Where.** Plan §3.2. DESIGN says airgapped users disable update checks; the lock + cache are self-contained.
- **Why it risks.** Failure modes need defined semantics: offline `sync` (some commits not cached), offline `update`, a Git host being down, a partial fetch. None of these should crash the browser or corrupt the cache/lock.
- **Recommendation (engineer-resolvable, confirm the posture).** (1) Plugins already in the cache load **offline** with zero network (lock + cache self-contained) — this is the airgapped/normal-launch path and must never require network. (2) `sync`/`update` fetch failures are **recoverable errors**, reported per-plugin, leaving existing cache/symlink/lock untouched (no half-state). (3) Bundled plugins never need network (embedded in binary). (4) First-party upstream polling (§7.2) is the *only* outbound check and is disable-able (`check_first_party = "never"`) and an inbound version query (Core Principle #9 carve-out). **Confirm** the posture; the failure-mode details are engineer-resolvable.

### R6. `[GAP]` Capability cycles in the consumes graph
- **Where.** Plan §4. The runtime's `resolve_consumes` requires a consumed capability's fulfiller to be **already loaded** at step 1. A cycle (A consumes a cap B fulfills, B consumes a cap A fulfills) cannot satisfy "fulfiller loads first" for both.
- **Why it gaps.** pluginmgr's topological load ordering has no defined behavior for a cycle; whichever loads first hits a dangling-consumer error.
- **Recommendation.** Document **capability cycles unsupported in v0.1** (the first-party set has none). Detect a cycle during topo-sort and surface it as a clear "circular capability dependency between A and B" integrity-panel error rather than an opaque dangling-consumer error. Engineer-resolvable; flag only if a real cycle appears.

### R7. `[DECISION]` Approval-dialog threading: modal-blocking vs async "awaiting approval"
- **Where.** Plan §6.1. `Runtime::load` calls `ApprovalPolicy::decide` **synchronously**; the CEF page rendering the dialog is `!Send` and lives on the pump thread. DESIGN §Hot Reload describes an async "awaiting approval" state for updates; the runtime's `reload(require_reapproval=false)` already implements it (returns `ApprovalDenied`, keeps the old instance running).
- **Why it blocks the flow unit (3.6).** Determines whether `decide` blocks the load on a user decision (needs to render + await on the pump thread, or block on a channel the pump thread fulfills) or whether load defers and the plugin enters "awaiting approval."
- **Recommendation.** **First install = modal blocking** (load waits for the user — there is no running instance to keep alive, so blocking is acceptable and simplest); **update/expansion = async "awaiting approval"** (use the runtime's existing `reload(require_reapproval=false)` → keep the old instance running → surface the prompt in the panel → on approval, `reload(require_reapproval=true)`). Load plugins on the **pump thread** (where the bridge + page live), as the Phase-2 `PluginHost` already does, so modal `decide` can render + nested-pump-await on the same thread. **Needs sign-off** because it shapes the 3.6 wiring; the recommendation aligns with the runtime API already built.

### R8. `[DECISION]` Approval state: global per-plugin vs per-identity
- **Where.** Plan §7.4. The cache is shared across identities (code is identity-independent). Per-identity `plugins.lua` can override the plugin *set* per identity. But is *approval* per-identity?
- **Why it risks.** Approval is about a manifest (`{permissions,capabilities,consumes,identity_scope}`), and the code is identical regardless of identity. Storing approval per-identity would re-prompt for the same approved manifest in each identity — annoying and arguably wrong, since the security decision is about the code, not the profile.
- **Recommendation.** **Global approval keyed by plugin + ApprovalHash** (not per-identity). The same approved manifest loads without re-prompt in any identity. **Confirm** — a maintainer who wants per-identity trust boundaries (e.g. "this plugin is approved for `personal` but not `work`") would reject this; the plan assumes global, matching "the cache is shared; the code on disk is the same regardless of identity."

---

## Engineer-resolvable (confirm where flagged)

### R9. `[GAP]` OS file-watch auto-reload scope
- **Where.** DESIGN §Hot Reload watches plugin files via inotify/FSEvents/ReadDirectoryChangesW; ROADMAP Phase 3 lists "OS file-watch triggering lands in Phase 3 (pluginmgr)" (Phase 1 note). The plan provides programmatic reload + a startup implicit-local scan but flags the live watcher as possibly-deferrable.
- **Why it gaps.** Whether v0.1 ships a live `notify`-crate watcher (re-runs the four-step pipeline on file change for dev-mode/path dirs) or only programmatic + `mote plugin reload`.
- **Recommendation.** Include a **minimal `notify`-based watcher** for dev-mode + `path:` dirs in unit 3.7 if time allows (it's the dev-loop ergonomics DESIGN promises); otherwise programmatic reload + `mote plugin reload` covers correctness and the watcher is a clean follow-up. Engineer-resolvable; add `notify` to `[workspace.dependencies]` only if built.

### R10. `[GAP]` `Runtime::load` takes Lua **source**, not a path or directory
- **Where.** `mote_runtime::Runtime::load(source: &str, …)` takes the plugin's Lua **source string** (Phase-2 `PluginHost` passes `include_str!`). A real plugin is a **directory** (`init.lua` + possibly more files); the cache stores directories; the dir hash covers the whole tree.
- **Why it risks.** pluginmgr must read the plugin **entry file** (`init.lua`) from the resolved dir and pass its source to `Runtime::load`, while the **integrity hash** covers the whole directory. Multi-file plugins (Lua `require` is stripped from the sandbox!) cannot `require` sibling files — so v0.1 plugins are effectively single-`init.lua` (plus non-Lua assets like WASM/filter-lists loaded via the host API). Confirm: the entry point is always `<dir>/init.lua`; the runtime loads that file's source; sibling `.lua` files are *not* loadable via `require` (sandbox strips it) but *are* part of the integrity hash.
- **Recommendation.** Engineer-resolvable: pluginmgr reads `<dir>/init.lua` and passes its source to `Runtime::load`; the dir hash covers the tree; document that multi-file *Lua* plugins are out of scope in v0.1 (no `require`), but non-Lua assets are part of the plugin dir and hashed. If multi-file Lua becomes a need, it's a runtime change (a controlled `require` within the plugin dir), not a pluginmgr change — flag for a future ADR.

### R11. `[RISK]` `path:`/implicit-local plugins change hash on every edit
- **Where.** Plan §3.4/§3.5. A `path:` or implicit-local plugin the user is actively editing changes its dir hash on every save → would show `IntegrityStatus::Mismatch` constantly.
- **Why it risks.** Would make the integrity panel cry wolf for exactly the plugins under active development.
- **Recommendation.** Engineer-resolvable: **dev-mode + `path:` plugins do not gate on hash match** (`IntegrityStatus::DevMode`/informational), per the plan's §3.5 status mapping. `mote plugin pin` re-anchors the hash when the user wants to lock a path plugin's current state. Only `github:`/`git+https:` (immutable, commit-pinned) sources enforce the mismatch refusal.

### R12. `[RISK]` `combinations.yaml` registry format for the approval dialog
- **Where.** Plan §6.1 builds the approval dialog's dangerous-combination warnings from `mote-registry::CombinationRegistry`. `risks-and-inconsistencies.md` B6 + DISCIPLINES §4 define it; Phase 1 built `CombinationRegistry`/`CombinationEntry`/`Severity`.
- **Why it risks.** The flow unit (3.6) depends on the combination registry being queryable by "given this permission set, which dangerous combinations apply?" — confirm the Phase-1 `CombinationRegistry` exposes that query.
- **Recommendation.** Engineer-resolvable: verify `CombinationRegistry` has a "matches against a granted set" method during 3.6; if not, it's a small additive method on a Phase-1 crate (additive-only, no schema bump).

### R13. `[GAP]` `gc` rollback-window policy
- **Where.** Plan §5 `gc` removes unreferenced cache entries. DESIGN retains "previous version for rollback" but never bounds how many previous commits to keep.
- **Why it gaps.** `gc` must know what's "referenced": the active commit (symlink target) + the lock's commit + how many *previous* commits to retain for rollback.
- **Recommendation.** Engineer-resolvable: retain the **active commit + the immediately-previous commit** (one-step rollback, matching `mote plugin rollback`'s "previous"); `gc` reaps everything older and anything no lock entry references. A deeper rollback history is a v0.2 nicety. Document the retention rule.

---

## Summary: what needs the maintainer/user before coding

| # | Item | Severity | Gates | Status |
|---|---|---|---|---|
| R1 | gix fetch-at-commit confirmation (+ system-git fallback only if spike fails) | DECISION | 3.1c | ✅ resolved (Wave 1: gix fetch-at-commit verified) |
| R2 | `eval_config` lives in `mote-lua`; Phase-3 scope = `plugins`/`dev_mode`/`updates`; separate restricted sandbox | DECISION | 3.1d | ✅ resolved (Wave 1: `eval_config` built) |
| R3 | `mote plugin add` config mutation | DECISION | 3.3 | ✅ resolved (ADR-0006: no rewrite; `managed.lua`) |
| R4 | `plugins.lua` key vs `PluginName` | INCONSISTENCY | 3.2, 3.3 | ✅ resolved (ADR-0006: quoted hyphenated `PluginName` keys) |
| R7 | Approval-dialog threading: modal first-install / async update | DECISION | 3.6 | ⏳ open — maintainer leans modal-install/async-update (confirm before 3.6) |
| R8 | Approval state global vs per-identity (recommend global) | DECISION | 3.4, 3.6 | ⏳ open — maintainer leans global per plugin+hash (confirm before 3.4) |
| R5 | Offline/network failure posture (recommend: cache self-contained, recoverable errors) | DECISION (posture) | 3.1c, 3.3 | ⏳ open (posture confirm) |

R6, R9–R13 are engineer-resolvable with the proposed defaults; confirm where convenient. **R1–R4 are now resolved.** The remaining decisions before the integrator units are **R7 (approval threading, gates 3.6), R8 (approval scope, gates 3.4), and R5 (offline posture)** — confirm these before building the flow/approval-store units.
