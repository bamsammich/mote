# ADR-0004 — Windowing Library: winit + à-la-carte macOS Crates

- **Status:** Accepted
- **Date:** 2026-05-26

---

## Context and Problem Statement

ADR-0003 (Accepted) mandates that Mote owns the OS window and the wgpu GPU surface, with the chrome rendered off-screen by CEF and composited by a thin wgpu compositor into that surface. CEF therefore never opens a top-level window. The windowing library — the crate that creates the OS window, drives the event loop, and provides the `raw-window-handle` to `wgpu::Surface` — is unspecified in ADR-0003 and unresolved in DESIGN.md's Open Decisions list.

**This is a project-lifetime lock-in.** The windowing library determines: the OS event loop, keyboard and mouse input source, DPI handling, platform-specific window flags (decorations, transparency, titlebar control), the `raw-window-handle` interface to wgpu, and the first-party integration surface with the wgpu ecosystem. Changing it later requires rewriting `mote-shell`'s window and event-loop code, all input routing, and all macOS platform integrations.

**macOS and Linux are equal first-class targets.** The Phase 10 polish goal explicitly requires all v0.1 plugins tested on both macOS (AeroSpace) and Linux (Hyprland). Mote is built for a persona who uses macOS as their daily machine as readily as Linux. A decision premised on "Linux first, macOS later" is therefore wrong about the threat model: macOS-citizen behaviour is a correctness requirement, not a nice-to-have.

**The chrome wants a custom titlebar.** DESIGN.md §Window model calls for a modern browser chrome — tab strip, omnibox — sitting where the system titlebar would be, in the style of Arc, Zen, and Chrome. That requires full native titlebar control: transparent titlebar, fullsize content view, hidden title text, movable-by-background, and — for precise traffic-light positioning — either native API access or a workaround.

The Phase 2 plan (`docs/plans/02-browser-shell.md`) records the windowing decision as settled (§Settled Phase-2 decisions) and this ADR formalises the rationale.

---

## Decision Drivers

- ADR-0003 puts the chrome in an *off-screen* CEF browser composited by wgpu; Mote owns the surface. CEF must never open a top-level window.
- The window crate must integrate with wgpu via `raw-window-handle` (the standard interface across the Rust GPU ecosystem).
- macOS and Linux are equal first-class targets. The windowing crate must not make either a second-class citizen.
- Mote's browser chrome requires custom titlebar treatment: transparent titlebar, tabs-in-titlebar, traffic light control. These are macOS-citizen requirements that affect the windowing library choice.
- DISCIPLINES §1: CEF FFI types stay behind `mote-cef`. The wgpu compositor and the winit window live in `mote-ui` and `mote-shell` respectively — neither pulls in CEF types.
- The choice must be well-maintained and ecosystem-standard. Mote does not have bandwidth to maintain a windowing library.
- macOS native feel requires: a native menu bar (app menu, Edit/Window/Help menus with standard roles); vibrancy/blur on toolbar or sidebar surfaces; a system-tray entry (optional, Phase 10).

---

## Considered Options

### (a) CHOSEN: winit + à-la-carte macOS crates

**winit** (currently 0.30.13; 0.31.0-beta.2 in progress) is the de-facto standard Rust windowing library, maintained by the Rust-windowing org and used by wgpu, Bevy, Iced, and most of the Rust GPU ecosystem. It uses the `ApplicationHandler` trait model introduced in 0.30, which maps cleanly onto `mote-shell`'s architecture: implement the trait, receive `resumed()` / `window_event()` / `user_event()` callbacks on the main thread.

macOS-specific titlebar primitives are available on `WindowAttributesExtMacOS` and `WindowExtMacOS`:

| Attribute / method | Available in winit 0.30.x |
|---|---|
| `with_titlebar_transparent` | Yes |
| `with_fullsize_content_view` | Yes |
| `with_title_hidden` | Yes |
| `with_titlebar_hidden` | Yes |
| `with_titlebar_buttons_hidden` | Yes |
| `with_movable_by_window_background` | Yes |
| `with_unified_titlebar` (larger titlebar style) | Yes (added 0.30.13) |
| `with_traffic_light_inset` (pixel-precise repositioning) | **No** — gap, see Consequences |

The macOS à-la-carte companion crates (all from `tauri-apps`, all Apache-2.0/MIT, all compatible with winit 0.30):

**muda 0.19.2** (released 2026-05-20): native menu bar, including the macOS app menu. Winit integration is explicit and first-class: `MenuEvent::set_event_handler` installs a closure that calls `EventLoopProxy::send_event`, waking the event loop with a typed `UserEvent`. The `ApplicationHandler::user_event` method receives and dispatches it. `init_for_nsapp()` attaches the menu to the macOS application; `set_as_windows_menu_for_nsapp()` and `set_as_help_menu_for_nsapp()` register the Window and Help menus with AppKit. `PredefinedMenuItem` covers the full macOS role set: Copy, Cut, Paste, SelectAll, Undo, Redo, Minimize, Zoom, Fullscreen, CloseWindow, Hide, HideOthers, ShowAll, BringAllToFront, Services, About, Quit. Main-thread-only constraint on macOS is not a problem given winit's `ApplicationHandler` model runs on the main thread.

**window-vibrancy 0.7.1** (released 2025-11-12): applies `NSVisualEffectView` materials to any `NSWindow`. Lists winit `^0.30` as a dev dependency; winit 0.30 compatible. Supports 19 `NSVisualEffectMaterial` variants: `Titlebar`, `Selection`, `Menu`, `Popover`, `Sidebar`, `HeaderView`, `Sheet`, `WindowBackground`, `HudWindow`, `FullScreenUI`, `Tooltip`, `ContentBackground`, `UnderWindowBackground`, `UnderPageBackground`, plus five deprecated pre-10.14 variants. Requires macOS 10.10+.

**tray-icon 0.24.0** (released 2026-05-07): system-tray icon for desktop applications. winit compatible (same `EventLoopProxy` / `set_event_handler` pattern as muda). Linux (GTK) and Windows support included. Optional — Mote has no current tray requirement; included in the picture for completeness.

**Integration summary:** wgpu 29.0.3 (latest) declares `raw-window-handle ^0.6.2` and exposes `Instance::create_surface()` as a safe API via `SurfaceTarget`. winit 0.30 exposes raw-window-handle 0.6. The triad winit 0.30 / wgpu 29 / raw-window-handle 0.6 is fully compatible without any `unsafe` block at the surface-creation seam (wgpu's `SurfaceTarget` accepts a winit `Window` directly since wgpu 0.20+). The earlier note in this ADR about a possible `unsafe` seam is resolved: pin wgpu ≥ 0.20 and use `SurfaceTarget::Window`.

### (b) tao (Tauri's winit fork)

tao 0.35.3 (released 2026-05-23) is a winit fork maintained by the Tauri team. Its `WindowBuilderExtMacOS` adds one meaningful primitive over winit: **`with_traffic_light_inset`** — pixel-precise repositioning of the close/minimize/zoom buttons relative to the upper-left corner of the window. This is the single concrete API that tao has and winit 0.30 lacks for the custom-titlebar use case.

However, tao's headline macOS features — menu bar, vibrancy, tray — are now available to winit users as standalone crates (`muda`, `window-vibrancy`, `tray-icon`), all from the same Tauri org. The reason to choose tao used to be that it bundled these features; that differentiation has largely dissolved.

tao replaces the Linux backend with GTK 3 rather than using direct X11/Wayland integration. GTK is a heavier dependency surface and introduces a third-party widget toolkit into a codebase that has explicitly avoided opinionated UI frameworks (DESIGN.md §Dependency Stack). CEF's own Ozone/X11 path runs independently of GTK; on Linux, tao adds GTK where winit needs only libxcb/libwayland.

tao does not expose an `ApplicationHandler`-equivalent trait aligned with winit 0.30's model; its event loop API diverges from upstream winit at the architectural level, not just at the extension level. Following winit's `ApplicationHandler` model is more valuable than inheriting tao's API, because winit's model is what the broader Rust windowing ecosystem (wgpu examples, Bevy, Iced) documents and targets.

tao is the right choice if its Tauri ecosystem context is a primary concern (e.g. building *inside* Tauri). For Mote — where windowing is a direct dependency, not an application framework — using a fork rather than the upstream is a maintenance liability without a compensating gain, given the à-la-carte crates cover the feature gap.

**Honest caveat on the traffic light gap:** tao's `with_traffic_light_inset` is the one thing winit 0.30 cannot match natively. If the Phase 10 browser chrome design requires precise traffic-light repositioning (e.g. to place them vertically in a custom sidebar titlebar), the winit path requires an `objc2` / `objc2-app-kit` raw call — a small, scoped `unsafe` block in `mote-shell`'s macOS platform module, analogous to the CEF FFI allows in `mote-cef`. This is a known, bounded scope of work, not a fundamental blocker.

### (c) glazier (rejected)

Linebender's low-level windowing library, used by Xilem and Masonry. More opinionated input model (IME, accessibility, text input events) than winit's raw event stream. Less ecosystem adoption. Not the right choice for a project where input events are routed to CEF rather than to a native Rust widget tree. Maintained but narrower community.

---

## Decision Outcome

**Chosen option: (a) — winit 0.30 + à-la-carte macOS crates (muda, window-vibrancy; tray-icon if needed).**

### Rationale

winit is the standard Rust windowing crate: wgpu's own examples, tutorials, and ecosystem integrations assume winit. Using it minimises friction at the `wgpu::Surface` seam. The `ApplicationHandler` event loop model (winit 0.30) maps cleanly onto `mote-shell`'s architecture: one event source, one main-thread trait implementation, menu events and tray events forwarded through `EventLoopProxy::send_event`.

The à-la-carte macOS crates from the Tauri org (`muda`, `window-vibrancy`, `tray-icon`) are actively maintained, well-downloaded, and explicitly support winit 0.30. They restore the macOS feature parity that tao used to hold exclusively. Mote can adopt each crate when the corresponding feature phase lands, rather than taking on all of tao's surface at once.

tao's remaining differentiation — `with_traffic_light_inset` — is real but workable via `objc2` when Phase 10 demands it. That is a one-function, scoped `unsafe` call, not an architecture change. tao's trade-off is a GTK dependency on Linux and a fork-not-upstream maintenance posture; those costs are not worth paying for one API that has an available bypass.

**The honest uncertainty:** the maintainer's daily development environment is Linux. macOS behaviour — vibrancy rendering, menu bar native feel, traffic light positioning — is not testable in CI here and is validated by the maintainer on a macOS machine. The à-la-carte crates are well-used in production (muda: 18M total downloads; window-vibrancy: 9.6M) but Mote will be among the first non-Tauri applications exercising this exact stack under winit 0.30's `ApplicationHandler` model. Any rough integration edges should be expected to surface in Phase 10 and fixed then.

---

## Consequences

### Workspace dependency additions

Added to `[workspace.dependencies]` in `Cargo.toml` at Phase 2:

```toml
winit        = { version = "0.30", features = ["x11"] }
wgpu         = { version = "29" }            # already added by the wgpu compositor decision (ADR-0003)
raw-window-handle = { version = "0.6" }
```

Added at Phase 10 (macOS polish), when each feature lands:

```toml
muda             = { version = "0.19" }      # native menu bar; macOS + Windows + Linux
window-vibrancy  = { version = "0.7" }       # NSVisualEffectView; macOS only
# tray-icon      = { version = "0.24" }      # system tray; add if/when Mote wants one
```

(Exact patch versions to be pinned by the work unit that adds each crate. Confirm and merge the `wgpu` entry with the Phase 2 compositor work.)

### Good

- Standard ecosystem integration: winit, wgpu, and raw-window-handle form a well-documented, commonly-used triad. Phase 2 engineers have ample prior art.
- X11 and XWayland work immediately; native Wayland (`--ozone-platform=wayland`) is a later hardening task.
- CEF never opens a top-level window; the OSR-compositor architecture is preserved end-to-end.
- `wgpu::Instance::create_surface()` is safe via `SurfaceTarget::Window` (wgpu ≥ 0.20). No `unsafe` block required at the surface-creation seam.
- macOS menu bar (muda), vibrancy (window-vibrancy), and tray (tray-icon) are available as standalone, drop-in additions in Phase 10 without changing the windowing library. The phase gate is clean: Phase 2 ships the winit window; Phase 10 adds the polish crates.
- macOS packaging (`.app` bundle, helper process, entitlements, Keychain integration) is a Phase 9 concern orthogonal to the windowing library choice.

### Neutral / risks

- **Traffic light repositioning gap.** winit 0.30 provides `with_titlebar_buttons_hidden` and `with_fullsize_content_view` but no `with_traffic_light_inset`. If the Phase 10 chrome design requires precise repositioning of the close/minimize/zoom buttons (as Arc, Chrome, and Zen do), the implementation requires a raw `objc2` / `objc2-app-kit` call in `mote-shell`'s macOS platform module. This is a scoped, documented `unsafe` block — analogous to `mote-cef`'s FFI allows in scope and justification (DISCIPLINES §1). It does not block the windowing choice; it is a Phase 10 detail flagged here so it is not a surprise.

- **Input routing is `mote-shell`'s responsibility.** winit events (mouse, keyboard, resize) are routed to either the chrome CEF browser or the focused page CEF browser by `mote-shell`'s hit-test + focus-owner logic (Phase 2 plan §1.3). winit delivers raw events; the routing layer is entirely Mote code. This is the correct division of responsibility and is already designed in `docs/plans/02-browser-shell.md`.

- **macOS validation requires a macOS machine.** The dev environment is Linux. Vibrancy rendering, menu bar integration, traffic light appearance, and the fullsize-content-view + transparent-titlebar combination are not testable in CI on this machine. Validation is the maintainer's responsibility on macOS during Phase 10. The risk is bounded: all three crates (`muda`, `window-vibrancy`) have production usage (18M+ and 9.6M+ downloads respectively) and explicit winit 0.30 compatibility.

- **winit 0.31 is in beta.** A 0.31.0-beta.2 exists. If 0.31 lands during Phase 2–10 development and is a breaking change, a one-time migration in `mote-shell` is expected. Given winit's maintenance cadence and the 0.30.x patch series still receiving fixes, this is a known and manageable upgrade cost.

- **muda main-thread constraint on macOS.** muda requires menus to be used from the main thread on macOS. The `ApplicationHandler` model already runs all window lifecycle and event delivery on the main thread; this is not a conflict. Menu construction must not happen from a background thread — document this at the Phase 10 menu implementation seam.

---

## Links / References

- ADR-0003 — Chrome UI as HTML/CSS in CEF (OSR) with a Thin wgpu Compositor (mandates Mote owns the OS window)
- DESIGN.md §Window model ("Mote ships as a standard windowed application; the WM places")
- DESIGN.md §Performance Architecture (sub-millisecond tab-switch target, texture swap model)
- DISCIPLINES §1 — CEF upgrade discipline (CEF FFI types behind `mote-cef`; wgpu/winit explicitly live in other crates)
- `docs/plans/02-browser-shell.md` §0 (ground rules), §1.1 (OS window and wgpu surface), §2 (crate mapping), §Settled Phase-2 decisions
- `docs/plans/02-risks.md` D1 (windowing decision, proposed resolution)
- ADR-0005 — chrome ↔ content host-bridge transport (pending spike; see `docs/plans/02-risks.md` D12)
- winit 0.30.13 — https://docs.rs/winit/0.30.13/winit/
- muda 0.19.2 — https://docs.rs/muda/0.19.2/muda/ / https://github.com/tauri-apps/muda
- window-vibrancy 0.7.1 — https://docs.rs/window-vibrancy/0.7.1/window_vibrancy/
- tray-icon 0.24.0 — https://docs.rs/tray-icon/0.24.0/tray_icon/
- wgpu 29.0.3 — https://docs.rs/wgpu/29.0.3/wgpu/
