# ADR-0021 — Test-mode CEF DevTools-Protocol Surface: Off-by-Default, Loopback-Only, Env-Gated CDP for the E2E Test Lane

- **Status:** Proposed (2026-06-08) — **Accepted pending CDP-spike validation**
- **Date:** 2026-06-08

---

## Context and Problem Statement

Mote's UI regression suite needs an end-to-end lane that drives the *real*
running app — the full shell ↔ chrome ↔ wgpu-compositor stack — to catch the
integration and native-seam bugs (window-resize proportion,
stale-content-after-tab-close, settings section-nav, idle-CPU paint) that
pure-DOM/unit tests structurally cannot reach. CEF (`cef-148.2.0+148.0.8`,
Chromium 148) can expose a Chrome DevTools Protocol (CDP) endpoint via
`Settings.remote_debugging_port`. But that endpoint is a **process-global,
out-of-band control channel**: CDP can attach to *any* renderer (chrome or
content), evaluate arbitrary JS in any frame, and read cross-origin state —
precisely the "arbitrary eval / broad attach" capability **ADR-0005** spent its
entire design making unreachable from the privileged `window.mote` bridge.
Enabling CDP is therefore a security-boundary-relaxing decision that must be
structurally confined to dev/test.

## Decision Drivers

- The E2E lane must drive + observe the real app at the seams where Mote's worst
  bugs live.
- ADR-0005's invariant — the privileged surface is safe-by-construction;
  misconfiguration must be *unrepresentable* — must not be silently undermined.
- The relaxation must be **impossible to reach in a shipped default run**.
- Consistency with the existing `EngineConfig.no_sandbox` precedent (debug-only,
  default-secure) and DESIGN.md's loopback-by-default posture for the MCP server.

## Considered Options

- **(a) `Settings.remote_debugging_port` field** — the Rust `cef` binding exposes
  it directly (`Settings.remote_debugging_port: c_int`); one field on
  `EngineConfig` + one line in `Engine::init`. **Chosen.**
- **(b) `on_before_command_line_processing` appending `--remote-debugging-port`**
  — rejected: more surface; the `Settings` path is authoritative.
- **(c) No CDP; pure window-screenshot + `/proc` sampling harness** — kept as the
  documented **fallback** if the spike shows CEF-OSR can't be driven over CDP.

## Decision Outcome

Chosen: **(a)**, with the following as **hard, tested invariants** (not prose
intentions):

- **Structurally off by default.** `EngineConfig.remote_debugging_port` defaults
  to `0` (disabled). A test asserts the default `EngineConfig` *and* a default
  `mote_shell::run()` (no env var) open **no** CDP listener.
- **Single enabler.** The *only* thing that turns it on is the
  `MOTE_REMOTE_DEBUG_PORT` env var, read in `mote_shell::run()` (the
  `MOTE_DISCARD_AFTER_SECS` pattern, `lib.rs:128`).
- **Loopback-only.** The bind address is forced to `127.0.0.1` inside `mote-cef`;
  there is **no** env var or config that can bind a public interface (mirrors
  DESIGN.md's `mcp:server:bind_loopback` default vs. the louder `bind_public`).
- **Orthogonal to the sandbox.** Enabling CDP must not require or imply
  `no_sandbox`; the two debug surfaces are independent.
- **Dev/test-only; grants no plugin-reachable capability.** This is an
  out-of-band test-harness channel, not a plugin-facing API. It does not alter
  DESIGN.md's `introspect:` permission domain or the per-plugin/per-directory
  dev-mode model ("never a global disable-security toggle"). An enabled endpoint
  *intentionally* relaxes the bridge-isolation guarantee for that process and is
  confined to dev/test or explicit env opt-in — never a shipped default.
- **Stays inside the `mote-cef` wrapper** (DISCIPLINES §1): no new `use cef::`
  outside `mote-cef`, no new `unsafe`.

## Consequences

- **Good:** the E2E lane can drive the real app at the exact seams where Mote's
  worst bugs live; the relaxation is structurally confined and tested rather than
  assumed.
- **Bad / risk:** an *enabled* endpoint is a real attack surface — anyone who can
  reach the loopback port can drive the renderer — so it must never ship enabled;
  the tested off-by-default + loopback-only invariants are the guardrails.
- **Bounded:** this ADR governs only the test-mode endpoint; it introduces no
  plugin-facing or production DevTools access.

## Relationship to Existing ADRs

- **ADR-0005 (host-bridge two-layer isolation):** this ADR intentionally opens a
  channel ADR-0005 makes unreachable, confined to dev/test. A back-link/amendment
  note will be added to ADR-0005 when this ADR moves to Accepted (after the
  spike).
- **ADR-0020 (UI regression-testing architecture, forthcoming):** references this
  as the enabler of its full-app E2E lane.
- Consistent with the `EngineConfig.no_sandbox` debug-only precedent and
  DESIGN.md's loopback-default posture.

**Proposed → Accepted gate:** this ADR stays *Proposed* until the CDP spike
confirms Playwright `connectOverCDP` attaches to CEF-148 in OSR mode and can
`evaluate`/screenshot. If the spike fails, option **(c)** (screenshot + `/proc`
fallback) is adopted and this ADR is revised or withdrawn.
