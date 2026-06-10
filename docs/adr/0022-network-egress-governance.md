# ADR-0022 — Network Egress Governance: In-Process No-MITM Proxy Chokepoint, Host-Granular Default-Deny, Plugin-Mediated Egress

- **Status:** Accepted (approved by the maintainer 2026-06-10)
- **Date:** 2026-06-09

---

## Context and Problem Statement

DESIGN.md Principle 9 (`DESIGN.md:37`) commits Mote to *"no user data leaves the machine
without explicit per-event consent; no continuous monitoring, no background analytics, no
opt-out telemetry,"* and the transparency thesis (`DESIGN.md:11`, `:1709`) to *"making
visible what other browsers leave in shadow."* An empirical NetLog capture of **idle Mote**
(method + evidence in [`docs/research/cl-xparency-h1-browser-outbound-feasibility.md`](../research/cl-xparency-h1-browser-outbound-feasibility.md))
showed the opposite is currently true: with no user action Mote phones home to Google on ~14
connections (component updater + `*.gvt1.com` downloads, `safebrowsing`, Gaia
`accounts.google.com/ListAccounts`, network time, translate-ranker, default-search preconnect).
Separately, Safe Browsing was proven **inert** (a known-malware test URL loaded, zero
`fullHashes` confirmation, no interstitial) — so Mote is simultaneously *noisy and unprotected,
and silent about both.*

The first design attempt — **suppress-and-attest** (disable the known background services via
command-line switches, then attest what we disabled) — was rejected by the maintainer as a
**denylist**: it is brittle against an upstream we do not control. A future Chromium can add a
new background service behind a flag we have never heard of, and our attestation silently
becomes false. We cannot vouch for code we cannot see (an extension of the DISCIPLINES §1 "wrap
and contain CEF, don't trust it broadly" principle to CEF's *network behavior*).

This ADR decides the robust alternative: **govern egress at a Mote-owned chokepoint with a
default-deny allowlist driven by Mote's own authorizations, and route every legitimate egress
through transparent, user-controllable plugin capabilities.** It supersedes the original "H1
Wave 2 / surface the browser's own outbound as audit rows" framing, which was refuted as
infeasible (Chromium browser-internal traffic does not reach per-browser CEF handlers, so it
cannot be *observed* in-CEF without fabrication — and the honest realization of that intent is
to *deny* what shouldn't happen, not to narrate it).

## Decision Drivers

- **Principle 9 is a commitment** (`DESIGN.md:37`) — the measured background phone-home violates
  it and must be closed *robustly*, not enumerated against.
- **Robustness over enumeration** — a *new* upstream phone-home must be denied by construction,
  not require us to discover and blocklist it.
- **Zero browsing friction** — the authorization model is make-or-break for UX: legitimate
  browsing (page loads, cross-origin API calls, CDN fetches, dynamic JS `fetch`/XHR) must work
  seamlessly, with no prompts and no perceptible latency.
- **Never MITM** — host/CONNECT-level visibility only, permanently. Decrypting the user's own
  TLS is antithetical to the privacy *and* transparency Mote is built on. This is a firm
  principle, not a default.
- **Honesty** (DISCIPLINES §8 Honest positioning; §5 enumerate-don't-overclaim) — disclose
  exactly what is denied, allowed, and residual; never claim "zero outbound."
- **Don't trade away real security blindly** — security-relevant phone-home (CRLSet revocation)
  is not blanket-disabled (same reasoning that kept Safe Browsing from a blind drop).
- **CEF containment** (DISCIPLINES §1) — all CEF interaction stays in `mote-cef`.

## Considered Options

- **(a) Accept Chromium defaults** — *Rejected:* measured, undisclosed Principle-9 violation.
- **(b) Suppress-and-attest (switch denylist + attestation)** — *Rejected:* brittle against
  upstream change; a new phone-home silently breaks the attestation.
- **(c) MITM observation/control** — *Rejected, permanently:* decrypting user TLS violates the
  privacy and transparency thesis.
- **(d) Per-connection-attributed default-deny** (per-context proxy or per-request capability
  tokens, so the proxy ties each connection to its initiator) — the *ideal* (would close the
  host-coincidence leak), but **verified infeasible on CEF 148's Chrome runtime**:
  `set_preference("proxy")` returns `rc=0` (per-context proxy rejected), and per-request
  proxy-auth tokens are defeated by Chromium's proxy-credential caching. Deferred to a future/
  upstream-CEF item.
- **(e) Host-granular default-deny + switch-suppression + plugin-mediated egress + disclosure**
  — **Chosen.** Robust by construction, zero-friction, feasible today.

## Decision Outcome

### The chokepoint

An **in-process, no-MITM, loopback forward proxy** (new crate, e.g. `mote-netgov`; a `tokio`
listener in the browser process — *in-process is required*, not merely simpler: it is what makes
zero-race authorization tractable, below). All CEF egress is routed through it via
`--proxy-server`. Verified: CEF's network service **does** route its background traffic through
`--proxy-server` (the spike saw `update.googleapis.com`, `*.gvt1.com`, Gaia, etc. at the proxy),
with **zero direct/bypass connections** (NetLog: 53× proxy-resolution, 0× DIRECT; `ss`: no
direct sockets). For HTTPS the proxy tunnels opaque bytes and logs only the CONNECT host.

**Mandatory config corollaries (verified/required):** `--disable-quic` (HTTP/3-over-UDP would
bypass an HTTP proxy) and **secure-DNS/DoH disabled** (so DNS resolves at the proxy and can't
bypass it).

### The allowlist (the discriminator)

Default-deny. The allowlist = **`(hosts authorized by Layer-1 page loads)` ∪ `(plugin `net:`
grants)`**. The discriminating principle:

> Egress that did **not** come through a per-browser CEF resource handler is, by definition, not
> user-page-content → deny it unless a plugin was granted it.

- **Page content** (navigations, subframes, scripts, CSS, images, fonts, **XHR/`fetch`** to
  cross-origin API servers, CDN loads) fires `on_before_resource_load` with frame context →
  Mote authorizes those hosts to the proxy. This covers modern web apps with no friction (a
  `docs.google.com` calling `apis.google.com` + a CDN is the *normal* case, all auto-authorized).
- **Background phone-home** bypasses per-browser handlers (proven) → never authorized → denied.
  A *new* background service to a *new* host is therefore denied **by construction** — the
  robustness requirement.
- **Plugin egress** → host-mediated `mote.net`, `net:`-permission-gated, audited (→ ADR-0023).

### Zero-race authorization (the make-or-break)

`on_before_resource_load` is a **gate**: CEF makes no connection until the callback returns
CONTINUE. Mote authorizes the host **synchronously, before returning CONTINUE**, into a
**shared in-process allow-set** (concurrent map / `RwLock`). Because the proxy is in-process,
the proxy thread reads that set with zero IPC latency and guaranteed memory visibility, and the
write *happens-before* the connection is ever initiated. No race, by construction. (A subprocess
proxy would reintroduce this race — hence in-process.)

### Switch-suppression layer (defense-in-depth)

In-process command-line switches via `BridgeApp::on_before_command_line_processing` (cef-rs 148
exposes `ImplApp::on_before_command_line_processing` + `ImplCommandLine::append_switch`; the
switches verifiably silence the heavy chatter). This is **not the security boundary** — the
default-deny proxy is — but it (a) reduces denied-traffic load, and (b) **closes the
host-coincidence residual** by disabling the services that use page-common hosts (Gaia/account
checks, Google as Chromium's internal default-search provider). Plus the mandatory
`--disable-quic` / secure-DNS-off. Security-relevant component channels (**CRLSet** revocation)
are **kept** (not blanket `--disable-component-update`) and disclosed.

### Fail-closed, engineered to never be reached

The posture is **fail-closed** (can't decide → deny). But a *user-facing* fail-closed event
would be a bug, not a behavior: the only thing that ever hits "deny" is the background egress we
want denied. Guaranteed by: **completeness** (the resource handler fires for all page-driven
requests), **race-freedom** (sync authorize-before-continue), **liveness** (O(1) in-memory
policy check; panic-isolated, supervised proxy thread; kernel TCP splice after the check), and
an **observe-then-enforce rollout** — the proxy ships allow-all + log first; enforcement is
flipped on only once logs prove zero legitimate requests would have been denied. Enforcement is
gated on evidence, not faith.

### The residual, disclosed

Per-connection attribution being infeasible (option (d)), the allowlist is host-granular, so a
*future* background service reusing a host a page is concurrently using would pass. This residual
is **narrow** (the measured background services use dedicated infra hosts pages never load),
**mitigated** by the switch layer (the page-host-colliding services are disabled), and
**disclosed** on the transparency surface. Airtight per-connection closure is tracked as a
future/upstream-CEF item.

### Audit / observability

Every egress decision (allow/deny, host, actor = tab|plugin) flows into `mote-audit` → the
integrity panel's network section becomes **true by construction** (not an attestation), and a
future **MCP-queryable egress ledger** for agent sessions (Phase 8). This is where the audit
*actor* lands honestly: the real tab/plugin observed at the proxy, not a synthetic "browser."
The observability/control surface is gated **exactly like the CDP surface (ADR-0021)**:
off-by-default, loopback, never plugin-reachable; read-only view in production, control
(allowlist mutation, record/replay) is dev/test-only.

### Scope and non-goals

- This ADR pins the **governance architecture**. The plugin network capability is **ADR-0023**;
  **URL-safety as a first-party local-feed plugin** (the committed replacement for inert SB) is
  **ADR-0024**.
- It does **not** add MITM, does **not** attempt per-connection attribution (verified
  infeasible), and does **not** claim "zero outbound."

### Build sequence (blast-radius ascending)

1. Switches + `--disable-quic` + secure-DNS-off + de-Google default-search (cheap, in-process).
2. In-process proxy in **observe-only** mode (route all egress, allow-all + log) — productizes
   the spike; makes the audit surface real with zero enforcement risk.
3. Host-granular **default-deny** enforcement (Layer-1 authorization feed + plugin grants),
   behind a flag, flipped on per the observe-then-enforce gate.
4. `mote.net` plugin capability (ADR-0023).
5. Audit-surface integration (egress events → integrity panel + ledger).
6. First-party plugins: URL-safety (ADR-0024); cert-revocation (independent/best-effort).

### Consequences

- **Good:** closes a measured Principle-9 violation robustly (new phone-home → denied by
  construction); zero browsing friction; transparency surface true-by-construction; keeps CRLSet
  revocation; never decrypts user TLS.
- **Cost:** Translate/Cast/spellcheck-auto-fetch and similar convenience features off by default
  (disclosed; re-enableable later as deliberate choices). A proxy on the egress path (negligible
  latency: O(1) check + kernel splice + loopback).
- **Residual (disclosed):** host-granular allow can't close the same-host coincidence leak on
  CEF 148; mitigated by the switch layer; airtight closure deferred to upstream CEF.

### Resolutions from adr-review (2026-06-09)

- **Audit actor vs the `AuditEvent` schema:** egress events use a **new additive `Actor` enum**
  in `mote-audit` (`Plugin(PluginName) | Tab(TabId)`), *not* a repurposing of the existing
  `plugin` field (DISCIPLINES §2 — additive, not redefinition). The proxy→audit write is
  **non-blocking** (the existing fire-and-forget `Producer::record`), so nothing stalls the
  egress hot path (`DESIGN.md:154`).
- **Switch-layer completeness — honesty fix:** the **de-Google internal-default-search mechanism
  is NOT yet verified** (CEF 148's chrome runtime is finicky about per-context prefs, as the
  per-context-proxy spike showed). The switch layer therefore *narrows*, not provably *closes*,
  the Gaia/default-search host-coincidence; **if that lever proves unavailable, the residual
  disclosure widens accordingly.** To verify at implementation (own ADR if non-trivial).
- **Observability/control gating = tested invariants, not prose:** mirror ADR-0021 — tests
  assert (a) the control plane (allowlist mutation, record/replay) is unreachable from the plugin
  API surface, and (b) no control plane is enabled in a default production run.
- **`mote-netgov` crate topology:** the proxy + policy engine live in a new `mote-netgov` crate
  that **depends on `mote-cef` (never vice-versa)** and is driven by `mote-shell`; it pulls `cef`
  only transitively via `mote-cef` (DISCIPLINES §1).
- **`net:` namespace (binding constraint on ADR-0023):** the plugin egress grant is a **new
  additive permission** (e.g. `net:egress:<host-glob>`), distinct from the existing
  `net:intercept_request`/`fetch_unsigned` domain (`DESIGN.md:308`); DISCIPLINES §2 forbids
  broadening an existing permission's surface.

### Open sub-decisions (flagged for maintainer)

1. **Authorization lifetime/granularity** — host valid while a referencing page/context is alive
   (lean) vs. time-windowed vs. ref-counted.
2. **Preconnect/speculative connections** — authorize the signal to preserve page-load speed
   (lean) vs. fail-soft to connect-on-demand.
3. **Exact Bucket-1 switch set + the de-Google-default-search mechanism** (verified the chrome
   runtime is finicky about per-context prefs; the default-search lever needs its own
   confirmation).
4. **Roadmap placement** — this ADR slots as a foundational phase **preceding Phase 6** (adblock
   needs `mote.net` + the page-lane veto hook) and **feeding Phase 8** (agent egress ledger).

## Relationship to Existing ADRs

- **ADR-0005 (host-bridge two-layer isolation):** the audit/attestation surface reuses the
  existing privileged integrity/settings chrome path — **no new bridge boundary or origin.**
- **ADR-0010 (collector dispatch / audit machinery):** egress events are a new actor stream into
  the same audit substrate — complementary, not conflicting.
- **ADR-0018 (omnibox URL-vs-search):** Mote's *omnibox* search engine (user-configured) is
  distinct from Chromium's *internal default-search provider*; this ADR removes the latter's
  Google default (the Gaia/preconnect source) without touching the former.
- **ADR-0021 (test-mode CDP):** the egress observability/control surface inherits 0021's
  gating discipline (off-by-default, loopback, never plugin-reachable). The NetLog/`ss`
  verification used to validate this ADR is dev/test; the governance posture is production.
- **ADR-0023 (`mote.net` plugin network capability — forthcoming)** and **ADR-0024 (URL-safety
  first-party plugin — forthcoming):** the consumers this ADR's substrate enables.
