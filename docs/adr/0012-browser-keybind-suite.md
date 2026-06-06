# ADR-0012 — Browser-Keybind Suite: Chord Table, Scope Rules, Contextual `⌘W`, and Plugin-Keybind Closure for v0.1

- **Status:** Accepted (approved by the maintainer 2026-06-02)
- **Date:** 2026-06-02

---

## Context and Problem Statement

Mote currently intercepts a small set of chords in
`ShellApp::intercept_keybind` (`crates/mote-shell/src/lib.rs:2811`):
`Ctrl+T` new tab, `Ctrl+W` close tab, `Ctrl+Tab` cycle tabs,
`Ctrl+Shift+I` integrity panel, `Mod+Space` workspace tab picker, `Esc` to
close panels, and a couple of debug-only chords. This is incomplete against
what users expect from a browser, and the existing entries aren't all
documented as a public surface — they're implementation choices spread
across one function. The R4 polish wave adds the rest of the standard
browser-shortcut suite and the missing window-lifecycle keybinds. Because
this is a stable public surface plugins, themes, and users will rely on,
the chord table and its scope rules need to be a recorded decision.

## Decision Drivers

- The shortcut set must match what users expect from Chrome / Safari /
  Firefox — surprise-free muscle memory.
- Each chord's **scope** (global vs. focus-routed vs. captured-modal) must
  be deterministic and documented, not implicit in event-loop order.
- `⌘W` is contextual in every modern browser: closes the tab normally,
  closes the window when only one tab remains. This behavior needs to be
  recorded, not left as an implementation detail.
- Plugin-registered keybinds are a real future need (productivity plugins,
  vim emulation, etc.) but they introduce conflict-resolution complexity
  (precedence, override of built-ins, per-scope binding) that warrants its
  own design pass. v0.1 keeps the keybind table closed so the v2 design has
  freedom.
- Linux dev convention uses `Ctrl` where the design spec writes `⌘`; the
  existing code comments call this out. Honour the same convention.

## Considered Options

- **Keep the keybind set ad-hoc in `intercept_keybind` with no recorded
  contract.** Rejected: makes it impossible to safely add plugin keybinds
  later without breaking existing users; no shared mental model for "where
  does this chord fire from."
- **Document the chord set in code comments only, no ADR.** Rejected: the
  scope rules + contextual `⌘W` + plugin-closure are decisions that
  outlive any single commit; recording them in code only loses the
  rationale.
- **Lock the v0.1 chord table + scope rules in an ADR, defer plugin
  keybinds to a future ADR** (this ADR). Captures the public surface,
  the rationale, and the explicit closure that plugins cannot register
  keybinds in v0.1.

## Decision Outcome

Chosen: **The v0.1 keybind table, scope rules, contextual `⌘W` behavior,
and the closure that plugin-registered keybinds are not supported in v0.1
are recorded here.**

### Chord table (v0.1)

> **Amended 2026-06-06 (CL-KEYMAP, per ADR-0019's "core ships Firefox/Chrome
> defaults" principle):** `Ctrl+1`–`9` was reassigned from *workspaces* to
> *tabs* — every mainstream browser maps `Ctrl+1`–`9` to tab-by-index, so the
> familiar default must do that. Workspace-by-index moved to `Ctrl+Alt+1`–`9`.
> Added `Ctrl+K` (omnibox-focus alias) and `Ctrl+Shift+Tab` (reverse cycle).
> These are core defaults; a keybind plugin overrides them (ADR-0019).

| Chord | Action | Scope |
|---|---|---|
| `Ctrl+T` | New tab in current workspace, become active | Global |
| `Ctrl+W` | Close active tab; if only one tab remains, close the window | Global |
| `Ctrl+Shift+W` | Close window (regardless of tab count) | Global |
| `Ctrl+Q` | Quit Mote | Global |
| `Ctrl+L` / `Ctrl+K` | Focus the omnibox; select existing text | Global |
| `Ctrl+R` | Reload the active tab | Global |
| `Ctrl+[` | Back (active tab's history) | Global |
| `Ctrl+]` | Forward (active tab's history) | Global |
| `Ctrl+1`..`Ctrl+8` | Select tab by index (1–8) in the active workspace | Global |
| `Ctrl+9` | Select the **last** tab (not the 9th — Chrome convention) | Global |
| `Ctrl+Alt+1`..`Ctrl+Alt+8` | Switch to workspace by index (1–8) | Global |
| `Ctrl+Alt+9` | Switch to the **last** workspace | Global |
| `Ctrl+Tab` | Cycle to next tab in current workspace | Global |
| `Ctrl+Shift+Tab` | Cycle to previous tab in current workspace | Global |
| `Ctrl+Shift+I` | Toggle integrity panel | Global |
| `Mod+Space` | Open workspace tab picker | Global |
| `Esc` | Close the topmost modal panel (integrity, approval, picker, palette) | Captured-modal |

### Scope rules

Three scopes are defined:

1. **Global.** The chord is intercepted before the focus owner sees the
   key. Fires from chrome, content, omnibox, or any focus state.
   `intercept_keybind` is the implementation seam.
2. **Captured-modal.** While a modal surface owns input (the workspace
   picker, an approval dialog, the integrity panel, the future palette),
   that surface receives all keys including the suite. Global keybinds
   are **suspended** while a modal is active — `Esc` is the only chord
   that the modal itself must honour (it closes the modal).
3. **Focus-routed.** Default behavior for any key not in the suite: route
   to the chrome page if focus is chrome, to the active content page
   otherwise. Existing `route_key` behavior; no change.

### Contextual `⌘W` (`Ctrl+W`) behavior

`Ctrl+W` is the only chord with state-dependent semantics:
- **`tabs.len() > 1`** → close the active tab; focus moves to the
  neighbouring tab.
- **`tabs.len() == 1`** → close the window (equivalent to `Ctrl+Shift+W`).
  Matches Chrome / Safari / Firefox behavior; without this, the user
  hitting `Ctrl+W` on the last tab would either be ignored or open a
  fresh tab (current behavior — `close_tab` re-opens a fresh tab when
  closing the last one). The contextual close-window is the established
  convention.

`Ctrl+Shift+W` always closes the window regardless of tab count.

### `⌘N` (new window) — deferred

`Ctrl+N` requires multi-window state in the shell. The shell is currently
single-window. Adding multi-window is a real scope expansion (window
lifecycle, per-window state, focus tracking across windows, the workspace
mapping question). Defer to a follow-up ADR that scopes the multi-window
work as a whole; v0.1 ships without `Ctrl+N`.

### Plugins cannot register global keybinds in v0.1

The keybind table is **closed** in v0.1. Plugin manifests cannot declare
new global chords. The future capability name `keybind:register-global`
is reserved (named here, not enforced) so the eventual grant UI is
forward-compatible. Enforcement, conflict resolution between
plugin-registered chords, and the per-scope binding model are the subject
of a future ADR.

Plugins can still receive key events through the normal focus-routed path
when their UI (e.g. a chrome-bound element they author) has focus —
that's not a keybind registration, it's standard DOM event handling.

### User customization is planned, not precluded

Per-user chord remapping — overriding any chord in the v0.1 table, binding
new chords to existing actions, and (when plugin-registered actions land)
binding chords to plugin actions — is **explicitly an intended future
feature, not closed by this ADR**. The v0.1 closure is a simplicity
choice for the initial release, not a stance on what the keybind system
should be.

The architecture supports this additively:
- The keybind reference panel (P6) reads from a live registry, so adding
  user overrides is purely a registry-write + lookup-order change. No
  rework of the v0.1 table required.
- The future user-keybind ADR will scope: the override file format
  (managed.lua per the canonical-config-set memory), conflict resolution
  rules (user override beats built-in beats plugin), the chord-validity
  rule (can a user remove a chord entirely? bind multiple chords to one
  action?), and the live-reload behavior. None of those are decided here.

Nothing in v0.1 makes any of that harder to add later.

## Consequences

- Good: every keybind has a recorded chord, action, and scope. Future
  contributors do not have to read the event loop to know what's bound.
- Good: `⌘W` matches the user's mental model from every other browser; no
  surprise "I closed my last tab and nothing happened."
- Good: the closure of plugin-registered keybinds keeps v0.1 simple; the
  reserved capability name keeps the future open.
- Bad: `Ctrl+N` is missing for v0.1; users wanting a second window must
  launch a second Mote process (acceptable given multi-window's real
  scope, but called out).
- Bad: the `Ctrl+9` "last workspace" rule (vs. literal 9th) is a
  Chrome-ism that will surprise users who haven't internalised it;
  mitigated by the P6 keybinds reference panel.
- Bounded scope: this ADR is the v0.1 chord table only. Adding any new
  global chord post-v0.1 is a contract change that updates this ADR;
  removing any is a breaking change that supersedes it.

## Relationship to existing ADRs

- **Independent of ADR-0005 / ADR-0007.** The keybind path is shell-side
  event interception; no host-bridge ops, no privileged-origin
  interaction. The chord-to-action map is internal to the shell.
- **Forward-references a future plugin-keybind ADR** (the
  `keybind:register-global` capability). That ADR will need to address
  conflict resolution (plugin-vs-builtin chord collisions), per-workspace
  vs. global scoping, and the binding lifecycle (manifest declaration vs.
  runtime registration). None of those are in scope here.
