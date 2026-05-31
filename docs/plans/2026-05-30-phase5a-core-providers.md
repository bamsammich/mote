# Phase 5a — Core First-Party Plugins (history, bookmarks, workspace-manager)

> **Status:** DRAFT — awaiting user approval. Phase 5 split: this is **5a (core providers)**; the
> password-manager stack (form-services + 1Password + Bitwarden + isolated-world injection) is a
> **separate, ADR-gated effort (5b)**, explicitly out of scope here.

**Goal:** Build three first-party plugins to working, bundled, dialog-free-loading quality, plus the
shell/chrome wiring that makes them visible and usable:

1. **history** — fulfills `ui:history_provider` (`query_history`, `record_visit`) AND
   `ui:urlbar_provider` (`query`). Records visits; ranks omnibox suggestions; **owns and collects the
   `urlbar:suggest` collector surface** (DESIGN.md:349/862) — contributors return matches, history merges.
2. **bookmarks** — fulfills `ui:bookmarks_provider` (`list_bookmarks`, `add_bookmark`,
   `remove_bookmark`). **Subscribes to `urlbar:suggest`** and returns matching bookmarks (the first real
   collector contributor; tab-search and others slot in later with no history change).
3. **workspace-manager** — flesh out the Phase-2 stub to a real `workspace:provider`
   (`list_workspaces`, `switch_workspace`; event `workspaces:on_change`) driving an actual tab-strip
   swap.

Plus: remove the now-redundant standalone `plugins/urlbar/init.lua` (DESIGN: history owns
`ui:urlbar_provider`), and wire the shell omnibox → provider → suggestions → navigate path.

**Architecture (verified in code):**

- **Bundled distribution is automatic.** `bundled_names()` (`crates/mote-pluginmgr/src/bundle.rs:75`)
  derives names by listing top-level dirs of the `include_dir!`-embedded `plugins/` tree — **no
  hardcoded name list**. Adding `plugins/history/init.lua` and `plugins/bookmarks/init.lua` makes them
  bundled automatically; deleting `plugins/urlbar/` removes it. `resolved_set`
  (`crates/mote-pluginmgr/src/manager.rs:801-810`) seeds *all* `bundled_names()` when no bundled plugin
  is declared in `plugins.lua`. `classify()` auto-grants `Provenance::Bundled` → dialog-free load
  (`crates/mote-shell/src/approval.rs`).
- **Capability dispatch:** manifest `capabilities` → `CapabilityMap::claim`
  (`crates/mote-runtime/src/capability.rs:57`); exclusive double-claim rejected. Lua consumers call
  `capabilities.invoke(cap, fn, arg)` → `Core::invoke_capability` (`crates/mote-runtime/src/core.rs:205`)
  with the S1 contract guard (fn ∈ `required_api`) and the 100 ms `INTER_PLUGIN_BUDGET` deadline.
  `invoke_capability_on` (targeted) is reserved for the secret route (ADR-0009) — **not** used here.
- **Storage is flat KV** surfaced to Lua: `mote.storage.get/set/delete`
  (`crates/mote-runtime/src/hostapi.rs:146-197`), gated by `storage:persistent`. `Namespace::list_keys()`
  (`crates/mote-storage/src/namespace.rs:166`) exists but is **not** exposed to Lua. `HostValue`
  (`value.rs:74`) has `List` + `Map` for array-of-record marshalling.
- **Shell↔runtime seam:** `ShellApp` owns `host: PluginHost` → single-threaded `Runtime`/`Core`.
  Op handlers only enqueue `ShellCommand`s; the pump thread applies them in `drain_commands`
  (`crates/mote-shell/src/lib.rs:894`) with `&mut self`. The omnibox `navigate` op (`lib.rs:608`,
  `navigate_active` `:1060`) loads the URL directly and never invokes `ui:urlbar_provider`.
- **No public Rust method to invoke an exclusive capability's `M.api`** exists yet (only the
  ADR-0009-bounded `invoke_capability_on` for secrets) — Task C1 adds the general
  `Runtime::invoke_capability`, the Rust mirror of the existing Lua `capabilities.invoke`.

**Tech stack / conventions:** Rust workspace; Lua via `mlua`/LuaJIT. Gates via `mise exec -- cargo`:
`fmt --check`; `clippy --all-targets --all-features --workspace -D warnings` (nursery incl.
`missing_const_for_fn`; `missing_docs` on pub items); `taplo fmt`; `typos`. TDD: failing test → verify
fail → minimal impl → verify pass → commit. Conventional commits (`<type>(<scope>): <subject>`,
imperative, ≤50 chars), to `main`, **no AI trailer**. `adr-review` gate runs on this plan (pre-approval)
and post-implementation.

---

## Resolved unknowns

### Unknown 1 — Collector dispatch: BUILD IT (user-approved 2026-05-30). DESIGN-faithful; ⚠ ADR-gated.

**Decision (user, 2026-05-30):** build real collector dispatch — *not* the earlier direct-invoke
shortcut (which hard-wired history→bookmarks by name and was not extensible to "other plugins" per
DESIGN.md:349). The shortcut would have been throwaway code.

**The schema already exists.** `urlbar:suggest` is declared `dispatch = "collector"`
(`crates/mote-registry/data/events/v1.toml:63-66`: "Collector surface emitted by the urlbar provider;
contributors return suggestions the provider merges/ranks") and `ui:urlbar_provider` is documented as
"May emit urlbar:suggest for contributors" (`capabilities/v1.toml:48`). So this is **implementing an
already-declared registry dispatch shape + DESIGN pattern**, not a new schema or a new architectural
decision in the registry. No registry/contract edits.

**What's missing (the work):**
1. **Engine path.** `core.emit()` (`core.rs:161-186`) iterates listeners but discards every return
   (`core.rs:182`, `let _ = call_hook_with_deadline(...)`); `hook_type_for` maps `Collector` →
   `HookType::Broadcast` (`runtime.rs:647-651`). Add a *collecting* path that captures each subscriber's
   return (marshalled to `HostValue`) instead of discarding it.
2. **Host API for the provider.** `mote.events.collect(event, payload) -> { contribution, ... }` —
   restricted to events whose registry dispatch is `Collector` (calling it on a broadcast event errors);
   gated by `events:emit`. Returns one entry per contributing subscriber (each a `HostValue`, typically a
   list of suggestion records). Error/timeout of one subscriber is isolated (audit-logged under the
   *subscriber*, dropped from results) — mirrors `emit` isolation.
3. **Subscriber contract.** A contributor declares an `events["urlbar:suggest"]` handler that **returns**
   its contributions (currently event-handler returns are discarded — the collecting path captures them).
   Needs the `events:on` permission.
4. **Deadline-budgeting contract (the genuine ADR substance — DISCIPLINES §3).** The provider's `query`
   already runs under the 100 ms `INTER_PLUGIN_BUDGET` (via C1). `collect` must bound *total* urlbar
   latency regardless of subscriber count: run the whole collection under a single shared deadline
   (the caller's remaining budget), each subscriber capped at `min(remaining, PER_SUBSCRIBER_CAP)`,
   stop collecting once the shared deadline passes (later subscribers contribute nothing that round).
   Deterministic subscriber order (name sort) for testability; since the provider re-ranks, order only
   affects which contributors get dropped under deadline pressure. **This contract is what the ADR
   documents and what needs user approval before the ADR is committed.**

**Flow:** history's `query(text)` = (1) gather its own visit-log matches, (2) `local contribs =
mote.events.collect("urlbar:suggest", {text=text})`, (3) merge/rank under history's own merge policy
(history owns the merge — DESIGN.md:862). bookmarks **subscribes** to `urlbar:suggest` and returns its
matches. Graceful degradation: zero subscribers → history-only suggestions. Tab-search contributions
naturally slot in later by subscribing — no history change needed (the extensibility DESIGN promises).

### Unknown 2 — Storage iteration: expose Lua `storage.list_keys()` (pure additive host API), keyed-record layout.

History is append-heavy; a single JSON blob per namespace = O(N) rewrite on every visit (write
amplification + unbounded marshalling under the 100 ms budget). `Namespace::list_keys()` already exists
and is namespace+identity-scoped by SQL (isolation proven by its own tests). Exposing it to Lua is a
**pure additive** host-API surface gated by the **existing** `storage:persistent` permission — it lists
only the plugin's own scoped keys, revealing nothing the plugin doesn't already know (it wrote them).
Adding an optional API method is allowed within v1 (DESIGN §additive-only / DISCIPLINES §2). **No ADR,
no schema bump** (assessment — confirmed by adr-review gate below).

**Host-API addition** (`crates/mote-runtime/src/hostapi.rs`, alongside get/set/delete): `storage.list_keys()
-> { "key1", ... }` (sorted, this plugin's scope only); same `storage:persistent` gate; `Err` →
empty table (default-deny).

**Data model:**
- **history** (`identity_scope = "per_identity"` per DESIGN §Defaults): one KV entry per normalized URL,
  key `v:<url>`, value JSON `{url, title, visit_count, last_visited}`. `record_visit` = O(1)
  get-bump-set. `query_history`/`query` = `list_keys()` filter `v:` prefix, `get`, rank by
  recency/frequency. `max_entries` write-side LRU trim keeps N bounded.
- **bookmarks** (`identity_scope = "per_identity"` per DESIGN §Defaults — "bookmarks, history"): one KV
  entry per bookmark, key `b:<id>`, value JSON `{id, url, title, added}`. add/remove = O(1).
  `list_bookmarks([filter])` = `list_keys()` + prefix filter + optional substring match.

### Unknown 3 — Omnibox suggestion UX wiring.

Path: omnibox `input` listener → new `urlbar_query` op → `ShellCommand::UrlbarQuery(text)` →
`drain_commands` calls `self.host.runtime.invoke_capability("ui:urlbar_provider","query",text)` →
`HostValue::List` of records → push to chrome via `eval_js applyOp('urlbar_suggestions', <json>)`
(mirrors the existing picker push at `lib.rs:1539-1549`) → chrome renders dropdown, arrow/enter selects
→ `navigate`. Files: `crates/mote-shell/src/lib.rs` (op + command + drain), `crates/mote-ui/chrome/`
(html/js/css). Chrome work is **frontend → MUST invoke `/mote-design` then `/frontend-design`** per
CLAUDE.md; the plan does not prescribe visual design.

### Unknown 4 — Workspace switch → tab strip.

DESIGN §Workspace: a workspace owns an ordered set of (pinned) tabs, theme, default identity, default
new-tab page; the workspace's tab list is canonical, window strips are views. `mote-session` already
keys tabs by `WorkspaceId` (`Session::add_tab(url, workspace)` `lib.rs:1081`; `tab_picker_ranked(workspace)`
`lib.rs:1494`). Shell currently hardcodes one `WORKSPACE` const (`lib.rs:110`).

**`switch_workspace(id)` (plugin, policy):** validate id ∈ registered set; persist `active_workspace`;
`events.emit("workspaces:on_change", {active=id})`. It does NOT itself swap tabs (owns policy; shell
owns mechanism). **Shell (mechanism):** on the switch, re-point `self.workspace`, rebuild the visible
tab strip from `session.tab_picker_ranked(new_ws)` (the existing build-initial-tabs routine
`lib.rs:364-394`, parameterized by workspace). **v0.1 in scope:** swap visible tab set per workspace +
persist active + emit/observe; ship ≥2 built-in workspaces so switching is demonstrable. **Deferred:**
per-workspace theme/accent/default-identity/default-new-tab-page; the `mote.workspace.define` config-Lua
surface; rich workspace switcher UI.

---

## TDD Task Sequence

Plugins are separate files (Groups A, B parallelizable). Anything touching `crates/mote-shell/src/lib.rs`
or the chrome bundle **serializes** (Groups C, D, E, F in order). Each task: failing test → verify fail →
minimal impl → verify pass → gate-green → commit.

### Group A — bookmarks plugin (+ shared storage API)

**A1. Add `storage.list_keys()` to the Lua host API.**
- Test (`crates/mote-runtime/tests/`): `list_keys_returns_scoped_keys` (plugin with `storage:persistent`
  sets a,b → `list_keys()` == sorted {a,b}); `list_keys_denied_without_permission` (no perm → empty).
- Impl: `list_keys` closure in `hostapi.rs install()`, gated `storage:persistent`, mirrors `delete`;
  update module doc.
- Commit: `feat(runtime): expose storage.list_keys to lua`.

**A2. bookmarks plugin — manifest loads + conforms.**
- Test (`bundled_providers.rs`): `bookmarks_provider_loads_and_conforms`,
  `bookmarks_passes_step1_and_step3_in_isolation` (mirror urlbar tests; `include_str!` the new file).
- Impl: `plugins/bookmarks/init.lua` manifest (`schema=v1`, perms `storage:persistent`, `bookmarks:read`,
  `bookmarks:write`, `events:on` [to subscribe to `urlbar:suggest` — added in B3], `events:emit`;
  `capabilities={"ui:bookmarks_provider"}`; `identity_scope="per_identity"`);
  `M.api={list_bookmarks,add_bookmark,remove_bookmark}`; empty events/hooks (the `urlbar:suggest`
  subscriber lands in B3); `setup()`.
- Commit: `feat(bookmarks): bundled provider plugin skeleton`.

**A3. bookmarks add/list/remove round-trip (black-box via a consumer Lua plugin harness).**
- Test (`crates/mote-runtime/tests/bookmarks_behavior.rs`): `add_then_list_round_trip`,
  `list_filters_by_query`, `remove_drops_entry`, `bookmarks_survive_reload`. Drive the API via a tiny
  consumer plugin calling `capabilities.invoke` (pure Lua→Lua; no C1 dependency).
- Impl: three API fns using `storage` + `b:<id>` layout.
- Commit: `feat(bookmarks): add/list/remove via keyed storage`.

### Group B — history plugin

**B1. history plugin — manifest loads + conforms (BOTH capabilities).**
- Test (`bundled_providers.rs`): `history_provider_loads_and_conforms` (claims BOTH
  `ui:history_provider` AND `ui:urlbar_provider`), `history_passes_step1_and_step3_in_isolation`.
- Impl: `plugins/history/init.lua` manifest (perms `storage:persistent`, `history:read`, `history:write`,
  `events:emit`, `events:on`; `capabilities={"ui:history_provider","ui:urlbar_provider"}`;
  `identity_scope="per_identity"`); `M.api={query_history,record_visit,query}`; `setup()`.
- Commit: `feat(history): bundled provider (history + urlbar)`.

**B2. record_visit + query_history round-trip.**
- Test (`crates/mote-runtime/tests/history_behavior.rs`): `record_visit_dedupes_and_counts`,
  `query_history_ranks_by_recency_frequency`, `history_survives_reload`.
- Impl: `record_visit`/`query_history` with `v:<url>` layout + `max_entries` write-side trim.
- Commit: `feat(history): record visits and rank query results`.

### Group BC — collector dispatch (engine + host API) — ⚠ ADR-gated (user-approved direction; ADR text needs approval before commit)

Lands **after B2** (so a real provider exists) and **before B3** (which consumes it). Registry already
declares `urlbar:suggest` = `collector` — no schema edits.

**BC1. ADR: collector dispatch + deadline-budgeting contract.**  ⚠ **needs user approval before commit.**
- Write `docs/adr/00NN-collector-dispatch.md` documenting: the collecting-emit path; `mote.events.collect`
  host API (collector-events only, `events:emit`-gated); the subscriber-returns contract; and the
  **deadline-budgeting rule** (single shared collection deadline = caller's remaining budget; per-subscriber
  cap; stop-when-exhausted; deterministic order; per-subscriber error isolation → audit + drop). Note it
  *implements* an already-declared registry shape (events/v1.toml) and the DESIGN.md:862 collector pattern,
  so it adds no registry/capability contract — it pins the runtime dispatch + deadline semantics.
- **STOP for user approval of the ADR** ([[adr-approval-required]]) before committing. Then `adr-review`.
- Commit (after approval): `docs(adr): collector dispatch and deadline budgeting`.

**BC2. Engine collecting path — capture subscriber returns under a shared deadline.**
- Test (`crates/mote-runtime/tests/collector.rs`): `collect_gathers_subscriber_returns` (2 subscribers →
  2 contributions, marshalled `HostValue`); `collect_isolates_failing_subscriber` (one errors → dropped,
  other still returned, audit records the failure under the subscriber); `collect_stops_at_deadline`
  (a slow subscriber past the shared deadline contributes nothing; total bounded);
  `collect_on_broadcast_event_errors` (only `Collector`-dispatch events are collectable).
- Impl: `Core::collect(event, payload) -> Vec<HostValue>` (mirror `emit`'s listener-gather, but capture
  each `Ok` return via `call_hook_with_deadline`, marshalled to `HostValue`); shared deadline + per-call
  cap; deterministic (name-sorted) subscriber order; reject non-`Collector` events using the event
  registry dispatch shape.
- Commit: `feat(runtime): collector dispatch gathers subscriber contributions`.

**BC3. Lua host API `mote.events.collect`.**
- Test (`crates/mote-runtime/tests/`): `events_collect_returns_contributions` (provider plugin with
  `events:emit` calls `collect` → list of subscriber returns); `events_collect_denied_without_permission`
  (no `events:emit` → empty/err); `events_collect_rejects_non_collector_event`.
- Impl: `collect` closure in the events host table (`hostapi.rs`), gated `events:emit`, delegating to
  `Core::collect`; returns a Lua array of contributions; `Err`/deny → empty table (default-deny). Update
  module docs.
- Commit: `feat(runtime): expose mote.events.collect to lua`.

**B3. urlbar `query` collects contributions + bookmarks subscribes (DESIGN collector path).**
- Test (`history_behavior.rs`, both plugins loaded): `query_merges_history_and_collected_bookmarks`
  (each suggestion tagged `source`; bookmark match arrives via the collector, not a direct invoke),
  `query_degrades_with_zero_subscribers` (history-only when bookmarks absent).
- Impl (history): `query` gathers its own visit matches + `mote.events.collect("urlbar:suggest",{text})`,
  flattens/merges/ranks under history's own merge policy. Impl (bookmarks): add
  `M.events["urlbar:suggest"] = function(p) return <matches for p.text> end` (uses `list_bookmarks`
  internally); `events:on` already in the A2 manifest.
- Commit: `feat(history): collect urlbar suggestions` + `feat(bookmarks): contribute urlbar suggestions`.

### Group C — Rust→capability bridge + shell omnibox wiring (serializes; lib.rs + runtime.rs)

**C1. `Runtime::invoke_capability` public Rust method (exclusive caps).**  ⚠ **adr-review item.**
- Test: `host_invokes_urlbar_query` (Rust calls → `HostValue::List`), `host_invoke_rejects_out_of_contract_fn`
  (S1 guard applies), no-fulfiller → `None`.
- Impl: `pub fn invoke_capability(&self, capability, function, arg: HostValue) -> Option<HostValue>` on
  `Runtime`, delegating to `Core::invoke_capability` with a constant pseudo-caller (e.g.
  `shell-subsystem`). General-purpose for **exclusive** UI providers — distinct from the bounded
  ADR-0009 `invoke_capability_on`. **adr-review must confirm** this general (single-exclusive-fulfiller,
  no fan-out) Rust mirror needs no new ADR; if it disagrees, a short ADR requires **user approval**
  before finalizing.
- Commit: `feat(runtime): host-side invoke_capability for ui providers`.

**C2. Shell `urlbar_query` op + `ShellCommand::UrlbarQuery` → invoke provider → push suggestions.**
- Test: headless host boot (`PluginHost::boot_in` + tempdirs) — load pass seeds history/bookmarks;
  assert the shell path resolves `ui:urlbar_provider` query → suggestions (`urlbar_query_op_produces_suggestions`).
  The eval_js→chrome render half is **live-verified** (integration seam).
- Impl: `ShellCommand::UrlbarQuery(String)`; register `urlbar_query` op; handle in `drain_commands` →
  `invoke_capability` → JSON → `eval_js applyOp('urlbar_suggestions', …)`.
- Commit: `feat(shell): urlbar_query op invokes provider`.  **Live verification required.**

### Group D — chrome frontend + bookmarks/history UI (serializes; chrome bundle + lib.rs) — FRONTEND (`/mote-design` + `/frontend-design`)

**D1. Omnibox input → urlbar_query; suggestion dropdown render + keyboard select.**
- Files: `crates/mote-ui/chrome/chrome.html`, `host.js`, `components/omnibox.css`.
- Behavior: `input` → `mote.invoke("urlbar_query",{text})`; `applyOp('urlbar_suggestions', rows)` renders
  a DOM-built dropdown (never HTML-string injection — matches existing `applyOp`/`__motePicker`
  discipline); Arrow/Enter selection → `navigate`.
- Verification: primarily **live in-app** (extend any chrome smoke tests if present).
- Commit: `feat(ui): omnibox suggestion dropdown` (after design skills run).  **Live verification required.**

**D2. "Bookmark this page" control + `bookmark_add` shell op.**
- Shell: `ShellCommand::BookmarkAdd` + `bookmark_add` op → reads active tab url/title →
  `invoke_capability("ui:bookmarks_provider","add_bookmark", {url,title})`. Chrome: a star/bookmark
  toggle button in the toolbar that reflects whether the current page is bookmarked.
- Test: headless shell — `bookmark_add` op invokes the provider and the bookmark is then listable
  (`bookmark_add_op_persists`). Visual/toggle state = live-verified.
- Commit: `feat(shell): bookmark current page op` + `feat(ui): bookmark toggle button`.  **Live verification.**

**D3. Bookmarks sidebar panel (bind the activity-bar bookmarks button).**
- Shell: `bookmark_list` op → `invoke_capability(... "list_bookmarks", nil)` pushes rows to chrome;
  `bookmark_remove` op → `invoke_capability(... "remove_bookmark", {id})`. Chrome: bind the existing
  bookmarks activity-bar button (`chrome.html`) to open a sidebar list (DOM-built from pushed rows),
  click-to-navigate, per-row remove control.
- Test: headless shell — `bookmark_list` returns seeded bookmarks; `bookmark_remove` drops one
  (`bookmark_list_and_remove_ops`). Panel render + click = live-verified.
- Commit: `feat(shell): bookmark list/remove ops` + `feat(ui): bookmarks sidebar panel`.  **Live verification.**

**D4. History sidebar panel (bind the activity-bar history button — consistency with D3).**
- Shell: `history_list` op → `invoke_capability("ui:history_provider","query_history", "")` (or a recent
  slice) pushes rows to chrome. Chrome: bind the existing history activity-bar button to a visit-log
  sidebar list (DOM-built), click-to-navigate. (Read-only list in v0.1; no per-row delete UI required.)
- Test: headless shell — `history_list` returns recorded visits (`history_list_op_returns_visits`).
  Panel render + click = live-verified.
- Commit: `feat(shell): history list op` + `feat(ui): history sidebar panel`.  **Live verification.**

### Group E — workspace-manager + urlbar removal

**E1. Remove the standalone urlbar plugin (history now owns the exclusive capability).**
- Two providers can't both claim the exclusive `ui:urlbar_provider`, so urlbar MUST be removed before
  history ships as a bundled default. Land **right after B1** (history exists/conforms) and before any
  boot/live test that seeds bundled defaults.
- Test: drop urlbar-specific tests; assert `bundled_names()` excludes `urlbar` and includes
  `history`/`bookmarks`; update the `runtime.rs` boot test and `bundle.rs` count test.
- Impl: delete `plugins/urlbar/`; `grep -rn '"urlbar"'` to confirm no dangling plugin-name refs.
- Commit: `refactor(plugins): remove urlbar plugin; history owns it`.

**E2. workspace-manager — multi-workspace list + persisted active + on_change emit.**
- Test (`crates/mote-runtime/tests/workspace_behavior.rs`): `lists_builtin_workspaces` (≥2, exactly one
  active), `switch_persists_active`, `switch_rejects_unknown_id`, `active_workspace_survives_reload`.
- Impl: rewrite `plugins/workspace-manager/init.lua` — built-in set, real `switch_workspace`
  (validate + persist + `events.emit("workspaces:on_change",{active=id})`), `list_workspaces` returns
  set with persisted active flagged. Keep `identity_scope="global"` (workspace *definitions* are
  cross-identity per DESIGN §Workspace — a deliberate divergence from the bookmarks/history per_identity
  default).
- Commit: `feat(workspace-manager): real list/switch with persistence`.

**E3. Shell observes workspaces:on_change → swaps visible tab strip.** (lib.rs — serialize after C2)
- Test: headless shell — seed tabs in two workspaces via `Session::add_tab(url, ws)`; drive a switch;
  assert `self.workspace` re-points and the visible tab set comes from `tab_picker_ranked(new_ws)`
  (`workspace_switch_swaps_visible_tabs`).
- Impl: parameterize build-initial-tabs by workspace; `ShellCommand::SwitchWorkspace(String)` + host hook
  so the shell, on switch, re-points workspace, rebuilds `self.tabs`, `on_active_changed`,
  `persist_and_push`. Defer theme/identity/new-tab-page.
- Commit: `feat(shell): swap tab strip on workspace switch`.  **Live verification required.**

**E4. (frontend, FULL SCOPE — user decision 2026-05-30) Visible workspace switcher.** (`/mote-design` + `/frontend-design`)
- A persistent, visible switcher in the chrome (not keyboard-only): shows the workspace set with the
  active one marked, click to switch. Built DOM-side from pushed rows (matches `applyOp` discipline),
  reflects `workspaces:on_change`.
- Shell: `ShellCommand::SwitchWorkspace(String)` + a `workspace_switch` op; push the workspace list to
  chrome on boot and on change so the switcher stays in sync.
- Test: headless shell — `workspace_switch` op drives E3's tab swap; switcher-list push asserted.
  Render + click = live-verified.
- Commit: `feat(ui): visible workspace switcher`.  **Live verification required.**

### Group F — close-out

**F1. End-to-end bundled-set integration test.** `phase5a_providers_all_load_bundled` — fresh profile,
load pass, assert history + bookmarks + workspace-manager load dialog-free and all four exclusive
capabilities are fulfilled with no claim conflict. Commit: `test(shell): phase5a providers load bundled`.

**F2. record_visit wired on navigation.** (lib.rs — serialize) — after `navigate_active`, call
`invoke_capability("ui:history_provider","record_visit",…)` so suggestions reflect real browsing (url
first; title updated when available). Test `navigate_records_visit`. Commit:
`feat(shell): record visits to history on navigate`.  **Live verification required.**

**F3. Roadmap.** Check off `bookmarks`, `history`, `workspace-manager`, and "First-party plugin bundled
distribution working" in `ROADMAP.md` Phase 5 (leave password-manager items unchecked). Commit:
`docs(roadmap): check off phase5a core plugins`.

---

## Bookmarks & history UI — APPROVED full scope (user decision 2026-05-30)

User decision: if bookmarks ships in 5a, it ships **complete** — no half-wired feature left to rot in a
later backlog. So 5a includes the full bookmarks feature: bookmark the current page (D2), a sidebar list
to view/remove and navigate (D3), plus suggestion contribution (B3). The history sidebar panel (D4) is
bound in the same pass for consistency (the chrome history button sits beside the bookmarks one and the
sidebar component is shared — binding one and not the other would leave a dead button).

## Frontend / live-verification / ADR flags

- **Frontend (`/mote-design` then `/frontend-design`):** D1, D2, D3, D4, E4 (E4 = visible workspace
  switcher, full scope per user decision 2026-05-30 — no longer optional).
- **Live in-app verification** (integration seams invisible to unit tests): C2, D1, E3, E4, F2 — and any
  bookmarks UI. **This box has NO mouse-injection tooling**; verify by launching the app against a
  scratch XDG profile (`XDG_CONFIG_HOME=<scratch> XDG_STATE_HOME=<scratch> LD_LIBRARY_PATH=$PWD/target/debug
  DISPLAY=:0 ./target/debug/mote --ozone-platform=x11`), screenshotting with `grim -g "<geom>"`
  (geometry from `hyprctl clients -j`, class `com.mote.Mote`), injecting keys via
  `hyprctl dispatch sendshortcut`, and having **the user drive mouse clicks**. See memory
  `running-and-cef-notes`.
- **adr-review gate:** runs on this plan (pre-approval) and post-implementation. Items: **BC1**
  (collector dispatch + deadline-budgeting — **new ADR, user approval required before commit**; it pins
  runtime dispatch + deadline semantics for an already-declared registry shape, adds no registry/capability
  contract). **C1** (general host-side `invoke_capability` — assessment: no new ADR, it's the Rust mirror
  of the existing Lua `capabilities.invoke`, exclusive-only, no fan-out, distinct from ADR-0009's bounded
  targeted path; reviewer confirms). **A1** (`storage.list_keys` — additive host API under existing
  `storage:persistent`, no schema bump). No registry/capability *schema* edits anywhere in 5a (the
  collector event row already exists in `events/v1.toml`).

## Honest v0.1 scope vs deferred

- **In:** real collector dispatch (`mote.events.collect` + engine path + deadline contract, ADR-gated);
  real visit recording + ranked omnibox suggestions; **complete bookmarks feature** (bookmark current
  page, sidebar list view/remove/navigate, and urlbar suggestion contribution **via the collector**);
  history sidebar list panel; suggestion dropdown with keyboard selection; multi-workspace list/switch
  with persisted active + real tab-strip swap + **visible workspace switcher**; all three plugins
  bundled/dialog-free; urlbar plugin removed.
- **Deferred:** tab-search suggestion contributions (slots into the collector later with no history
  change); per-workspace theme/identity/new-tab-page; `mote.workspace.define` config-Lua surface; history
  sharding for very large logs; the password-manager stack (5b).
