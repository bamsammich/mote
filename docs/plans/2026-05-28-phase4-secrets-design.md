# Phase 4 — Secret Management — Design

- **Date:** 2026-05-28
- **Status:** Validated (brainstorming complete; implementation plan + adr-review gate to follow)
- **Roadmap:** ROADMAP.md §"Phase 4 — Secret management"
- **Authority:** DESIGN.md §Secret Management; DISCIPLINES.md; this doc resolves the
  open decisions and one DESIGN correction (see §10) recorded as ADR-0009.

---

## 1. Goal & scope

Provide the substrate for plugin credentials: a `secrets.lua`-declared set of named
secrets, each resolved through a user-chosen backend, exposed to a plugin only as a
string via `secrets.get(name)` and only when the plugin holds `secret:read:<name>`.
Per-identity override; integrity-panel visibility + per-secret revoke.

**In scope (this phase):**
- `secrets.lua` parsing (`mote.secrets.define({...})`), global + per-identity overlay.
- Backends: `keyring`, `env`, `file` (opt-in), `age` (identity-file), `password-manager`
  (targeted route to a named `secret:provider` fulfiller).
- `secrets.get(name)` host API gated by the existing `secret:read:<name>` grant.
- Per-read audit event (`secret:read:<name>`, backend in detail).
- Integrity panel: per-plugin secret rows (name, last-read, backend) + per-secret revoke.
- CLI: `mote secrets list`; `mote secrets link` returns a clear "no active provider"
  message until Phase 5 ships a real provider.
- Targeted capability invocation (`invoke_capability_on`) — new dispatch primitive.
- DESIGN/registry/B7 reconciliation for non-exclusive `password-manager:provider`
  (ADR-0009).

**Out of scope (deferred, with markers left in the docs):**
- `mote.plugin_config(name, {...})` storage and launch-time `$secret:<name>` resolution
  — `plugin_config` does not exist yet; `secrets.get` is the spec's primary API. Deferred
  until `plugin_config` is built.
- Phase-5 autofill multi-provider **user-picks** UX (`fill_credential`/`list_credentials`
  across >1 password manager).
- `mote secrets link` vault picker (needs a real `secret:provider` plugin — Phase 5).

## 2. Decisions (from brainstorming, 2026-05-28)

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Build `secrets.get(name)` + backends only; defer `plugin_config`/`$secret:`. | ROADMAP Phase 4 scope; `plugin_config` is unbuilt. |
| D2 | `password-manager` backend lands as targeted routing + a 2-provider test fixture; real provider in Phase 5. | No first-party `secret:provider` plugin until Phase 5. |
| D3 | `age` uses an identity file (no interactive passphrase in v0.1). | Simplest, testable, no new GUI prompt surface. |
| D4 | Keyring is live-verified against a real Secret Service daemon stood up on the dev box. | Quality bar; matches "verify by running". |
| D5 | **No fan-out guessing.** `password-manager` secret entries name their `provider` explicitly; resolution is targeted to that one fulfiller. | User principle: established PM UX = the user chooses the manager; multiple PMs (work + personal) coexist. |

## 3. Crate architecture

Mirrors the `plugins.lua` precedent: **parse to raw strings in `mote-lua`, give typed
meaning in the consumer.** No dependency cycles.

| Crate | Responsibility | New deps |
|-------|----------------|----------|
| **mote-lua** | `eval_secrets_config` (additive in `config.rs`): a `mote.secrets.define({...})` capture closure in the existing config sandbox, producing `Vec<SecretEntry>` (raw `name`, `backend` string, raw param map). No backend-semantics parsing — mirrors `PluginEntry.source` staying raw. | none |
| **mote-secrets** | Typed core: `SecretDef`, `Backend` enum + `resolve` trait, the 5 backend impls, `SecretResolver` (`resolve(name) -> Result<SecretString>`), and the `SecretProviderRouter` callback trait (for the `password-manager` route, to avoid a runtime↔secrets cycle). | `keyring`, `age`, `secrecy`, `zeroize` |
| **mote-pluginmgr** | `composed_secrets_config(identity)` (mirrors `composed_config`): global + per-identity overlay, per-name last-wins; convert `SecretEntry` → `SecretDef` with validation (`file` opt-in; `env` needs `var`; `age` needs `path`(+identity); `password-manager` needs `provider`+`reference`; `keyring` needs `id`). `mote secrets` CLI. | — |
| **mote-runtime** | Wire `mote.secrets.get(name)` into `hostapi` (gated by the existing `Gate`); implement `SecretProviderRouter` over the new `invoke_capability_on`; add that targeted-dispatch primitive to `Core`. | — |
| **mote-shell** | Build the per-identity `SecretResolver` at startup from the manager's composed config; integrity-panel secret rows + per-secret revoke op. | — |

**Rejected:** (B) `mote-secrets` depending on `mote-runtime::Core` — runtime↔secrets cycle.
(C) all resolution in `mote-runtime` — drags `keyring`/`age` into runtime, backends un-unit-testable.

## 4. Config schema (`secrets.lua`)

```lua
-- ~/.config/mote/secrets.lua  (or identities/<id>/secrets.lua)  — NOT in dotfiles
mote.secrets.define({
  anthropic_api_key = { backend = "keyring",          id        = "mote/anthropic" },
  my_custom_secret  = { backend = "env",              var       = "MY_CUSTOM_SECRET" },
  legacy_token      = { backend = "file",  opt_in = true, path   = "~/.config/mote/secrets/legacy.txt" },
  bitwarden_key     = { backend = "age",   path = "…/bw.age", identity = "…/key.txt" },  -- identity optional → default key path
  onepassword_token = { backend = "password-manager",
                        provider  = "password-manager-1password",   -- REQUIRED; names the secret:provider fulfiller
                        reference = "op://Personal/1Password Connect/credential" },
})
```

Per-name last-wins across calls and across global→identity overlay. Unknown backend,
missing required field, or `file` without `opt_in = true` → a config-load error naming
the offending secret; non-fatal to the browser (ADR-0007 posture).

### 4a. Canonical config file set & ownership (recorded here, not in ADR-0006)

> The adr-review gate (2026-05-28) noted that `secrets.lua`'s location and ownership are
> not captured in an authoritative decision record. ADR-0006 is Accepted and must not be
> edited, so the canonical set is recorded here for future sessions/engineers. ADR-0006
> remains the governing decision for the *read-only-user-config / managed-mutation-layer*
> principle; this table only enumerates the files that principle applies to.

| File | Location | Owner | Mote writes? |
|------|----------|-------|--------------|
| `plugins.lua` | `~/.config/mote/` (+ `identities/<id>/`) | User-authored | No (read-only, ADR-0006) |
| `secrets.lua` | `~/.config/mote/` (+ `identities/<id>/`) — **NOT in dotfiles** (carries references, never values; lives beside `plugins.lua`) | User-authored | No (read-only, ADR-0006) |
| `managed.lua` | `~/.config/mote/` | **Mote-managed** | Yes — the managed mutation layer (grants, approvals, Phase-3 narrowing, per-secret revoke) |
| `plugins.lock` | `~/.config/mote/` | Mote-generated lockfile | Yes |

Per-secret revoke (this phase) writes to the **managed layer**, never to user-authored
`secrets.lua` — see §8 and ADR-0006.

## 5. Resolution flow

1. Plugin Lua calls `secrets.get("anthropic_api_key")`.
2. hostapi `Gate::check("secret","read","anthropic_api_key")` — already enforces
   exact-name match against the effective grant (no registry change). Deny → return
   `nil` + `Decision::Deny` audit.
3. Allow → `SecretResolver::resolve(name)` looks up the `SecretDef`, dispatches to the backend:
   - `env`/`file`/`age`/`keyring` → resolve directly.
   - `password-manager` → `SecretProviderRouter::resolve(provider, reference)` →
     runtime `invoke_capability_on(provider, "secret:provider", "resolve_secret", reference)`
     → that one fulfiller's `resolve_secret`. Provider not loaded / `NoFulfiller` → error.
4. Value returned into Lua as a string; `AuditEvent::new(plugin, "secret:read:<name>", Allow).with_detail("<backend>")`.

## 6. Security (DISCIPLINES)

- Secret values live in `secrecy::SecretString` (zeroized on drop; redacting `Debug`,
  which also satisfies the workspace `missing_debug_implementations` lint). Unwrapped
  only at the Lua boundary. Never logged, never `Debug`-printed, never persisted by the subsystem.
- New DISCIPLINES.md entry — *temptation:* "log the resolved value to debug a backend";
  *discipline:* never; *mechanism:* `SecretString` redaction + no `String` in backend return types.
- A plugin sees only secrets it was granted by name; cannot enumerate names, read other
  plugins' secrets, or see backend metadata (the `get` closure takes a name and returns a
  value or `nil` — no listing surface exposed to Lua).

## 7. Per-identity override

`composed_secrets_config(identity)` reads global `secrets.lua`, then
`identities/<id>/secrets.lua`, per-name last-wins — exact mirror of `composed_config`.
Absent overlay is a no-op.

## 8. Integrity panel & CLI

- Panel: add per-plugin secret rows (secret name; last-read timestamp from an audit query
  over `secret:read:<name>` events; resolving backend) + `PluginAction::RevokeSecret { name }`.
  Revoke reuses Phase-3 **narrowing**: drop `secret:read:<name>` from the stored grant and
  reload, leaving other secrets untouched. *(Panel rendering is frontend → `/mote-design` +
  `/frontend-design` invoked when building it.)*
- CLI: `mote secrets list` (names + backends, **never values**); `mote secrets link <name>`
  returns "no active password manager" until Phase 5.

## 9. Dispatch addition

`Core` currently does exclusive→one fulfiller (`Ok`) or non-exclusive→**all** fulfillers
(`Multi`, registration order). Add `invoke_capability_on(provider_name, capability,
function, payload, audit)`: filter the fulfiller set to the named plugin, invoke only it
under its own permissions/deadline (same machinery), return `Ok`/`NoFulfiller`/error. Used
only by the `password-manager` secret route in this phase.

## 10. DESIGN / registry / B7 reconciliation (ADR-0009)

- `crates/mote-registry/data/capabilities/v1.toml`: `password-manager:provider`
  `composability` → `non-exclusive`; document that invocation is **targeted** (caller names
  the fulfiller), not fan-out. `secret:provider` description loses the "singular because
  gated by exclusive PM (B7)" rationale; reframed to targeted resolution.
- DESIGN.md: fix the exclusivity assertions at `:360`, `:494`, `:1300`, `:1769`, `:1847`,
  `:1851`; rewrite the §Secret Management routing paragraph to the explicit-provider model.
- `docs/plans/risks-and-inconsistencies.md` B7: mark resolved; its old "effectively
  singular" resolution is superseded by ADR-0009.
- Leave a one-line "deferred" marker where DESIGN implies `plugin_config`/`$secret:` exist.

## 11. Testing & verification

- **mote-lua:** parser units (valid/invalid entries, multiple `define` calls, per-name override).
- **mote-secrets:** per-backend units — `env` (set var), `file` (temp file + opt-in gate),
  `age` (generated identity + encrypted fixture), `keyring` (daemon-gated integration test),
  `password-manager` (via a `SecretProviderRouter` double, incl. targeted-not-other assertion).
- **mote-runtime:** integration — granted plugin reads value; ungranted plugin gets `nil` +
  `Deny`; no enumeration; `invoke_capability_on` targets exactly the named fulfiller (2-provider fixture).
- **Live verification:** stand up gnome-keyring/Secret Service; seed one secret per backend;
  run the app with a small verification plugin; confirm the panel shows reads/backends/last-read
  and per-secret revoke works (user drives the click). Headless CLI e2e: `mote secrets list`.

## 12. PR topology (provisional; finalized in the plan)

By file overlap, ascending blast radius: (1) mote-lua parser + mote-secrets backends
(disjoint, foundational); (2) pluginmgr compose + convert + CLI; (3) runtime
`invoke_capability_on` + hostapi `secrets.get`; (4) shell panel + revoke + wiring;
(5) ADR-0009 + DESIGN/registry/B7 reconciliation (lands with or before the routing change).
