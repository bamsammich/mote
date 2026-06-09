# Mote — Architectural Decision Records

This directory contains Architectural Decision Records (ADRs) for the Mote project, written in [MADR](https://adr.github.io/madr/) format.

ADRs 0001–0003 are **Accepted** (approved by the maintainer 2026-05-25). ADRs 0004–0005 are **Accepted** (approved by the maintainer 2026-05-26). ADRs 0006–0008 are **Accepted** (approved by the maintainer 2026-05-27). ADR-0009 is **Accepted** (approved by the maintainer 2026-05-28). ADR-0010 is **Accepted** (approved by the maintainer 2026-05-30). ADR-0011 is **Accepted** (approved by the maintainer 2026-06-01). ADRs 0012–0017 are **Accepted** (approved by the maintainer 2026-06-02). ADR-0018 is **Accepted** (approved by the maintainer 2026-06-05). ADR-0019 is **Accepted** (approved by the maintainer 2026-06-06). ADR-0012's chord table was **amended 2026-06-06** (CL-KEYMAP: `Ctrl+1–9` → tabs, workspaces → `Ctrl+Alt+1–9`, per ADR-0019's Firefox/Chrome-defaults principle). ADR-0021 is **Proposed** (2026-06-08) — Accepted pending CDP-spike validation; ADR-0020 (UI regression-testing architecture) is forthcoming.

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-declarative-plugin-registration.md) | Declarative Plugin Registration | Accepted |
| [0002](0002-inter-plugin-dependencies-via-capability-contracts-only.md) | Inter-Plugin Dependencies via Capability Contracts Only | Accepted |
| [0003](0003-chrome-ui-html-css-in-cef-with-wgpu-compositor.md) | Chrome UI as HTML/CSS in CEF (OSR) with a Thin wgpu Compositor | Accepted |
| [0004](0004-windowing-library-winit.md) | Windowing Library: winit | Accepted |
| [0005](0005-host-bridge-cef-message-router-two-layer-isolation.md) | Chrome↔Content Host Bridge: CEF Message Router with Two-Layer Isolation | Accepted |
| [0006](0006-user-config-read-only-to-mote.md) | User Config is Read-Only to Mote; Mutations Go to a Managed Layer | Accepted |
| [0007](0007-plugin-management-ui-privileged-async-approval.md) | Plugin Management UI: Privileged-Chrome Surfaces with Async Approval | Accepted |
| [0008](0008-approval-carve-outs-bundled-and-dev-mode.md) | Approval Carve-Outs: Bundled and Dev-Mode Plugins Auto-Approve | Accepted |
| [0009](0009-password-manager-provider-non-exclusive-targeted-routing.md) | `password-manager:provider` Non-Exclusive; Secret/Provider Routing is Targeted | Accepted |
| [0010](0010-collector-dispatch-deadline-budgeting.md) | Collector Dispatch: Runtime Semantics and Deadline Budgeting for Provider Contribution Surfaces | Accepted |
| [0011](0011-popup-behavior-in-window-tab.md) | Popup Behavior: In-Window Tab + User-Gesture Activation + Opt-Out Path | Accepted |
| [0012](0012-browser-keybind-suite.md) | Browser-Keybind Suite: Chord Table, Scope Rules, Contextual `⌘W`, and Plugin-Keybind Closure for v0.1 | Accepted |
| [0013](0013-themable-icon-contract.md) | Themable Icon Contract: `theme.icons.<action>` Mapping + `theme:set_icon` API | Accepted |
| [0014](0014-rail-as-plugin-declarable-slot.md) | Rail as a Plugin-Declarable Slot: Isolation Boundary, Declaration Model, Collision Policy | Accepted |
| [0015](0015-mote-newtab-slot-architecture.md) | `mote://newtab` Slot Architecture + `mote://` Global-Request-Context Constraint | Accepted |
| [0016](0016-status-line-plugin-api.md) | Status-Line Plugin API: Declarative Registration, Read-Only v1, Clickable v2 Planned | Accepted |
| [0017](0017-settings-panel-layout-and-write-target.md) | Settings Panel: Multi-Section Layout, Deep-Link Contract, `managed.lua` Write Target, URL-Install Deferral | Accepted |
| [0018](0018-omnibox-url-vs-search-determination.md) | Omnibox URL-vs-Search Determination: Public-Suffix-Based, HTTPS-Default with Loopback Exception | Accepted |
| [0019](0019-editing-paradigm-as-swappable-plugin.md) | Editing Paradigm (vim/emacs) as a Swappable First-Party Plugin: Declarative Keymap, Capability Contract, Bounded Command Host-API, Content-Keystroke Withholding | Accepted |
| [0021](0021-test-mode-cef-devtools-protocol-surface.md) | Test-mode CEF DevTools-Protocol Surface: Off-by-Default, Loopback-Only, Env-Gated CDP for the E2E Test Lane | Proposed |

## Numbering convention

ADRs are numbered sequentially with a four-digit zero-padded prefix: `0001`, `0002`, etc. Numbers are never reused. Superseded ADRs remain in the directory and are marked `Superseded by ADR-XXXX` in their Status line.

New ADRs start as `Proposed`. They move to `Accepted` when approved by the project maintainer. A superseding ADR sets the old one to `Superseded`.
