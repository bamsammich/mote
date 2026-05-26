# Mote — Architectural Decision Records

This directory contains Architectural Decision Records (ADRs) for the Mote project, written in [MADR](https://adr.github.io/madr/) format.

ADRs 0001–0003 are **Accepted** (approved by the maintainer 2026-05-25). ADRs 0004–0005 are **Accepted** (approved by the maintainer 2026-05-26).

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-declarative-plugin-registration.md) | Declarative Plugin Registration | Accepted |
| [0002](0002-inter-plugin-dependencies-via-capability-contracts-only.md) | Inter-Plugin Dependencies via Capability Contracts Only | Accepted |
| [0003](0003-chrome-ui-html-css-in-cef-with-wgpu-compositor.md) | Chrome UI as HTML/CSS in CEF (OSR) with a Thin wgpu Compositor | Accepted |
| [0004](0004-windowing-library-winit.md) | Windowing Library: winit | Accepted |
| [0005](0005-host-bridge-cef-message-router-two-layer-isolation.md) | Chrome↔Content Host Bridge: CEF Message Router with Two-Layer Isolation | Accepted |

## Numbering convention

ADRs are numbered sequentially with a four-digit zero-padded prefix: `0001`, `0002`, etc. Numbers are never reused. Superseded ADRs remain in the directory and are marked `Superseded by ADR-XXXX` in their Status line.

New ADRs start as `Proposed`. They move to `Accepted` when approved by the project maintainer. A superseding ADR sets the old one to `Superseded`.
