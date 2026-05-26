# Mote — Identity Isolation Surface

**Date:** 2026-05-26
**Status:** Living document — surfaces marked "to verify" will be confirmed and updated when identity isolation is implemented in Phase 2.

This document enumerates what Mote's identity model isolates, what it does not fully isolate, and the rationale for each. It is the authoritative reference for isolation claims. DESIGN.md's Identity section and DISCIPLINES §5 both point here.

DISCIPLINES §5 discipline: design doc and code comments say "isolated across [enumerated list]" — never "fully isolated." Any PR touching identity-relevant code must update this document in the same PR if the isolation surface changes.

---

## Implementation substrate

Each identity in Mote is implemented as a distinct Chromium profile, via `mote-cef::ProfileHandle` wrapping a CEF `RequestContext` with a per-identity cache and storage path. The profile mechanism is the primary isolation substrate; the guarantees below derive from how Chromium implements that mechanism.

---

## What IS isolated per identity (per Chromium profile)

These surfaces are partitioned per `RequestContext` / profile and do not leak between identities under normal operation.

| Surface | Notes |
|---|---|
| **Cookies** | Each profile has an independent cookie store. A cookie set in identity A is invisible to identity B. This is the primary isolation mechanism for authentication state. |
| **localStorage and sessionStorage** | Partitioned per profile. Web SQL (deprecated) follows the same partition. |
| **IndexedDB** | Partitioned per profile. Each identity's databases are stored under the profile's data path and are not shared. |
| **Browsing history** | Each profile maintains its own history database. History from identity A does not appear in identity B's history or urlbar suggestions. |
| **HTTP disk cache directory** | Each profile is configured with a distinct cache directory path. Cached resources from one identity's browsing are not served to requests from another identity. |
| **Autofill and saved passwords** | Partitioned per profile. Passwords saved in identity A's profile are not accessible from identity B. |
| **Site permissions** (geolocation, camera, microphone, notifications, etc.) | Permission grants are stored per profile. A grant in identity A does not carry to identity B. |
| **Service worker registrations** (to verify) | Service workers are managed at the storage-partition level in Chromium, which corresponds to the profile's `RequestContext`. Registrations from one identity should not be visible to another. **To be verified with a test during Phase 2 identity implementation** (see `docs/plans/02-browser-shell.md` W-A2). |
| **Extension/plugin state** | Mote's plugin storage is scoped per identity via `identity_scope = "per_identity"` in the plugin manifest. This is enforced at the runtime layer (Mote), not by the CEF profile directly. See DESIGN.md §Plugin Identity Scope. |
| **Session state (tabs, scroll, form drafts)** | Stored in per-identity SQLite at `~/.local/state/mote/<identity>/session.db`. Not shared across identities. |

---

## What is NOT fully isolated / known leakage surfaces

These surfaces are shared across profiles or have known partial leakage. Each entry states the nature of the limitation and either a mitigation or a note that it is accepted/tracked.

### GPU and shader caches

**Nature:** The GPU process and its shader compilation cache are shared across all profiles in a Chromium instance. Shader programs compiled for one profile's pages may be reused by another profile's pages through the shared in-process GPU cache. This creates a potential timing side-channel: pages in one identity could infer, via shader cache timing, whether a particular site was visited in another identity.

**Severity:** Low in practice. Shader cache hits are a micro-optimization; exploiting them for cross-profile tracking requires a sophisticated, page-controlled timing attack against a very narrow signal.

**Mitigation:** Known limitation. Fully separating the GPU cache would require separate GPU processes per profile, which is outside scope for v0.1. Mitigation: Mote's plugin permission model prevents page-injected scripts from making high-precision timing calls without `introspect:*` permission, which reduces the attack surface.

**Status:** Accepted limitation for v0.1. To revisit if GPU process isolation becomes practical in a future Chromium/CEF release.

### HTTP cache partitioning and third-party keying

**Nature:** Chrome 86+ introduced HTTP cache partitioning keyed on the top-level site and the requesting frame's origin (double-keyed, sometimes triple-keyed). This prevents cross-site cache attacks within a single profile. However, this partitioning is *within* a profile — it does not partition between profiles. Because Mote uses per-profile cache directories, the disk cache is already directory-isolated per identity; the concern is the in-memory cache state that exists while both identities are active simultaneously (e.g., two browser windows open in different identities). In-process cache state (connection pools, socket pools) may be transiently shared before the per-profile isolation takes effect at the network layer.

**Severity:** Low to medium. The disk cache is directory-isolated; in-memory transient state is the residual risk.

**Mitigation:** Mote always configures distinct cache directory paths per identity (`ProfileHandle`). In-memory cache sharing within a session is a Chromium-level concern. CEF's `RequestContext` isolation is the designed mitigation; its completeness is to be verified.

**Status:** To verify during Phase 2 identity implementation. Specifically: with two identities active, confirm that a resource cached by identity A is not served from identity A's in-memory cache to a request from identity B.

### Network-level state: TLS sessions, DNS, connection pools

**Nature:** Some network-layer state is shared across profiles or at least shared within the same browser process lifetime:

- **TLS session tickets / session resumption:** TLS session resumption tickets (RFC 5077) allow a client to resume a previous TLS connection without a full handshake. If the same server is visited in two different identities within a short window, the second connection may resume the first identity's TLS session, creating a correlation signal visible to the server. The server can observe that the two requests came from the same client (same session ticket) even if cookies and storage differ.
- **DNS cache:** DNS resolutions are cached in the browser process and may be shared across profiles, reducing per-identity network isolation at the transport layer.
- **HTTP/2 connection pooling:** HTTP/2 connections are pooled and may be reused across same-origin requests from different profiles within the same browser process. This is a known cross-profile state sharing vector.

**Severity:** Medium. These are real correlation vectors visible to servers and network observers. They do not expose stored data (cookies, localStorage) but they do allow a server to correlate that two requests came from the same browser process, potentially linking identities.

**Mitigation:** Known limitation for v0.1. Full transport-layer isolation would require separate network processes per identity, which is outside scope. Mitigation: users requiring strict transport-layer isolation between identities should use separate Mote instances (separate processes). This limitation is documented here and will be surfaced in Mote's privacy documentation.

**Status:** Accepted limitation. Tracked for potential future mitigation if CEF exposes per-profile network process isolation.

### Chromium process-level telemetry and crash reporting

**Nature:** Chromium's own telemetry and crash-reporting infrastructure (when not disabled) operates at the process level, not per profile. Mote disables Chromium telemetry (DESIGN: no-data-without-consent principle), but this is a configuration concern to verify in the CEF bootstrap path.

**Mitigation:** Mote's CEF bootstrap passes the relevant Chromium command-line flags to disable telemetry and crash reporting. To be confirmed during Phase 2 CEF integration.

**Status:** To verify during Phase 2.

---

## Test coverage plan (Phase 2)

The following tests will be added as part of `mote-cef` `ProfileHandle` implementation (W-A2 in the Phase 2 work breakdown):

1. **Cookie isolation test:** Set a cookie in identity A's profile; confirm it is not visible in identity B's profile.
2. **localStorage isolation test:** Write to localStorage under identity A; confirm identity B cannot read it.
3. **History isolation test:** Navigate to a URL under identity A; confirm it does not appear in identity B's history.
4. **Service worker isolation test:** Register a service worker under identity A's profile; confirm identity B does not see the registration (to verify the "to verify" entry above).
5. **Cache directory test:** Confirm that identity A and B's cache directories are distinct on disk and that a resource cached by A is not served from disk to B.

Results from these tests will update the "to verify" entries in this document.

---

## README pointer

The README (`README.md`) is currently minimal (one-line project description). A privacy/security section referencing this document is deferred to Phase 11 (documentation pass), when the README is expanded for public-facing content. Until then, this document is reachable via DESIGN.md §Identity and DISCIPLINES §5.
