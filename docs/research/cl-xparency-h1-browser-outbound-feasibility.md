# CL-XPARENCY-DATA H1 Wave 2 — "browser's own outbound" feasibility

- **Date:** 2026-06-09
- **Project:** Mote (AI-native browser)
- **Context:** UI-polish phase, CL-XPARENCY-DATA cluster. Wave 1 (H1-allow/H4/H14/H16/G7)
  is closed. This investigates **H1 Wave 2** — surfacing the *browser's own* outbound
  network requests as audit rows (`actor="browser"`), per the DESIGN.md:1711 integrity
  mockup (`browser itself: 142 requests to telemetry.example.com (explain | block)`).
- **Status:** Feasibility established; **decision pending from maintainer** (which honest
  scope to pursue). No code written.

## Verdict: FEASIBLE BUT NARROW — the literal feature is not honestly buildable

The mockup's "browser itself made N requests to <telemetry host>" row **cannot be populated
truthfully** with Mote's current (or per-browser CEF) plumbing. Building it anyway would
fabricate the flagship transparency surface — forbidden by DISCIPLINES §5 (honesty / no
overclaim) and the project thesis.

### Why (the load-bearing finding)

Chromium's truly browser-internal outbound traffic — component updater, SafeBrowsing
real-time checks, variations/telemetry (`variations.googleapis.com`), OCSP/CRL, DNS-over-HTTPS,
GCM/push — is issued by **browser-process / network-service background services**, independent
of any `CefBrowser`. CEF's per-browser `CefRequestHandler` / `CefResourceRequestHandler`
callbacks fire only for requests **tied to a `CefBrowser`+`CefFrame`** (page content). There
is no `CefClient`-level global network tap; the request-context-level handler
(`CefRequestContextHandler`) is broader but still does not surface the truly-internal
background services. *(Standard CEF/Chromium model; the exact set of services active in
CEF 148 should be confirmed empirically — see Option B.)*

## Repo facts (cited)

- **CEF crate/version:** `cef = "148.2.0"` (`Cargo.toml:32`) — `cef-rs`, Chromium 148.
- **Network handlers wired** (all in `crates/mote-cef/src/ffi.rs`):
  - `on_before_browse` (`:1087`) — top-level nav; used only for the S1 `mote://` guard.
  - `resource_request_handler` (`:1107`) — per-request handler factory; discards
    `_request_initiator` (origin).
  - `on_before_resource_load` (`:1041`) — per-resource; calls `interceptor.on_before_request`.
- **`RequestInfo` captures** url/method/is_navigation/is_download (`crates/mote-cef/src/interceptor.rs:26-35`).
  **Discards** (all available in the params already passed): frame identity,
  `CefRequest::GetResourceType()` (RT_MAIN_FRAME/RT_FAVICON/RT_XHR/…),
  `GetTransitionType()`, request-initiator origin. No `on_resource_load_complete`.
- **No background-chatter suppression:** `crates/mote-cef/src/engine.rs` `Engine::init`
  injects **no `--disable-*` switches**, registers **no `BrowserProcessHandler`**, no
  `on_before_command_line_processing` (comment `:102-103` confirms "none are injected").
  → Chromium's background services are not suppressed.
- **No plugin HTTP path:** `http:fetch` permission exists (`crates/mote-registry/data/permissions/v1.toml:374-378`)
  but has **no host-API implementation** (`mote-lua` has no fetch). "plugin-caused outbound"
  is currently a non-category.

## Audit model (data side is NOT the bottleneck)

- `AuditEvent` (`crates/mote-audit/src/event.rs:41-78`) has `plugin: PluginName` — **no actor
  concept**; `AuditEvent::new` requires a `PluginName`. Adding a real browser actor cleanly =
  introduce an `Actor` enum (`Plugin(PluginName) | Browser`) at the event level + update
  `query.rs` (recent_for_plugin / *_counts_per_plugin) + `dispatch/src/audit.rs`
  (`ChainStep.performer`) + `mote-shell/src/runtime.rs` `build_panel` aggregation.
- **View-model already supports it:** `AuditRow.actor` is already `String`
  (`crates/mote-ui/src/integrity.rs:294`); sample data already has `actor:"browser"` (`:572`);
  `panels.js buildAuditSummary` reads `r.actor` as a plain string (`:509`). **No UI change.**

## Design/discipline constraints

- **DESIGN.md:37 (Principle 9):** no background analytics / no opt-out telemetry — the browser
  should be **silent**. The mockup row illustrates the *capability*, not an expected event.
- **DESIGN.md:1709-1721:** the "browser itself" audit row mockup; body text (:1721) defines the
  log as *permission calls* (plugin actions) — the gap an ADR must close.
- **DESIGN.md:150-151:** lock-free crossbeam→ring→SQLite permission audit log.
- **DISCIPLINES §1:** all CEF instrumentation must live in `mote-cef`.
- **DISCIPLINES §5:** don't overclaim what's captured — enumerate observable vs not.
- **DISCIPLINES §6:** data-persisting audit features need integrity-panel clear/disable.

## The honest options (decision pending)

- **A — Observe-what-we-can (page-traffic transparency).** Capture the attribution already
  flowing through `on_before_resource_load` (resource type, initiator origin, frame), surface
  real per-origin/per-tab network activity. Real data, feasible now. Reframes H1-W2 from
  "browser's own outbound" → "all observable network activity, honestly attributed."
- **B — Suppress-and-attest (the honest browser actor).** Bring CEF in line with Principle 9:
  inject `--disable-*` switches to kill Chromium background phone-home; surface the **posture**
  ("Mote disables: variations, safe-browsing phone-home, component updates, …") as the claim,
  optionally **empirically verified** (capture real outbound during a session via packet
  capture or the test-mode CDP surface, ADR-0021) to *prove* the browser is quiet.
- **C — A + B** — strongest honest realization of "shows you what others hide": observe+attribute
  page traffic AND prove the browser itself is silent.
- **D — Defer H1-W2, do H2-part-2** (origin→plugin permission inversion) — fully in our control,
  no feasibility risk (per the audit-model investigation).

## EMPIRICAL BASELINE (2026-06-09) — what idle Mote actually phones home

Method: launched `target/debug/mote` under Xvfb `:99`, real network, **90s idle, no
navigation**, with Chromium NetLog (`--log-net-log`) + `ss` polling. NetLog sees the
network-service-level traffic (the background services CDP/per-page handlers can't).
Profile = fresh split-XDG scratch. Artifacts in `/tmp/mote-netbaseline/`.

**Result: Mote is NOT quiet out of the box.** During pure idle it contacted Google on
~14 distinct connections. **HTTP status distribution: 94×200, 4×302, 2×400** — i.e. these
requests are *succeeding*, not 403-ing. This **refutes the "Safe Browsing / Google services
are inert without API keys" hypothesis** for this CEF build.

Browser-own background services (network-service level, page-independent):
| Service | Endpoint | Note |
|---|---|---|
| Component updater (Omaha) | `update.googleapis.com/service/update2/json` | chattiest — 232 refs |
| Component downloads | `edgedl.me.gvt1.com/edgedl/release2/chrome_component/*.crx3`, `diffgen-puffin/*` | real downloads: SSL Error Assistant, CRLSet, Subresource Filter, Origin Trials, etc. |
| **Safe Browsing** | `safebrowsing.googleapis.com/v4/threatListUpdates:fetch` | **LIVE, 200 OK — DB update path works** |
| Network time | `clients2.google.com/time/1/current` | clock sync |
| Spellcheck dict | `redirector.gvt1.com` → `r2---sn-*.gvt1.com/edgedl/chrome/dict/en-us-10-1.bdic` | hunspell |
| Translate ranker | `www.gstatic.com/chrome/intelligence/.../translate_ranker_model_*.pb.bin` | ML model |

Default-search-engine driven (Google is Chromium's built-in default search provider; our
`newtab.html` does NOT reference Google — confirmed by grep, so this is browser machinery):
| Service | Endpoint | Note |
|---|---|---|
| Search preconnect/warmup | `www.google.com/`, `www.google.com/async/folae` | default-search preconnect |
| Account presence | `accounts.google.com/ListAccounts` | Gaia multi-login check **with no user signed in** |
| CSP reporting | `csp.withgoogle.com/csp/report-to/...` | report-to from Google pages |

Caveats (honesty): a 200 on `threatListUpdates:fetch` proves the SB **database-update path**
works; it does **not** prove **enforcement** (blocking a known-bad URL) works without API keys
— that needs a navigation to an SB test URL. `accounts.google.com/ListAccounts` + `www.google.com`
have **no switch-only fix** (per the switch research; CEF issues #4078/#20351) — Gaia is the
default-search-provider's doing; changing the default search engine away from Google may be the
real lever, not a `--disable` flag.

Implications for the ADR:
- The Bucket-1 suppression case is now **empirically grounded**, not hypothetical.
- The SB question **inverts**: SB isn't inert — it's already ON and undisclosed. The decision is
  *keep (disclosed/controlled) vs. off vs. replace-with-local*, not "turn it on."
- Default search = Google is itself a Principle-9 leak (startup account check). Worth its own line.
- DEFAULT_START_URL is already the local `mote://chrome/newtab.html` (good — newtab isn't a leak).

## SAFE BROWSING ENFORCEMENT TEST (2026-06-09) — DEFINITIVE: SB is INERT

Resolves a conflict: the idle baseline *contacted* `safebrowsing.googleapis.com/v4/threatListUpdates`,
but the deep-research agent claimed keyless CEF = SB architecturally absent. To settle it, seeded
Google's official SB test-malware URL (`http://testsafebrowsing.appspot.com/s/malware.html`) as the
startup tab. Result (`/tmp/mote-sb-enforce/`):
- **Malware page LOADED normally** (page + favicon + subresources all fetched, committed). No block.
- **Zero `fullHashes:find` calls** — the confirmation call SB makes on a flagged URL never fired.
- No interstitial, no SAFE_BROWSING activity (the "111 safebrowsing" string hits = NetLog constant
  tables, not requests).

**Conclusion: Safe Browsing does NOT enforce in Mote's CEF 148 build — it is inert** (matches the
research's keyless-CEF claim). The earlier "200 ⇒ SB functional" read was a misattribution: the 200s
were the keyless services (component-updater/downloads/network-time); SB needs a Google API key the
build lacks. **Mote ships zero malware/phishing protection today, silently.** Suppressing SB's
(non-functional) update fetches therefore loses nothing real.

## URL-SAFETY OPTIONS (from deep research — full report in session transcript)

- **Turning ON Google SB is a poor fit:** needs Google API keys third parties can't obtain (not CEF-
  supported), **non-commercial-use-ONLY license** (hard blocker if Mote ever monetizes — re-evaluate
  gate), v4 **deprecated 2027-03-31** (must target v5 + OHTTP relay infra), and SB efficacy is declining
  (recent studies: 16–80% phishing miss). Re-introduces phone-home for a service that's barely working.
- **DNS-based (Quad9/Cloudflare 1.1.1.2):** just moves the trust to the resolver — still phone-home,
  domain-level only. Not a real privacy win. NOT privacy-equivalent to local checking.
- **Mote-native answer (recommended): URL-safety as a transparent first-party PLUGIN.** Local
  hash-prefix DB synced from open feeds — **URLhaus (CC0, malware)** + **OISD (permissive, domain-level)**
  (avoid OpenPhish/PhishTank — non-commercial/no-redistribution terms). Checked locally at
  `on_before_browse` (the hook ALREADY exists in mote-cef) → **zero per-URL phone-home**, fully
  auditable, swappable, opt-in. Brave's Rust `adblock` crate is a reusable filter-matching primitive.
  ~2–4 weeks. Coverage < Google but honest + disclosed. This is the capability-plugin thesis applied to
  security, and a BETTER security+transparency story than opaque baked-in SB.
- Licensing constraints to carry into any ADR: Google SB/Web Risk non-commercial gate; OpenPhish/PhishTank
  redistribution terms; verify URLhaus CC0 + OISD license directly before depending on them.

## PROXY CHOKEPOINT SPIKE (2026-06-09) — premise validated

Decision pivoted (maintainer): instead of suppress-and-attest (a fragile *denylist* of
known background services), adopt **plugin-mediated egress with a Mote-owned proxy as a
default-deny chokepoint** — robust-by-construction against future Chromium phone-homes.
Firm constraints set: **never MITM** (host/CONNECT-level visibility only, ever); the
observability/control surface is gated like the CDP surface (ADR-0021): off-by-default,
loopback, never plugin-reachable, read-only-view in prod / control dev-test-only.

**Spike (`/tmp/mote-proxy-spike/`, `/tmp/mote-proxy.js`):** idle Mote under Xvfb with a tiny
no-MITM Node forward proxy (`--proxy-server=http://127.0.0.1:8888 --disable-quic`), NetLog +
`ss` polling. The load-bearing question — *does CEF route its network-service background
traffic (which bypasses per-browser handlers) through `--proxy-server`?* — is **YES**:
`update.googleapis.com`, `edgedl.me.gvt1.com`, `redirector/r2.gvt1.com`, `accounts.google.com`,
`clients2.google.com`, `www.gstatic.com`, `www.google.com` all appeared **at the proxy**.

**Bypass accounting (3 independent signals agree → no bypass):**
- NetLog complete per-request accounting: **53× `PROXY 127.0.0.1:8888`, 0× `DIRECT`** proxy
  resolutions (the `DIRECT` strings elsewhere are NetLog's event-type constant table, not
  resolutions). Complete, not sampled.
- `ss` (TCP+UDP): **zero** non-loopback endpoints in mote's rows — mote connected only to
  `127.0.0.1:8888`.
- Proxy log: the external hosts were logged at the proxy.

**Confirmed production requirements:** `--disable-quic` is **mandatory** (HTTP/3 over UDP
would bypass an HTTP proxy; the clean UDP result depended on it). `safebrowsing` didn't fire
this run (inert, erratic) — immaterial (SB is being replaced by a plugin).

**Limitations (honest):** proxy-resolution read from raw NetLog string counts (the `pac_string`
key was absent — possibly SIGTERM truncation), so "strong triangulated evidence," not formal
certification. The egress-OBSERVATION/bypass question is closed; the **enforcement mechanism**
(default-deny: direct egress *fails gracefully*, proxy-only path, Mote still works) genuinely
needs a netns harness or the real implementation to validate — and **could not be tested here**:
no `slirp4netns`/`pasta` (the userspace-NAT helpers) and no sudo, so an unprivileged netns has no internet path and
can't be bridged to the host proxy. That validation moves to build-time (the netns harness
becomes a standing enforcement test once slirp4netns is available / in CI).

**Verdict:** the proxy-chokepoint architecture rests on verified ground. Safe to design the
"network egress governance" phase (its own v0.1 phase, precedes adblock/MCP).

## PER-CONTEXT ATTRIBUTION SPIKE (2026-06-09) — REFUTED on CEF 148 chrome runtime

To close the host-coincidence leak (background egress riding a host a page also
authorized), the proposed mechanism was per-`CefRequestContext` proxy identity (distinct
loopback port per context → proxy attributes connections to context). Spiked it with a
throwaway change in `ProfileHandle::create` (`crates/mote-cef/src/profile.rs`, since
reverted) calling `set_preference("proxy", {mode:fixed_servers, server:127.0.0.1:8890})`
on the per-identity context, with the global proxy on :8888. Proxy listened on both ports;
seeded `https://example.com`.

**Result: REFUTED.**
- `set_preference("proxy") → rc=0` — **CEF 148's Chrome runtime rejects per-`RequestContext`
  proxy config.** (The Chrome runtime is monolithic about proxy; per-context proxy was an
  Alloy-era capability, and Alloy is deprecated/removed.)
- Consequently **all traffic — `example.com` (page) AND every Google background service —
  landed on the global :8888**; :8890 received nothing. No per-context distinction exists.

Combined with the earlier finding that per-*request* proxy-auth tokens are defeated by
Chromium's proxy-credential caching, **connection→context/request attribution is not
achievable on CEF 148 chrome runtime** (the only other channel, MITM, is permanently banned).

**Consequence for v0.1:** the airtight per-connection leak-closure the maintainer wanted is
**not feasible** on this CEF. Achievable model = **host-granular default-deny** (allowlist fed
by Layer-1 page authorizations ∪ plugin `net:` grants) **+ aggressive switch-suppression** of
the page-host-colliding background services (Gaia/account-link, Google default-search) **+
honest disclosure** of the narrow residual. This still blocks ~all measured phone-home — the
background services use dedicated infra hosts (`update.googleapis.com`, `*.gvt1.com`,
`safebrowsing.googleapis.com`, `clients2.google.com`) that pages never load as subresources,
so they're always denied. The residual leak is only a (future) background service reusing a
host a page is concurrently using — narrow, switch-mitigated, disclosed. Core robustness (a
NEW phone-home to a NEW host → denied) is preserved. Airtight closure tracked as a
future/upstream item (needs CEF to expose per-context proxy or a per-request network
annotation).

## Knowledge-cache note

Obsidian MCP not connected this session → written locally per the fallback rule. Sync to
`/research/mote/` when MCP is available. No prior research doc covered this; DESIGN.md:1711 is
the only prior art.
