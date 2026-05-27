# Mote — Config-Mutation Model (Design)

**Date:** 2026-05-27
**Status:** Approved (brainstorming → design). Spec changes (DESIGN amendment + ADR-0006) authorized by maintainer.
**Context:** Phase 3 (plugin management). Surfaced by `03-risks.md` R3 ("how `mote plugin add` rewrites the `plugins.lua` call") and generalized during brainstorming to the whole question of *any* Mote surface mutating user configuration.

---

## Problem

`plugins.lua` (and the future `init.lua`) is **Lua — a program, not structured data**. The Phase-3 plan (§5.1) and `DESIGN.md` (line 1352) assumed the CLI could "mutate it programmatically by rewriting the call." That assumption is unsound:

- A user may build their plugin set with loops, conditionals, variables, or string concatenation — there is no literal `mote.plugins({...})` call to locate and edit.
- The only fallback ("evaluate, then regenerate the call from the captured table") discards the user's program structure and comments, silently rewriting their authored file into something they did not write.
- Even the simple literal case requires parsing enough Lua to avoid corrupting trailing commas, nested tables, multiline strings, and comments.

The same impossibility applies to a future in-browser **settings GUI** that "writes config": you cannot reliably write back into a program. `DESIGN.md` even cites lazy.nvim as the model — but lazy.nvim does **not** rewrite your Lua; it reads your spec and reconciles, and only the lockfile is machine-generated.

## Principle (load-bearing)

> **The human's authored config (`plugins.lua`, future `init.lua`) is read-only to every Mote surface — CLI and GUI alike. Mote reads it; Mote never modifies it.**

Rationale: *generating* a file Mote owns wholesale is reliable; *rewriting* a human-authored program is not. All Mote-originated state therefore persists to Mote-owned artifacts, never the human's program.

## The machine-managed layer

Two Mote-owned, committable dotfiles (both travel with the user's dotfiles → the user's *full* preferences are portable):

| File | Owner | Form | Role |
|---|---|---|---|
| `~/.config/mote/plugins.lock` | Mote | TOML (data) | resolved commits + checksums; reproducibility anchor (already built, Wave 1) |
| `~/.config/mote/managed.lua` | Mote | generated Lua | `add`-ed plugin declarations now; GUI setting-overrides later (model B) |

### `managed.lua` contract

- **Wholesale-generated** from a structured in-memory model on every mutation, via **atomic temp-write + `rename()`** (no in-place editing; corruption-safe).
- Carries a header: `-- DO NOT EDIT — managed by Mote (see: mote plugin ...)`.
- **Human edits to it are not preserved** — Mote overwrites. Same contract as `Cargo.lock` / `lazy-lock.json` / Emacs `custom-file`.
- **Loaded last** by the config loader (after the human's `plugins.lua` and any per-identity overlay). Because `mote.plugins({...})` is additive and config setters are last-writer-wins, load-order-last yields **"managed layer overrides human config"** for free — no separate merge engine.
- One file (not a `conf.d` directory): one labeled artifact the user can read top-to-bottom, one line in a git diff, no config dir littered with managed files.

## CLI behavior (reconcile, never rewrite)

| Command | Behavior | Writes |
|---|---|---|
| `add <source> [--version]` | fetch + cache + lock + drive approval + activate; record declaration | `managed.lua`, lock, cache, symlink |
| `remove <name>` | drop declaration; **cache retained** | `managed.lua`, lock, symlink |
| `source <name> <new-source>` | re-point source; re-fetch + re-hash + re-link | `managed.lua`, lock, cache, symlink |
| `sync` | reconcile on-disk state to **human config + `managed.lua`** (the fresh-machine command) | cache, symlinks |
| `import <name>` | promote an implicit-local **or** a `managed.lua` entry into the user's *own* config (see migration below); drop it from `managed.lua` | `managed.lua` (removal); human config only via opt-in `--write` |
| `rollback`/`diff`/`gc`/`review`/`pin` | as Phase-3 plan §5; none write human config | lock / cache / approval-store |

`add` no longer requires a paste step: the entry it writes to `managed.lua` is itself committable, so the plugin is reproducible immediately.

### Migration: managed → owned config

Moving an entry out of `managed.lua` into the user's own `plugins.lua` must respect the principle. The §1 line is enforced as: **Mote never *modifies* the human's file; appending a self-contained new statement is the single, opt-in exception** (appending never parses or preserves existing structure, so it cannot corrupt the file).

- **Default (strict):** `mote plugin import <name>` **prints** the exact `mote.plugins({...})` snippet (and copies it to the clipboard if `wl-copy`/`pbcopy`/`xclip` is present), then drops the entry from `managed.lua`. The user pastes it where they want. Trust statement is absolute: *Mote never touches a file you wrote.*
- **Opt-in (`--write`):** `mote plugin import <name> --write` **appends** a complete `mote.plugins({ ["name"] = {...} })` statement to the end of `plugins.lua` and drops it from `managed.lua` — one command, no paste. Guardrails: the file must parse first; append with newline separation; **append-only ever** (never edits existing lines); fall back to print if the file does not parse cleanly.

The reverse (owned → managed) never happens: a plugin declared in the human's `plugins.lua` is treated as the user's; Mote prunes any duplicate from `managed.lua`.

## GUI direction (deferred; reuses the managed layer)

Model **(B) override layer**: the future settings GUI persists toggles into the *same* `managed.lua` mechanism (or a sibling `settings.lua` under the same managed-file contract). Power users hand-write Lua; standard users toggle in the GUI; both compose via load order. This satisfies `DESIGN.md` line 84 (non-programmers configure without writing logic) without violating the principle.

**Deferred sub-questions** (settings phase — *not built now*):
- **Precedence transparency** — when a GUI override shadows a value the human set in Lua, the GUI surfaces "this overrides a value set in your `init.lua:NN`" and is clearable. (Recommended default: override wins, transparently.)
- **Overridable scope** — the GUI overrides *leaf settings* (theme tokens, keybinds, plugin enable/disable, narrowings), never *logic*.

Phase 3 does **not** depend on these: its only config-touching surfaces (approval dialog, integrity-panel actions) persist *approval/lock state*, which is already Mote-owned.

## Risk resolutions

- **R3 (add-rewrite) — deleted.** No Lua rewriting exists anywhere; `managed.lua` is generated wholesale.
- **R4 (key↔name) — resolved.** `plugins.lua` keys must be valid `PluginName`s (`["vim-mode"] = {...}`, quoted because hyphens are illegal in bare Lua keys). `add` generates valid-keyed entries; human keys are validated against the manifest name at sync. This corrects DESIGN's underscore examples (`vim_mode`).

## Spec changes (maintainer-approved)

1. **Amend `DESIGN.md`:**
   - Line 1352: remove "the CLI (`mote plugin add` etc.) can mutate it programmatically by rewriting the call"; replace with the read-only principle + the `managed.lua` mechanism.
   - Lines 1334–1337: change example keys to valid hyphenated `PluginName`s (quoted).
   - Lines 1389–1396 (CLI surface comments): `add`/`remove`/`source`/`import` describe writing `managed.lua` (not `plugins.lua`).
   - Lines 1345/1349: example checksums `sha256:` → `blake3:` (drive-by; matches `mote_types::Checksum`).
2. **New ADR-0006** (`config-is-read-only-to-mote`): records the principle, the managed-layer mechanism, the import migration policy, and the model-B GUI direction.

## Testing implications (for the Phase-3 plan)

- `managed.lua` round-trip: generate → load via `eval_config` → identical typed model; atomic-write leaves no partial file on simulated failure.
- Config loader composes multiple captures: human `plugins.lua` + `managed.lua` + per-identity overlay → additive plugins, last-writer-wins settings.
- `import` default prints exact, re-parseable snippet; `--write` append-only leaves a parseable file and never edits existing lines; refuses (falls back to print) on a target that does not parse.
- `add`/`remove`/`source` mutate only Mote-owned files; a human `plugins.lua` is byte-identical before and after.
