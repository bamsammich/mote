# ADR-0006 — User Config is Read-Only to Mote; Mutations Go to a Managed Layer

- **Status:** Accepted
- **Date:** 2026-05-27

---

## Context and Problem Statement

Mote's user config (`plugins.lua`, future `init.lua`) is Lua — a program, not
structured data. DESIGN.md (line 1352) and the Phase-3 plan assumed the CLI
could "mutate it programmatically by rewriting the call," and a future settings
GUI would likewise write config. Reliably rewriting a Turing-complete program
(loops, variables, comments) is not possible; the only fallback — regenerate the
call from an evaluated table — silently destroys the user's structure and
comments. The same impossibility blocks any GUI that writes config.

## Decision Drivers

- Generating a Mote-owned file wholesale is reliable; rewriting a human-authored
  program is not.
- DESIGN cites lazy.nvim as the model, and lazy.nvim does **not** rewrite user
  Lua — it reconciles and generates only a lockfile.
- Non-programmers must still be able to configure via a GUI (DESIGN line 84)
  without forcing every change into hand-edited Lua.
- Portability: the user's full preferences should travel with their dotfiles.

## Considered Options

- **Rewrite the user's `plugins.lua`** (span-edit literal calls; regenerate as fallback).
- **Reconcile-only with a Mote-owned managed layer** (CLI/GUI never write user config).
- **Drop programmatic config entirely** (hand-edit + `sync` only; no `add`/GUI writes).

## Decision Outcome

Chosen: **reconcile-only with a Mote-owned managed layer.** The human's authored
config is read-only to every Mote surface; all Mote-originated state persists to
Mote-owned, committable artifacts — `plugins.lock` (data) and `managed.lua`
(generated Lua, loaded last so it composes additively with and overrides user
config). The CLI installs/records into `managed.lua`; the future settings GUI
(model B) reuses the same managed layer. `mote plugin import` migrates an entry
into the user's own config by printing the snippet (default) or, with `--write`,
appending it — appending being the single, reliability-safe exception, since it
never parses or preserves existing content.

### Consequences

- Good, because the trust statement is near-absolute: Mote never modifies a file
  the user authored (the only write is opt-in, append-only `import --write`).
- Good, because `managed.lua` + `plugins.lock` make the full plugin/preference
  set portable across machines, and the GUI (model B) gets a mechanism for free.
- Bad, because the user's full effective config now spans two files (their own +
  `managed.lua`); the managed file carries a "DO NOT EDIT" contract.
- Bad, because `managed.lua` is generated Lua (machine-written code), tolerated
  only because Mote owns it wholesale and never preserves human edits to it.

## Notes

- Resolves `03-risks.md` R3 (deletes the rewrite problem) and R4 (`plugins.lua`
  keys are valid hyphenated `PluginName`s, quoted).
- Deferred to the settings phase: GUI override precedence-transparency and the
  overridable-leaf-vs-logic scope. Phase 3 does not depend on these.
