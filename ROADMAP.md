# Mote — Roadmap

The plan from today through v1.0 and beyond. The MVP (v0.1) section is concrete — a working list of what needs to exist to call Mote shippable. Post-MVP is directional — known wants and likely directions, but with the explicit understanding that this is a personal-first project and priority follows what the primary developer actually needs next.

This document is meant to be read alongside `mote-design-decisions.md` (the architecture) and `mote-disciplines.md` (the operational discipline that keeps the architecture honest).

## Operating principles for the roadmap

- **Personal-first.** Priority is set by what the primary developer needs to use Mote daily. Adoption follows; it's not the driver.
- **Ship in public.** Versioned releases with clear notes from v0.1 onward. No private development phase.
- **Ship truthfully.** Marketing claims match what's in the tagged release. If a capability isn't in a release, it's listed as planned, not promised.
- **No deadline-driven scope.** Phases ship when they're done, not when a calendar says so. Time estimates in the design doc are budget guidance, not commitments.

## Status legend

- `[ ]` — Not started.
- `[~]` — In progress.
- `[x]` — Complete.
- `[?]` — Decision deferred or scope uncertain.

## MVP — v0.1

The version Mote becomes a daily driver for its primary developer and a credibly usable alternative to Zen/qutebrowser/Vieb for the target audience.

### Phase 1 — Plugin runtime foundation

The substrate everything else stands on. Until this works, nothing else can be tested end-to-end.

**Status: complete** — all 10 runtime crates built and wired, proven by an end-to-end test (`mote-runtime/tests/end_to_end.rs`). Commits `f80a05c..162ffc1`; GitHub issue #1 closed. A foundation security + code review is in progress (issue #2) before Phase 2 builds on top.

- [x] Rust workspace scaffolded (virtual `[workspace]`, `crates/mote-*` boundaries matching the design doc's layered architecture)
- [x] `mote-cef` wrapper crate — all CEF interaction goes through here; a guard test fails on any `cef::` import outside it
- [~] CEF integration: engine launch, render a hardcoded URL (off-screen), and multi-process security all working; **OS window lifecycle lands in Phase 2**
- [x] `mlua` + LuaJIT integration; sandboxed Lua environment with `io`, `os`, `debug`, `loadstring` (plus `package`, `ffi`, `require`, `load*`) removed
- [x] Capability dispatch layer: load a Lua plugin, parse manifest, register declared hooks/events
- [x] Permission registry v1 implemented (all 48 permission terms from the design doc; `introspect:` deferred to v0.2)
- [x] Capability registry v1 implemented (all 12 capabilities + their contracts) — contracts are a v1 interpretation pending maintainer sign-off before schema freeze
- [x] Per-hook-type dispatch (filter chains, broadcasts, keybind handlers) with the documented budgets and timeout semantics (risk D1 resolved: 10ms hard timeout enforced on LuaJIT)
- [x] Schema validation, module load, contract conformance, permission approval load-time pipeline (four-step)
- [x] Plugin permission enforcement working end-to-end
- [x] Plugin event bus (declarative `M.hooks` and `M.events`)
- [x] Inter-plugin dependency resolution via capability contracts (`consumes`) + dangling-consumer resolution. **Semver/`requires` removed per ADR-0002** (superseded the original "semver constraints" wording)
- [x] Per-plugin storage namespaces (SQLite-backed; per-identity isolation too)
- [x] Plugin lifecycle: load, run, hot reload (programmatic, re-approval-hashed), unload. **OS file-watch triggering lands in Phase 3 (pluginmgr)**
- [x] `wasmtime` integration for WASM plugins (minimum viable; host-import ABI)
- [x] Permission audit log (lock-free, crossbeam channels) with SQLite persistence

### Phase 2 — Browser shell

The user-facing chrome that turns "plugin runtime + rendering engine" into "browser."

**Status: substantially complete + security-reviewed** (issue #3; Phase-2 security review issue #4 closed — no MUST). HTML/CSS-in-CEF chrome + wgpu compositor (ADR-0003), winit window (ADR-0004), the chrome↔Rust host bridge on the `mote://chrome` scheme (ADR-0005 + amendment), and the plugin runtime running in-process. Drivable on Linux (a Mac build needs Phase-9 packaging).

- [~] Window management — single window working (the v0.1 ask); multi-window deferred
- [x] Tab strip with the documented tab states (active/hidden/closed)
- [x] URL bar — functional minimal navigation works; the urlbar *provider* plugin is loaded but suggestions need Phase-5 `history`
- [x] Workspace model with three-axis state (Identity / Workspace / Session)
- [x] Identity isolation (Chromium profile-based; cross-profile cookie isolation runtime-verified; `docs/identity-isolation.md`)
- [x] Workspace tab picker (the `Mod+Space` UX)
- [x] Session persistence with continuous SQLite flush, WAL mode, crash recovery
- [x] Active-tab discarding after 30min idle
- [x] Hidden-tab TTL (default 30 days)
- [ ] Settings model — config is **Lua** (resolved per DESIGN over the ROADMAP "TOML" wording). A restricted config-Lua context lands in Phase 3 (for `plugins.lua`) and grows into the full settings system
- [x] Integrity panel: active plugins, permissions (requested→effective), audit log, storage, provenance, integrity status — live data. (revoke/update/rollback/reload **actions** wire with Phase-3 plugin management)
- [~] Permission approval dialog — surface built (narrowing UI, dangerous-combo warnings); the install→approval **flow** pairs with Phase-3 plugin install

### Phase 3 — Plugin management

The infrastructure that makes plugins dotfile-driven and reproducible.

- [ ] `plugins.lua` and `plugins.lock` parsing and resolution
- [ ] Plugin source types: `github:`, `git+https://`, `path:`, `bundled`
- [ ] Content-addressed plugin cache at `~/.cache/mote/plugins/<name>/<commit>/`
- [ ] BLAKE3 hash computation per the documented spec
- [ ] CLI surface: `add`, `remove`, `update`, `source`, `sync`, `rollback`, `diff`, `import`, `gc`, `review`, `pin`, `link`
- [ ] Dependency graph resolution (library plugins, transitive fetches)
- [ ] Update flow with prominent permission-change surfacing
- [ ] First-party plugin update notifications (poll canonical sources, surface in integrity panel, prompt to switch source)
- [ ] Implicit local plugins (bare files in `plugins/` not in `plugins.lua`) detected and approval-flowed
- [ ] Plugin dev mode (per-plugin or per-directory; visual marking everywhere)
- [ ] Per-identity `plugins.lua` support

### Phase 4 — Secret management

The substrate for plugin credentials.

- [ ] `secrets.lua` parsing
- [ ] Backend: `keyring` (OS-native; macOS Keychain, Linux Secret Service)
- [ ] Backend: `password-manager` (routes to `secret:provider` plugin)
- [ ] Backend: `age` (encrypted file with user-unlocked key)
- [ ] Backend: `env` (environment variable)
- [ ] Backend: `file` (plaintext, opt-in only)
- [ ] Per-secret permission grants (`secret:read:<name>`)
- [ ] Integrity panel surfaces which plugin reads which secret with revoke controls
- [ ] Per-identity `secrets.lua` override

### Phase 5 — First-party plugins (Tier 1)

Core behavior. The browser is barely usable without these.

- [ ] `bookmarks` — store, organize, search; fulfills `ui:bookmarks_provider`
- [ ] `history` — visit log, urlbar suggestions; fulfills `ui:history_provider` and `ui:urlbar_provider`
- [ ] `workspace-manager` — full workspace functionality; fulfills `workspace:provider`
- [ ] `password-manager-core` — library plugin; form detection, autofill UX, isolated-world injection helpers
- [ ] `password-manager-1password` — vendor plugin using 1Password SDK
- [ ] `password-manager-bitwarden` — vendor plugin using Bitwarden SDK
- [ ] First-party plugin bundled distribution working (embedded in Mote binary)

### Phase 6 — First-party plugins (Tier 2)

The magnetic plugins that draw the target persona.

- [ ] `adblock` — uBlock-equivalent. WASM rule engine, Lua orchestration. Filter list updating.
- [ ] `vim-mode` — Tridactyl-equivalent. `f`/`F`/`gg`/`G`, hint mode, search, command mode, keybind discovery.

### Phase 7 — First-party plugins (Tier 3)

Off by default; one config line to enable.

- [ ] `reader-mode` — article extraction
- [ ] `dark-mode` — site-by-site dark mode
- [ ] `download-manager` — replaces Chromium's downloads with queueing/hashing/notification integration
- [ ] `mote-plugin-devtools` — per-plugin console, error traces, audit filtering, effective-permissions view, reload, storage inspection

### Phase 8 — AI-native primitives (minimum viable)

The MCP infrastructure that makes the second-pillar pitch real, even if minimal.

- [ ] MCP server endpoint (loopback-only binding)
- [ ] `mcp:server` capability dispatch (non-exclusive, namespaced tools under one endpoint)
- [ ] `mcp:client:<server-name>` permission flow
- [ ] Demo plugin: minimal `browser-mcp-bridge` exposing `list_open_tabs` and one or two other browser-state tools to external MCP clients
- [ ] Integrity panel surfaces MCP-server activity (which tools, which external clients, call counts)

### Phase 9 — Distribution and updates

Getting the binary to users.

- [ ] Build pipeline: macOS arm64, Linux x86_64
- [ ] GitHub Releases automation (signed builds, NOTICE file, LICENSE)
- [ ] Binary update notification (poll Releases, surface in integrity panel, user-initiated install)
- [ ] Crash dialog (shows captured content, asks consent to send, never auto-sends)
- [ ] Apache 2.0 LICENSE file, NOTICE file with CEF/Chromium attributions

### Phase 10 — Polish to daily-drive quality

The work that turns "works on the happy path" into "doesn't make me regret using it."

- [ ] Performance targets met: plugin call overhead <100μs Lua / <500μs WASM, tab switch <1ms, no GC pauses, cold start <500ms
- [ ] Memory targets met: shell process at/near its ~200–250 MB Chromium-embedding floor, per-page CEF renderer overhead ~10–35 MB (see DESIGN.md Performance targets)
- [ ] No crashes during ~1 week of primary-developer daily use
- [ ] All v0.1 plugins tested on both macOS (AeroSpace) and Linux (Hyprland)
- [ ] Permission approval UI is readable and not overwhelming
- [ ] Integrity panel renders well at realistic plugin counts (~20 plugins)
- [ ] `mote plugin sync` is fast (<10s for the default first-party plugin set on a normal connection)

### Phase 11 — Project artifacts

The documents and metadata a credible OSS project ships with.

- [ ] `README.md` — what Mote is, who it's for, install instructions, link to design docs
- [ ] `CONTRIBUTING.md` — what's accepted, what's negotiable, what's not (from disciplines doc section 10)
- [ ] `STATUS.md` — current state of each major capability (per disciplines doc section 8)
- [ ] `MIGRATION-v1.md` template ready for future schema bumps
- [ ] `docs/identity-isolation.md` — enumerated isolation surfaces (per disciplines doc section 5)
- [ ] Plugin author docs: writing a plugin, manifest grammar, permission reference, capability reference, dispatch model
- [ ] User docs: dotfile setup, plugin management workflow, dev mode, secret backends

### v0.1 ships when

- All Phase 1–11 items above are complete or explicitly deferred with a documented reason.
- The primary developer has used Mote as their default browser for at least two weeks without major frustrations.
- The disciplines doc's mechanisms (CEF wrapper, contract tests, no-data-without-consent, etc.) are enforced, not just declared.
- README and STATUS.md accurately describe what's in the release.

---

## Post-MVP — directional

After v0.1 ships, the project's direction is determined by what the primary developer needs next and what (if any) community emerges. The list below is what's most plausible based on the design doc's deferred items and the realistic feature pressure points.

### v0.2 — Stability and the flagship use case

The version that makes the second pillar real for users beyond the primary developer.

- [ ] `frontend-introspection-mcp` flagship plugin — accessibility tree, framework state introspection, visual diff, semantic assertions; MCP tools usable from Claude Code, Cursor, Cline, etc.
- [ ] `introspect:` permission domain implementation (a11y tree, framework devtools protocols, console capture, network history, computed styles)
- [ ] Additional first-party plugins as needed
- [ ] Real-world plugin development feedback addressed (whatever has been most painful for plugin authors)
- [ ] Plugin author tooling improvements based on what hurt in v0.1

### v0.3 — Polish and adjacent audiences

The version that becomes usable by non-target audiences.

- [ ] `mote-settings-ui` first-party plugin (Tier 3, off by default) for users who don't live in dotfiles
- [ ] Windows support (low priority unless demand surfaces)
- [ ] Linux ARM64 support
- [ ] Documentation expansion: tutorials, plugin development guide, common-recipes cookbook
- [ ] Performance regression suite

### v0.4–v0.6 — Ecosystem evolution

These are conditional on demand actually appearing.

- [?] Tree-style tabs as a first-party plugin
- [?] Plugin discovery / curated registry (if a community has formed and the demand is real)
- [?] In-window multi-pane (`:split` / `:vsplit`) if real demand surfaces
- [?] Sync infrastructure (probably plugin-delivered rather than core; user-hosted by default)
- [?] Cross-version event compatibility helpers for plugin authors managing v1 → v2 migrations

### v1.0 — Mature substrate

The version where the project's substrate properties are stable enough to commit to a major version.

- [ ] Permission/capability schema v1 declared frozen in its current shape (additive changes only)
- [ ] Plugin author ecosystem stable enough that breaking changes would harm real users
- [ ] First schema migration (v1 → v2) executed cleanly if needed during v0.x — proving the migration mechanism works in practice
- [ ] License, governance, contribution model all settled into the patterns the project will live with long-term
- [ ] At least one external production user beyond the primary developer

### Indefinite / decision-deferred

These are real possibilities that may or may not happen depending on how the project evolves. They're listed for completeness, not as commitments.

- **Theming in detail** — what themes control, runtime-switchable vs. load-time, `theme:provider` stacking semantics, native Astro Red support
- **External bundle support** (`bundled:<name>` resolving to user-configured bundle paths) — for corporate offline distribution, airgapped users, curated bundles
- **Plugin signing and verification** — only if a third-party plugin registry exists and supply-chain attacks become a credible concern
- **Cross-machine session sync** — if the dotfile-driven model proves insufficient and someone wants a real solution
- **Workspace persistence model** — currently SQLite-per-identity; might evolve to flat files or per-workspace directories
- **UI framework decision** — **RESOLVED (ADR-0003):** the chrome is authored as HTML/CSS rendered by CEF off-screen and composited by a thin `wgpu` layer. Decided via a three-way prototype spike (custom-wgpu / egui / HTML-in-CEF); see `docs/research/ui-spike-*.md`
- **Mote-as-CI** — Mote running headless in a CI environment with the `frontend-introspection-mcp` plugin enabled, becoming the deployment target for AI-driven E2E tests
- **Plugin marketplace** — only if the bootstrap problem is solved and a curated community emerges

### What's explicitly not on the roadmap

These have been considered and ruled out for the foreseeable future. They could change, but the burden of proof is high.

- **Built-in AI UI** — chatbot panel, AI summaries, urlbar AI suggestions. AI features are plugin-delivered, not runtime-delivered. (Core principle 8.)
- **Telemetry, analytics, opt-out data collection.** (Core principle 10.)
- **Corporate sponsorship that influences the roadmap.** (Sustainability posture.)
- **Hosted services as a paid component of Mote.** Separate concerns OK; built-in revenue model that compromises substrate neutrality not OK.
- **Forking Chromium directly.** CEF is the intentional choice; reinventing CEF's contribution is not Mote's job.
- **Becoming an LLM router.** Plugins handle LLM access via `http:fetch` + secrets; the runtime doesn't track provider APIs.
- **In-browser tab tiling (`#1` from the tiling-WM discussion).** Defer to the user's actual WM. The browser stays out of that job.
- **Manifest V3 or WebExtension compatibility.** The whole point of building Mote is the cleaner plugin model. Compatibility would defeat the purpose.

## How this document evolves

The roadmap is meant to be edited freely. Add items as the project's direction sharpens; check items off as they ship; move items between MVP and post-MVP if scope discoveries demand it. The only discipline: don't claim things are in v0.1 that aren't in v0.1, and don't pretend deferred items don't exist.

This is a tool for the primary developer to set goals and for users to see the vision. It is not a contract. Schedule slippage isn't a violation. The only commitments are the principles in the design doc and the operational disciplines in the disciplines doc. Everything here is plan, not promise.
