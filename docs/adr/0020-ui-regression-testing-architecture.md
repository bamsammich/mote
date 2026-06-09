# ADR-0020 — UI Regression-Testing Architecture: Three Lanes, Shared Golden-Fixture Contract, Snapshot-Review Forcing Function

- **Status:** Accepted (approved by the maintainer 2026-06-08)
- **Date:** 2026-06-08

---

## Context and Problem Statement

Mote has no programmatic UI regression coverage; bugs are found by hand
(screenshots + pixel math). A single session surfaced 5 bugs that way, 4 of them
at integration/native seams. We need a hand-written regression suite — a test for
every UI bug found — that is git-hook-enforced and, on failure, **forces a
conscious choice: change the UI intentionally, or fix the code.**

## Decision Drivers

- Catch a regression **at the commit that introduces it** (the maintainer commits
  to `main` constantly, pushes in batches) — so the heavy enforcement must be
  **pre-commit-capable**.
- The worst bugs live at **shell↔chrome↔compositor seams** — tests must exercise
  the real thing there, not only isolated units.
- A failing test must make "intended change vs. bug" an explicit decision.
- The shell-side contract and the chrome-side consumer must not **drift** (tested
  separately today → both green, prod red).

## Considered Options

- **Single unified real-browser-over-CDP harness** for everything — max fidelity,
  but loses fast pre-commit feedback. Rejected.
- **happy-dom component tests** — fast, but no real CSS/layout, so visual/theming
  regressions slip through. Rejected for the component lane.
- **Three lanes + a fast pure-logic shim, bound by a shared golden-fixture
  contract** (this ADR). Chosen.

## Decision Outcome

**Three lanes + a pure-logic shim:**

1. **Chrome component (real Chromium, headless — Playwright):** renders each
   chrome renderer in a real headless browser against an injected **shared
   fixture** + stubbed `window.mote.invoke`; asserts DOM + op-calls **and** takes
   **per-theme (dusk/vellum) visual snapshots**. → **pre-commit** (headless, no
   display, seconds).
2. **Rust (`insta`):** snapshots the payload structs **and emits the golden
   `__fixtures__/*.json`** the component lane consumes. → fast subset pre-commit;
   full suite pre-push.
3. **Full-app E2E (Playwright over CDP into CEF, per ADR-0021):** drives the real
   app for integration + native-seam bugs. → **pre-push**; a **glob-scoped
   pre-commit** variant under headless Xvfb fires only on commits touching
   resize/compositor/nav code; native bugs also get pure-logic **pre-commit
   proxies** where one exists.
4. **Pure-logic JS shim (node, no browser):** classifiers / nav-math →
   **pre-commit** (milliseconds).

**Binding decisions:**

- **Shared golden-fixture contract:** Lane 2 serializes the *real* Rust payloads
  to checked-in `__fixtures__/*.json`; Lane 1 consumes those same files. One
  source of truth — a field rename breaks both lanes. (Closes the drift seam.)
- **Snapshot-review is the forcing function:** every test pairs **explicit
  assertions** (intent) with a **secondary** snapshot (drift net) — never
  snapshot-only. Hooks run `--no-update`; accepting a change is a deliberate
  `vitest -u` / `cargo insta review`. `.snap` diffs are code-reviewed.
- **`__MOTE_TEST__`-guarded test exports** in chrome JS (stricter than
  `panels.js`'s current unconditional exports — those get brought under the
  guard).
- **Node tooling** (Playwright + `node_modules` scoped to `crates/mote-ui/`, node
  pinned to a concrete LTS, invoked via `mise exec`); a **missing-deps state
  fails loud, never skips**.
- **Found bugs land as RED/quarantined tests**, fixed as follow-ups (each fix
  flips one test).

## Consequences

- **Good:** regressions caught at commit time; visual/theming covered by real
  rendering; the contract can't drift silently; the forcing function is
  structural (a stale snapshot fails the gate).
- **Cost:** introduces a JS toolchain (`node_modules`) into the Rust repo
  (scoped, pinned, lockfile committed); the full-app E2E tier is heavier
  (pre-push / glob-scoped pre-commit under Xvfb).
- **Bounded:** this ADR is the testing architecture; the CDP enablement it
  depends on is ADR-0021. CI is deferred (lefthook-only for now).

## Relationship to Existing ADRs

- **ADR-0021** is the enabler of Lane 3 (the test-mode CDP surface).
- **ADR-0003 / ADR-0005:** the component lane exercises the same structured-DOM,
  no-`innerHTML` boundary those ADRs require (existing grep tests still apply over
  the guarded exports).
