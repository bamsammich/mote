# Mote — Master Implementation Plan (v0.1 MVP)

**Status:** Draft for orchestrator review.
**Scope:** ROADMAP Phases 1–8 (full v0.1 MVP through the AI-native MCP goal). Phases 9–11 (distribution, polish, project artifacts) are referenced for cross-cutting concerns but are not the build target of this plan.
**Source of truth:** `DESIGN.md` (architecture), `DISCIPLINES.md` (guardrails), `ROADMAP.md` (phasing), `CLAUDE.md` (toolchain/quality contract). Where this plan and those documents disagree, those documents win — and the disagreement is recorded in `docs/plans/risks-and-inconsistencies.md`.

> **Read `risks-and-inconsistencies.md` before starting any work.** Several design ambiguities and one major design-vs-spec contradiction gate clean implementation. Items flagged `[BLOCKER]` there must be resolved by the user before the affected crate is built.

---

## 0. Ground rules carried from the contract

These apply to *every* work unit below and are not repeated per task:

- **Edition 2024, MSRV 1.95.0.** Member crates inherit via `edition.workspace = true`, `rust-version.workspace = true` (CLAUDE.md §Conventions).
- **Lint policy is workspace-global.** Each crate adds `[lints] workspace = true`. No scattered `#![allow]`; relaxations change the root `[workspace.lints]` table with a justifying comment (CLAUDE.md §Quality gates).
- **`unsafe_code = "deny"` workspace-wide.** Only `mote-cef` re-enables it locally with a justifying comment (CLAUDE.md §Quality gates, DISCIPLINES §1).
- **All tooling through mise:** `mise exec -- cargo …`. CI and lefthook run clippy `-D warnings` and `cargo test --workspace --all-features`.
- **Shared dep versions live in `[workspace.dependencies]`**, referenced with `dep.workspace = true`.
- **`crates/mote-placeholder` is disposable.** Delete it in the first Phase-1 work unit once `mote-types` exists and the workspace still builds.
- **Feature-scoped commits to main**, executed by subagents (per orchestrator brief). Conventional Commit format from the global rules: `<type>(<scope>): <subject>`.
- **Every bug-fix PR answers the three gate questions** (which gate should have caught it, why it didn't, what changed) per the global verification rule.

---

## 1. Crate Topology

The workspace is layered to mirror DESIGN.md's *Layered architecture* (DESIGN §Performance Architecture) and the dependency rules in DISCIPLINES. Arrows in "depends on" point **downward only** — no cycles. The cardinal rule (DISCIPLINES §1): **only `mote-cef` may import `cef`/`cef_rs`.**

### 1.1 Dependency layering (high → low)

```
                      mote-app (binary)
                          │
        ┌─────────────────┼──────────────────────────┐
        │                 │                           │
   mote-shell         mote-mcp                   mote-cli
        │                 │                           │
        ├──────┬──────────┴────────┬─────────┬────────┤
        │      │                   │         │        │
  mote-ui  mote-session     mote-pluginmgr  mote-secrets
        │      │                   │         │        │
        └──────┴─────────┬─────────┴─────────┘        │
                         │                            │
                   mote-runtime ────────────┐         │
                    │  │  │  │               │         │
        ┌───────────┘  │  │  └────────┐  mote-audit    │
        │              │  │           │      │         │
  mote-lua       mote-wasm  mote-dispatch    │         │
        │              │        │            │         │
        └──────────────┴────────┴────────────┴─────────┘
                         │
                   mote-registry
                         │
                   mote-storage
                         │
                  mote-permissions
                         │
                    mote-types
                         │
                    mote-cef  (CEF isolation boundary)
```

`mote-types`, `mote-permissions`, and `mote-cef` are the foundational leaves. `mote-cef` is depended on only by `mote-runtime`/`mote-session`/`mote-shell` (anything that drives the engine), never by leaf domain crates.

### 1.2 Crate-by-crate

For each: **responsibility · key public types/traits · depends on · governing DESIGN/DISCIPLINES section.**

---

#### `mote-cef` — CEF isolation wrapper
- **Responsibility.** The *only* crate permitted to `use cef::` / `use cef_rs::`. Wraps the CEF lifecycle (initialize/shutdown, message loop), `CefBrowserHost` (tab lifecycle), `CefResourceRequestHandler` (network hooks), `CefRenderHandler` (off-screen render), isolated-world script injection, and Chromium profile (= identity) management. Translates CEF C++ idioms into safe Rust types. When a CEF upgrade breaks, the breakage lives here only.
- **Key public types/traits.** `CefApp` (process/lifecycle owner), `Browser` (one tab/view), `ResourceRequestHook` (trait the dispatch layer implements to receive `OnBeforeResourceLoad`), `RenderTarget` (off-screen surface handle), `IsolatedWorld` (per-plugin V8 world handle), `ProfileHandle` (identity → Chromium profile), `CefError`. All CEF pointer types are wrapped; **no `cef::*` type appears in any public signature.**
- **Depends on.** `cef`/`cef-rs`, `mote-types` (for the shared error/url newtypes only). `unsafe_code` re-enabled locally with justification.
- **Governs.** DESIGN §Engine — CEF; DESIGN §Script Injection and Isolated Worlds; DISCIPLINES §1.
- **CI mechanism.** A workspace-level test/lint (`xtask` or a `deny`-style grep gate in CI) fails the build on any `use cef` / `use cef_rs` outside this crate. See §3.7.

#### `mote-types` — shared vocabulary
- **Responsibility.** Zero-dependency (within the workspace) primitives shared everywhere: `PluginName`, `Origin`, `Glob` (permission-pattern glob with negation), `SchemaVersion`, `Checksum` (BLAKE3 newtype), `IdentityId`, `WorkspaceId`, `TabId`, common error enums. No logic that belongs to a domain layer.
- **Key types.** Listed above; plus `serde` derives and the glob-matching impl used by permission scoping.
- **Depends on.** `serde`, `blake3` (newtype only). Nothing else in-workspace.
- **Governs.** DESIGN §Permission Primitives (glob syntax incl. `!` negation), §Integrity verification (BLAKE3).

#### `mote-permissions` — permission model & enforcement primitives
- **Responsibility.** The permission grammar (`domain:action[:resource]`), parsing, glob/negation matching, requested-vs-effective narrowing, and the gatekeeper API the dispatch layer queries ("does plugin X hold permission P for resource R?"). Holds the per-plugin effective grant set; exposes `permissions.effective()` data.
- **Key types/traits.** `Permission`, `PermissionSet`, `EffectiveGrant`, `Gatekeeper` (trait: `check(plugin, permission) -> Decision`), `NarrowingDialogModel` (requested → effective, the data the UI renders). Negative/deny precedence enforced here.
- **Depends on.** `mote-types`.
- **Governs.** DESIGN §Security Model — Permissions and Capabilities; §Permission Primitives; §User narrowing at install time; §Revocation. DISCIPLINES §4 (combination risk inputs).

#### `mote-storage` — SQLite-backed persistence primitives
- **Responsibility.** Owns all `rusqlite` access, connection pooling, WAL mode, migrations, and the **storage-namespace abstraction**. Hands out per-plugin namespaces (`<plugin-name>`, partitioned per-identity when `identity_scope = per_identity`). Backs plugin `storage:persistent`, the audit log sink, the plugin cache index, and session state. Single owner of the SQLite dependency so schema/migration discipline is centralized.
- **Key types/traits.** `Db` (pool), `Migration`, `StorageNamespace` (scoped K/V or table handle), `IdentityScope` (per_identity | global), `open_session_db(identity)`, `open_audit_db()`. WAL + continuous-flush helpers for session.
- **Depends on.** `mote-types`, `rusqlite`, `parking_lot`.
- **Governs.** DESIGN §Per-plugin storage and permissions; §Session schema; §Tab Persistence; DISCIPLINES §6 (data-persistence visibility hooks).

#### `mote-registry` — versioned permission/capability/token registries
- **Responsibility.** Loads and validates the machine-readable registry files (`permissions/v1.yaml`, `capabilities/v1.yaml`, `combinations.yaml`, plus the token registry). Resolves a plugin's targeted `schema = "vN"` to the correct registry version. Provides capability **contract** descriptors (required API surface, required events, exclusivity, dispatch shape, `critical` flag) that the loader uses for conformance checks. Enforces additive-only-within-version.
- **Key types/traits.** `Registry` (versioned), `PermissionDef`, `CapabilityDef { exclusivity, dispatch_shape, critical, contract }`, `Contract { required_api: [..], required_events: [..] }`, `Combination` (dangerous pair + warning text), `TokenRegistry`.
- **Depends on.** `mote-types`, `mote-permissions`, `serde`, `serde_yaml` (or `toml` — see risks doc on registry file format).
- **Governs.** DESIGN §Permission and Capability Registries; §Critical capabilities; §Styling tokens; DISCIPLINES §2, §4.
- **Repo artifacts it reads.** `permissions/v1.yaml`, `capabilities/v1.yaml`, `combinations.yaml` (ship in the repo root or `registry/` — location is a small open item, see risks).

#### `mote-audit` — lock-free permission/network audit log
- **Responsibility.** The append-only audit pipeline: a `crossbeam-channel` MPSC feed from every gatekeeper check and dispatch decision into a dedicated audit thread that writes an in-memory ring buffer and periodically flushes to SQLite (via `mote-storage`). One atomic append per logged call. Surfaces query APIs for the integrity panel (per-plugin call counts, network decisions, denials, MCP activity).
- **Key types/traits.** `AuditSink` (the cheap append handle cloned to producers), `AuditEvent` (permission call, net decision, MCP call, denial), `AuditQuery` (panel read API), `RingBuffer`.
- **Depends on.** `mote-types`, `mote-storage`, `crossbeam-channel`, `parking_lot`, `tracing`.
- **Governs.** DESIGN §Lock-free permission audit log; §Observability; §Transparency — Integrity Panel; DISCIPLINES §6.

#### `mote-lua` — sandboxed Lua runtime
- **Responsibility.** `mlua` + LuaJIT embedding. Constructs the sandboxed Lua environment with `io`, `os`, `debug`, `loadstring` removed. Loads a plugin module (constructs `M`, populates `M.api`, reads `M.hooks`/`M.events`/`M.manifest`/`M.mcp_tools` declaratively) **without calling `setup()`**. Marshals Rust host functions (the `mote.*` API surface) into Lua. Owns the Lua-side of synchronous host calls in the hot path.
- **Key types/traits.** `LuaPlugin` (loaded module handle), `LuaSandbox` (env factory), `HostFn` registration, `ManifestRaw` (parsed-but-unvalidated manifest table), `LuaError`. The `mote.*` global table is assembled from host functions registered by `mote-runtime`.
- **Depends on.** `mote-types`, `mlua` (luajit feature).
- **Governs.** DESIGN §Plugin Language Choice; §Sandboxed runtime; §Enforcement Rules (step 2: module load); §Config and plugins are the same language.

#### `mote-wasm` — WASM plugin runtime
- **Responsibility.** `wasmtime` embedding (Cranelift JIT, instance pooling). Exposes the same host-function surface to WASM plugins via explicitly exported host functions only — no ambient capability. Minimum-viable in v0.1 (ROADMAP Phase 1 permits deferring full WASM plugin support); the host-call ABI and at least the `adblock` rule-engine path must work.
- **Key types/traits.** `WasmPlugin`, `WasmHostImports` (the exported host fns), `InstancePool`, `WasmError`.
- **Depends on.** `mote-types`, `wasmtime`.
- **Governs.** DESIGN §Plugin Language Choice (escape hatch); §WASM plugins are more constrained.

#### `mote-dispatch` — differentiated hook dispatch
- **Responsibility.** The hook-type-differentiated dispatch engine (DISCIPLINES §3). Implements **filter chains** (10ms sync, hard timeout → `defer`, `block`/`modify`/`allow`/`defer` semantics, first-block-wins, modify-cascades, priority ordering), **broadcasts** (100ms async-allowed, no return semantics, error isolation), **keybind handlers** (input-coalescing, no raw-timeout auto-disable), and the **collector** pattern (used inside exclusive capabilities). Enforces the three-errors-in-24h auto-disable (excluding keybinds) and emits the auto-disable system notification. Routes capability API invocations to the current fulfiller, executing under the **fulfiller's** permissions.
- **Key types/traits.** `HookType` (FilterChain | Broadcast | Keybind), `Dispatcher`, `FilterDecision { Block, Modify(payload), Allow, Defer }`, `DispatchOrder` (priority + user-pinned override), `CapabilityInvoker` (routes `capabilities.invoke`), `Budget`. The registration API **requires** the hook type so the runtime enforces the matching contract (DISCIPLINES §3 mechanism).
- **Depends on.** `mote-types`, `mote-permissions`, `mote-registry`, `mote-audit`, `mote-lua`, `mote-wasm`.
- **Governs.** DESIGN §Plugin Dispatch and Composition (all sub-sections incl. Runtime guarantees table); §Permissions and capability invocation; DISCIPLINES §3.

#### `mote-runtime` — plugin lifecycle orchestrator
- **Responsibility.** The heart of Phase 1. Drives the **four-step load pipeline** (schema validation → module load → contract conformance → permission approval) in order, gated by each preceding step. Owns the live plugin table, the capability fulfillment map (exclusive resolution, non-exclusive dispatch-shape resolution, dangling-consumer detection), hot reload (file-watch via `notify`/`tokio`, the three reload scenarios, re-approval triggering), and assembly of the `mote.*` host API exposed through `mote-lua`/`mote-wasm`. Threads the gatekeeper, audit sink, and dispatcher into every plugin call. Coordinates `setup()` invocation only after all four checks pass.
- **Key types/traits.** `Runtime`, `PluginHandle`, `LoadPipeline`, `CapabilityMap`, `ConsumesResolver`, `HotReloader`, `HostApi` (the `mote.*` surface builder), `IdentityScope` wiring.
- **Depends on.** `mote-cef` (isolated-world injection, page hooks), `mote-lua`, `mote-wasm`, `mote-dispatch`, `mote-registry`, `mote-permissions`, `mote-storage`, `mote-audit`, `mote-types`, `notify`, `tokio` (coordination only — not hot path), `parking_lot`, `crossbeam-channel`.
- **Governs.** DESIGN §Enforcement Rules; §Hot Reload; §Inter-plugin communication; §Resolution at load time; §Plugin Identity Scope; §AI-Native (host API for tabs/pages).

#### `mote-session` — identity/workspace/session state
- **Responsibility.** The three-axis state model. Identity = Chromium profile (via `mote-cef::ProfileHandle`); workspace definitions and pinned tabs; session state (open tabs, scroll, history stack, form drafts, hidden-tab metadata) persisted continuously to per-identity SQLite (`mote-storage`). Tab states (active/hidden/closed), hidden-tab TTL + hold, active-tab discarding, crash-recovery-equals-clean-exit, restoration model. Maintains `docs/identity-isolation.md`'s enumerated surface as code-level truth.
- **Key types/traits.** `Identity`, `Workspace`, `Session`, `Tab { state: TabState }`, `TabPicker` (ranking), `HiddenTabReaper` (TTL), `Discarder` (30min idle), `FormDraftStore` (opt-in, sensitivity filters).
- **Depends on.** `mote-cef`, `mote-storage`, `mote-types`, `mote-audit`, `tokio` (timers/flush).
- **Governs.** DESIGN §User State Model; §Tab Persistence and Session Behavior; §Form drafts; DISCIPLINES §5 (identity isolation honesty), §6 (form drafts opt-in).

#### `mote-secrets` — secret subsystem
- **Responsibility.** `secrets.lua` parsing; `$secret:<name>` resolution at plugin-launch; the five backends (`keyring`, `password-manager` → `secret:provider` plugin, `age`, `env`, `file` opt-in); per-secret permission enforcement (`secret:read:<name>`); per-identity `secrets.lua` override. Never exposes backend metadata or other secret names to a plugin.
- **Key types/traits.** `SecretStore`, `SecretBackend` (trait), `KeyringBackend`/`AgeBackend`/`EnvBackend`/`FileBackend`/`PasswordManagerBackend`, `SecretRef`.
- **Depends on.** `mote-types`, `mote-permissions`, `mote-registry`, `mote-audit`, `keyring`, `age`, `ring`. Routes the `password-manager` backend through `mote-dispatch`'s capability invocation (so it depends on the runtime/dispatch for that path).
- **Governs.** DESIGN §Secret Management (all sub-sections); §Password manager as a secret backend.

#### `mote-pluginmgr` — plugin management & provenance
- **Responsibility.** `plugins.lua` + `plugins.lock` parse/resolve; source types (`github:`, `git+https://`, `path:`, `bundled`); content-addressed cache (`~/.cache/mote/plugins/<name>/<commit>/`); BLAKE3 directory-hash computation per the documented spec; the full `mote plugin` CLI surface; dependency-graph resolution; update flow with permission-change surfacing; implicit-local detection; dev mode (per-plugin/per-directory); first-party bundled distribution (unpack from the binary); upstream poll for bundled plugins. Stores per-plugin approved permission/capability/consumes/identity_scope hashes (DISCIPLINES §9 mechanism).
- **Key types/traits.** `PluginManifestFile` (plugins.lua model), `LockFile`, `Source`, `Cache`, `Fetcher` (git), `BundleProvider` (embedded), `ApprovalState { last_approved_hash }`, `DiffReport`, `DevMode`.
- **Depends on.** `mote-types`, `mote-registry`, `mote-permissions`, `mote-storage`, `mote-secrets` (link helper), `mote-runtime` (to trigger load/reload), `blake3`, a git client (`gix` preferred; see risks), `mlua` (plugins.lua is Lua — via `mote-lua`).
- **Governs.** DESIGN §Plugin Management (all sub-sections); §Integrity verification; §Hash computation; DISCIPLINES §9.

#### `mote-ui` — chrome rendering & widgets
- **Responsibility.** The slot/element/theme rendering surface and the runtime-owned UI: tab strip, urlbar host, sidebar, integrity panel, permission-approval dialog, workspace tab picker. Hosts plugin-provided elements into theme-arranged slots. **GATED on the UI-framework ADR** (DESIGN §Open Decisions; orchestrator brief). The crate's public seam (the `UiHost` trait that the shell talks to) can be defined early; the rendering backend cannot be built until the ADR lands.
- **Key types/traits.** `UiHost` (trait: render a slot graph, surface dialogs), `Slot`, `ElementKind`, `Element`, `Theme` (layout + styling + tokens), `TokenResolver`, `IntegrityPanel`, `ApprovalDialog`, `TabPickerView`.
- **Depends on.** `mote-cef` (off-screen surfaces / window), `mote-runtime` (element registration, host API), `mote-session` (tab/workspace state to render), `mote-registry` (token vocabulary, slot/kind sets), plus the chosen UI framework (TBD).
- **Governs.** DESIGN §UI Composition — Slots, Elements, and Themes; §Themes are plugins; §Styling tokens; §Transparency — Integrity Panel; §Permission approval dialog. **Also** the `spec/` design system — but see the major design-vs-spec contradiction in the risks doc; the integrity panel and "no AI UI" are core decisions, the spec's `ask` mode / `mote.ai` are not.

#### `mote-mcp` — Model Context Protocol endpoint
- **Responsibility.** The MCP server endpoint (loopback by default via `mcp:server:bind_loopback`; public only via `mcp:server:bind_public`). Aggregates tools from all plugins fulfilling the non-exclusive `mcp:server` capability, namespaced `<plugin-name>.<tool-name>`, exposed at one endpoint. Routes incoming tool calls to the owning plugin via `mote-dispatch`, executing under the **owning plugin's** permissions. Implements the `mcp:client:<server-name>` permission path for plugins calling out. Feeds MCP activity to the audit log for the integrity panel.
- **Key types/traits.** `McpServer`, `ToolCatalog` (namespaced), `ToolRouter`, `McpClient` (outbound), `BindScope { Loopback, Public }`.
- **Depends on.** `mote-runtime`, `mote-dispatch`, `mote-permissions`, `mote-audit`, `mote-types`, an MCP/JSON-RPC implementation (`rmcp` or a minimal JSON-RPC over the chosen transport; see risks), `tokio`.
- **Governs.** DESIGN §AI-Native Architecture — MCP integration; §LLM access lives in plugins; ROADMAP Phase 8.

#### `mote-shell` — browser composition root (library)
- **Responsibility.** Wires runtime + session + UI + secrets + pluginmgr + mcp into "a browser": window management (single window v0.1, multi-window working), tab lifecycle bridging session ↔ CEF ↔ UI, config loader (user `init.lua` → runtime state), the event loop integration. The glue layer where the integration seams live (and therefore where the global verification rule's "happy path end-to-end" matters most).
- **Key types/traits.** `Shell`, `Window`, `ConfigLoader`, `EventLoop` bridge.
- **Depends on.** Everything above except the binary/CLI.
- **Governs.** DESIGN §Window model; §Layered architecture; ROADMAP Phase 2.

#### `mote-cli` — `mote` command-line surface
- **Responsibility.** The `mote plugin …`, `mote secrets link`, etc. CLI. Thin; delegates to `mote-pluginmgr`/`mote-secrets`. Can run without launching the engine (for `add`/`diff`/`gc`/`sync`).
- **Depends on.** `mote-pluginmgr`, `mote-secrets`, `clap`.
- **Governs.** DESIGN §CLI surface.

#### `mote-app` — the binary
- **Responsibility.** `main`. Parses args, dispatches to `mote-cli` (management subcommands) or boots `mote-shell` (browser). Owns the CEF subprocess entry shim (CEF's multi-process model requires the binary to handle the helper-process role) via `mote-cef`.
- **Depends on.** `mote-shell`, `mote-cli`, `mote-cef`.
- **Governs.** DESIGN §Engine; ROADMAP Phase 1 (launch engine), Phase 2.

#### First-party plugins (not Rust crates — Lua/WASM under a `plugins/` tree)
Per DESIGN §v0.1 First-Party Plugins, these are **plugins, not crates** ("first-party plugins are still plugins"). They live in a repo tree (e.g. `plugins/`) and are bundled into the binary by `mote-pluginmgr`'s `BundleProvider`. The WASM-heavy `adblock` rule engine is the one place a first-party plugin may also have a Rust→WASM crate (`plugins/adblock/engine/` compiled to WASM). Listed in the phase breakdown (Phases 5–8), not the crate topology, because they exercise the public plugin API rather than extend the workspace's internal layering.

### 1.3 Crate → DESIGN section index (quick map)

| Crate | Primary DESIGN section | Primary DISCIPLINE |
|---|---|---|
| `mote-cef` | Engine — CEF; Isolated Worlds | §1 |
| `mote-types` | Permission Primitives; Hash computation | — |
| `mote-permissions` | Security Model; User narrowing; Revocation | §4 |
| `mote-storage` | Per-plugin storage; Session schema | §6 |
| `mote-registry` | Permission/Capability Registries; Critical capabilities; Tokens | §2, §4 |
| `mote-audit` | Lock-free audit log; Observability | §6 |
| `mote-lua` | Plugin Language Choice; Sandbox; Module load | — |
| `mote-wasm` | Plugin Language Choice (escape hatch) | — |
| `mote-dispatch` | Plugin Dispatch and Composition; Runtime guarantees | §3 |
| `mote-runtime` | Enforcement Rules; Hot Reload; Inter-plugin comms | §2, §9 |
| `mote-session` | User State Model; Tab Persistence | §5, §6 |
| `mote-secrets` | Secret Management | — |
| `mote-pluginmgr` | Plugin Management; Integrity verification | §9 |
| `mote-ui` | UI Composition; Integrity Panel; Approval dialog | §4, §6, §7 |
| `mote-mcp` | AI-Native — MCP integration | §8 |
| `mote-shell` | Window model; Layered architecture | — |
| `mote-cli` | CLI surface | §9 |
| `mote-app` | Engine; binary entry | §1 |

---

## 2. Ordered Work Breakdown (Phases 1–8)

Notation per work unit: `[UI-INDEPENDENT]` = can start before the UI-framework ADR; `[UI-GATED]` = blocked on the ADR; `‖` = parallelizable with siblings (disjoint files); `→` = hard dependency / must serialize (file overlap or logical dependency). Blast-radius rule (global): when batching independent PRs, land the smallest-surface change first.

### The UI-framework ADR gate

> **`mote-ui`'s rendering backend and every `[UI-GATED]` unit below are blocked until the UI-framework ADR resolves** (custom wgpu/Skia vs `iced` vs `egui`; DESIGN §Open Decisions). **Phase 1 in full is `[UI-INDEPENDENT]`** and is the critical path that starts immediately. The ADR should be authored in parallel with Phase 1 so it lands before Phase 2's UI units are reached. The `UiHost` *trait* (the seam between `mote-shell` and `mote-ui`) can and should be defined `[UI-INDEPENDENT]` so shell wiring isn't blocked on the backend choice.

---

### Phase 1 — Plugin runtime foundation `[ALL UI-INDEPENDENT]`

This is the critical path; it begins before the ADR and before any UI work. Sequencing is gated by the dependency layering, not by phase order within the list.

**1A — Workspace skeleton & foundational leaves (serialize first; smallest blast radius).**
- `1A.1 →` Scaffold real crates, delete `mote-placeholder`, confirm `cargo build`/clippy/fmt clean. Populate `[workspace.dependencies]` with the DESIGN dependency stack. *(touches root `Cargo.toml`; everything else waits on it.)*
- `1A.2 → mote-types`. Newtypes, glob matcher (with `!` negation), BLAKE3 checksum type. Unit-tested in isolation.
- `1A.3 ‖ mote-permissions` (after 1A.2). Grammar parse, narrowing model, deny-precedence, `Gatekeeper` trait + in-memory impl.
- `1A.4 ‖ mote-storage` (after 1A.2). SQLite pool, WAL, migration runner, `StorageNamespace`, identity-scoped namespacing.
- `1A.5 → mote-cef` (after 1A.2). The wrapper: init/shutdown, message loop, `Browser` for one view, `ProfileHandle`, the `ResourceRequestHook` trait, `IsolatedWorld`. This is the single hardest external-integration unit; give it room. **Gate the CEF-import CI rule here (§3.7).**

**1B — Registries & audit (parallel after 1A.2–1A.4).**
- `1B.1 ‖ mote-registry`. Load `permissions/v1.yaml`, `capabilities/v1.yaml`, `combinations.yaml`, token registry; version resolution; contract descriptors; additive-only validation. **Authoring the v1 registry files is part of this unit** and is a precondition for the contract-conformance tests (DISCIPLINES §2).
- `1B.2 ‖ mote-audit` (after 1A.4). Crossbeam pipeline, ring buffer, SQLite flush, query API.

**1C — Plugin language runtimes (parallel after 1A.2).**
- `1C.1 ‖ mote-lua`. mlua+LuaJIT sandbox (`io`/`os`/`debug`/`loadstring` removed), module load without `setup()`, host-fn registration scaffolding, declarative `M.manifest`/`M.hooks`/`M.events`/`M.api`/`M.mcp_tools` parsing.
- `1C.2 ‖ mote-wasm`. wasmtime embedding, host-import ABI, instance pool. Minimum viable.

**1D — Dispatch (after 1B.1, 1C.1, 1C.2, 1A.3, 1B.2).**
- `1D.1 → mote-dispatch`. Filter chains (10ms/timeout→defer), broadcasts (100ms/error-isolation), keybind coalescing, collector pattern, priority+user-pin ordering, capability invocation under fulfiller permissions, 3-errors-in-24h auto-disable + notification hook. Hook-type-required registration API.

**1E — Runtime orchestration (after 1D.1, 1A.5, 1B.1, 1A.4).**
- `1E.1 → mote-runtime` core: four-step load pipeline, live plugin table, `setup()` gating.
- `1E.2 → ` Capability fulfillment map: exclusive resolution + conflict error, non-exclusive dispatch-shape resolution, `consumes` dangling-consumer detection & error.
- `1E.3 → ` Host API assembly (`mote.tabs`, `mote.workspaces`, `events`, `capabilities.invoke`, `permissions.effective`, `secrets.get` stub, etc.) wired through `mote-lua`/`mote-wasm`. *(Note: which host-API surface is canonical is a `[BLOCKER]` — see risks doc, DESIGN-vs-spec API divergence.)*
- `1E.4 → ` Per-plugin SQLite storage namespaces wired (`storage:persistent`), identity-scope aware.
- `1E.5 → ` Hot reload: file-watch (`notify`), the three reload scenarios, re-approval triggering on `permissions`/`capabilities`/`consumes`/`identity_scope` change (DISCIPLINES §9 — note the `requires` ambiguity, risks doc).

**Phase 1 parallelization summary.** After `1A.1→1A.2`, three streams run in parallel: **engine** (`1A.5`), **security/storage** (`1A.3 ‖ 1A.4 → 1B.*`), **languages** (`1C.*`). They converge at `1D.1` then `1E.*`. The engine stream (`1A.5`) is the long pole and should be staffed first.

---

### Phase 2 — Browser shell

**`[UI-INDEPENDENT]` units (start during/after Phase 1):**
- `2.1 ‖ mote-session`: identity (Chromium profile via `mote-cef`), workspace model + pinned tabs, session SQLite (continuous flush, WAL, crash recovery), tab states, hidden-tab TTL + hold, active-tab discarding, form-draft store (opt-in + sensitivity filters). *Pure state/persistence — no rendering.*
- `2.2 ‖ ` `docs/identity-isolation.md` authored from the enumerated isolation surface (DISCIPLINES §5). Code in 2.1 references it.
- `2.3 ‖ ` `UiHost` trait + slot/element/theme **data model** in `mote-ui` (no backend): `Slot`, `ElementKind`, `Element`, `Theme`, `TokenResolver`. Lets `mote-shell` wiring proceed.
- `2.4 → mote-shell` window/tab/config-loader wiring against the `UiHost` trait + `mote-session`. Config loader (`init.lua` → runtime state) is Lua-only, UI-independent.

**`[UI-GATED]` units (blocked on ADR):**
- `2.5 → ` `mote-ui` rendering backend (the chosen framework), slot host, token resolution to concrete styles.
- `2.6 → ` Tab strip, URL bar host, workspace tab picker (`Mod+Space`) views.
- `2.7 → ` **Integrity panel** (active plugins, requested→effective permissions, audit log, storage, provenance, integrity status, revoke/update/rollback/reload). This is the load-bearing transparency surface (DESIGN §Transparency) and the plugin-management UI (DISCIPLINES §9).
- `2.8 → ` **Permission approval dialog** with the narrowing UI (multi-pattern editor) and dangerous-combination surfacing (DISCIPLINES §4).

> Settings model = TOML/Lua config files only in v0.1; no settings GUI except the integrity panel (ROADMAP Phase 2; DISCIPLINES §7).

---

### Phase 3 — Plugin management `[mostly UI-INDEPENDENT]`

- `3.1 ‖ ` `plugins.lua`/`plugins.lock` parse/resolve; source types; content-addressed cache; BLAKE3 directory hash (exact spec from DESIGN §Hash computation).
- `3.2 → ` Dependency-graph resolution (note `requires` vs `consumes` ambiguity — risks doc; resolve before building transitive fetch).
- `3.3 → ` CLI surface (`add/remove/update/source/sync/rollback/diff/import/gc/review/pin/link`) in `mote-cli` + `mote-pluginmgr`.
- `3.4 → ` Update flow with prominent permission-change surfacing; last-approved-hash storage (DISCIPLINES §9).
- `3.5 ‖ ` Bundled first-party distribution (embed in binary, unpack to cache) + upstream poll for bundled plugins.
- `3.6 ‖ ` Implicit-local detection + approval flow; per-identity `plugins.lua`.
- `3.7 ‖ ` Dev mode (per-plugin/per-directory; visual-mark flag surfaced to `mote-ui`). *(The visual marking is `[UI-GATED]`; the dev-mode state machine is not.)*

> The *approval dialog rendering* for new/changed plugins is `[UI-GATED]` (lives in 2.8); the CLI `diff`/`review` path (DISCIPLINES §9 `mote plugin diff`) is `[UI-INDEPENDENT]` and provides the same diff headless.

---

### Phase 4 — Secret management `[UI-INDEPENDENT except audit surfacing]`

- `4.1 ‖ ` `secrets.lua` parsing + `$secret:<name>` resolution at plugin-launch.
- `4.2 ‖ ` Backends: `keyring`, `age`, `env`, `file` (opt-in). *(Independent files; parallel.)*
- `4.3 → ` `password-manager` backend routing to `secret:provider` plugin via capability invocation (depends on Phase 5 `password-manager` plugins existing for an end-to-end test, but the routing code only needs the dispatch path).
- `4.4 → ` Per-secret permission grants (`secret:read:<name>`); per-identity `secrets.lua` override.
- `4.5 ‖ ` `mote secrets link` CLI helper.
- `4.6 → ` Integrity-panel secret audit surface — `[UI-GATED]` (renders in 2.7); the audit *data* is UI-independent.

---

### Phase 5 — First-party plugins (Tier 1) `[needs Phase 1 host API + Phase 2 session; some UI-GATED]`

These exercise the public plugin API; they are Lua/WASM, bundled.
- `5.1 ‖ workspace-manager` (fulfills `workspace:provider`, **critical**). Needs `mote-session` + workspace host API.
- `5.2 ‖ history` (fulfills `ui:history_provider` + `ui:urlbar_provider`, both **critical**; internal `urlbar:suggest` collector surface).
- `5.3 ‖ bookmarks` (fulfills `ui:bookmarks_provider`, **critical**).
- `5.4 → password-manager-form-services-plugin` (fulfills `password-manager-form-services`; form detection, autofill picker UX, isolated-world injection helpers).
- `5.5 → password-manager-1password` (fulfills `password-manager:provider`; consumes form-services; 1Password SDK/Connect, never shells to `op`).
- `5.6 → password-manager-bitwarden` (same shape, Bitwarden).
- `5.7 → ` Bundled distribution proven end-to-end (first launch unpacks all Tier-1 from binary, no network).

> Critical-capability plugins (5.1–5.3) gate basic browser usability; their *UI elements* (panels, urlbar) are `[UI-GATED]`, but their data/logic and capability fulfillment are `[UI-INDEPENDENT]` and can be validated headless via the host API + contract-conformance tests.

---

### Phase 6 — First-party plugins (Tier 2) `[UI-GATED for visible behavior]`

- `6.1 ‖ adblock` — WASM rule engine (`plugins/adblock/engine/` → WASM via `mote-wasm`), Lua orchestration, filter-list updating; hooks `net:intercept_request` as a filter chain. The rule-engine + interception path is `[UI-INDEPENDENT]` (verifiable headless); the integrity-panel block counts render in 2.7.
- `6.2 ‖ vim-mode` — `f`/`F`/`gg`/`G`, hint mode, search, command mode, keybind discovery. Exercises keybind-coalescing dispatch (DISCIPLINES §3 e2e test). `[UI-GATED]` for hint overlay; keybind logic testable against the dispatch layer.

---

### Phase 7 — First-party plugins (Tier 3) `[UI-GATED for visible behavior]`

- `7.1 ‖ reader-mode` (article extraction).
- `7.2 ‖ dark-mode` (site-by-site; `page:inject_css` fan-out).
- `7.3 ‖ download-manager` (queueing/hashing/notification; note: `downloads:*` permission is **deferred** in DESIGN — see risks; v0.1 uses `net:intercept_request` coverage only).
- `7.4 ‖ mote-plugin-devtools` (per-plugin console, error traces, audit filtering, effective-permissions view, reload, storage inspection; enabled when dev mode active). Heavily `[UI-GATED]`.

---

### Phase 8 — AI-native primitives (the goal) `[UI-INDEPENDENT except panel surfacing]`

- `8.1 → mote-mcp` endpoint: loopback bind (`mcp:server:bind_loopback`), JSON-RPC/MCP transport.
- `8.2 → ` `mcp:server` capability dispatch: aggregate tools from all fulfillers, namespace `<plugin>.<tool>`, route to owner under owner's permissions.
- `8.3 → ` `mcp:client:<server-name>` permission flow (outbound).
- `8.4 → browser-mcp-bridge` demo plugin exposing `list_open_tabs` (+ one or two more browser-state tools).
- `8.5 → ` Integrity panel surfaces MCP activity (tools, external clients, call counts) — `[UI-GATED]` rendering; audit data UPI-independent.

> **The ultimate end-to-end proof (§3.8) lands here:** a real CEF window rendering a page + an external MCP client round-tripping `list_open_tabs`. This is the v0.1 goal per the orchestrator brief and ROADMAP Phase 8.

---

### 2.x Cross-phase parallelization map (what runs concurrently)

- **Immediately (no ADR needed):** Phase 1 in full, plus Phase 2's `2.1/2.2/2.3` (session, identity-isolation doc, UI data model), plus Phase 3's `3.1` (cache/hash) and Phase 4's `4.1/4.2` (secrets parsing/backends).
- **After the UI ADR lands:** Phase 2's `2.5–2.8`, then the visible portions of Phases 5–8.
- **Long poles to staff first:** `mote-cef` (`1A.5`), `mote-dispatch` (`1D.1`), the integrity panel (`2.7`).
- **Serialize within a crate's files; parallelize across disjoint crates.** Phase 5's password-manager chain (5.4→5.5→5.6) serializes on the form-services contract; Tier-1 providers (5.1‖5.2‖5.3) parallelize.

---

## 3. Verification Strategy per Layer

"Done" = the happy path is proven end-to-end at the integration seam, not just unit-green (global verification rule). A live display is available (`DISPLAY=:1`, Wayland) so GUI verification is in scope.

### 3.1 `mote-types` / `mote-permissions`
Unit tests for glob matching (incl. `!` negation precedence), narrowing (requested→effective union of user patterns), deny-precedence. Property tests on glob match where cheap. **Done:** a permission set narrowed by user patterns yields the documented effective scope; a deny pattern beats an overlapping allow.

### 3.2 `mote-storage` / `mote-session`
Integration tests against a temp SQLite db: namespace isolation (plugin A can't read plugin B), per-identity partitioning, WAL durability (kill mid-write, reopen, state intact = "crash recovery equals clean exit"), hidden-tab TTL reaping, form-draft sensitivity filtering (password/`autocomplete=off` never saved). **Done:** simulated crash recovers to ~5s-old state; identity A sees nothing from identity B.

### 3.3 `mote-cef`
Headless smoke test: initialize CEF, create a `Browser`, load `about:blank`, receive a load callback, shut down clean. Off-screen render produces a non-empty frame. **Done:** engine boots and tears down without leaking processes; the import-isolation CI gate (§3.7) is green.

### 3.4 Contract-conformance plugin tests (DISCIPLINES §2) — **mandatory CI**
`tests/contract-conformance/` holds **one minimal plugin per schema version** that exercises *every* permission and capability in that version's registry. CI runs them on every commit; any drift fails the build. New permissions may be added to v1, but existing-behavior tests must keep passing (additive-only). **Done:** the v1 conformance plugin loads, every permission resolves, every capability contract validates (required API surface present, required events declared), and a `cargo test` target enforces it. This directory is created in Phase 1 (alongside `1B.1`) and grows with the registry.

### 3.5 Differentiated-dispatch e2e tests (DISCIPLINES §3) — **mandatory**
- **Filter-chain budget test:** a handler that sleeps past 10ms produces `defer` (not `block`/`modify`/`allow`), logs a warning; first-`block`-wins and `modify`-cascades verified with three ordered handlers (the DESIGN §Observability scenario: privacy-headers modifies, adblock blocks, logger observes → result BLOCKED, full chain recorded in audit).
- **Keybind-coalescing test:** bursty keybind input under realistic load does **not** auto-disable vim-mode (DISCIPLINES §3 mechanism); queued events discarded, latest handled.
- **Auto-disable test:** three errors in 24h auto-disables a non-keybind plugin and fires the system notification (not just a panel entry).
**Done:** all three pass in CI; the audit log records the full per-handler decision chain with timings.

### 3.6 `mote-runtime` load pipeline
End-to-end: a plugin with an unknown permission fails at step 1 with a clear error; a plugin claiming a capability without the required API fails at step 3; a plugin consuming an unfulfilled capability fails with the dangling-consumer error; `setup()` runs only after all four pass. Hot-reload scenarios: code-only (no prompt), narrowing (no prompt), expansion (awaiting-approval). **Done:** each enforcement rule and each reload scenario is covered by an integration test driving a real (tiny) Lua plugin.

### 3.7 CEF import-isolation gate (DISCIPLINES §1) — **mandatory CI**
A CI step (an `xtask` or scripted check) fails the build on any `use cef::` or `use cef_rs::` outside `crates/mote-cef`. Implemented in `1A.5`. The code-review checklist line ("does this PR add CEF-direct usage outside the wrapper?") is documented in `CONTRIBUTING.md` (Phase 11) but the *automated* gate is the real mechanism and ships in Phase 1. **Done:** a deliberately-planted `use cef::` outside the wrapper fails CI in a test of the gate itself.

### 3.8 Ultimate end-to-end proof (the v0.1 goal)
Two integration scenarios, both runnable on the live display:
1. **Real CEF window renders a page.** Boot `mote-app`, open a window, navigate to a real URL, confirm a frame paints (screenshot via the available `DISPLAY=:1`; Playwright/`browser_take_screenshot` MCP tools available for capture/verification). Tab switch < target latency observed.
2. **External MCP client round-trips `list_open_tabs`.** With `browser-mcp-bridge` loaded and the loopback MCP endpoint up, an external MCP client connects, lists the namespaced tool catalog, calls `browser-mcp-bridge.list_open_tabs`, and receives the actual open-tab set — proving the tool executed under the owning plugin's permissions and the routing/audit path works.
**Done:** both scenarios pass on the live display; the integrity panel shows the MCP call in the audit log.

### 3.9 Per-layer "done" summary
| Layer | Proof of done |
|---|---|
| types/permissions | glob+narrowing+deny unit/property tests |
| storage/session | crash-recovery + namespace-isolation integration |
| cef | headless boot/teardown + render frame + import gate |
| registry | additive-only validation + contract descriptors load |
| audit | append→flush→query roundtrip; ring buffer under load |
| dispatch | §3.5 filter/keybind/auto-disable e2e |
| runtime | §3.6 four-step pipeline + reload scenarios |
| pluginmgr | BLAKE3 hash determinism; lock roundtrip; `diff` shows permission deltas |
| secrets | per-secret scoping; backend resolution; no metadata leak |
| ui | integrity panel + approval dialog render on `DISPLAY=:1` |
| mcp | §3.8 external-client round-trip |
| contract-conformance | §3.4 every-permission/-capability plugin, CI-enforced |

---

## 4. Cross-Cutting Concerns (threaded through every phase)

### 4.1 Permission & capability registries
- **Authored first** (`1B.1`) as `permissions/v1.yaml` / `capabilities/v1.yaml` / `combinations.yaml`; nothing that references a permission/capability can be built before its registry entry exists.
- **Every new permission** ships with enforcement code, docs, integrity-panel UI strings, audit handling, and a combination-risk review captured in the registry entry (DESIGN §Permission registry growth; DISCIPLINES §4). The PR template enforces this.
- **Additive-only within v1** is CI-enforced via the contract-conformance plugin (§3.4). A schema bump is a release event, not a code shortcut (DISCIPLINES §2).
- **Critical capabilities** (`workspace:provider`, `ui:urlbar_provider`, `ui:bookmarks_provider`, `ui:history_provider`) carry the `critical: true` tag and the extended-deprecation semantics; their first-party fulfillers ship `bundled` so the browser is functional from first launch (DESIGN §Critical capabilities).

### 4.2 Audit log
- `mote-audit`'s `AuditSink` is cloned into the gatekeeper (`mote-permissions`), the dispatcher (`mote-dispatch`), the MCP router (`mote-mcp`), and the network hook (`mote-cef`→`mote-runtime`). Every privileged action and every dispatch decision is logged at the point it happens, recording **which plugin actually performed it** (DESIGN §Permissions and capability invocation: capability calls run under the fulfiller, and the audit reflects that).
- The integrity panel (`2.7`) is the read surface; the audit query API is its backend. The audit log records the full filter-chain (DESIGN §Observability) — this is "the single most valuable thing the browser can show."
- Logging is one atomic append (crossbeam channel → dedicated thread), never a mutex on the hot path (DESIGN §Performance Architecture).

### 4.3 Storage namespaces
- `mote-storage` is the single SQLite owner. Per-plugin namespace = `<plugin-name>`; partitioned per-identity when `identity_scope = per_identity`, single shared namespace when `global`, user-picked when `user_choice` (DESIGN §Plugin Identity Scope).
- Session state, audit history, and plugin storage are distinct databases/namespaces under the same owner so the migration discipline is centralized and the "Data Mote is keeping" integrity view (DISCIPLINES §6) can enumerate every category with clear/disable controls.
- **Any feature writing user data** includes the DISCIPLINES §6 PR-description section (what's saved, where, opt-in/out default, how to discover/clear). Form drafts ship opt-in (DESIGN §Form drafts).

### 4.4 Identity isolation honesty (DISCIPLINES §5)
- `docs/identity-isolation.md` is authored in Phase 2 (`2.2`) enumerating exactly what's isolated and what isn't (Chromium has known shared-state surfaces). Marketing/README claims say "isolated across [enumerated list]," never "fully isolated."
- PR-review checklist for identity-relevant code: "does this affect `docs/identity-isolation.md`? Update it in the same PR." Newly discovered leakage is P1 with a fix or an explicit mitigated-limitation note.

### 4.5 Plugin approval boundary (DISCIPLINES §9)
- The install dialog (`2.8`) is the security boundary, not the filesystem. Re-approval triggers on any change to `permissions`/`capabilities`/`consumes`/`identity_scope` (note the `requires` vs `consumes` wording inconsistency — risks doc). Last-approved hashes stored per plugin (`3.4`).
- Dev mode is per-plugin/per-directory only — **never** a global auto-approve toggle — and dev-mode plugins are visually marked everywhere (`3.7` + UI marking in `2.7`).
- `mote plugin diff` reproduces the approval-dialog diff headlessly (`3.3`).

### 4.6 Transparency defaults (DISCIPLINES §6) & honest positioning (§8)
- No continuous telemetry, ever. Update checks are inbound version queries only.
- `STATUS.md` (Phase 11) enumerates each capability's state; the first MCP server plugin (`8.4`) exists precisely so the second pillar isn't aspirational. No marketing claim about unshipped capabilities.

---

## 5. Open decisions that gate or shape this plan

These are detailed in `docs/plans/risks-and-inconsistencies.md`. The load-bearing ones for sequencing:

1. **UI-framework ADR** — gates all `[UI-GATED]` work. Author in parallel with Phase 1. *(DESIGN §Open Decisions.)*
2. **DESIGN-vs-`spec/` contradiction** `[BLOCKER]` — the spec's HTML/CSS chrome, `ask` AI mode, and `mote.ai` API directly contradict DESIGN's "no AI UI" core principle and the wgpu/Skia/iced/egui open decision. The canonical Lua host-API surface (`1E.3`) cannot be finalized until this is resolved.
3. **`requires` vs `consumes`** — glossary defines both; body and DISCIPLINES §9 disagree on which triggers re-approval. Resolve before `3.2`/`1E.5`.
4. **Registry file format** (YAML per DESIGN vs the workspace's `toml`-leaning tooling) — resolve before `1B.1`.

---

*End of master plan. See `risks-and-inconsistencies.md` for the full list of ambiguities and contradictions, which must be reviewed before implementation begins.*
