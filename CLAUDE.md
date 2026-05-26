# CLAUDE.md — Mote

Project context and the operating contract for working in this repository.
Read `DESIGN.md` (architecture), `DISCIPLINES.md` (the rules that keep the
architecture honest), and `ROADMAP.md` (what ships when) before implementing
anything non-trivial.

## ALWAYS reference the design docs when implementing code

**Every code change must be checked against `DESIGN.md` and `DISCIPLINES.md`.**
These are the source of truth for the architecture and its operational
guardrails — not optional background reading. Before writing or modifying code:

- Confirm the change matches the relevant section of `DESIGN.md` (security
  model, dispatch contracts, registries, state model, etc.). Code conforms to
  the design; the design does not bend to make code easier.
- Check it against `DISCIPLINES.md` — the temptation/discipline/mechanism
  entries exist precisely to catch shortcuts made under pressure (CEF wrapper
  isolation, schema versioning, transparency defaults, plugin approval boundary,
  …). If a change would violate a discipline, find another way.

## ALWAYS use the design skills for ANY frontend work

For **any** frontend work — chrome, components, themes, slots/elements, or any
visual artifact (mocks, prototypes, slides) — these two skills are the **source
of truth for Mote's frontend branding and design** and must be invoked:

- **`/mote-design`** — Mote's design system and UI implementation guide
  (slots, elements, themes, branding).
- **`/frontend-design:frontend-design`** — production-grade, distinctive
  frontend quality.

Invoke both before touching any frontend surface. Do not improvise Mote's visual
language; it lives in these skills.

## Toolchain: everything runs through mise

All project tooling is pinned in `mise.toml` and locked in `mise.lock`. **Do not
rely on system-installed Rust or globally installed tools.** Always invoke
managed tools through mise so you get the exact pinned versions:

```
mise exec -- <tool command>
```

Examples:

| Task              | Command                                                            |
| ----------------- | ------------------------------------------------------------------ |
| Build             | `mise exec -- cargo build`                                         |
| Test              | `mise exec -- cargo test --workspace --all-features`               |
| Lint (as errors)  | `mise exec -- cargo clippy --all-targets --all-features --workspace -- -D warnings` |
| Format code       | `mise exec -- cargo fmt --all`                                     |
| Check formatting  | `mise exec -- cargo fmt --all --check`                             |
| Format TOML       | `mise exec -- taplo fmt`                                           |
| Spell check       | `mise exec -- typos`                                               |
| Install git hooks | `mise exec -- lefthook install`                                    |

Pinned tools (`mise.toml`): **rust** (with `clippy` + `rustfmt`), **lefthook**,
**taplo**, **typos**. Add a tool by editing `mise.toml`, then run
`mise install`; the resolved version is written to `mise.lock` — commit both.

## Quality gates

Quality is enforced by code, not convention. The same checks run locally
(lefthook) and should run in CI.

- **Lint policy lives in one place.** The root `Cargo.toml` `[workspace.lints]`
  table is the single source of truth: clippy `all` + `pedantic` + `nursery` +
  `cargo` groups at `warn`, plus rustc lints (`unsafe_code = "deny"`,
  `missing_docs`, `missing_debug_implementations`, `unreachable_pub`, …). Every
  crate opts in with `[lints] workspace = true`. **Do not** scatter
  `#![allow(...)]` across crates; if a lint genuinely needs relaxing, change the
  workspace policy with a comment explaining why.
- **Warnings are errors at the gate.** The policy uses `warn` so local
  `cargo build` stays readable, but pre-commit and CI run clippy with
  `-D warnings`. Code must be warning-clean to commit.
- **`unsafe_code` is denied workspace-wide.** A crate that legitimately needs it
  (e.g. CEF FFI in the future `mote-cef` crate — see `DISCIPLINES.md` §1)
  re-enables it locally with a justifying comment, and isolates it there.

## Git hooks (lefthook)

Configured in `lefthook.yml`. Install once per clone:

```
mise exec -- lefthook install
```

- **pre-commit** (parallel): `cargo fmt --check`, `cargo clippy -D warnings`,
  `taplo fmt --check`, `typos`.
- **pre-push**: `cargo test --workspace --all-features`.

Hooks invoke every tool via `mise exec --`, so they use pinned versions
regardless of PATH.

## Conventions

- **Edition 2024**, MSRV **1.95.0** (set in `[workspace.package]`; inherit with
  `edition.workspace = true` etc. in member crates).
- Crates live under `crates/`; the workspace uses `members = ["crates/*"]`, so a
  new crate joins automatically.
- New crates inherit shared metadata and lint policy:
  ```toml
  [package]
  name = "mote-<thing>"
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true
  repository.workspace = true
  homepage.workspace = true
  authors.workspace = true

  [lints]
  workspace = true
  ```
- Shared dependency versions go in `[workspace.dependencies]` and are referenced
  with `dep.workspace = true`.

## Current state

Phase 1 (plugin runtime foundation) is underway — see ROADMAP.md and GitHub
issue #1. The workspace is scaffolded with the full v0.1 crate topology under
`crates/` (per `docs/plans/00-master-plan.md`); `mote-types` (shared
vocabulary) is implemented. Architecture decisions of record live in
`docs/adr/`. The disposable `mote-placeholder` scaffold has been removed.
