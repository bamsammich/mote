# Mote — Phase 3 Implementation Plan: Plugin Management

**Status:** Draft for orchestrator/maintainer review.
**Scope:** ROADMAP Phase 3 — the dotfile-driven, reproducible, content-addressed plugin-management layer. This is the phase that turns "a runtime that can `load(source)`" (Phase 1) and "a browser shell with a runtime host + integrity panel + approval-dialog *surface*" (Phase 2) into "a browser whose plugin set is declared in `plugins.lua`, pinned in `plugins.lock`, fetched from Git/path/bundle into a content-addressed cache, integrity-verified on load, and managed through `mote plugin …`." It also lands the **install→approval flow** deferred from Phase 2: the wiring that makes installing or detecting a plugin actually drive the approval dialog through the runtime's `ApprovalPolicy`.
**Source of truth:** `DESIGN.md` §Plugin Management (in full), §Enforcement Rules, §Hot Reload, §Per-plugin storage; `DISCIPLINES.md` §2 (schema/contract conformance), §9 (plugin approval boundary); `docs/adr/0001` (declarative registration), `docs/adr/0002` (consumes-only, **no semver/`requires`**); `docs/plans/risks-and-inconsistencies.md` (B4 BLAKE3, F2 gix, `requires` removal); `docs/plans/00-master-plan.md` §1.2 (crate topology), §Phase 3. Where this plan and those documents disagree, those documents win.
**Companion:** `docs/plans/03-risks.md` — read it before starting any work unit. It carries the decisions that need maintainer sign-off (gix confirmation, the `plugins.lua` config-Lua-context approach, the install→approval UI wiring, the `plugins.lua` key↔`PluginName` mapping, offline/network behavior).

> **What is already built (do not rebuild).** Phase 1 gives `Runtime::{load,reload,unload}` with the four-step pipeline, `ApprovalPolicy`/`Approval`/`Narrowing`, `ApprovalHash` (the re-approval fingerprint over `{permissions,capabilities,consumes,identity_scope}` with `is_expansion_of`), `RunningPlugin`, `mote_lua::{load_plugin, Manifest}`, `mote_types::{Checksum (blake3:<hex>), PluginName}`, `mote_storage::{Store, Namespace, IdentityScope}`, and the v1 `Registry`. Phase 2 gives `mote_ui::{IntegrityPanel, PluginRow, PluginKind, IntegrityStatus, PluginAction, ApprovalRequest, NarrowablePermission, NarrowMode}` (the management/approval **view-models**, already rich enough to render every Phase-3 state) and `mote-shell`'s `PluginHost` (currently boots from a hardcoded `BUNDLED` array). Phase 3 **replaces the hardcoded bundle loop with a real resolver** and **plugs a UI-backed `ApprovalPolicy` into `Runtime::load`**.

---

## 0. Ground rules carried from the contract

These apply to every work unit and are not repeated per task (from `CLAUDE.md` / `00-master-plan.md` §0):

- Edition 2024, MSRV 1.95.0; `[lints] workspace = true`; `unsafe_code = "deny"` (Phase 3 adds **no** `unsafe` — `gix` is pure Rust, the cache is std `fs`).
- All tooling through mise (`mise exec -- cargo …`); CI runs clippy `-D warnings`, `cargo test --workspace --all-features`, the CEF import-isolation gate, and the contract-conformance plugin.
- Shared dep versions in `[workspace.dependencies]`; `missing_docs = "warn"` (every public item ships docs from the first commit).
- Branch + PR per work unit; conventional commits; never co-author AI.
- **DISCIPLINES §9 is the spine of this phase.** The install dialog is the security boundary, not the filesystem. Every code path that loads a plugin — declared, implicit-local, updated, hot-reloaded — routes through approval unless the plugin is in (per-plugin/per-directory) dev mode. There is no global auto-approve.
- **DISCIPLINES §6 (data persistence) applies.** `plugins.lock`, the cache, and the per-plugin approval state are data Mote writes; each PR that adds a write ships the §6 PR-description section.

---

## 1. Crate responsibilities

Phase 3 fills two stub crates and rewires one shell module. No new internal layering crates; the topology in `00-master-plan.md` §1.2 already places `mote-pluginmgr` and `mote-cli` above `mote-runtime`.

### 1.1 `mote-pluginmgr` — the management engine

The engine. Everything that decides *which* plugin code is on disk, *whether it matches what was approved*, and *how the lifecycle is driven*. It does **not** own the runtime's load pipeline (that's `mote-runtime`); it resolves sources to on-disk directories, reads `init.lua`, computes/verifies integrity, decides approval state, and calls `Runtime::{load,reload,unload}`.

**Responsibility:**
- Parse + evaluate `plugins.lua` (a restricted **config-Lua** context exposing `mote.plugins`, distinct from the plugin sandbox — see §2) into a `PluginSpecSet`.
- Parse/serialize `plugins.lock` (TOML via `serde`+`toml`).
- Source fetching: `github:`/`git+https://` via **gix** (pure Rust; F2), `path:`, `bundled` (embedded in binary).
- The content-addressed cache `~/.cache/mote/plugins/<name>/<commit>/` + the `~/.config/mote/plugins/<name>` symlink-vs-real-dir scheme.
- BLAKE3 **directory** hash per DESIGN §Hash computation + integrity verification on load.
- Capability-contract dependency resolution: resolve the `consumes` graph, surface dangling-consumer gaps, order the fetch/load set. **NOT semver** (ADR-0002).
- Approval-state persistence: store the last-approved `ApprovalHash` per plugin (in `mote-storage`); compare on update/reload to decide re-approval (DISCIPLINES §9).
- Update flow: fetch new commit, diff manifests, surface permission changes, mark needing-re-approval.
- First-party update notification: poll canonical Git sources for `bundled` plugins (inbound version query only).
- Implicit-local detection: bare dirs in `plugins/` not in `plugins.lua`.
- Dev mode: per-plugin/per-directory auto-approve state machine + the visual-mark flag surfaced to `mote-ui`.
- Per-identity `plugins.lua` (`~/.config/mote/identities/<id>/plugins.lua` overrides global).
- Produce the `mote-ui` integrity-panel/approval view-models from live state (provenance, integrity status, requested→effective, pending diffs).

**Key public types (sketch):**
```
PluginSpec { name: PluginName, source: Source, version: Option<String> }
Source     = Github { owner, repo } | Git { url } | Path { path } | Bundled { bundle: Option<String> }
PluginSpecSet  // the parsed plugins.lua, keyed by PluginName
LockEntry  { commit: Option<String>, checksum: Checksum, source: Source }
LockFile   // the parsed plugins.lock; serde+toml round-trip
Cache      // ~/.cache/mote/plugins; insert(name, commit, tree) -> CacheKey; link(name, commit)
DirHash    // the DESIGN §Hash computation BLAKE3 directory hasher -> Checksum
Fetcher (trait) { fn fetch(&self, src, version) -> Result<(commit, TempTree)> }
  GixFetcher          // github:/git+https:
  PathFetcher         // path: (no fetch; resolve + real-dir)
  BundleProvider      // bundled (embedded via include_bytes!/include_dir)
Provenance  = DeclaredGit { source, commit } | PathLocal { path } | ImplicitLocal { path } | DevMode { path } | Bundled
ApprovalStore  // wraps mote-storage Namespace; get/put last-approved ApprovalHash per plugin
ResolvedPlugin { name, dir: PathBuf, provenance: Provenance, integrity: IntegrityStatus, manifest: Manifest }
DiffReport { permission_changes: Vec<PermDelta>, capability_changes, consumes_changes, identity_scope_change }
DevMode { plugin_dirs: Vec<PathBuf> }  // from mote.dev_mode in config
PluginManager  // the façade the shell + CLI call; owns Cache + paths + ApprovalStore + a &mut Runtime seam
```

**Depends on:** `mote-types` (PluginName, Checksum), `mote-lua` (the config-Lua evaluator for `plugins.lua` + `Manifest` re-export), `mote-registry` (only transitively, via runtime — pluginmgr does not validate the registry itself), `mote-storage` (approval-state + identity scope), `mote-runtime` (drive `load`/`reload`/`unload`, read `RunningPlugin`), `mote-ui` (produce view-models), `blake3`, `gix`, `serde`+`toml`. `mote-secrets` for the `link` helper (Phase 4 — `link` is stubbed to delegate; see §5).

**Governs:** DESIGN §Plugin Management; DISCIPLINES §9; ADR-0001/0002.

### 1.2 `mote-cli` — the `mote` / `mote plugin` command surface

Thin argument layer. Parses `mote plugin <subcommand>`, builds a `PluginManager`, calls into it, renders text output. Must run *without* launching the engine for the offline subcommands (`add`, `diff`, `gc`, `sync`-fetch-phase, `import`, `source`, `pin`, `remove`, `rollback`) — these only touch files + cache + lock, not a live `Runtime`. Subcommands that change the running set (`update`'s reload, `review`'s approve→reload) need a live runtime; in v0.1 these are invoked **in-process from the running browser** (the integrity panel actions), and the CLI variant either operates on next-launch state (writes lock + marks pending) or is a no-op-with-message when the engine isn't running. See §6 for the in-process vs CLI split.

**Responsibility:** the `clap` command tree; map each subcommand to a `PluginManager` call; format `DiffReport`/`ResolvedPlugin` for the terminal; exit codes. No business logic.

**Key types:** a `clap`-derived `Cli { command: PluginCommand }` enum; a `run(cli) -> ExitCode` entry the binary calls.

**Depends on:** `mote-pluginmgr`, `mote-secrets` (link), `clap`.

**How `mote-app` dispatches to it.** `mote-app::main` currently runs the CEF process split, then unconditionally boots `mote_shell::run()`. Phase 3 inserts a branch **after** the subprocess check and **before** the shell boot: if `std::env::args()` names a management subcommand (first non-exec arg is `plugin` or `secrets`), dispatch to `mote_cli::run(...)` and return its exit code; otherwise boot the shell. The CEF subprocess shim must still run first (a CLI invocation is the browser-role process and never spawns renderers, so the split returns `Browser` and we proceed to the CLI branch). This keeps `mote plugin sync` runnable without a window.

---

## 2. `plugins.lua` evaluation + `plugins.lock` format

### 2.1 `plugins.lua` is Lua — the config-Lua context

`plugins.lua` is **not** TOML; it is Lua that calls `mote.plugins({...})` (DESIGN §Manifest and lock file):

```lua
-- ~/.config/mote/plugins.lua
mote.plugins({
  adblock         = { source = "github:mote-browser/adblock" },
  vim_mode        = { source = "github:mote-browser/vim-mode" },
  cool_plugin     = { source = "github:them/cool-plugin", version = "v1.2.3" },
  my_local_plugin = { source = "path:~/code/my-plugin" },
})
```

This requires evaluating Lua, but in a context **distinct from the plugin sandbox**. The plugin sandbox (`mote_lua::new_sandbox`) strips `io`/`os`/`require` and exposes the `mote.*` plugin host API. The config context needs a *different* surface: it exposes exactly `mote.plugins` (and, because the same file family carries `mote.dev_mode`, `mote.updates.configure`, and per-DESIGN config calls, those too) and nothing the plugin host API exposes. It is read-only with respect to the browser: calling `mote.plugins(t)` records a table; it does not mutate runtime state.

**Implementation:** add a `config` module to `mote-lua` (it already owns `mlua` + the sandbox machinery; it is the only crate that may). Provide `mote_lua::eval_config(source, chunk_name) -> Result<ConfigCapture>`, where `ConfigCapture` is a host-owned struct collecting the recorded calls (`plugins`, `dev_mode`, `updates`). The config Lua state:
- starts from a minimal sandbox (no `io`/`os`/`loadstring`/`require`) — a config file should not read arbitrary files or shell out;
- installs a `mote` global table whose `plugins`/`dev_mode`/`updates.configure` functions are Rust closures that capture their table argument into the `ConfigCapture` (via `mlua`'s app-data or an `Rc<RefCell<…>>`);
- runs the chunk once; the captured tables are then converted to typed Rust (`PluginSpecSet`, `DevMode`, update cadence).

**Relationship to the deferred settings/config-is-Lua system (flag, do not build).** DESIGN's core principle #5 ("config-is-plugin") and §UI Composition imply a *much* larger config-Lua surface eventually (`mote.theme_overrides`, `mote.workspace.define`, `mote.keys.bind`, `mote.dispatch.order`, `mote.on(...)`, etc. — see risks A2). That full settings/config system is **deferred** (ROADMAP Phase 2 "Settings model — deferred"; DISCIPLINES §7 settings-UI-is-a-plugin). Phase 3 builds **only** the `mote.plugins` / `mote.dev_mode` / `mote.updates.configure` slice of the config-Lua context — the minimum to make plugin management dotfile-driven. The `eval_config` entry is designed to be **extended additively** (new `mote.*` config functions register more closures) so the deferred settings system grows into it rather than replacing it. `mote-pluginmgr` owns the `plugins`/`dev_mode`/`updates` interpretation; when the settings system lands it will own the rest. **Decision needed:** confirm pluginmgr owns `eval_config` invocation for now and the broader config loader (`mote-shell`'s future `init.lua` loader) will share the same `mote-lua::config` module. See `03-risks.md` R2.

`PluginManager` calls `mote_lua::eval_config` on the resolved `plugins.lua` path (global, then per-identity overlay) and builds `PluginSpecSet`.

### 2.2 `plugins.lock` TOML format

Machine-managed, checked into dotfiles, opaque to users (DESIGN: "its TOML format is an implementation detail"). One table per plugin:

```toml
# ~/.config/mote/plugins.lock  (machine-managed)
[plugins.adblock]
source   = "github:mote-browser/adblock"
commit   = "abc123def456..."
checksum = "blake3:..."          # DIRECTORY hash (B4: blake3:, not sha256:)

[plugins.cool-plugin]
source   = "github:them/cool-plugin"
commit   = "def456abc789..."
checksum = "blake3:..."

[plugins.my-local-plugin]
source   = "path:~/code/my-plugin"
# no commit for path sources; checksum is the dir hash at last sync
checksum = "blake3:..."
```

- Keyed by the **canonical `PluginName`** (hyphenated), not the `plugins.lua` Lua key (which DESIGN writes with underscores, e.g. `vim_mode`). The Lua-key↔PluginName mapping is a real ambiguity — see `03-risks.md` R4. The lock and cache always use the manifest's `PluginName`.
- `commit` is present for Git sources, absent for `path:`/`bundled` (no commit; `checksum` is the dir hash at last sync). `source` is recorded so `sync` on a fresh machine knows where to fetch.
- `checksum` is the **directory** BLAKE3 hash (§3.3), the integrity anchor. There is **no per-manifest checksum** (B4 resolution: drop `checksum` from the manifest; the lock's directory hash is the mechanism). `mote_lua::Manifest.checksum` is read but ignored for integrity (kept for forward-compat / display only).
- Serialize/deserialize with `serde` derive + the `toml` crate (`toml = "1.1"` already used by `mote-registry`). Round-trip must be stable (deterministic key order — `BTreeMap<PluginName, LockEntry>`).

---

## 3. Sources, fetching, cache, integrity

### 3.1 Source types (v0.1)

| Source | Syntax | Fetch mechanism | Cache form |
|---|---|---|---|
| GitHub | `github:<owner>/<repo>` | gix clone of `https://github.com/<owner>/<repo>.git` at resolved commit | symlink → cache/`<name>`/`<commit>`/ |
| Generic Git | `git+https://…` | gix clone at resolved commit | symlink → cache/`<name>`/`<commit>`/ |
| Path | `path:<local-path>` | none — resolve `~`, canonicalize | **real dir** (the path itself is the plugin dir; nothing copied) |
| Bundled | `bundled` (`bundled:<name>` reserved v0.2+) | unpack embedded tree from the binary into cache | symlink → cache/`<name>`/`bundled-<mote-version>`/ |

`Source::parse(&str)` lives in pluginmgr (the prefixes are pluginmgr's grammar). `github:` is sugar over `git+https://github.com/...`.

### 3.2 Fetching with gix (F2)

gix is pure Rust (no libgit2 FFI), matching the `unsafe_code = "deny"` posture and DESIGN §Implementation Language's memory-safety rationale. The fetch contract `Fetcher::fetch(src, version) -> (commit_sha, TempTree)`:
- resolve `version` (a tag/branch/commit-ish; default = default branch HEAD) to a commit SHA;
- fetch that commit's tree into a temp dir (a blobless/shallow checkout of the single commit is sufficient — we only need the working tree at that commit, not history);
- return the SHA + the temp tree; the caller hashes + moves it into the cache under `<name>/<sha>/`.

**gix capability to confirm at spike time (`3.1`):** does the pinned gix version do a single-commit fetch + working-tree checkout into an arbitrary directory without a full clone? If shallow-at-commit is awkward in the gix API, the fallback is a normal clone into the temp dir then checkout the commit — slower but correct. This is the one gix-API uncertainty; spike it in `3.1` before building `update`/`sync`. See `03-risks.md` R1. (If gix proves unworkable for fetch-at-commit in v0.1, the documented fallback is shelling to `git` via `std::process::Command` — but that reintroduces a system-git dependency and is the last resort, flagged for maintainer decision, not chosen here.)

Network/offline behavior: a fetch failure (offline, host down) is a recoverable error, not a crash. `sync`/`update` report which plugins couldn't be fetched and leave the existing cache/symlink untouched. Plugins already in the cache load fine offline (the lock + cache are self-contained). See `03-risks.md` R5.

### 3.3 The content-addressed cache + symlink scheme

Layout (DESIGN §Cache layout):
```
~/.cache/mote/plugins/
  cool-plugin/abc123def456/   init.lua …      # one dir per fetched commit
  cool-plugin/def456abc789/   init.lua …      # previous version retained → instant rollback
~/.config/mote/plugins/
  cool-plugin/                 -> ~/.cache/mote/plugins/cool-plugin/def456abc789/   (symlink, git source)
  my-local-plugin/             (real dir, path: source)
  pasted-plugin/               (real dir, implicit local)
```

- `Cache::insert(name, commit, temp_tree)` moves a fetched tree into `<cache>/<name>/<commit>/` (idempotent — if the commit dir already exists and verifies, reuse it; this is what makes re-`sync` cheap and shared across identities).
- `Cache::link(name, commit)` (re)points `~/.config/mote/plugins/<name>` at the cache dir. Rollback (`mote plugin rollback`) is just `link` to the previous commit — no file copies.
- `path:` and implicit-local plugins are **real directories** under `~/.config/mote/plugins/<name>`, never symlinks. `Cache` does not touch them; their integrity hash is computed in place.
- On Windows (not v0.1 target, but keep the seam clean) symlinks need privilege; abstract link-vs-junction-vs-copy behind `Cache::link` so the platform detail is one function.

### 3.4 BLAKE3 directory hash (DESIGN §Hash computation, exact spec)

`DirHash::of(dir) -> Checksum`:
1. Enumerate files **recursively** from the plugin root.
2. Sort file paths **lexicographically** (paths relative to the root, as UTF-8 strings) for cross-filesystem determinism.
3. For each file, feed the hasher the **path string** (UTF-8 bytes) **and** the file's **byte contents** (with an unambiguous separator/length framing so `path="ab"+contents="c"` ≠ `path="a"+contents="bc"`).
4. **Symlinks within the dir are not followed** — hash by the **target path string**, not the pointed-to contents.
5. Transient state in the plugin dir is a hashing hazard (DESIGN: plugins must not write logs/scratch into their own dir; that's what `storage:persistent` is for). The hasher hashes whatever is there; the *contract* is documented, not enforced. (One consequence: a `path:`/implicit plugin the user is editing will change hash on every edit — handled by dev mode and `mote plugin pin`.)

Output is a `mote_types::Checksum` (`blake3:<hex>`), so it round-trips into `plugins.lock` and the integrity panel directly.

### 3.5 Integrity verification on load (DESIGN §Integrity verification)

Before `Runtime::load`, pluginmgr computes the on-disk `DirHash` of the resolved plugin dir and compares it to the lock's `checksum`:
- **match** → `IntegrityStatus::Verified`; proceed to load.
- **mismatch** → **refuse to load**; surface `IntegrityStatus::Mismatch` in the integrity panel ("checksum mismatch — run `mote plugin sync` to restore"); the user may `mote plugin pin <name>` to accept the current state (recomputes + writes the new hash to the lock as an intentional local edit).
- `bundled` → `IntegrityStatus::Bundled` (the bundle is the binary; the hash is computed at unpack time and is trustworthy by construction).
- dev-mode → `IntegrityStatus::DevMode` (hash is informational; dev plugins are expected to change).
- no lock entry yet (freshly added / implicit-local before pin) → `IntegrityStatus::Unknown` until first sync/pin writes a checksum.

Integrity is *integrity*, not *trust* — the trust decision is the approval dialog (§6). The checksum guarantees the file Mote runs is the file the user approved.

---

## 4. Dependency resolution = capability contracts (ADR-0002)

ROADMAP's "dependency graph resolution (library plugins, transitive fetches)" is **reinterpreted** per ADR-0002. There is **no semver resolver**, no `requires`, no named plugin dependency. "Dependencies" are entirely capability contracts.

**What pluginmgr does:**
- The fetch set is exactly the plugins **declared in `plugins.lua`** (plus bundled defaults + detected implicit-locals). There are **no transitive *fetches*** — a plugin cannot pull in another plugin by name. "Transitive" means capability-chain *load ordering*, not fetching more code.
- After resolving the declared set's manifests, build the `consumes`→`capability fulfiller` graph. A plugin that consumes a capability **no installed plugin fulfills** is a **dangling consumer** — the runtime already raises `LoadError::DanglingConsumer` at load-step 1; pluginmgr surfaces it *before* attempting load, in the integrity panel, with the DESIGN-specified message and the resolution hint ("install a plugin that fulfills `<cap>`").
- **Load ordering:** a consumer must load *after* its fulfiller (the runtime checks `is_fulfilled` at step 1). pluginmgr topologically orders the declared set by the consumes→fulfills edges and loads fulfillers first. A cycle (A consumes a cap B fulfills and vice-versa) is rare but possible; load both, then resolve — actually the runtime's `resolve_consumes` requires the fulfiller already loaded, so a true cycle is a dangling-consumer error for whichever loads first. Document this: capability cycles are unsupported in v0.1 (flag in `03-risks.md` R6 if it bites; the first-party set has no cycles).
- "Library plugin" (e.g. `password-manager-form-services-plugin`) = a plugin that fulfills a consumed capability but ships no leaf UI. No special handling — it's just a fulfiller that must load first.

`mote-runtime` already owns the actual dangling-consumer + exclusive-double-claim checks (`resolve_consumes`, `check_exclusive_claims`). pluginmgr's job is **ordering + pre-flight surfacing**, not re-implementing resolution.

---

## 5. The CLI surface

Each `mote plugin <cmd>`, what it does, and what it mutates. (M = mutates.)

| Command | Behavior | Mutates |
|---|---|---|
| `add <source> [--version <v>]` | Parse source, fetch (Git) / resolve (path), compute dir hash, write a `plugins.lua` entry (call-rewriting, §5.1) + a `plugins.lock` entry, cache + symlink. Does **not** auto-approve — approval happens at next load / `review`. | M: plugins.lua, plugins.lock, cache, symlink |
| `remove <name>` | Remove the `plugins.lua` entry + lock entry; **cache entry retained** (DESIGN: enables re-add/rollback; `gc` reclaims later). Drop the symlink. | M: plugins.lua, plugins.lock, symlink |
| `update [<name>]` | Fetch latest matching the version constraint; compute new hash; diff manifests (§6.3). If `{permissions,capabilities,consumes,identity_scope}` expanded → mark **needs re-approval**, do not relink until approved; else relink + update lock. For a `bundled` plugin → prompt to switch source to Git (DESIGN §First-party). | M: plugins.lock, cache, (symlink iff non-expanding) |
| `source <name> <new-source>` | Change a plugin's source (e.g. `bundled` → `github:…`). Sticky thereafter (DESIGN §User-chosen sources are sticky). Re-fetch + re-hash + re-link. | M: plugins.lua, plugins.lock, cache, symlink |
| `sync` | Reconcile cache + plugins dir with the lock: for each lock entry, ensure the pinned commit is cached (fetch if absent) and the symlink points at it; verify dir hashes. The fresh-machine command. | M: cache, symlinks (NOT the lock — sync obeys it) |
| `rollback <name>` | Relink to the previous cached commit (no fetch, no copy). | M: symlink, (lock's active commit pointer) |
| `diff <name>` | Show what an update *would* change, including the permission delta — the same diff the approval dialog renders, **headless** (DISCIPLINES §9 mechanism). Read-only. | none |
| `import <name>` | Promote an implicit-local plugin into `plugins.lua` (write the `path:` entry + lock entry) for reproducibility. | M: plugins.lua, plugins.lock |
| `gc` | Remove unreferenced cache entries (commits no lock entry and no rollback-window points at). | M: cache |
| `review <name>` | Show pending permission changes and approve them (drives the approval flow headlessly or marks approved for next launch). | M: approval-state store; (triggers reload if engine running) |
| `pin <name>` | Checksum-pin + approve a manually-written / edited plugin: compute current dir hash, write it to the lock, record the current `ApprovalHash` as approved. Resolves a `Mismatch`. | M: plugins.lock, approval-state store |
| `link <secret-name>` | CLI helper mapping a secret to a vault item — **Phase 4** (`mote-secrets`). v0.1 Phase-3 stub: parse + delegate to `mote-secrets` if present, else a clear "secrets backend lands in Phase 4" message. | M: secrets.lua (Phase 4) |

### 5.1 How `mote plugin add` rewrites the `plugins.lua` call

DESIGN: "the CLI can mutate it programmatically by rewriting the call." `plugins.lua` is Lua, so a naïve regex rewrite is fragile. Two candidate approaches (decision in `03-risks.md` R3):
- **(A) Evaluate-then-regenerate.** `eval_config` already captures the `mote.plugins({...})` table. `add`/`remove`/`source` mutate the captured `PluginSpecSet` and **regenerate** the `mote.plugins({...})` call from the typed model, replacing the original call's source span. Pro: robust, no Lua-string surgery. Con: loses user comments/formatting inside the call (a real cost for a hand-edited dotfile).
- **(B) Targeted source-span edit.** Locate the `mote.plugins({ … })` call span and edit only the affected key's line, preserving the rest verbatim. Pro: preserves comments/order. Con: needs light Lua-table-literal parsing (not full Lua), brittle for unusual formatting.

**Recommendation:** (B) with a documented fallback to (A) when the call can't be span-located (e.g. the user built the table dynamically). The common case — a literal `mote.plugins({ key = { source = "…" } })` — is span-editable while preserving comments; the dynamic case regenerates. **Maintainer decision required** because it trades comment-preservation against implementation complexity.

---

## 6. Install → approval FLOW (the Phase-2-deferred item)

This is the load-bearing integration of Phase 3. Phase 2 built the approval-dialog **surface** (`mote_ui::ApprovalRequest` + `chrome/approval-dialog.html`) and the runtime's `ApprovalPolicy` **seam**, but left the *flow* — "installing/detecting a plugin actually drives the dialog and persists the decision" — to Phase 3.

### 6.1 The seam: a UI-backed `ApprovalPolicy`

`Runtime::load(source, identity, policy: &dyn ApprovalPolicy)` already calls `policy.decide(plugin, requested) -> Approval` at step 4. Phase 2's shell uses `GrantAsRequested` (trusted bundle). Phase 3 introduces `DialogApprovalPolicy`, living in `mote-shell` (it bridges runtime ↔ chrome; it needs the bridge to render the dialog and block for the user's decision):

```
DialogApprovalPolicy {
  bridge,                 // the mote-cef host bridge to render approval-dialog.html
  approval_store,         // mote-pluginmgr ApprovalStore (last-approved hashes)
  dev_mode: &DevMode,     // per-plugin/per-directory auto-approve
  plugin_kind,            // for the dialog's provenance + dev-mark
}
impl ApprovalPolicy for DialogApprovalPolicy {
  fn decide(&self, plugin, requested) -> Approval {
    if self.dev_mode.covers(plugin) { return Approval::GrantAsRequested; }   // dev-mode auto-approve
    if let Some(prior) = self.approval_store.get(plugin) {
        // re-approval path: compare hashes; if not an expansion, grant silently
        if !ApprovalHash::of(manifest).is_expansion_of(&prior) { return Approval::GrantAsRequested; }
    }
    // build mote_ui::ApprovalRequest from `requested` + registry descriptions
    // + combinations.yaml dangerous-combos; render via bridge; block for user;
    // map the dialog result (grant / narrow per-permission / deny) to Approval.
  }
}
```

The dialog construction reuses `mote_ui::{ApprovalRequest, NarrowablePermission, NarrowMode}` (already built, with `effective_string()` mapping narrowing → effective permission). The dangerous-combination warnings come from `mote-registry::CombinationRegistry` (built in Phase 1). The mapping of the dialog's per-permission narrowing back to `Approval::Narrow { narrowings }` is mechanical: each `NarrowMode::GrantOrigins(globs)` → a `Narrowing { domain, action, resources }`; `Deny` → drop the permission (or `Approval::Deny` if a *required* one is denied — the plugin then refuses to run, which is fine, it reads `permissions.effective()`).

> **Threading note (`03-risks.md` R7).** The CEF page is `!Send` and lives on the pump thread; `decide` is called synchronously inside `Runtime::load` on whatever thread the load runs on. The cleanest wiring loads plugins **on the shell's pump thread** (where the bridge + page live) so `decide` can render + await on the same thread via a nested pump, OR loads off-thread and `decide` posts a render request + blocks on a channel the pump-thread fulfills. The shell already runs the runtime on the main loop (Phase 2 `PluginHost`), so in-process loads on the pump thread are the natural choice. **Maintainer decision: confirm the approval dialog is modal-blocking (load waits) vs. async (load defers, plugin enters "awaiting approval").** DESIGN §Hot Reload's "awaiting approval" state suggests the async path is the real model for *updates*; first-install can be modal. Recommend: **first install = modal blocking; update/expansion = async "awaiting approval"** (the runtime's `reload(require_reapproval=false)` already returns `ApprovalDenied` and keeps the old instance running — exactly the async model).

### 6.2 Approval-state persistence + comparison

`mote-pluginmgr::ApprovalStore` persists the last-approved `ApprovalHash` per plugin in `mote-storage` (a dedicated namespace, not a plugin's namespace). The stored form is the `ApprovalHash` fingerprint (the four field-lists) so `is_expansion_of` can run, plus its `Checksum` for compact display. On every load/reload/update:
- **no stored hash** → first install → full approval dialog → on grant, store the hash.
- **stored hash, not an expansion** (code-only or contraction) → no prompt; the runtime intersects (DESIGN §Hot Reload). Update the stored hash to the new (possibly contracted) one.
- **stored hash, expansion** → re-approval dialog showing the *delta* (`is_update=true`, `new_permissions` populated) → on grant, store the new hash.

This is exactly the DISCIPLINES §9 mechanism: "permission/capability/consumes/identity_scope hashes stored per plugin; load-time compares against the last-approved hash."

### 6.3 `diff` / `review` / `pin`

- `mote plugin diff <name>`: fetch (or read cached) the candidate manifest, compute its `ApprovalHash`, diff against the stored approved hash, render the `DiffReport` as text — the same delta the dialog shows, headless (DISCIPLINES §9). Pure read.
- `mote plugin review <name>`: show the pending diff + approve. In-process (engine running): drive the dialog or directly store the new approved hash + `reload(require_reapproval=true)`. CLI (engine not running): store the approved hash so the next launch loads it without prompting.
- `mote plugin pin <name>`: for a manually-written/edited plugin — compute the current dir hash → write to lock (resolves `Mismatch`), and record the current `ApprovalHash` as approved (so it loads without a dialog). This is the "I edited this on purpose" escape hatch.

### 6.4 Implicit-local detection → approval flow

On startup (and on a `plugins/` dir change later), pluginmgr scans `~/.config/mote/plugins/<name>/` for dirs **not** in `PluginSpecSet` and **not** symlinks into the cache. Each is an implicit-local plugin (`Provenance::ImplicitLocal`). It is loaded through the **same** approval flow (DISCIPLINES §9: "the install dialog is invoked on first detection of any plugin, declared or implicit local"). The integrity panel labels it `◇ implicit` (the `PluginKind::ImplicitLocal` glyph already exists in `mote-ui`). `mote plugin import` promotes it to a declared `path:` entry. This preserves the Claude-Code-drops-a-plugin workflow.

### 6.5 Wiring the integrity-panel actions (revoke / update / rollback / reload / adjust scope)

Phase 2 built `PluginAction` and the panel rows but the **actions are unwired** (`actions: Vec::new()` in the current `PluginHost::build_panel`; ROADMAP Phase 2 "revoke/update/rollback/reload actions wire with Phase-3 plugin management"). Phase 3 wires each panel action through the bridge → an op → a `PluginManager`/`Runtime` call:

| Action | Wiring |
|---|---|
| Update | `PluginManager::update(name)` → fetch + diff → if expansion, async re-approval; else relink + `Runtime::reload`. |
| Rollback | `PluginManager::rollback(name)` → relink previous commit → `Runtime::reload(require_reapproval=false)` (rollback never expands). |
| Reload | `Runtime::reload` (dev-mode / path-local; forced reload regardless of file change). |
| Revoke | revoke a permission / disable: `Runtime::unload(name)` + record the revocation in approval state (revocation is persistent + dotfile-checkable per DESIGN §Revocation — full revocation persistence is a thin Phase-3 addition; the *granular* per-permission revoke editor may lean on Phase-2 narrowing UI). |
| Adjust scope | re-open the narrowing editor (the approval dialog's per-permission UI) → produce new `Narrowing`s → `Runtime::reload` with the narrowed grant. |

`PluginHost::build_panel` is updated to populate `kind`/`integrity`/`actions` from `PluginManager` provenance + integrity instead of hardcoding `Bundled`/empty.

---

## 7. Update flow, first-party notifications, dev mode, per-identity

### 7.1 Update flow with permission-change surfacing (DESIGN §Update flow)

`update` fetches the new commit, computes the new `ApprovalHash`, and produces a `DiffReport`. CLI output makes permission changes **prominent** (DESIGN's exact format):
```
Updating cool-plugin: v1.2.0 → v1.3.0
Permission changes:
  + http:fetch:https://api.new-analytics.com/*   (NEW)
  + tabs:get_history                              (NEW)
  - sys:notify                                    (REMOVED)
cool-plugin requires re-approval before it will load.
Run `mote plugin review cool-plugin` to view and approve.
```
If the delta is an expansion → mark needs-re-approval, do **not** relink (the running instance keeps running — the runtime's async model). If code-only/contraction → relink + reload transparently. In the integrity panel, permission-changing updates render **visually distinct** from code-only ones (DISCIPLINES §9: users autopiloting on updates never lose visibility on permission expansion) — `mote-ui` carries this via the `is_update`/`new_permissions` fields on `ApprovalRequest` and a distinct badge on the panel row.

### 7.2 First-party update notifications (DESIGN §First-party plugins and updates)

For plugins still on `source = "bundled"`, pluginmgr periodically polls the canonical Git source (`mote.updates.configure { check_first_party = "weekly" }`; `never` disables; default weekly). This is an **inbound version query**, not outbound user data (Core Principle #9 carve-out). When a newer upstream version exists, the integrity panel surfaces "Switch to Git and update" / "Dismiss" — never auto-switches (user-chosen sources are sticky). `mote plugin update <bundled-plugin>` prompts to switch source rather than erroring (DESIGN's exact prompt). The poll cadence is read from the config-Lua `mote.updates.configure` capture.

### 7.3 Dev mode (DISCIPLINES §9; DESIGN §Plugin dev mode)

`mote.dev_mode({ directories = { … } })` (captured by `eval_config`) → `DevMode { plugin_dirs }`. A plugin whose resolved dir is under a dev-mode directory is **auto-approved on every load and every permission change** (the `DialogApprovalPolicy` short-circuit in §6.1). Dev mode is **per-plugin/per-directory only** — there is **no global auto-approve toggle** (DISCIPLINES §9). Dev-mode plugins are visually marked everywhere: `PluginKind::DevMode` + `IntegrityStatus::DevMode` (both already in `mote-ui`), the `⊙` glyph, and the dev-mark surfaced to any UI the plugin owns (the `mote-plugin-devtools` Tier-3 plugin mirrors this — Phase 7). The dev-mode **state machine** is `[UI-INDEPENDENT]` (3.7); only the marking is UI-gated.

### 7.4 Per-identity `plugins.lua` (DESIGN §Identity and the cache)

The cache is **shared across identities** (code-on-disk is identity-independent). An identity may carry its own `~/.config/mote/identities/<id>/plugins.lua` + `plugins.lock` that **overrides** the global manifest while still drawing from the shared cache. pluginmgr resolves the effective `PluginSpecSet` for an identity as: global `plugins.lua`, then the per-identity overlay (per-plugin override by `PluginName`). Storage namespaces remain per-`identity_scope` (already handled by `mote-storage`/runtime). Approval state is keyed per plugin (the *code* is the same; approval is about the manifest, not the identity) — confirm whether approval should be per-identity too (`03-risks.md` R8; recommend global approval keyed by plugin+ApprovalHash, since the approved manifest is identical regardless of identity).

---

## 8. Ordered work breakdown + verification

### 8.1 Work units (file/crate overlap → parallelism)

The phase splits into a **foundation layer** (`mote-pluginmgr` pure logic, no runtime/UI) that parallelizes heavily, and a **wiring layer** (shell + CLI) that serializes on the foundation.

| Unit | Title | Crate(s)/files | Depends on | Parallel? |
|---|---|---|---|---|
| **3.1a** | `Source` parse + cache + symlink scheme | `mote-pluginmgr` (`source.rs`, `cache.rs`) | — | ‖ (leaf) |
| **3.1b** | BLAKE3 `DirHash` (exact DESIGN spec) + integrity verify | `mote-pluginmgr` (`dirhash.rs`) | `mote-types::Checksum` | ‖ (leaf) |
| **3.1c** | gix `Fetcher` (the one API-uncertainty spike) | `mote-pluginmgr` (`fetch.rs`) + workspace dep `gix` | 3.1a | ‖ after 3.1a (own file) |
| **3.1d** | `eval_config` config-Lua context (`mote.plugins`/`dev_mode`/`updates`) | `mote-lua` (`config.rs`) | — | ‖ (separate crate, separate file) |
| **3.1e** | `plugins.lock` serde+toml model + round-trip | `mote-pluginmgr` (`lock.rs`) | `mote-types` | ‖ (leaf) |
| **3.2** | `PluginSpecSet` from `eval_config`; consumes-graph ordering + dangling pre-flight | `mote-pluginmgr` (`resolve.rs`) | 3.1d, 3.1e, `mote-runtime` types | → after 3.1d/3.1e |
| **3.3** | CLI surface (`add/remove/update/source/sync/rollback/diff/import/gc/review/pin/link`) + `add` call-rewriting | `mote-cli` (whole), `mote-pluginmgr` (`manager.rs` façade), `mote-app` dispatch branch | 3.1a–e, 3.2 | → (the integrator) |
| **3.4** | `ApprovalStore` + update-flow diff + re-approval-hash compare | `mote-pluginmgr` (`approval_store.rs`, `diff.rs`) | 3.1e, `mote-runtime::ApprovalHash`, `mote-storage` | ‖ with 3.3 (own files) |
| **3.5** | Bundled distribution (embed + unpack) + first-party upstream poll | `mote-pluginmgr` (`bundle.rs`), `plugins/` tree | 3.1a/b | ‖ |
| **3.6** | `DialogApprovalPolicy` + install→approval flow + implicit-local detection + per-identity overlay | `mote-shell` (`approval.rs`, rewire `runtime.rs`/`PluginHost`) | 3.2, 3.4, `mote-ui` view-models, bridge | → (last; needs the engine) |
| **3.7** | Integrity-panel action wiring (revoke/update/rollback/reload/adjust-scope) + dev-mode marking | `mote-shell` (`runtime.rs` build_panel + ops) | 3.6 | → after 3.6 |

**Parallel groups:**
- **Group A (immediately, fully parallel — pure logic, no engine/UI):** 3.1a, 3.1b, 3.1d, 3.1e, and 3.5's embed step. These touch disjoint files in `mote-pluginmgr`/`mote-lua` with no cross-deps. Land smallest-surface first (DISCIPLINES merge order): 3.1b (dirhash) and 3.1e (lock) are the smallest.
- **Group B (after their group-A dep, still parallel):** 3.1c (after 3.1a), 3.4 (after 3.1e).
- **Group C (serializes — the integrators):** 3.2 → 3.3 (CLI) and 3.6 (flow) → 3.7 (action wiring). 3.3 and 3.4 can overlap (disjoint files); 3.6 needs both 3.2 and 3.4.

**Merge order = ascending blast radius:** pure leaves (dirhash, lock, source/cache, eval_config) → fetch/approval-store → resolve → CLI → flow → action wiring. A break in a leaf has a tiny diff to bisect; the flow/wiring units (largest surface, touch the shell) land last.

### 8.2 Verification strategy

**Per-unit (unit/property tests):**
- `DirHash`: determinism (same tree → same hash across runs/ordering), sensitivity (any path or content change → different hash), symlink-by-target (a symlink change → hash change without following), framing (path/content boundary can't be confused). The master plan's "BLAKE3 hash determinism" proof-of-done.
- `LockFile`: TOML round-trip stability (parse→serialize→parse identity; deterministic key order). "Lock roundtrip" proof.
- `eval_config`: a `plugins.lua` with `mote.plugins`/`dev_mode`/`updates` parses to the right typed model; a config file that tries `io.open`/`os.execute`/`require` fails (sandbox honored); a non-table arg errors cleanly.
- `Source::parse`: all four prefixes + `github:` sugar; malformed → clear error.
- `add` call-rewriting: add/remove/source round-trips preserve other entries (and, for approach B, comments).
- `diff`/`ApprovalStore`: `diff` shows the permission delta the dialog would; expansion vs contraction classification matches `ApprovalHash::is_expansion_of`. "`diff` shows permission deltas" proof.
- `Fetcher` (gix): against a local on-disk Git fixture repo (no network in CI) — fetch a known commit, verify the checked-out tree + SHA.

**End-to-end (the required integration proof):** a `mote-pluginmgr`/`mote-shell` integration test that exercises the **whole spine**:
1. Write a `path:` plugin to a temp dir (a minimal valid `init.lua` with a manifest + one capability + one permission).
2. Write a `plugins.lua` declaring it (`mote.plugins({ my_plugin = { source = "path:<tmp>" } })`).
3. `PluginManager::sync()` → resolves the spec, computes the dir hash, writes `plugins.lock`, links the dir.
4. Load it through `Runtime::load` (the **four-step pipeline** — Phase 1, unchanged) with a **scripted `ApprovalPolicy`** standing in for `DialogApprovalPolicy` (grant-as-requested, then a narrowing variant) — proving the approval flow seam, headless (no `DISPLAY` needed).
5. Assert the plugin appears in `PluginHost::build_panel()` with `Provenance::PathLocal`, `IntegrityStatus::Verified`, the granted permissions, and (for the narrowing run) requested→effective rows.
6. Mutate the plugin's `init.lua` to **expand** a permission; `update`/reload → assert it enters needs-re-approval (does not silently load); re-approve → loads with the new grant; assert the integrity panel reflects the change distinctly.
7. Corrupt the cached file → `sync`/load → assert `IntegrityStatus::Mismatch` and refusal; `pin` → assert it recovers to `Verified`.

This is the master plan's §3 e2e intent ("declare a `path:` plugin → `mote plugin sync` → loads through the four-step pipeline → approval flow → appears in the integrity panel"), runnable headless in CI.

**Full happy-path (manual / `DISPLAY=:1`, per the global verification rule):** boot the real browser with a `plugins.lua` declaring the `path:` plugin, confirm the **real** approval dialog renders (the `DialogApprovalPolicy` → bridge path), approve, and see the plugin in the live integrity panel with working update/rollback/reload buttons. Integration seams (bridge ↔ policy ↔ runtime ↔ pluginmgr) are exactly where this phase can break, so the end-to-end run — not unit tests — is the gate for closing the phase.

---

## 9. Out of scope / deferred (honest scope)

- **`bundled:<name>` external bundles** — config grammar reserves it; only the binary-embedded bundle is wired (DESIGN §Supported sources). v0.2+.
- **The full config-is-Lua settings system** — Phase 3 builds only the `mote.plugins`/`mote.dev_mode`/`mote.updates.configure` slice of `eval_config`; the rest (`mote.theme_overrides`, `mote.workspace.define`, `mote.keys.bind`, `mote.dispatch.order`, …) is the deferred settings system (ROADMAP Phase 2 "Settings model — deferred"; DISCIPLINES §7). The `eval_config` module is built to grow into it additively.
- **`mote plugin link`** (secret↔vault mapping) — Phase 4 (`mote-secrets`); Phase 3 stubs it to delegate/inform.
- **A registry/discovery source + a graphical plugin browser** — deliberately not in v0.1 (DESIGN §UI for plugin management; Open Decisions). Management UI manages what you have.
- **Plugin signing** — deferred until a third-party registry exists (DESIGN §Threat Model). Integrity (dir hash) ≠ trust (approval); v0.1 ships integrity only.
- **OS file-watch auto-reload** — DESIGN §Hot Reload watches via inotify/FSEvents. Phase 3 provides the *programmatic* reload + implicit-local *startup scan*; the live file-watcher (notify crate) can land in 3.7 or be deferred to polish — flag in `03-risks.md` R9 (the master plan puts "OS file-watch triggering" in Phase 3, so include a minimal `notify`-based watcher for dev-mode/path dirs if time allows).
