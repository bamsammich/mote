# Mote — Risks, Ambiguities & Internal Inconsistencies

Every item found while reading `DESIGN.md`, `DISCIPLINES.md`, `ROADMAP.md`, `CLAUDE.md`, `Cargo.toml`, and `spec/`. Each entry: **what & where**, **why it blocks/risks clean implementation**, **proposed resolution**. Severity tags:

- `[BLOCKER]` — must be resolved by the user before the affected code is built; building either way risks rework.
- `[DECISION]` — a genuine open choice the design leaves unbound; pick before the dependent work unit.
- `[INCONSISTENCY]` — two parts of the docs disagree; pick the canonical one.
- `[GAP]` — referenced but never defined; needs specification.
- `[RISK]` — not contradictory, but likely to bite during implementation.

---

## A. The big one: `spec/` describes a different browser than `DESIGN.md`

### A1. `[BLOCKER]` AI UI: `spec/` has an `ask` mode and `mote.ai` API; DESIGN forbids AI UI
- **Where.** `spec/01_architecture.md` defines a `mote.ai` Lua API (`mote.ai.ask`, `mote.ai.conversation`, `mote.ai.context.add`) and `ai.message`/`ai.complete` events. `spec/00_overview.md` and `spec/components/omnibox.md` define an omnibox `[ask]` mode "send a query to the AI assistant," `⌘L` to "open in ask mode," and a sidebar `assist` panel ("AI chat thread + composer," Lucide `sparkles`). `spec/components/sidebar.md` lists `assist` as a canonical built-in panel.
- **Conflict.** DESIGN §Core Principles #8 and §AI-Native Architecture are explicit: *"The browser itself ships no AI UI; AI features are entirely plugin-delivered,"* and §"What we explicitly do not build" forbids "No chatbot in the sidebar. No … urlbar AI suggestions." ROADMAP §"What's explicitly not on the roadmap" lists "Built-in AI UI — chatbot panel, AI summaries, urlbar AI suggestions" as ruled out.
- **Why it blocks.** The canonical Lua host-API surface (`1E.3`) and the omnibox/sidebar element set (`mote-ui`) cannot be finalized while the two source documents disagree on whether AI is a first-class runtime surface. Building the `spec` version violates a core principle; building the DESIGN version means the `spec` is wrong about a shipped feature.
- **Proposed resolution.** Treat **DESIGN.md as authoritative on product scope**; the `spec/` is a *visual design system* (its own README §"What's NOT in this spec" disclaims runtime/product architecture). Reconcile by: (a) the `ask` omnibox mode and `assist` sidebar panel are reserved *element slots a plugin may fill*, not runtime features — the runtime ships them empty/absent and a future AI plugin contributes them; (b) drop `mote.ai` from the v0.1 host API entirely (LLM access is `http:fetch` + secrets, DESIGN §"LLM access lives in plugins"). User must confirm this reading, or amend `spec/` to remove the AI surface.

### A2. `[INCONSISTENCY]` Two different Lua API surfaces
- **Where.** `spec/01_architecture.md` defines `mote.theme.*`, `mote.plugin.register/use/list`, `mote.bind`, `mote.palette`, `mote.omnibox`, `mote.sidebar`, `mote.tabs.{new,close,list,switch,hibernate,pin}`, `mote.on(event, fn)`. DESIGN defines a *different* surface: `mote.on("net:intercept_request", …)`, `mote.keys.bind`, `mote.tabs.{current,move}`, `mote.theme_overrides`, `mote.workspace.define`, `mote.session.configure`, `mote.tabs.configure`, `mote.dispatch.order`, `mote.plugins{}`, `mote.secrets.define`, `mote.plugin_config`, `mote.dev_mode`, `mote.updates.configure`, plus the plugin-facing `events.emit`, `capabilities.invoke`, `permissions.effective`, `secrets.get`, `ui.register_element`.
- **Conflict.** Function names overlap but differ in shape: `mote.tabs.list` (spec) vs the DESIGN tab model; `mote.plugin.register(id, manifest)` (spec, imperative) vs DESIGN's declarative `M.manifest`/`M.events`/`M.hooks` module-table model (DESIGN §Enforcement Rules: "declarative, not imperative … the only registration path"). `mote.bind` (spec) vs `mote.keys.bind` (DESIGN). Event names differ: spec uses `tab.opened`/`page.loaded`; DESIGN uses `tabs:on_change`/`page:on_load`.
- **Why it blocks.** `mote-lua` host-fn registration (`1C.1`) and runtime host-API assembly (`1E.3`) need one canonical surface. The declarative-vs-imperative split is especially load-bearing: DESIGN's static contract-conformance check (DISCIPLINES §2 / DESIGN §Enforcement step 3) **only works if registration is declarative** (`M.events` table read without running the plugin). The spec's `mote.plugin.register(...)` imperative model would break static conformance.
- **Proposed resolution.** Adopt the **DESIGN surface as canonical** for the plugin/security model (declarative `M.*` tables, `domain:action` events, `mote.keys.bind`, capability/permission APIs). Map the spec's *UI-authoring* helpers (`mote.theme.*`, `mote.palette.*`, `mote.omnibox.*`, `mote.sidebar.*`, `mote.bind` alias) onto the DESIGN model as the **theme/UI-config layer** where they don't conflict, treating them as additive sugar. User to confirm; the merged surface should be written down as a single API reference before `1E.3`.

### A3. `[INCONSISTENCY]` Slot names and element model differ
- **Where.** DESIGN §UI Composition fixes 6 slots (`top-bar`, `left-sidebar`, `right-sidebar`, `bottom-bar`, `urlbar-inline`, `tab-row`) and 8 element kinds (`urlbar`, `tabstrip`, `bookmarks-bar`, `sidebar-panel`, `action-button`, `status-indicator`, `urlbar-extension`, `widget`). `spec/01_architecture.md` fixes a *different* slot set (`tab_bar`, `omnibox`, `sidebar`, `viewport`, `status_line`, `palette`) and a theme API (`theme:slot`, `theme:bind`) not present in DESIGN.
- **Conflict.** Two incompatible slot taxonomies and two theming APIs (DESIGN's `M.theme = { layout = {...}, styling = {...} }` table vs spec's `theme:slot(name, attrs)`/`theme:bind` builder). Naming convention also differs (`kebab-case` vs `snake_case`).
- **Why it blocks.** `mote-ui`'s slot/element data model (`2.3`) and `mote-registry`'s token/slot/kind registry (`1B.1`) need one taxonomy.
- **Proposed resolution.** DESIGN's slot/kind set governs the *runtime registry* (it's the security-relevant, versioned surface). The spec's `viewport`/`omnibox`/`palette`/`status_line` are the *visual realization* — map them: spec `omnibox` ≈ DESIGN `urlbar` element in `top-bar`/`urlbar-inline`; spec `viewport` is the CEF page host (not a plugin element); spec `palette` is a `widget`/overlay. Decide a single casing convention. User confirms the mapping; record it before building either crate.

### A4. `[INCONSISTENCY]` Chrome rendering technology
- **Where.** `spec/00_overview.md` §"Stack assumptions": *"Mote's chrome renders via a web technology … delivered as CSS variables + HTML structural conventions,"* and `spec/01_architecture.md` renders slots as `<div data-slot="…">` HTML with CSS variables. DESIGN §Dependency Stack lists "UI framework: TBD … likely build a thin custom UI layer over `wgpu` or Skia rather than adopting `iced` or `egui`," and §Open Decisions keeps it open.
- **Conflict.** The spec assumes an HTML/CSS chrome (which would imply a webview/CEF-rendered chrome, or Tauri-web). DESIGN's leading candidates are native (wgpu/Skia/iced/egui), none of which consume HTML/CSS. A wgpu/Skia chrome cannot directly consume the spec's `colors_and_type.css` / `[data-slot]` conventions.
- **Why it blocks.** This is *the* UI-framework ADR input. If the chrome is CEF-rendered HTML (off-screen CEF surface for chrome), the spec's CSS is directly usable and CEF off-screen render (DESIGN §Engine) is the mechanism; if it's wgpu/Skia/iced, the spec's tokens must be re-expressed as a Rust token table and the HTML conventions are advisory only.
- **Proposed resolution.** Make this an explicit ADR alternative: **"chrome as off-screen CEF HTML/CSS surface"** vs **"native wgpu/Skia/iced."** The off-screen-CEF option is attractive because (a) it makes the entire `spec/` directly implementable, (b) CEF off-screen rendering is already a listed capability, (c) it keeps one rendering technology. Flag the tradeoff (a second CEF surface for chrome vs. native perf/footprint) for the ADR. **Do not pick here** — record as the central ADR question.

### A5. `[GAP]` Spec references files that don't exist in the repo
- **Where.** `spec/README.md` and component files reference `../ui_kits/browser/index.html`, `../colors_and_type.css`, `ui_kits/browser/Sidebar.jsx`, `preview/components-omnibox.html`. None exist (`spec/` contains only the markdown and `components/`).
- **Why it risks.** The spec calls `colors_and_type.css` "the only source the chrome should import" and `index.html` "the canonical, working source of truth," but they're absent — so the *actual* token values (beyond the table in `03_tokens.md`) and reference CSS are unavailable.
- **Proposed resolution.** Either (a) the missing `ui_kits/`/`colors_and_type.css`/`preview/` assets need to be added to the repo, or (b) the token tables in `spec/03_tokens.md`/`04_typography.md` are promoted to canonical and the dangling references removed. Resolve before any `mote-ui` styling work. The `/mote-design` skill (per CLAUDE.md) may carry these assets — confirm.

---

## B. Permission / capability / dependency model

### B1. `[INCONSISTENCY]` `requires` vs `consumes` — which is real, which triggers re-approval
- **Where.** DESIGN §Glossary defines **both**: `Consumes` ("capabilities this plugin needs *some* other plugin to fulfill") **and** `Requires` ("dependencies on other plugins, with semver constraints … imports the dependency's exported API"). But DESIGN §Security Model body and the manifest example use only `consumes`, and §Inter-plugin communication is emphatic: *"Plugins do not import each other directly. There is no `require("other-plugin")` … no version constraint on specific plugin names."* That directly contradicts the glossary's `Requires` definition. Meanwhile **DISCIPLINES §9** says re-approval triggers on *"`permissions`, `capabilities`, `requires`, or `identity_scope`"* — naming `requires`, not `consumes`. DESIGN §Hot Reload says re-approval triggers on *"`permissions`, `capabilities`, `consumes`, or `identity_scope`."*
- **Why it blocks.** The manifest schema (`mote-runtime`/`mote-registry`), the dependency-graph resolver (`3.2`), and the re-approval trigger logic (`1E.5`, DISCIPLINES §9 mechanism: the per-plugin approval hash) all need to know whether `requires` exists at all, and which field set is hashed for re-approval.
- **Proposed resolution.** The body and §Inter-plugin communication are the considered position ("no direct imports, only capability contracts"). **`consumes` is canonical; `requires` is a glossary leftover from an earlier model.** Resolution: (1) treat `requires` as not-in-v1; (2) the re-approval trigger set is `{permissions, capabilities, consumes, identity_scope}` (DISCIPLINES §9's "requires" is a stale term for "consumes"); (3) the approval-hash mechanism hashes those four fields. *But* ROADMAP Phase 1 still lists "Plugin dependency resolution (semver constraints, library vs leaf plugins)" and Phase 3 "Dependency graph resolution (library plugins, transitive fetches)" — see B2. **User must confirm** `requires` is dead, because that contradicts ROADMAP's semver-dependency language.

### B2. `[INCONSISTENCY]` ROADMAP wants semver plugin dependencies; DESIGN forbids them
- **Where.** ROADMAP Phase 1: *"Plugin dependency resolution (semver constraints, library vs leaf plugins, version-naive code)."* Phase 3: *"Dependency graph resolution (library plugins, transitive fetches)."* DESIGN §Inter-plugin communication: *"no version constraints on specific plugin names … all inter-plugin interaction is mediated by capability contracts."* DESIGN §Per-plugin storage: *"There is no longer a notion of multiple versions of the same plugin loaded concurrently."* The phrase "library plugins" appears in ROADMAP and in the `password-manager-core` description ("library plugin") but DESIGN never defines a library-plugin mechanism distinct from capabilities.
- **Why it blocks.** Determines whether `mote-pluginmgr` builds a real transitive-dependency resolver with semver (significant work) or whether "dependencies" are *only* capability-contract resolution (much simpler — just dangling-consumer checks). Phase 3's "transitive fetches" implies fetching dependency *plugins*, which requires a dependency declaration the manifest doesn't have if `requires` is dead.
- **Proposed resolution.** Reconcile to the DESIGN model: "dependency resolution" = **capability-contract resolution** (consumer + fulfiller, dangling-consumer error), and "library plugin" = a plugin that fulfills a capability others consume but provides no UI/leaf behavior (e.g., `password-manager-form-services`). "Transitive fetches" then means: installing a consumer surfaces the unfulfilled capability and prompts to install a fulfiller — not automatic semver resolution. **User confirms**, because ROADMAP's wording suggests a richer model someone may actually want.

### B3. `[GAP]` Manifest schema is never fully specified
- **Where.** Manifest fields appear scattered: `schema`, `name`, `version`, `permissions`, `capabilities`, `consumes`, `identity_scope`, `homepage`, `checksum` (DESIGN §Manifest Example), plus `hooks = { ["net:intercept_request"] = { priority = 70 } }` (DESIGN §Dispatch ordering), `M.api`, `M.events`, `M.hooks`, `M.mcp_tools`, `M.theme` as sibling module fields. The spec's manifest (`spec/01`) adds `elements`, `commands`, `palette` and omits permissions entirely.
- **Why it blocks.** `mote-lua` manifest parsing (`1C.1`) and schema validation (`1E.1`) need the complete, authoritative field list and types, including: is `checksum` in the manifest or only in `plugins.lock`? (DESIGN shows it in both — §Manifest Example has `checksum = "sha256:abc123"` but §Integrity verification computes BLAKE3 over the directory and stores it in `plugins.lock`; see B4.) Where do `priority` and hook-type live — in `M.hooks` entries or the manifest `hooks` table?
- **Proposed resolution.** Produce a single **manifest grammar spec** (a `docs/manifest-v1.md`) before `1C.1`, enumerating every field, type, optionality, and which module-table fields (`M.api`/`M.events`/`M.hooks`/`M.mcp_tools`/`M.theme`) are part of the contract. Resolve the per-field questions in B4/B5 there. This is also a ROADMAP Phase 11 deliverable ("manifest grammar") but it's needed in Phase 1.

### B4. `[INCONSISTENCY]` Checksum algorithm: `sha256:` in manifests vs BLAKE3 in lock/integrity
- **Where.** DESIGN §Manifest Example: `checksum = "sha256:abc123..."`. DESIGN §Plugin Management lock file: `checksum = "sha256:..."`. But DESIGN §Integrity verification §"Hash computation" specifies **BLAKE3** over directory contents, and §Dependency Stack lists `blake3` "for fast plugin checksum verification." ROADMAP Phase 3: "BLAKE3 hash computation per the documented spec." DISCIPLINES §"common thread" says "checksum pinning."
- **Why it blocks.** `mote-types::Checksum` and `mote-pluginmgr` hashing (`3.1`) must pick one algorithm and one serialized prefix. The `sha256:` literals are almost certainly stale (the prose and the dependency stack both say BLAKE3).
- **Proposed resolution.** **BLAKE3 is canonical** (prose + dep stack + ROADMAP agree). The `sha256:` strings in the examples are illustrative leftovers; serialize as `blake3:<hex>`. Update the examples when touched. Also resolve: is there a per-manifest checksum at all, or only the directory checksum in `plugins.lock`? Recommend **only the directory checksum in the lock** (§Integrity verification is the real mechanism); drop `checksum` from the manifest example. User confirms.

### B5. `[DECISION]` Declarative vs imperative event registration is explicitly left open
- **Where.** DESIGN §Enforcement Rules, the call-out box: *"Open at implementation time. The declarative model is the design choice; whether it survives contact with real plugin authoring is an implementation-time question … the fallback is imperative `events.on(...)` inside `setup()` with conformance becoming a dynamic check. … reversible only before the v1 schema locks."*
- **Why it blocks.** This decides whether contract conformance (DISCIPLINES §2, DESIGN §Enforcement step 3) is a **static** check (read `M.events` table without running) or a **dynamic** one (run `setup()`, observe registrations). It shapes `mote-lua`'s loader, `mote-dispatch`'s registration API, and the entire conformance-test design (§3.4). It must be settled before the v1 schema locks.
- **Proposed resolution.** **Keep declarative** (`M.events`/`M.hooks` tables) as DESIGN intends — it's what makes static conformance and "validate without executing" work, which is a security property, not just ergonomics. Note the fallback exists but recommend committing to declarative for v1 and revisiting only if plugin authors revolt. **User decision required** because it's flagged irreversible-after-lock.

### B6. `[GAP]` `combinations.yaml` schema undefined
- **Where.** DISCIPLINES §4 mandates a `combinations.yaml` "alongside `permissions/v1.yaml`" listing dangerous permission combinations + warning text, read by the install dialog. DESIGN never mentions it; no schema given.
- **Why it gaps.** `mote-registry` (`1B.1`) and the approval dialog (`2.8`) must load and render it, but its structure (list of `{permissions: [...], warning: "..."}`?) is unspecified. One example combination is given (`page:read_dom` + `mcp:server`).
- **Proposed resolution.** Define a minimal schema: a list of entries `{ combination: [perm, perm, ...], severity, warning }`. Seed it with the `page:read_dom + mcp:server` example. Ship in `1B.1`. Missing entries don't block install (DISCIPLINES §4: "added when discovered").

### B7. `[GAP]` `secret:provider` is referenced as a capability but absent from the capability examples
- **Where.** DESIGN §Capability Roles non-exclusive examples list `secret:provider`. §Secret Management uses it. But it's not in the §Critical capabilities list nor given a contract. Its dispatch shape (non-exclusive, but only one password manager is active since `password-manager:provider` is exclusive) is described in prose, not as a registry contract.
- **Why it gaps.** `mote-registry` needs a contract entry for `secret:provider` (required API, dispatch shape). The prose says it's non-exclusive but effectively singular via the exclusive password-manager capability — the registry must encode this clearly.
- **Proposed resolution.** Add a `secret:provider` contract to `capabilities/v1.yaml` with `composability: non-exclusive`, documenting that resolution is unambiguous in practice because the fulfiller is gated by exclusive `password-manager:provider`. Define its required API (the secret-resolution function the `password-manager` backend calls). Part of `1B.1`.

### B8. `[GAP]` `mcp:server` dispatch contract for tool-name collisions
- **Where.** DESIGN §AI-Native: tools are namespaced `<plugin-name>.<tool-name>` under one endpoint. Two plugins can both fulfill `mcp:server` (non-exclusive).
- **Why it gaps.** Namespacing by plugin name resolves cross-plugin collisions, but within one plugin's `M.mcp_tools`, duplicate tool names are undefined, and the contract for a *malformed* tool (missing handler/description) isn't specified for the conformance check.
- **Proposed resolution.** `mcp:server` contract requires each tool to have unique-within-plugin `name`, a `description`, and a `handler`; duplicate names within a plugin fail conformance (step 3). Define in `1B.1`/`mote-mcp` (`8.2`).

---

## C. Permission domains — coverage gaps

### C1. `[GAP]` Permissions used in examples but absent from the registry list
- **Where.** DESIGN §Permission Primitives lists the domains. But examples use permissions not in that list:
  - `page:on_load` (used as a hook in the worked example, §Worked example) — but `page:` domain lists only `inject_script`, `inject_unsafe_script`, `inject_css`, `read_dom`. Is `page:on_load` a permission, a hook, or both? (Likely a *hook/event*, not a permission — but the manifest worked example puts it under `M.hooks`, suggesting hooks ≠ permissions, which needs to be explicit.)
  - `tabs:modify_state` and `tabs:reveal` appear in the domain list and §New permissions; consistent.
  - `crypto:seal_to_plugin` — listed; consistent.
- **Why it gaps.** The line between *permission names* (gated, in the registry) and *hook/event names* (dispatch targets, also need a registry?) is blurry. `net:intercept_request` is both a permission and a hook/event name; `page:on_load` appears only as a hook.
- **Proposed resolution.** Clarify in the manifest spec (B3): there are **two namespaces** — permissions (in `permissions/v1.yaml`, gated) and hook/event names (the dispatch targets). Some strings appear in both (`net:intercept_request` = the permission to participate + the hook to handle). Hook/event names also need a versioned registry so conformance can validate `M.hooks`/`M.events` keys. Add an `events/v1.yaml` or fold event names into the capability contracts. Decide in `1B.1`.

### C2. `[INCONSISTENCY]` `introspect:` domain is "in" the registry list but "deferred to v0.2"
- **Where.** DESIGN §Permission Primitives includes `introspect: accessibility_tree, framework_state, console, network_history, computed_styles` in the v1 domain list. But §"Semantic introspection" and the Problem statement say the `introspect:` *implementation* is for the v0.2–v0.3 `frontend-introspection-mcp` flagship, and ROADMAP puts "`introspect:` permission domain implementation" under **v0.2**, not v0.1.
- **Why it risks.** Does `permissions/v1.yaml` *declare* the `introspect:` domain in v1 (so plugins targeting v1 can name it) even though enforcement is unimplemented until v0.2? Declaring a permission whose enforcement code doesn't exist violates DESIGN §"Permission registry growth" ("each new permission requires implementation in the runtime").
- **Proposed resolution.** Two clean options: (a) **declare `introspect:` in v1 but stub enforcement to deny-by-default** until v0.2 (it's in the registry, but any grant is inert/denied with a clear "not yet implemented in this Mote version" message); or (b) **add `introspect:` to v1 in the v0.2 release** as an additive change (allowed within a schema version per DISCIPLINES §2). Recommend (b) — it matches "adding a permission is a browser release event" and avoids shipping a no-op permission. **User decides**; affects whether `1B.1`'s v1 registry includes `introspect:`.

### C3. `[GAP]` `sys:clipboard:read` / `sys:clipboard:write` use a 3-segment form
- **Where.** DESIGN §Permission Primitives: `sys: native_message, clipboard:read, clipboard:write, notify`. This implies `sys:clipboard:read` — a `domain:action:sub-action`? But the IAM syntax is defined as `domain:action[:resource]`, so `sys:clipboard:read` reads as `domain=sys, action=clipboard, resource=read` — but `read`/`write` aren't resources, they're actions.
- **Why it gaps.** The permission parser (`mote-permissions`, `1A.3`) must know whether `clipboard:read` is one action token (`clipboard:read`) or `action=clipboard, resource=read`. The grammar `domain:action[:resource]` doesn't cleanly express "clipboard read vs write."
- **Proposed resolution.** Treat `clipboard.read`/`clipboard.write` as distinct *actions* under `sys` (i.e., `sys:clipboard_read`, `sys:clipboard_write`), or formally allow compound actions. Pick one form and encode it in `permissions/v1.yaml`. Recommend `sys:clipboard_read` / `sys:clipboard_write` (single action token) to keep the parser's 3-part grammar unambiguous. Decide in `1B.1`.

### C4. `[GAP]` `mcp:client:<server-name>` and `secret:read:<name>` have dynamic resource segments
- **Where.** `mcp:client:<server-name>`, `secret:read:<name>` use a runtime-named resource. The registry lists the *domain:action* (`mcp:client`, `secret:read`) but the `<name>` is plugin-supplied.
- **Why it risks.** Schema validation (step 1) checks permissions against "known terms from the registry." But `secret:read:anthropic_api_key` has a name the registry can't know. The validator must distinguish "the `secret:read` action is known" (validate) from "the `anthropic_api_key` resource" (free-form, validated against `secrets.lua` at resolution, not load).
- **Proposed resolution.** Registry entries flag actions that take a free-form resource segment (`resource: freeform` vs `resource: glob` vs `resource: none`). Schema validation checks the `domain:action` is known and the resource *shape* is permitted; the actual `<name>`/origin is validated at resolution/grant time. Encode in `permissions/v1.yaml`'s per-permission schema. Decide in `1B.1`; affects `mote-permissions` parsing.

---

## D. Dispatch & runtime

### D1. `[INCONSISTENCY]` Filter-chain budget: 10ms (table) vs "10ms" but Lua-call-latency budget elsewhere
- **Where.** DESIGN §Runtime guarantees table: filter chains 10ms hard timeout. §Performance Architecture targets "Plugin call overhead: <100 μs for Lua." §What plugin authors need: "10ms for filter chains, 100ms for broadcasts."
- **Why it risks.** A 10ms hard timeout on a synchronous Lua call requires either cooperative cancellation (mlua doesn't preempt) or running the handler on a thread with a watchdog. Lua/LuaJIT cannot be hard-interrupted mid-execution safely without `lua_sethook` debug hooks (and `debug` is removed from the sandbox!). The design removed `debug` (DESIGN §Plugin Language Choice) which is the standard mechanism for instruction-count-based timeouts.
- **Why it blocks.** `mote-dispatch` (`1D.1`) must implement the 10ms hard timeout, but the sandbox removed the `debug` library that `mlua`'s `set_hook`/interrupt mechanism may rely on. There's a real tension between "remove `debug`" and "hard 10ms timeout."
- **Proposed resolution.** Use `mlua`'s interrupt callback (`Lua::set_interrupt`, available without exposing the `debug` *library* to plugins — it's a host-side hook, not a Lua-visible API) to enforce the budget; removing the `debug` *library from the plugin environment* is separate from the host installing an interrupt. Verify `mlua` + LuaJIT supports `set_interrupt` (LuaJIT compatibility is the risk — JIT-compiled traces may not honor interrupts at fine granularity). **Spike this in `1C.1`/`1D.1`**; if LuaJIT can't honor sub-10ms interrupts, the budget semantics need adjusting (e.g., timeout enforced on the *next* dispatch, or non-JIT mode for hooked functions). Flag as an implementation risk that may force a design note.

### D2. `[RISK]` "No async runtime in the hot path" vs broadcasts are "async-allowed"
- **Where.** DESIGN §Performance Architecture #7: "No async runtime in the hot path. `tokio` is used only for high-level coordination … Plugin dispatch is synchronous." But §Runtime guarantees: broadcasts are "Async-allowed" with a 100ms budget.
- **Why it risks.** "Async-allowed" for broadcasts is ambiguous — does a broadcast handler get to `await`? Lua has no native async; "async-allowed" likely means "the 100ms budget is lenient and the handler may do slower work," not "the handler runs on tokio." `mote-dispatch` needs a precise definition.
- **Proposed resolution.** Interpret "async-allowed" as "**not on the synchronous-critical filter path; may run on a worker thread with a generous 100ms budget and errors are isolated**" — not "uses tokio await." Broadcast handlers are still synchronous Lua calls, just dispatched off the critical path. Confirm and document in the dispatch contract.

### D3. `[GAP]` Auto-disable counter scope: per-plugin or per-hook?
- **Where.** DESIGN §Runtime guarantees: "Three timeouts or errors in a 24-hour window → plugin auto-disables." Keybind handlers excluded.
- **Why it gaps.** Is the count per-plugin (3 total across all its hooks) or per-hook-registration? A plugin with 5 hooks each erroring once = 5 errors but maybe shouldn't disable if no single hook is consistently bad.
- **Proposed resolution.** Per-plugin count (the prose says "plugin auto-disables") across all non-keybind hooks in a rolling 24h window. Document explicitly in `mote-dispatch`. Minor; flag for confirmation.

### D4. `[GAP]` Capability invocation under fulfiller permissions — how does the audit attribute it?
- **Where.** DESIGN §"Permissions and capability invocation": B's call runs under B's permissions; "the audit log shows which plugin actually performed each privileged action." §Worked example: A (1Password) invokes B (form-services) `inject_isolated`.
- **Why it risks.** The audit must record both "A invoked capability X" and "B performed the privileged `page:inject_script`." The data model (`mote-audit::AuditEvent`) needs a caller/performer distinction. Not contradictory, just under-specified.
- **Proposed resolution.** `AuditEvent` carries `performer: PluginName` (whose permission gated it) and optional `invoked_via: Option<(caller, capability)>`. The integrity panel shows the performer for the privileged action and the invocation chain. Specify in `mote-audit` (`1B.2`).

---

## E. Identity, session, storage

### E1. `[INCONSISTENCY]` Identity isolation claim vs DISCIPLINES §5 honesty mandate vs glossary
- **Where.** DESIGN §Identity / Glossary "Identity": *"A fully isolated user-state container,"* "effectively different browser instances." DISCIPLINES §5 explicitly forbids claiming "fully isolated" because Chromium has known cross-profile leakage (HTTP cache key, service worker storage, network state).
- **Why it risks.** The DESIGN glossary uses the exact phrase ("fully isolated") DISCIPLINES §5 prohibits. Any code comment or doc copying the glossary would violate the discipline.
- **Proposed resolution.** Amend DESIGN's "fully isolated" language to "isolated across [enumerated list]" per DISCIPLINES §5, and author `docs/identity-isolation.md` (`2.2`) as the canonical enumerated surface. This is a doc fix, but flag it because it's an active contradiction between the two source docs. **User should approve the DESIGN wording change.**

### E2. `[DECISION]` Workspace persistence model is explicitly open
- **Where.** DESIGN §Open Decisions: "Workspace persistence model. SQLite, flat files, or a per-workspace directory layout." ROADMAP "Indefinite": same. But DESIGN §Session says session is SQLite-per-identity, and workspaces are "dotfile-checkable" (config, Lua).
- **Why it risks.** `mote-session` (`2.1`) needs to know where workspace *definitions* live (dotfile Lua per §Workspace) vs workspace *runtime state* (resized slots persist "per workspace" per §Slot resize — but where?). Resized-slot state is "per workspace" UI state, yet workspaces are config; this crosses the config/session line.
- **Proposed resolution.** Workspace *definitions* = dotfile Lua (`mote.workspace.define`). Workspace *runtime UI state* (resized slots, last-active tab) = session SQLite keyed by workspace id. Confirm the split; it's mostly resolved in the body but the "resizable slot persists per workspace" detail (§Slot resize) needs a home. Decide before `2.1`.

### E3. `[GAP]` `mote.tabs.hibernate`/`pin` (spec) vs DESIGN tab state model
- **Where.** `spec/01` has `mote.tabs.hibernate(id)` and `pin(id)`. DESIGN's tab states are active/hidden/closed with "hold" and "pin (workspace)" as distinct concepts, and "discarding" (not "hibernate"). DESIGN §Tab Persistence never uses "hibernate."
- **Why it gaps.** Terminology mismatch (hibernate vs discard/hide) and unclear mapping. The sidebar spec even shows "4 open · 1 hibernated."
- **Proposed resolution.** Map spec `hibernate` → DESIGN "hidden in workspace" (renderer destroyed); spec `pin` → DESIGN workspace pin. Drop "hibernate" terminology in favor of DESIGN's vocabulary, or document the alias. Part of the API reconciliation (A2).

---

## F. Toolchain / repo / process

### F1. `[DECISION]` Registry file format: YAML (DESIGN) vs the repo's TOML tooling
- **Where.** DESIGN §Registries shows `permissions/v1.yaml`, `capabilities/v1.yaml` (YAML). The repo's tooling is TOML-centric: `taplo` is pinned for TOML, `Cargo.toml`, `plugins.lock` is TOML. No YAML formatter/linter is pinned in `mise.toml`.
- **Why it risks.** Adding YAML means adding `serde_yaml` (now somewhat unmaintained) and a YAML lint/format story the toolchain doesn't currently have. The combinations file (DISCIPLINES §4) is also `combinations.yaml`.
- **Proposed resolution.** Either (a) honor DESIGN's `.yaml` and add a YAML tool to `mise.toml` + a maintained crate (`serde_yaml` is deprecated; consider `serde_yml` or `serde_norway`), or (b) use TOML for registries to match existing tooling (the format is an internal artifact, not a plugin-author surface). Recommend **(b) TOML** for tooling consistency unless there's a reason YAML is user-facing. **User decides**; affects `1B.1`'s `mote-registry` and the on-disk file extensions. Low-stakes but pick before authoring the files.

### F2. `[DECISION]` Git client crate for `mote-pluginmgr`
- **Where.** DESIGN §Dependency Stack doesn't list a git crate, but §Plugin Management requires `github:`/`git+https://` fetching.
- **Why it gaps.** `mote-pluginmgr` (`3.1`) needs a git client. `git2` (libgit2 FFI — triggers `unsafe`, but it's a dependency not our code) vs `gix` (pure Rust, aligns with the no-`unsafe` posture and Rust-ecosystem alignment value).
- **Proposed resolution.** Prefer **`gix`** (pure-Rust, no C FFI, matches the memory-safety rationale in DESIGN §Implementation Language). Confirm it covers shallow clone at a pinned commit. Add to `[workspace.dependencies]`. Decide before `3.1`.

### F3. `[DECISION]` MCP/JSON-RPC implementation for `mote-mcp`
- **Where.** ROADMAP Phase 8 needs an MCP server endpoint. DESIGN §Dependency Stack lists no MCP crate.
- **Why it gaps.** Need to choose: the official Rust SDK (`rmcp`) vs a minimal hand-rolled JSON-RPC over loopback. `rmcp` is async/tokio (fine — MCP is coordination, not hot path).
- **Proposed resolution.** Use `rmcp` (official) if its API surface and licensing fit; otherwise minimal JSON-RPC. Decide before `8.1`. Note: the endpoint binds loopback by default (`mcp:server:bind_loopback`), public only via `mcp:server:bind_public` — the transport choice must support both.

### F4. `[RISK]` `cef`/`cef-rs` crate maturity and `panic = "abort"` + FFI
- **Where.** DESIGN pins `cef` (tauri-apps/cef-rs) tracking Chromium 140. `Cargo.toml` release profile sets `panic = "abort"`. CEF's multi-process model needs the binary to act as helper subprocesses.
- **Why it risks.** (1) `cef-rs` is a young binding; the API may not map cleanly to all the handlers DESIGN needs (`CefResourceRequestHandler`, `CefRenderHandler` off-screen, isolated worlds). (2) `panic = "abort"` across an FFI boundary is actually *correct* (unwinding across FFI is UB), but any CEF callback that panics aborts the whole process — needs `catch_unwind` at every Rust→CEF callback boundary inside `mote-cef`. (3) The CEF binary distribution (~100–200MB) and its build/link setup is non-trivial and must work under mise/CI.
- **Proposed resolution.** `1A.5` (the `mote-cef` spike) is the riskiest unit; budget accordingly (DISCIPLINES §1: 20% CEF overhead). Establish at spike time: which CEF version `cef-rs` actually tracks (verify "140+"), whether off-screen render + isolated worlds are exposed, and the helper-subprocess entry pattern. Wrap every callback in `catch_unwind` → return a CEF-level error rather than unwinding. Document the CEF download/build step for CI. This is a `[RISK]`, not a blocker, but it's the highest-uncertainty external dependency.

### F5. `[GAP]` `missing_docs` lint vs disposable/early crates
- **Where.** `Cargo.toml` `[workspace.lints.rust] missing_docs = "warn"`, and CI runs `-D warnings`. Every public item needs a doc comment.
- **Why it risks.** Every public type/trait listed in the crate topology needs doc comments from the first commit or CI fails. Not a blocker, just a standing constraint subagents must honor.
- **Proposed resolution.** Note in each work-unit brief: public API ships with docs. No action needed beyond awareness; flagged so subagents don't get surprised by CI.

### F6. `[RISK]` `wasmtime` + `panic = "abort"` + `unsafe_code = deny`
- **Where.** `mote-wasm` uses `wasmtime`; workspace denies `unsafe_code`.
- **Why it risks.** `wasmtime`'s host-function definitions sometimes involve `unsafe` for raw memory access. Our *code* must stay safe; `wasmtime`'s safe API (`Linker`, typed funcs) should suffice, but the adblock rule-engine host calls (`6.1`) may tempt raw memory access.
- **Proposed resolution.** Use only `wasmtime`'s safe typed-function API; if raw access is unavoidable, isolate it (but `mote-wasm` is not on the §1 exception list — only `mote-cef` re-enables `unsafe`). If WASM genuinely needs `unsafe`, that's a workspace-lint-policy amendment (with comment) — flag to user. Likely avoidable.

---

## G. Smaller items (specify before the relevant unit)

- **G1 `[GAP]`** `downloads:*` is **deferred** (DESIGN §"Deferred to a later release") but the Tier-3 `download-manager` plugin (ROADMAP Phase 7) needs download observation. DESIGN says download-manager is "Tier 3" needing `downloads:*` "in Tier 3," yet `downloads:*` is deferred past v0.1. **Contradiction:** Phase 7 ships `download-manager` in v0.1 but its required permission domain is deferred. *Resolution:* either move `download-manager` to v0.2, or scope its v0.1 version to only what `net:intercept_request` covers (DESIGN hints at this). **User decides** — affects whether `7.3` ships in v0.1.

- **G2 `[GAP]`** `session:exclude_forms` is "v0.2+" (DESIGN §Form drafts, §New permissions) but appears in the v1 domain list (`session: manage_hidden, exclude_forms`). Same shape as C2 (`introspect:`). *Resolution:* declare in v1 or add additively in v0.2; recommend additive in v0.2, exclude from v1 registry. Decide in `1B.1`.

- **G3 `[GAP]`** `mote.ai` aside: even setting A1 aside, no LLM/AI permission domain exists (DESIGN §"LLM access lives in plugins" — deliberate). So the spec's `mote.ai` API has **no permission backing** in the model. Reinforces that `mote.ai` cannot exist in v0.1 (any AI plugin uses `http:fetch` + `secret:read`). Confirms A1's resolution.

- **G4 `[RISK]`** "version-naive code" (ROADMAP Phase 1) is undefined jargon. Likely means plugins don't pin versions of other plugins (consistent with B1/B2's "no version constraints"). *Resolution:* confirm it means "plugins are version-naive about each other" and isn't a separate feature.

- **G5 `[GAP]`** `identity_scope = user_choice` storage migration: if a user switches a `user_choice` plugin from global to per_identity later (DESIGN §Plugin Identity Scope: "You can change this in plugin settings later"), what happens to existing global storage? Undefined. *Resolution:* specify migration behavior (likely: data stays in the old namespace, new identity gets fresh storage, with a documented note) before `mote-storage` finalizes namespacing (`1A.4`/`1E.4`). Touches the global "don't corrupt the past" rule.

- **G6 `[GAP]`** Hook/event name registry (see C1) — needed for conformance to validate `M.hooks`/`M.events` keys. Currently event names are scattered in prose. *Resolution:* enumerate them in a versioned registry (`events/v1.yaml` or within capability contracts) in `1B.1`.

- **G7 `[INCONSISTENCY]`** `password-manager-form-services` capability name vs plugin name. DESIGN uses `password-manager-form-services` as the **capability** (consumed) and `password-manager-form-services-plugin` as the **plugin** (fulfiller). The manifest `consumes = { "password-manager-form-services" }` references the capability; the worked example file is `password-manager-form-services-plugin/init.lua`. Easy to conflate. *Resolution:* keep the `-plugin` suffix discipline for the fulfiller's name; the capability is the unsuffixed contract name. Document in the capability registry.

- **G8 `[RISK]`** MVP estimate tables disagree: DESIGN §MVP Scope has a **5-phase** table (different from ROADMAP's 11 phases) with different groupings (e.g., DESIGN "Phase 3 = Browser shell," ROADMAP "Phase 2 = Browser shell"). Not load-bearing (the ROADMAP is the operative phasing per the orchestrator brief) but the two phase-numbering schemes will confuse anyone cross-referencing. *Resolution:* treat **ROADMAP's 11-phase numbering as canonical** for work tracking; DESIGN §MVP Scope is a coarser budget estimate, not a work breakdown.

---

## H. Summary of items requiring a user decision before coding

| # | Item | Severity | Blocks |
|---|---|---|---|
| A1 | AI UI: spec `ask`/`mote.ai` vs DESIGN "no AI UI" | BLOCKER | host API, omnibox/sidebar elements |
| A2 | Two Lua API surfaces (spec vs DESIGN); declarative vs imperative | BLOCKER | `mote-lua`, `1E.3` |
| A4 | Chrome tech: HTML/CSS (spec) vs wgpu/Skia/iced (DESIGN) | BLOCKER (it *is* the UI ADR) | all `[UI-GATED]` work |
| B1 | `requires` vs `consumes` — which exists, what re-triggers approval | BLOCKER | manifest schema, re-approval, `3.2` |
| B2 | Semver plugin deps (ROADMAP) vs capability-only (DESIGN) | BLOCKER | `mote-pluginmgr` resolver |
| B5 | Declarative vs imperative event registration (flagged irreversible) | DECISION | conformance design, `1C.1`/`1D.1` |
| C2 | `introspect:` in v1 registry vs v0.2 implementation | DECISION | `1B.1` v1 registry contents |
| F1 | Registry file format YAML vs TOML | DECISION | `1B.1` |
| G1 | `download-manager` (v0.1) needs deferred `downloads:*` | DECISION | whether `7.3` ships in v0.1 |
| E1 | "Fully isolated" wording violates DISCIPLINES §5 | INCONSISTENCY (doc fix) | identity docs/marketing |

Everything else (`[GAP]`/`[RISK]`) can be resolved by the implementing engineer with the proposed default, but should be confirmed where flagged. The four `[BLOCKER]`s plus B1/B2 are the ones that, if guessed wrong, cause the most rework — surface these first.

---

## Implementation findings (Phase 1)

### Resource normalization before `Gatekeeper::check` (found building `mote-permissions`, 2026-05-25)
DESIGN's `net:intercept_request:!*.banking.com` example implies the resource string checked against a permission is a **normalized host** (e.g. `secure.banking.com`), not a full URL with scheme/path (`https://secure.banking.com/login`) — the glob `*.banking.com` only matches the host form. So the runtime seam (`mote-dispatch` / `mote-cef`) MUST normalize each operation's resource to the form permission patterns are written against *before* calling `Gatekeeper::check`. This normalization contract is undocumented in DESIGN. **Assign to `mote-dispatch`/`mote-runtime`; document the canonical resource form per permission domain.**
