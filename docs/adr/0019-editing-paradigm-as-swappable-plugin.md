# ADR-0019 — Editing Paradigm (vim/emacs) as a Swappable First-Party Plugin: Declarative Keymap, Capability Contract, Bounded Command Host-API, Content-Keystroke Withholding

- **Status:** Accepted (approved by the maintainer 2026-06-06)
- **Date:** 2026-06-06

---

## Context and Problem Statement

Mote's differentiator is that **first-party behavior IS plugins** (Neovim-style,
composable, swappable). The *editing paradigm* — modal editing (vim's
NORMAL/INSERT/…), motions (`j`/`k`/…), the `:` command-line, and the keybind
set — is the strongest expression of that: a vim plugin should own it so that an
**emacs** plugin can replace it and an emacs user is instantly at home
(`C-n`/`C-p`/`M-x`).

Today, vim-flavored behavior is **interspersed through core** instead:

- the omnibox `[cmd]` (`:`/`>`) command-line and `[find]` (`/`) modes are baked
  into the core chrome (`crates/mote-ui/chrome/host.js`);
- a `mote.mode` status-line element is a **core built-in hardcoded to `NORMAL`**
  (`crates/mote-types/src/statusline.rs:223`) — vim's modal indicator in core;
- `j`/`k` motions were threaded into the core roving helper during CL-KBNAV;
- the keybind suite is a closed Rust chord table (ADR-0012), which **explicitly
  deferred plugin-registered keybinds to "a future ADR" and reserved the
  capability name** `keybind:register-global`.

This ADR is that future ADR. The gating question — *can a sandboxed plugin
**securely** own this surface (it sees every keystroke and can run browser
commands)?* — was answered by a feasibility study
(`docs/research/secure-plugin-command-api-feasibility.md`): **yes, via a
declarative keymap model with content-keystroke withholding.** The architecture
was designed for it (`vim-mode` is a named Tier-2 first-party plugin in
DESIGN.md:1776; `keys:bind`/`keys:intercept_input` already exist in the
permission registry; `HookType::Keybind` dispatch is built).

## Decision Drivers

- **Paradigm is swappable** — vim and emacs are interchangeable first-party
  plugins via one capability contract (the "persona magnet"), not core forks.
- **Security/transparency holds** — the plugin sees keystrokes; it must NOT be
  able to see content-page input (passwords) by construction, and must not be
  able to exfiltrate without a separate, loud, audited grant.
- **Latency** — modal editing is per-keystroke; the hot content-input path must
  stay crisp.
- **Core stays a capability surface** — core owns *capabilities* (navigate,
  tabs, find-in-page, …) + a plugin keybind/mode/command **API**; it does not
  own a *paradigm*.
- Honor the sandbox (`mote-lua` — no ambient capability), the permission/audit
  model (ADR-0001/0002), and declarative-registration (ADR-0001).

## Considered Options

1. **Keep the editing paradigm in core** — rejected: not swappable; an emacs
   user cannot replace it; contradicts the core differentiator.
2. **Imperative per-key callback** — the shell calls the plugin on every keydown
   with "consume / pass-through + actions." Rejected: to decide, the plugin must
   *receive every key including content-destined ones* (passwords), maximizing
   exposure; and it adds a synchronous Lua hop on the content-input path,
   harming the path that must stay crisp.
3. **Declarative keymap + capability contract + bounded command host-fns +
   content-keystroke withholding** (chosen) — the plugin *declares* its modes
   and `chord→command` grammar at load; the **shell evaluates chords in Rust**
   and calls Lua only for *fired actions*; content-destined keystrokes are
   *withheld by construction*.

## Decision Outcome

Adopt **option 3**.

### The boundary
- **Core** provides browser **capabilities** (navigate, open/close/select tab,
  switch workspace, reload/stop, theme switch, zoom, find-in-page, …) and a
  **plugin-facing keybind/mode/command API**. Core does **not** own modes,
  motions, the `:` runner, or a keybind *paradigm*.
- The **editing paradigm** is a **swappable first-party plugin** bound via
  **two exclusive capabilities** (decided — split for least privilege):
  - **`editing-mode:provider`** — owns the modal keymap and the global keystroke
    routing; requires the loud `keys:intercept_input` grant.
  - **`command:provider`** — owns the `:` command-line: command registration and
    the command-callback. **Low-privilege** — it sees only command-line text
    (chrome-side, on submit), never the global keystream, so it does NOT require
    `keys:intercept_input`.
  `vim`/`emacs` fulfill both; a non-modal fuzzy command-palette plugin fulfills
  only `command:provider` and never touches global keys. `vim` is the reference
  fulfiller; `emacs` drops in by fulfilling the same contracts (ADR-0002:
  capability contracts are the only coupling).

### Mainstream-browser keybinds are core defaults
Core ships the **mainstream browser keybind set** (Firefox/Chrome-compatible —
`⌘T`/`⌘W`/`⌘L`/`⌘R`/`⌘-Tab`/…) as **defaults**, so Mote is familiar out of the
box with no keybind plugin installed. A keybind plugin may override any default.
**Precedence: user override > keybind-plugin > core default.** (This shapes
CL-KEYMAP: the core suite is deliberately Firefox/Chrome-aligned.)

### Inviolable safety floor
A **tiny set** of controls is never shadowable by any plugin — the can't-get-
trapped escapes: **open the Integrity Panel** and **disable/revoke a plugin**.
Rationale is the same one Chrome/Firefox use (a captured keyboard must never lock
the user out of the controls that disable the capturer). Non-keyboard backstops
exist regardless — the rail click that opens the Integrity Panel and
`mote plugin disable <name>` from the CLI — so the inviolable keyboard set is
defense-in-depth, not the sole escape.

### The mechanism — two input channels (different security profiles)

The paradigm plugin receives input through two deliberately separate channels.
The distinguishing rule: **imperative-per-global-keystroke is unsafe (declarative
only); imperative-on-the-command-line is safe (callback).**

**Channel 1 — modal keystrokes (declarative keymap, shell-evaluated).** The
plugin **declares** a keymap/grammar in a module-level table at load (validated
at ADR-0001 load-step 3): modes, `chord→command` bindings, and a small motion
grammar. The shell **evaluates chords in Rust** against that grammar and
dispatches; Lua runs only for actions that genuinely need plugin logic. Most
keystrokes never enter Lua. This is the hot, global stream — so it is declarative
(no per-key Lua callback) and content-withheld (below).

**Channel 2 — the `:` command-line (imperative callback).** A `command:provider`
plugin registers a callback that receives the **typed command string** when the
user submits the command-line (and, optionally, incrementally *within the
command-line field only*, for live completion). The plugin parses it in Lua and
acts via the bounded command host-fns. This imperative callback is safe — the
opposite of a global key intercept — because it is **explicitly invoked** (the
user opened `:`), the text is a **dedicated chrome-side command field** (never
content, never a password), and it fires **on submit, not per global keystroke**
(no hot-path latency, no keylogging surface). Sandboxed Lua may parse freely but
can only *act* through the gated allowlist, so arbitrary parse logic still cannot
escape.

**Bounded command surface (both channels act through this).** Browser commands
are a **registry-defined allowlist** invoked via a new
`mote.command.dispatch(name, args)` host-fn — capability-gated and audited per
call. **No** raw chrome-bridge-op access and **no** arbitrary-navigation
primitive.

### Security (the crux)
- **Content-keystroke withholding by construction:** when focus is a content
  page **and** an editable field is active (or the active mode is INSERT),
  keystrokes bypass the plugin entirely and flow straight to CEF. The plugin
  never sees passwords/messages typed into pages. Enforced at the shell
  chokepoint using existing signals (`FocusOwner`, CEF `focus_on_editable_field`).
- **No exfiltration without a separate, loud, audited grant** (sandbox = no
  ambient network/fs). `keys:intercept_input` is a **loud special-class grant**
  in the approval dialog (peer of `page:inject_unsafe_script`).
- **Add the keylogger combination** `keys:intercept_input` + any exfil
  permission (`http:fetch`/`mcp:client`/`sys:native_message`/`clipboard:write`)
  to `crates/mote-registry/data/combinations/v1.toml` as `severity = "danger"`.
- Every command-dispatch and key-hook is **audited and attributed** to the
  plugin (Integrity Panel).

### What moves, what stays
- **Moves out of core → the paradigm plugin:** the omnibox `[cmd]` mode + the
  `:` command-line; the `mote.mode` NORMAL indicator (the provider drives a
  mode status-line element, ADR-0016; the built-in is delegated or removed);
  `j`/`k` and other motion bindings; the `/`-to-find binding.
- **Stays in core (capabilities):** find-in-page itself (Ctrl+F is universal);
  the browser-command implementations (now exposed to the provider via the
  bounded host-fns); the `url` omnibox (address bar).

### Resolves prior items
- **CL-SPECDRIFT A3/A6** (cmd-prefix `:` vs `>`, sticky-mode/consume-char):
  dissolved — `[cmd]` leaves core, so there is no core prefix to reconcile; the
  vim plugin uses `:` per spec when it ships. `[find]` capability stays core;
  the `/` binding is the plugin's.
- **B1** (the frozen `NORMAL` chip): removed from core built-ins; provided by the
  paradigm plugin.

## Resolved decisions (maintainer, 2026-06-06)

1. **Capability shape:** **split** into `editing-mode:provider` +
   `command:provider` (see "The boundary"), for least privilege — a command
   palette can exist without owning global keys.
2. **Precedence:** **user override > keybind-plugin > core default**, where core
   defaults are the mainstream Firefox/Chrome-compatible suite, plus the tiny
   inviolable safety floor (see above). Supersedes ADR-0012's deferral of this.
3. **`j`/`k` in core today (CL-KBNAV):** **keep** arrows+`j`/`k` as a documented
   v0.1 default, plugin-overridable once the keymap API lands (no vim plugin yet
   to provide them; the Neovim-crowd audience keeps vim nav in the interim).
4. **Timeline:** **pin the boundary now** (this ADR) + two cheap v0.1 cleanups
   (suppress the cosmetic `[cmd]` mode and the hardcoded `NORMAL` chip from
   core); build the full keymap/command API + the `vim` plugin in **Phase 6**.
   The `keys:intercept_input`+exfil `danger` combination entry is cheap security
   hygiene that can land any time.

## Consequences

- **Enables** vim/emacs swappability as the headline persona feature; keeps core
  paradigm-agnostic.
- **New primitives to build** (see the research doc): input→runtime wiring; the
  capability contract; a declarative keymap schema; `mote.command.dispatch` over
  a bounded allowlist; an omnibox-mode primitive; mode-element delegation; the
  content-keystroke-withholding policy; the danger-combination registry entry;
  conflict-resolution.
- **Security posture is explicit and defensible** — observation is bounded by
  the sandbox + no-exfil-without-audited-grant; content input is withheld by
  construction; the broad grant is loud and audited.
- **Cross-references:** ADR-0001 (declarative registration), ADR-0002 (capability
  contracts), ADR-0010 (per-keystroke dispatch + deadline budgeting), ADR-0012
  (this is the deferred plugin-keybind ADR it reserved), ADR-0016 (status-line
  mode element). Feasibility: `docs/research/secure-plugin-command-api-feasibility.md`.
- **Does not ship the vim plugin** — this ADR fixes the boundary + the secure
  API so the plugin (and an emacs alternative) can be built against a stable
  contract.
