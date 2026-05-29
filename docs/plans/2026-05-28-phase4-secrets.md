# Phase 4 — Secret Management — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

**Goal:** Ship the secret subsystem — `secrets.lua`-declared named secrets resolved through five backends and exposed to a plugin only as a string via `secrets.get(name)`, gated on `secret:read:<name>`, with per-identity override and integrity-panel visibility + per-secret revoke.

**Architecture:** Mirror the `plugins.lua` boundary — `mote-lua` parses `secrets.lua` to **raw** `SecretEntry`s; `mote-secrets` gives them typed meaning (`SecretDef` + a `Backend` trait + five impls + `SecretResolver`, with secret values wrapped in `secrecy::SecretString`). `mote-pluginmgr` composes global + per-identity files and converts/validates. `mote-runtime` exposes `secrets.get` through the existing `Gate` and adds a **targeted** capability-invocation primitive (`invoke_capability_on`) for the explicit-provider `password-manager` route — no fan-out. `mote-shell` builds the per-identity resolver and renders the panel surface.

**Tech Stack:** Rust (edition 2024). New deps: `keyring`, `age`, `secrecy`, `zeroize`.

**Source of truth:** `docs/plans/2026-05-28-phase4-secrets-design.md`; DESIGN.md §Secret Management (as corrected by **ADR-0009**); the Phase-4 Explore map. Where they disagree, ADR-0009 + the actual code win.

**Gate (this plan is blocked on):** ADR-0009 must be **Accepted** by the maintainer before Task 4/5 land — it authorizes `password-manager:provider` becoming non-exclusive. Do not begin Task 4 until then.

---

## Prerequisites (verified against current code)
- `secret:read` is registered with `resource = "dynamic"` (`mote-registry/data/permissions/v1.toml:401`) and the gatekeeper enforces exact-name match (`registry.rs:631` test). **No permission/registry change for `secret:read`.**
- `secret:provider` is registered `non-exclusive` / `fan-out`, contract `required_api = ["resolve_secret"]` (`capabilities/v1.toml:133`).
- Config parser pattern: `mote-lua/src/config.rs` — `eval_config(source, chunk_name) -> Result<ConfigSpec, ConfigError>`, capture closures installed into `mote.*` via `install_config_api`, shared `Rc<RefCell<Capture>>`; `ConfigSpec { plugins, dev_mode, updates }`.
- Host API injection: `mote-runtime/src/hostapi.rs::install(lua, ctx: HostContext)`; `Gate::check(domain, action, resource, detail) -> bool`; sub-tables wired via `mote_set`. `HostContext { plugin, gatekeeper, effective, storage, audit, core }`.
- Dispatch: `Core::invoke_capability(caller, capability, function, &HostValue, &audit) -> InvokeOutcome`; exclusive→`invoke_exclusive` (one fulfiller→`Ok`), non-exclusive→`invoke_non_exclusive` (all fulfillers→`Multi`). **No targeted path exists.**
- Per-identity overlay precedent: `mote-pluginmgr/src/manager.rs` `composed_config(identity)`, `identity_plugins_lua_path`.
- CLI: `mote-cli/src/lib.rs` — `SecretsCommand::Link { name }` routes to `mgr.link(name)` (stub → `ManagerError::SecretsNotAvailable`).
- Panel: `mote-ui/src/integrity.rs` — `IntegrityPanel { plugins, network_audit, storage, denials }`, `PluginRow`, `PluginAction { AdjustScope, Revoke, Update, Rollback, Settings, Reload }`; built by `mote-shell/src/runtime.rs::build_panel`.
- Audit: `AuditEvent::new(plugin, operation, decision).with_detail(...)`; free-form operation string.

**Gates per task** (via `mise exec --`): `cargo test -p <crate> --all-features`; `cargo clippy -p <crate> --all-targets --all-features -- -D warnings`; `cargo fmt --all --check`; `taplo fmt --check` for TOML. NO `unsafe`, NO `#![allow]`, document all public items (`missing_docs`). Use "unparsable" not the typos-rejected spelling. Commit per task (conventional, **no AI co-author**). TDD: failing test first.

---

## Task 1 — `secrets.lua` parser (mote-lua, additive to `config.rs`)

Smallest, foundational, disjoint. Mirrors `make_plugins_fn` exactly.

**Files:**
- Modify: `crates/mote-lua/src/config.rs` (public types, `Capture`, `install_config_api`, `ConfigError`).
- Test: `config.rs` module tests.

**1a. Raw types (no backend semantics — mirror `PluginEntry` keeping `source` raw).**
```rust
/// A raw parameter value from a secret entry. The parser stays backend-agnostic;
/// `mote-secrets` interprets these per backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretParam { Str(String), Bool(bool) }

/// A single secret declared in `secrets.lua` via `mote.secrets.define({…})`.
/// `name` is the key; `backend` and `params` are raw — typed/validated by
/// `mote-pluginmgr`/`mote-secrets`, not here (mirrors `PluginEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEntry {
    pub name: String,
    pub backend: String,
    pub params: std::collections::BTreeMap<String, SecretParam>,
}
```
Add `pub secrets: Vec<SecretEntry>` to `ConfigSpec` and `secrets: Vec<SecretEntry>` to `Capture`. Add `ConfigError::BadSecretEntry { name, got }` + `MissingSecretBackend { name }`.

**1b. `make_secrets_fn` closure for `mote.secrets.define`.**
- Mirror `make_plugins_fn`: iterate the outer table (key = secret name); each value must be a table with a required string `backend`; collect every other field into `params` (`Value::String` → `Str`, `Value::Boolean` → `Bool`; other types → `BadSecretEntry`). Same-name-later-wins accumulate (mirror plugins).
- Install into `mote.secrets.define` inside `install_config_api` (create a `secrets` sub-table holding `define`, like `mote.updates.configure`).

**Steps:** write failing parser test → run (fail) → implement → run (pass) → commit.
**TDD:**
- `mote.secrets.define({ k = { backend = "env", var = "X" } })` → one `SecretEntry{ name:"k", backend:"env", params:{var:Str("X")} }`.
- `opt_in = true` captured as `Bool(true)`.
- missing `backend` → `MissingSecretBackend`.
- non-table entry / non-string non-bool param → `BadSecretEntry`.
- two `define` calls, same name → later wins; different names → accumulate.
- a `secrets.lua` that also harmlessly omits `mote.plugins` yields empty `plugins`.

Commit: `feat(lua): parse secrets.lua via mote.secrets.define`.

---

## Task 2 — `mote-secrets` typed core + env/file/age backends

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `keyring`, `age`, `secrecy`, `zeroize`), `crates/mote-secrets/Cargo.toml`.
- Create: `crates/mote-secrets/src/{lib.rs (replace stub), backend.rs, resolver.rs, def.rs}`.
- Test: each module.

**2a. Types.**
```rust
use secrecy::SecretString;

/// The resolved value handed to a plugin. Zeroized on drop; redacting Debug.
pub type SecretValue = SecretString;

/// Which named provider resolves a `password-manager` secret (ADR-0009: explicit).
#[derive(Debug, Clone)]
pub enum BackendKind {
    Keyring { id: String },
    Env { var: String },
    File { path: PathBuf, opt_in: bool },
    Age { path: PathBuf, identity: Option<PathBuf> },
    PasswordManager { provider: String, reference: String },
}

#[derive(Debug, Clone)]
pub struct SecretDef { pub name: String, pub backend: BackendKind }

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResolveError { /* NotFound, BackendUnavailable, FileNotOptedIn, ProviderNotLoaded(String), Decrypt(..), Io(..), … */ }
```
- `SecretDef` and `BackendKind` derive `Debug` but **never** carry a secret value (only locators). `missing_debug_implementations` satisfied.

**2b. `Backend` trait + `SecretProviderRouter`.**
```rust
/// Routes a password-manager reference to a specific named secret:provider
/// fulfiller (ADR-0009). Implemented by mote-runtime over invoke_capability_on.
pub trait SecretProviderRouter: std::fmt::Debug + Send + Sync {
    fn resolve(&self, provider: &str, reference: &str) -> Result<SecretValue, ResolveError>;
}

#[derive(Debug)]
pub struct SecretResolver {
    defs: std::collections::BTreeMap<String, SecretDef>,
    router: Option<std::sync::Arc<dyn SecretProviderRouter>>, // None until a PM route is configured
}
impl SecretResolver {
    pub fn resolve(&self, name: &str) -> Result<SecretValue, ResolveError> { /* dispatch by BackendKind */ }
    /// The backend label for audit/panel ("keyring"|"env"|"file"|"age"|"password-manager").
    pub fn backend_label(&self, name: &str) -> Option<&'static str> { … }
    pub fn names(&self) -> impl Iterator<Item = &str> { … }      // for CLI/panel ONLY — never exposed to plugin Lua
}
```

**2c. env / file / age impls (TDD each).**
- `env` → `std::env::var(var)`; unset → `NotFound`. Test with a set var.
- `file` → if `!opt_in` → `FileNotOptedIn`; else read + trim trailing newline. Test temp file + opt-in gate.
- `age` → load identity (entry `identity` or default `~/.config/mote/secrets/key.txt`), decrypt `path` with the `age` crate. Test: generate an `age::x25519::Identity`, encrypt a fixture in the test, decrypt → plaintext; wrong/missing identity → error. (No passphrase path — D3.)

Commit per backend. Final: `feat(secrets): typed core + env/file/age backends`.

---

## Task 3 — keyring backend + Secret Service verification

**Files:** `crates/mote-secrets/src/backend.rs`, `crates/mote-secrets/Cargo.toml`.

**3a. keyring impl.** `keyring::Entry::new(service, account)` from the `id` (`"service/account"` split on the last `/`, or service-only); `get_password()` → `SecretValue`; not-found / no-backend → mapped `ResolveError`.

**3b. Verification setup (D4).** Stand up a Secret Service daemon on the dev box (`gnome-keyring-daemon --start --components=secrets`, unlock a login keyring) so the live e2e (Task 10) exercises keyring for real. Add a **daemon-gated** integration test (`#[ignore]` by default, run when the daemon is up): write a secret via `keyring::Entry::set_password`, resolve it, assert match; clean up.

Commit: `feat(secrets): keyring backend (OS Secret Service)`.

---

## Task 4 — ADR-0009 reconciliation: registry flip + DESIGN/B7 edits

**Blocked on ADR-0009 = Accepted.** Lands before Task 5 so the 2-provider fixture can load two password managers.

**Files:**
- Modify: `crates/mote-registry/data/capabilities/v1.toml` (`password-manager:provider`), DESIGN.md (6 spots), `docs/plans/risks-and-inconsistencies.md` (B7).

**4a. Registry.**
- `password-manager:provider`: `composability = "exclusive"` → `"non-exclusive"`; add the dispatch shape the registry requires for non-exclusive caps (note in a comment: invocation is **targeted by caller** — `invoke_capability_on` — not broadcast; the `dispatch` field documents the multi-fulfiller listing shape used by Phase-5 autofill). Verify `from_toml`'s "non-exclusive needs a dispatch shape" invariant is satisfied.
- `secret:provider` description: drop "singular in practice … gated by exclusive password-manager:provider (risk B7)"; replace with "resolution is unambiguous because the caller names the provider (ADR-0009)".
- TDD: `Registry::load` / `from_toml` succeeds; a registry-level test that two plugins fulfilling `password-manager:provider` no longer trips `check_exclusive_claims`.

**4b. DESIGN.md + B7 edits.** Fix `:360`, `:494`, `:1300`, `:1769`, `:1847`, `:1851` to the non-exclusive + explicit-provider model; rewrite the §Secret Management routing paragraph. Mark B7 **Resolved (superseded by ADR-0009)**. Add a one-line "deferred — not yet implemented" marker where DESIGN shows `mote.plugin_config`/`$secret:`.

**4c.** Flip ADR-0009 `Status: Proposed` → `Accepted` (date) and update `docs/adr/README.md` once the maintainer signs off.

Commit: `docs(adr): accept ADR-0009; reconcile DESIGN/registry/B7 to non-exclusive PM`.

---

## Task 5 — Targeted dispatch `invoke_capability_on` + password-manager backend

**Files:**
- Modify: `crates/mote-runtime/src/core.rs` (new method), `crates/mote-secrets/src/backend.rs` (PM backend).
- Create: a runtime-side `SecretProviderRouter` impl (in `mote-runtime`, e.g. `secrets_router.rs`).
- Test: `core.rs` + a runtime integration test (2 fixture providers).

**5a. `Core::invoke_capability_on`.**
```rust
/// Invoke `function` on the SINGLE fulfiller named `provider` (ADR-0009: explicit,
/// no fan-out). Same contract validation as invoke_capability. Returns Ok(value),
/// NoFulfiller (provider not a fulfiller / not loaded), or an error outcome.
pub(crate) fn invoke_capability_on(
    &self, caller: &PluginName, provider: &PluginName,
    capability: &str, function: &str, arg: &HostValue, audit: &EventProducer,
) -> InvokeOutcome
```
- Reuse the capability-in-registry + function-in-contract checks; resolve the fulfiller set, **filter to `provider`**; if present, call only it via the same per-fulfiller primitive `invoke_exclusive`/`invoke_non_exclusive` use; else `NoFulfiller`.
- TDD (mirror existing core tests): two plugins fulfill `secret:provider`; `invoke_capability_on(provider=A, "resolve_secret", ref)` runs A's `resolve_secret` and returns its value; the other is never called (assert via audit having no record for B); unknown provider → `NoFulfiller`; non-contract function → `NotInContract`.

**5b. PM backend + router.**
- `mote-secrets` `BackendKind::PasswordManager { provider, reference }` → `router.resolve(provider, reference)`; `router == None` → `ResolveError::BackendUnavailable`.
- `mote-runtime` impl `SecretProviderRouter`: `resolve(provider, reference)` builds a `HostValue` string for `reference`, calls `core.invoke_capability_on(secret_subsystem_caller, provider, "secret:provider", "resolve_secret", arg, audit)`, maps `Ok(v)`→value, `NoFulfiller`→`ProviderNotLoaded(provider)`, else error.
- TDD: a fixture provider plugin whose `resolve_secret` returns a known value for a known reference; resolver with a `password-manager` def routes to it and returns the value; a second provider that would return a *different* value for the same reference is **not consulted** (proves targeting).

Commit: `feat(runtime): targeted invoke_capability_on + password-manager secret route`.

---

## Task 6 — `secrets.get` host API (mote-runtime/hostapi.rs)

**Files:** `crates/mote-runtime/src/hostapi.rs`, `crates/mote-runtime/src/runtime.rs` (HostContext construction in `commit`).

**6a. Thread the resolver in.** Add `secrets: std::sync::Arc<SecretResolver>` to `HostContext`; construct/clone it where `commit` builds the context (the shell supplies the per-identity resolver — Task 7 wires it; until then construct an empty resolver so runtime tests compile).

**6b. The gated `get` closure.**
```rust
// inside install(): build a `secrets` table
let g = gate.clone(); let secrets = ctx.secrets.clone();
let get = lua.create_function(move |_lua, name: String| {
    if !g.check("secret", "read", &name, None) { return Ok(mlua::Value::Nil); } // Deny audited by Gate
    match secrets.resolve(&name) {
        Ok(val) => {
            g.audit_secret_read(&name, secrets.backend_label(&name)); // Allow + backend detail
            Ok(mlua::Value::String(lua_str(secrets, &val)?))           // unwrap SecretString only here
        }
        Err(_) => Ok(mlua::Value::Nil), // resolution failure → nil (+ audited)
    }
})?;
// wire into mote.secrets.get and global `secrets` via mote_set
```
- **No `list`/enumeration function** is exposed to plugin Lua.
- TDD (runtime integration): a plugin granted `secret:read:k` with an `env` def gets the value; a plugin **without** the grant gets `nil` and a `Decision::Deny` audit; a granted plugin reading an undefined name gets `nil`; there is no Lua-reachable way to list names.

Commit: `feat(runtime): secrets.get host API gated on secret:read:<name>`.

---

## Task 7 — pluginmgr: compose secrets.lua + convert/validate + resolver wiring

**Files:** `crates/mote-pluginmgr/src/manager.rs`; a new `secrets` conversion module; `crates/mote-shell/src/runtime.rs` (build + pass resolver).

**7a. Compose (mirror `composed_config`).** `identity_secrets_lua_path(id) -> <config>/identities/<id>/secrets.lua`; `composed_secrets_config(identity) -> Vec<SecretEntry>`: eval global `<config>/secrets.lua` then identity overlay via `mote_lua::eval_config`, per-name last-wins. Missing files → empty (no error).

**7b. Convert + validate `SecretEntry` → `SecretDef`.** Per backend: `keyring` needs `id`; `env` needs `var`; `file` needs `path` and `opt_in == Bool(true)` (else `FileNotOptedIn`); `age` needs `path` (+ optional `identity`); `password-manager` needs `provider` **and** `reference` (ADR-0009). Unknown backend or missing field → a `ManagerError` naming the secret; non-fatal (collect + surface, don't abort).
- TDD: each backend's happy path converts; each missing-field case errors with the secret name; `file` without `opt_in` errors; `password-manager` without `provider` errors.

**7c. Build the resolver.** A `PluginManager` method (or shell helper) that returns a `SecretResolver` for an identity (defs from 7a/7b + the runtime-supplied router). Shell constructs it at boot and hands it to the runtime/HostContext path.
- TDD: composed global+overlay resolver resolves an env secret end-to-end (no plugin needed).

Commit: `feat(pluginmgr): compose+validate secrets.lua per identity`.

---

## Task 8 — CLI: `mote secrets list` + fix `link`

**Files:** `crates/mote-cli/src/lib.rs`.

- Add `SecretsCommand::List`; `dispatch_secrets` prints each secret **name + backend label** (NEVER values), one per line, from `composed_secrets_config` + convert. Empty → a friendly "no secrets defined" line.
- `SecretsCommand::Link { name }`: until a `secret:provider` plugin exists, return a clear message ("no active password manager; vault linking arrives with Phase 5 providers") and a non-zero exit only if the user expected success — keep it informational. Replace the raw `SecretsNotAvailable` error.
- TDD (headless, tempdir config): `list` over a 2-secret `secrets.lua` prints both names+backends and no values; `list` over empty prints the friendly line.

Commit: `feat(cli): mote secrets list`.

---

## Task 9 — Integrity panel: secret rows + per-secret revoke (frontend)

**Invoke `/mote-design` + `/frontend-design` before touching chrome JS/markup (CLAUDE.md).**

**Files:** `crates/mote-ui/src/integrity.rs`, `crates/mote-ui/chrome/panels.js`, `crates/mote-shell/src/runtime.rs` (`build_panel`) + the panel-action op path in `mote-shell/src/lib.rs`.

**9a. Data model.** Add to `PluginRow` a `secrets: Vec<SecretAccessRow>` where `SecretAccessRow { name, backend, last_read: Option<String> }`; add `PluginAction::RevokeSecret { name: String }`. Derive `Serialize` (panel crosses the bridge as JSON — ADR-0005).
- `build_panel`: for each loaded plugin, list its granted `secret:read:<name>` perms; `backend` from the resolver's `backend_label`; `last_read` = most-recent audit event matching `secret:read:<name>` for that plugin (audit query). Add `RevokeSecret` to each secret row's actions.

**9b. Chrome render (structured DOM only, never innerHTML — ADR-0005).** Extend `panels.js` to render the per-plugin secret rows + a revoke button per secret that invokes a new `plugin_revoke_secret` op.

**9c. `plugin_revoke_secret` op (mote-shell).** Validate `{plugin, name}` at the boundary; revoke via Phase-3 **narrowing** — drop `secret:read:<name>` from the stored grant and `Runtime::reload(require_reapproval=false)`; re-render panel. Other secrets untouched.
- TDD: `build_panel` row carries the right secret name/backend/last_read; revoke narrows exactly that perm (unit-test the grant-mutation helper). Boundary test: a secret name containing `<script>`/quotes renders as inert text.

Commit: `feat(shell): integrity-panel secret rows + per-secret revoke`.

---

## Task 10 — End-to-end live verification (the phase gate)

**This, not unit tests, closes the phase.**

1. Build: `mise exec -- cargo build`.
2. Stand up Secret Service (Task 3b); seed one secret per backend: keyring (`keyring` set), env (export), file (opt-in temp file), age (encrypt a fixture with a generated identity), password-manager (load the **fixture provider** plugin + a `provider`-named entry).
3. Author a tiny verification plugin requesting `secret:read:<each>`; in `setup()` it reads each and reports success/length (NEVER prints the value) to the audit/panel.
4. Live run: `XDG_CONFIG_HOME=<scratch> LD_LIBRARY_PATH=$PWD/target/debug DISPLAY=:1 ./target/debug/mote --ozone-platform=x11`.
   - Confirm the plugin loads and each backend resolves (keyring incl., daemon up).
   - `Ctrl+Shift+I`: panel shows the plugin's secret rows — name, backend, last-read — and a revoke button per secret.
   - User drives a **Revoke** click on one secret → that secret’s next read returns nil / the row updates; other secrets unaffected.
   - A plugin **without** a grant reading a name → nil + a Deny in the panel’s denials.
5. Headless e2e: `mote secrets list` prints names+backends, no values.
6. Screenshot panel + a resolved read (grim + hyprctl per running notes). Update handoff memory: Phase 4 complete; record follow-ups; check off ROADMAP Phase 4 items.

---

## Out of scope (tracked follow-ups, not this plan)
- `mote.plugin_config(name, {...})` + launch-time `$secret:<name>` resolution (deferred — `plugin_config` unbuilt).
- Phase-5 autofill multi-provider **user-picks** dispatch (`fill_credential`/`list_credentials`).
- `mote secrets link` vault picker + the v0.2 install-dialog "find in your vault" button.
- `age` passphrase-protected files / interactive unlock.
- Windows Credential Manager parity (keyring crate supports it; not verified on this box).
- Audit-backed "last read" is an O(N) scan today; an indexed surface is a v0.2 candidate (matches existing audit-history follow-up).
