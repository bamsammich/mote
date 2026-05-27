# ADR-0008 — Approval Carve-Outs: Bundled and Dev-Mode Plugins Auto-Approve

- **Status:** Accepted
- **Date:** 2026-05-27

---

## Context and Problem Statement

ADR-0001's four-step load sequence ends in permission approval, and DISCIPLINES
§9 says the install dialog fires "on first detection of any plugin, declared or
implicit local." Phase 3's loader must decide whether *every* provenance class
truly passes through that dialog. Two classes are special: `bundled` plugins
(compiled into the Mote release binary) and `dev-mode` plugins (a directory the
developer explicitly marked for development).

## Decision Drivers

- A bundled plugin ships inside the binary the user already chose to run; an
  approval dialog for it adds no real security (a compromised binary could lie),
  and prompting for the default plugins on every fresh profile is poor UX
  (DESIGN: "functional from first launch").
- Dev-mode exists precisely so a developer is not re-prompted on every edit
  (DISCIPLINES §9), and it is per-plugin/per-directory opt-in — never a global
  toggle and never a production default.
- Transparency must be preserved without a blocking dialog.

## Considered Options

- **Auto-approve bundled + dev-mode; dialog for all other provenances** (this ADR).
- **Prompt every provenance once**, including bundled (strict ADR-0001 reading).
- **Auto-approve bundled only**, still prompt dev-mode.

## Decision Outcome

Chosen: **bundled and dev-mode plugins auto-approve at step 4; `DeclaredGit`,
`Path`, and `ImplicitLocal` always go through the approval dialog.** Bundled is
trusted by construction (in the binary, BLAKE3-dirhash-covered, auditable at
release). Dev-mode is an explicit per-directory developer opt-in with no global
toggle. Transparency for auto-approved plugins is provided by the always-
available integrity panel (which shows their effective permissions), not a
blocking dialog.

### Consequences

- Good, because a fresh Mote is usable immediately without prompting for its own
  default plugins, and the dev edit loop is not interrupted.
- Good, because the security-relevant prompt is reserved for code whose trust is
  *not* already established (third-party Git, arbitrary local paths, ad-hoc drops).
- Bad, because it is a carve-out from ADR-0001's "all plugins approve" sequence;
  the carve-out set must stay small and explicit (only these two classes).
- Bad, because dev-mode auto-approval trusts locally-modifiable code; this is
  bounded by dev-mode being explicit per-directory opt-in (never global, never a
  production build default).

## Relationship to ADR-0001

Refines, does not supersede, ADR-0001. The four-step load sequence stands; this
ADR records that step 4 (approval) is satisfied automatically for the `bundled`
and `dev-mode` provenance classes, and that the integrity panel is the
transparency surface for them. The `ApprovalStore` still records the approved
`ApprovalHash` for auto-approved plugins so a later permission *expansion* is
still detected.
