# Mote — Architectural Decision Records

This directory contains Architectural Decision Records (ADRs) for the Mote project, written in [MADR](https://adr.github.io/madr/) format.

ADRs 0001–0003 are **Accepted** (approved by the maintainer 2026-05-25). ADRs 0004–0005 are **Accepted** (approved by the maintainer 2026-05-26). ADRs 0006–0008 are **Accepted** (approved by the maintainer 2026-05-27). ADR-0009 is **Accepted** (approved by the maintainer 2026-05-28). ADR-0010 is **Accepted** (approved by the maintainer 2026-05-30). ADR-0011 is **Accepted** (approved by the maintainer 2026-06-01). ADR-0012 is **Accepted** (approved by the maintainer 2026-06-02).

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

## Numbering convention

ADRs are numbered sequentially with a four-digit zero-padded prefix: `0001`, `0002`, etc. Numbers are never reused. Superseded ADRs remain in the directory and are marked `Superseded by ADR-XXXX` in their Status line.

New ADRs start as `Proposed`. They move to `Accepted` when approved by the project maintainer. A superseding ADR sets the old one to `Superseded`.
