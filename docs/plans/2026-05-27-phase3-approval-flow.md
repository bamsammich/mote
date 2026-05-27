# Phase 3 (3.6/3.7) — Install→Approval Flow + Integrity-Panel Wiring — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

**Goal:** Wire the runtime's approval seam to a real, async approval dialog on the privileged chrome origin, drive plugin loading through `PluginManager`, and make the integrity panel's actions live — proven by a real browser run.

**Architecture (per ADR-0007):** Plugin loading moves to *after* the winit event loop is live. A shell **approval coordinator** decides, per plugin, whether approval is needed (dev-mode/bundled → auto-grant; prior non-expanding approval → silent grant; first-install/expansion → dialog). Plugins needing approval enter an **async "awaiting approval"** state; the dialog renders on the **privileged `mote://chrome`** origin and resolves via an `approve_plugin` bridge op, which then calls `Runtime::load` with a `DecidedPolicy` carrying the user's choice. The integrity panel likewise renders on `mote://chrome` and its action buttons invoke new privileged ops → `PluginManager`/`Runtime` calls.

**Tech Stack:** Rust (edition 2024), `mote-shell` (winit + CEF pump), `mote-cef` HostBridge, `mote-ui` view-models + HTML, `mote-pluginmgr` façade, `mote-runtime` approval seam.

**Source of truth:** `docs/plans/03-plugin-management.md` §6/§6.5; `docs/adr/0007-...`; the shell integration map (this session's Explore). Where they disagree, ADR-0007 + the actual code win.

**Policy baked in (note, not a new ADR):** `bundled` plugins auto-approve (trusted by construction — they ship in the binary the user already runs; `IntegrityStatus::Bundled`). This prevents prompting for first-party urlbar/workspace-manager on every fresh launch. If the maintainer wants bundled to prompt, flip the one classification branch in Task 4.

---

## Prerequisites (verified)
- Embedded bundle contains `urlbar` + `workspace-manager` (`mote_pluginmgr::bundled_names`, tested).
- `mote_pluginmgr::{PluginManager, ApprovalStore, compose, load_order, diff}` exist and are tested.
- `mote_runtime::{ApprovalPolicy, Approval, Narrowing, ApprovalHash, Runtime::{load,reload,unload}}` exist; `reload(require_reapproval=false)` returns `ApprovalDenied` and keeps the old instance.
- `mote_ui::{ApprovalRequest, NarrowablePermission, NarrowMode, IntegrityPanel, PluginRow, PluginKind, IntegrityStatus, PluginAction}` exist.
- `mote_registry::CombinationRegistry::triggered_by(&BTreeSet<String>)` exists.
- Run: `DISPLAY=:1 LD_LIBRARY_PATH=$PWD/target/debug ./target/debug/mote --ozone-platform=x11`.

Gates per task (run via mise): `cargo test -p <crate> --all-features`, `cargo clippy -p <crate> --all-targets --all-features -- -D warnings`, `cargo fmt --all --check`. Avoid the typos-rejected misspelling of "unparsable". NO `unsafe`, NO `#![allow]`, document public items. Commit per task (conventional, no AI co-author).

---

## Task 1 — `approval_html` renderer (mote-ui) + `Runtime::registry()` accessor (mote-runtime)

Smallest, mechanical, additive. No shell, no CEF.

**Files:**
- Modify: `crates/mote-ui/src/integrity.rs` (or a new `crates/mote-ui/src/approval.rs` module) — add `pub fn approval_html(req: &ApprovalRequest) -> String`.
- Modify: `crates/mote-runtime/src/runtime.rs:111` area — add `pub fn registry(&self) -> &Registry`.
- Test: in each crate's module tests.

**1a. `Runtime::registry()` accessor.**
- Step: add `#[must_use] pub fn registry(&self) -> &Registry { &self.registry }`.
- Test: a `Runtime` built with a registry returns it (assert `version()` matches). Run `cargo test -p mote-runtime`. Commit.

**1b. Structured approval payload (NOT an HTML string) — ADR-0005 compliance.**
- **ADR-0005 forbids HTML strings crossing into the privileged chrome world** ("text nodes / structured DOM construction, never innerHTML"; escaping is the rejected weaker mitigation). So we do **not** render an `approval_html() -> String`. Instead:
  - Ensure `ApprovalRequest` (and its `NarrowablePermission`/`NarrowMode`) derive `serde::Serialize` (additive if missing). The shell sends the request to chrome as **JSON data**; trusted chrome-side JS (in the chrome bundle, authored by us — never plugin-derived) builds the DOM with `createElement`/`textContent`/`setAttribute`. No plugin string is ever interpolated into HTML or assigned to `innerHTML`.
  - Add a small helper to compute the per-request fields the chrome side needs (each permission's `effective_string()`, `high_risk`, the `dangerous_combinations`, `is_update`, `new_permissions`) — exposed as serializable fields, not markup.
- TDD: `serde_json::to_string(&ApprovalRequest::sample())` round-trips back to an equal request; the serialized JSON contains the plugin name, each permission's domain + `effective_string()`, every `dangerous_combinations` entry, and (for `is_update`) each `new_permissions` entry. Run `cargo test -p mote-ui`. Commit.
- (The chrome-side typed DOM builder + a script-injection boundary test land in Task 4.)

---

## Task 2 — `DecidedPolicy` + approval-need classification (mote-shell, headless)

The pure brain of 3.6. No CEF. Fully unit-tested.

**Files:**
- Create: `crates/mote-shell/src/approval.rs` (module; add `mod approval;` to `lib.rs`).
- Test: in `approval.rs`.

**2a. `DecidedPolicy`.**
```rust
/// An ApprovalPolicy that replays a decision already made (by the coordinator
/// or the approval dialog). decide() never renders or blocks (ADR-0007: the
/// dialog/await happens in the shell, not inside the synchronous decide()).
pub(crate) struct DecidedPolicy { decision: Approval }
impl ApprovalPolicy for DecidedPolicy {
    fn decide(&self, _plugin: &str, _requested: &[Permission]) -> Approval { self.decision.clone() }
}
```
- TDD: a `DecidedPolicy` built with `GrantAsRequested` / `Narrow{..}` / `Deny{..}` returns exactly that from `decide`. (Confirm `Approval: Clone`; if not, store the parts and rebuild — check `mote-runtime/src/approval.rs`.) Commit.

**2b. Classification: does this plugin need a dialog?**
```rust
pub(crate) enum ApprovalOutcome {
    AutoGrant,                  // dev-mode, bundled, or prior non-expanding approval
    NeedsDialog(ApprovalRequest),
}
/// Decide whether `manifest` can load silently or needs the dialog.
pub(crate) fn classify(
    manifest: &Manifest,
    provenance: &Provenance,          // from PluginManager (Bundled / DevMode / DeclaredGit / Path / ImplicitLocal)
    store: &ApprovalStore,
    combos: &CombinationRegistry,
) -> Result<ApprovalOutcome, ...>
```
Logic:
- `Provenance::Bundled` or `DevMode` → `AutoGrant` (policy note above).
- else compute `cand = ApprovalHash::of(manifest)`; `prior = store.get(name)?`:
  - `None` → `NeedsDialog(build_request(manifest, provenance, combos, is_update=false))`.
  - `Some(p)` and `!cand.is_expansion_of(&p)` → `AutoGrant`.
  - `Some(p)` and `cand.is_expansion_of(&p)` → `NeedsDialog(build_request(..., is_update=true, new_permissions=delta))` (use `mote_pluginmgr::diff` to populate `new_permissions`).
- `build_request` maps each manifest permission → `NarrowablePermission` (narrowable iff it has a resource/origin axis — start: `*`-scoped origin perms are narrowable, others `GrantFull`), and fills `dangerous_combinations` via `combos.triggered_by(&keys)`.
- TDD (build `Manifest`s via `mote_lua::load_plugin`, `ApprovalStore` via `Store::open_in_memory`):
  - bundled → AutoGrant; dev-mode → AutoGrant.
  - first install (no prior) → NeedsDialog, `is_update=false`.
  - prior == candidate → AutoGrant.
  - candidate contracts vs prior → AutoGrant.
  - candidate expands vs prior → NeedsDialog, `is_update=true`, `new_permissions` lists the added perms.
  - a manifest whose perms trigger a known dangerous combo → request `dangerous_combinations` non-empty.
- Commit.

**2c. Dialog-result → `Approval` mapping.**
```rust
/// Map the dialog's per-permission decisions to a runtime Approval.
pub(crate) fn approval_from_dialog(result: &DialogResult) -> Approval
```
where `DialogResult` is the deserialized `approve_plugin` op payload (per-permission `NarrowMode` + an overall grant/deny). `GrantOrigins(globs)` → `Narrowing{domain,action,resources:globs}`; all-full → `GrantAsRequested`; overall deny (or a *required* perm denied) → `Approval::Deny`.
- TDD: all-full → `GrantAsRequested`; one origin-narrowed → `Narrow` with the right `Narrowing`; deny → `Deny`. Commit.

---

## Task 3 — Drive loading through `PluginManager` in `PluginHost` (mote-shell)

Replace the `BUNDLED` include_str! loop with a manager-driven resolved set. Still mostly headless (the load itself is synchronous Rust; the *dialog* part is Task 4/5).

**Files:**
- Modify: `crates/mote-shell/src/runtime.rs:37-167` (`Bundled`, `PluginHost`, `boot`, `build_panel`).

**3a. `PluginHost` owns a `PluginManager` + `ApprovalStore`.**
- Add fields; construct in `boot(store)` using the shell's config/cache dirs (reuse the CLI's `resolve_dirs` logic or a shared helper — extract `mote_cli::resolve_dirs` to a shared spot or duplicate minimally; prefer a small `mote-pluginmgr` path helper if one exists). `ApprovalStore::new(&store)`.
- Replace `BUNDLED` with: `manager.resolved_set(identity)?` → returns ordered `Vec<ResolvedPlugin>` (compose plugins.lua+managed.lua+bundled defaults+implicit-local, resolve sources to dirs, `load_order`). **You may need to add a thin `PluginManager` method** that returns the ordered resolved set (provenance + dir + manifest) without loading — additive to `manager.rs`. Bundled defaults: if neither plugins.lua nor managed.lua declares the bundled first-party set, seed them (unpack + link) so a fresh profile still gets urlbar/workspace-manager.
- TDD (headless, tempdir + in-memory store): `resolved_set` of a profile with one `path:` plugin + the bundled defaults returns them in dependency order with correct provenance. Commit.

**3b. Load each resolved plugin via the coordinator (auto-grant path only here).**
- For each `ResolvedPlugin`: `classify(...)`. If `AutoGrant` → `Runtime::load(init_source, identity, &DecidedPolicy::grant())` then on success `store.put(name, &ApprovalHash::of(manifest))`. If `NeedsDialog(req)` → enqueue into a `pending_approvals: Vec<(ResolvedPlugin, ApprovalRequest)>` field (rendered in Task 5; do NOT load yet).
- `init_source` = read `<dir>/init.lua` (the manager/`load_manifest_from_dir` already does this; expose the source).
- TDD: a profile with only bundled + a prior-approved path plugin loads all of them headlessly (no pending); a profile with a never-approved path plugin leaves it pending and does not load it. Commit.

**3c. `build_panel` from real provenance/integrity.**
- Replace hardcoded `IntegrityStatus::Bundled` / `PluginKind::Bundled` / `actions: Vec::new()` with values derived from each plugin's `ResolvedPlugin` (provenance → `PluginKind`; integrity → `IntegrityStatus`; actions populated by kind: git → `[Update, Rollback, Revoke, AdjustScope]`, path/dev → `[Reload, Revoke, AdjustScope]`, bundled → `[Update, Revoke]`). Include pending-approval plugins as rows with an "awaiting approval" marker.
- TDD: panel rows for a bundled + a path + a pending plugin carry the right kind/integrity/actions. Commit.

---

## Task 4 — Move approval dialog + integrity panel to the privileged `mote://chrome` origin

CEF integration. Verified by running (logic where possible unit-tested).

**Files:**
- Modify: `crates/mote-shell/src/lib.rs` (panel/overlay creation ~1086-1127; `push_state_to_chrome` ~897-912; `build_op_registry` ~562-608).
- Possibly: `crates/mote-cef/src/bridge.rs` if a chrome-page render helper is missing.

**ADR-0005 discipline for this whole task:** data crosses the bridge as **JSON only**; the chrome page's trusted JS builds DOM via `createElement`/`textContent`/`setAttribute` — **never `innerHTML`** with any plugin-derived string. Add/keep a chrome-document **CSP** that blocks inline script and `unsafe-eval`. Authored chrome JS (static, in the chrome bundle) is the only code that touches the DOM.

**4a. Render the integrity panel inside the chrome page (not the `mote://overlay` page).**
- The shell pushes the panel **as structured JSON** (`IntegrityPanel` derives `serde::Serialize`; additive if missing) to a chrome hook `mote.renderIntegrityPanel(data)`; the chrome JS builds the panel DOM structurally from `data`. Toggle visibility on `Ctrl+Shift+I`; the DOM lives in the bridged chrome page so its buttons can `window.mote.invoke(...)`.
- Verify by running: `Ctrl+Shift+I` shows the panel; buttons present (wired in 4b/5).

**4b. Render the approval dialog inside the chrome page.**
- A `mote.showApprovalDialog(data)` chrome hook receives the **JSON** `ApprovalRequest`; the chrome JS builds the dialog DOM structurally. The dialog's buttons invoke `approve_plugin`.
- Verify by running (after Task 5 wires the op): pending plugin → dialog appears.

**4c. Boundary test (ADR-0005 required).** Add a test (headless where possible, else a documented manual check) that a plugin whose name/permission/source contains `<script>`, an `onerror=` attribute, and a quote **cannot** inject script or markup into the chrome DOM — i.e. the structured builder renders them as inert text. This is the test ADR-0005 mandates at the chrome boundary.

**Note:** keep `mote://overlay` for web-adjacent overlays; only these two trust-critical surfaces move to chrome (ADR-0007).

---

## Task 5 — New privileged bridge ops + async resolution + boot restructure

The load-bearing integration. Live-verified.

**Files:**
- Modify: `crates/mote-shell/src/lib.rs` — `build_op_registry`, the `ShellApp` boot/first-tick, op handlers.

**5a. Boot restructure: load after the event loop is live.**
- Move the `PluginHost` plugin-load pass out of pre-loop `boot` into a one-shot on the first `AboutToWait` (or first resumed/redraw) tick, once the chrome page + bridge are ready to render dialogs. `boot` still constructs the runtime/manager/store; the *load pass* runs post-loop. Guard with a `did_initial_load: bool`.
- Verify by running: bundled plugins load on launch (auto-grant), window comes up.

**5b. `approve_plugin` op.**
- Register `approve_plugin` in `build_op_registry`. Payload: `{ plugin, decision: "grant"|"deny", permissions: [{domain, action, mode, origins?}] }`. **Validate the payload at the op boundary (ADR-0005 "closed structured operations"):** `plugin` must match a pending entry; `domain`/`action` must match the request's permissions; each `origins` glob must pass a format + length check (bounded character set, max length, max count) before it becomes a `Narrowing` — reject malformed input with `OpResponse::err` rather than storing arbitrary strings. Handler: `approval_from_dialog` → `Approval`; `Runtime::load(source, identity, &DecidedPolicy::new(approval))`; on success `store.put(name, hash)`, remove from pending, re-render panel + dismiss dialog. On `Deny` → drop pending, audit.
- The op handler runs on the pump thread (where runtime/bridge live) — no cross-thread issues.
- Verify by running: a never-approved `path:` plugin → dialog → approve → plugin loads → appears in panel as Verified.

**5c. Panel-action ops (3.7).**
- Register ops: `plugin_update`, `plugin_rollback`, `plugin_reload`, `plugin_revoke`, `plugin_adjust_scope`. Map each to the façade/runtime per `docs/plans/03-plugin-management.md` §6.5:
  - update → `PluginManager::update(name)`; `NeedsReapproval` → enqueue dialog (re-approval, `is_update`); `Applied` → `Runtime::reload(require_reapproval=false)`.
  - rollback → `PluginManager::rollback(name)` → `reload(false)`.
  - reload → `Runtime::reload(false)` (dev/path).
  - revoke → `Runtime::unload(name)` + `store.remove(name)`.
  - adjust_scope → re-open the dialog's per-permission narrowing → `reload` with the new grant.
- After each, re-render the panel.
- Verify by running: load a path plugin, click Reload (re-runs), click Revoke (unloads, leaves the panel), and an Update that expands perms → dialog reappears.

---

## Task 6 — Implicit-local detection, per-identity overlay, dev-mode marking

**Files:** `crates/mote-shell/src/runtime.rs`, `crates/mote-shell/src/approval.rs`.

**6a. Implicit-local detection.**
- On the load pass, scan `<config>/plugins/<name>/` for real dirs not in the resolved spec set and not cache symlinks → `Provenance::ImplicitLocal` → run through `classify` (→ dialog on first detection). TDD the scan headlessly (tempdir with a bare plugin dir → detected). Commit.

**6b. Per-identity overlay.**
- Resolve the effective spec set as global `plugins.lua` + `<config>/identities/<id>/plugins.lua` overlay (compose already supports layering). Wire the current identity into `resolved_set(identity)`. TDD: an overlay that adds/overrides a plugin for an identity composes correctly. Commit.

**6c. Dev-mode marking.**
- `PluginKind::DevMode` / `IntegrityStatus::DevMode` already exist; ensure dev-mode plugins (from `mote.dev_mode` capture) render with the `⊙` marker in `build_panel`. TDD the panel row. Commit.

---

## Task 7 — End-to-end live verification (the phase gate)

Per `docs/plans/03-plugin-management.md` §8.2 full happy-path. **This, not unit tests, closes the phase.**

1. Build: `mise exec -- cargo build`.
2. Headless e2e (already green from 3.3): `mote plugin add path:<tmp> && mote plugin sync`.
3. Live run: `DISPLAY=:1 LD_LIBRARY_PATH=$PWD/target/debug ./target/debug/mote --ozone-platform=x11`.
   - Bundled urlbar/workspace-manager load (auto-grant), window + chrome render.
   - With a `plugins.lua` declaring a `path:` plugin: the **real approval dialog renders**, shows permissions + any dangerous-combo warning.
   - Approve → plugin loads → appears in the integrity panel (`Ctrl+Shift+I`) as `Verified` with action buttons.
   - Click **Reload** (re-runs), **Revoke** (unloads). Mutate the plugin's `init.lua` to expand a permission → **Update** → re-approval dialog → approve → reloads with the new grant.
   - Corrupt a cached git plugin → integrity panel shows `Mismatch`; `mote plugin pin` recovers.
4. Screenshot the dialog + panel (grim + hyprctl per running notes) as evidence.
5. Update handoff memory: Phase 3 complete; record any follow-ups.

---

## Out of scope (tracked follow-ups, not this plan)
- `mote plugin update` (no-arg) update-all.
- Cross-file `dev_mode`/`updates` merge in `compose`.
- `IdentityScope` `Display`.
- Live OS file-watch auto-reload (notify crate) — programmatic reload + `mote plugin reload` cover correctness (R9).
- The narrowing UI richness beyond origin-glob (the `NarrowablePermission` axis set).
