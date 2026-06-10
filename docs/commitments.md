# Mote — Our Security, Privacy & Transparency Commitments

> **Living document.** This states what Mote commits to, how we uphold each
> commitment, and — honestly — what is in place **today** versus still being built.
> It is kept current as Mote evolves (see the changelog at the end). We hold
> ourselves to the same standard we hold the web to: **we tell you what is actually
> true, not what sounds good.**

Mote is built on one idea: a browser should make visible what others leave in
shadow, and should never act against you in the dark. These commitments are the
concrete form of that idea.

## Our commitments

1. **Your data does not leave your machine without your consent.** No background
   telemetry, no opt-out analytics, no silent phone-home. (DESIGN.md Principle 9.)
2. **We never decrypt your traffic.** Mote will not man-in-the-middle your encrypted
   connections — not for features, not for "security," not ever. To govern network
   access we can see *which host* a connection goes to; we never see its contents.
3. **Transparency by construction, not by promise.** What Mote and its plugins do is
   shown to you from the real record — the integrity panel reflects actual activity,
   not a hand-written claim.
4. **Capabilities, not trust.** Every plugin's power is an explicit, permissioned,
   audited capability you grant — and can inspect, disable, or replace. No actor is
   exempt.
5. **We are honest about our limits.** We enumerate exactly what we protect and what
   we don't (e.g. `docs/identity-isolation.md`). We never say "fully private" when we
   mean "private across this specific list."

## How we uphold them — and where each stands today

Status is marked honestly: **✅ In place · 🚧 In progress · 🔭 Planned.**

### Network: no silent phone-home  🚧 In progress
We are building **network egress governance** (`docs/adr/0022-network-egress-governance.md`):
all of the browser's network traffic flows through a Mote-owned, in-process checkpoint
that **denies by default** and only allows (a) the sites you actually visit and (b)
destinations a plugin you installed was explicitly granted. A *new* hidden phone-home is
blocked by construction — not by us having to discover it first.

**Honest current state:** the Chromium engine Mote embeds *does* make background requests
today (software-component update checks and similar). We measured this ourselves, and this
work exists to close it. We would rather tell you that than pretend otherwise.

### We never MITM your traffic  ✅ Commitment, enforced by design
The egress checkpoint above tunnels your encrypted connections opaquely — it reads the
destination host, never the contents. This is a permanent design rule, not a setting.

### URL safety, the transparent way  🔭 Planned
Mainstream browsers protect you from malicious sites by sending data to a third party
(e.g. Google Safe Browsing). We are instead building URL safety as a **transparent, local,
replaceable plugin** that uses open threat lists: checks happen on your machine, nothing
about your browsing is sent away, and you can see exactly what it does or swap it out.
**Honest current state:** Mote does **not** yet warn you about malicious or phishing URLs —
until this ships, assume Mote provides no malware/phishing blocking.

### Permission & audit model  ✅ In place
Plugins run sandboxed and can only do what you have granted. Privileged calls are recorded
to an audit log and surfaced in the integrity panel, so you can see what a plugin *actually
did* — not just what it declared it could do. (Network activity joins this record as the
egress work above lands.)

### Identity isolation  ✅ In place, with enumerated limits
Separate identities keep cookies, storage, history, and disk cache apart. We document the
exact isolation boundary — and what it does *not* cover — in `docs/identity-isolation.md`,
rather than claiming blanket isolation.

### Secrets  ✅ In place
Secrets a plugin uses are resolved through gated, per-secret grants; values are held as
in-memory secrets, never broadcast to other plugins, and every access is auditable.

## How to hold us accountable
- **The integrity panel** (in-app) shows live plugin activity — and, as the egress work
  lands, your network activity — from the real record, not a written promise.
- **This repository is open.** Every architectural decision is recorded as an ADR
  (`docs/adr/`), and the evidence behind security/privacy claims is kept in
  `docs/research/`. When we say we verified something, the verification is there to read.
- **If a claim here ever fails to match reality, that is a bug in this document** — please
  tell us, and we will fix the document or the behavior.

## Changelog
- **2026-06-10** — Initial version. Network egress governance approved (ADR-0022) and
  underway; honest disclosure that the embedded engine currently makes background requests
  we are closing; URL-safety planned as a transparent local plugin.
