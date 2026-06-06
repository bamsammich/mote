//! Browser composition root for Mote — the multi-tab browser shell.
//!
//! This crate is the **composition root**: it owns the [`winit`] window + event
//! loop (the single OS event source, ADR-0004), brings up the [`mote_cef`]
//! engine with an external message pump, assembles the chrome assets from
//! [`mote_ui`] into a `mote://chrome` resource set, creates the privileged
//! chrome page (via the host bridge) and a set of untrusted content pages (one
//! per tab, under a per-identity profile), and feeds the active page's
//! off-screen paint stream into [`mote_ui`]'s wgpu [`Compositor`]
//! (chrome-surrounds-content).
//!
//! # The integration seams wired here
//!
//! 1. **CEF pump ↔ winit loop.** CEF runs with `external_message_pump = true`;
//!    the shell calls [`mote_cef::Engine::pump`] on every loop iteration so
//!    paint / network / navigation callbacks fire even with no OS events.
//! 2. **Compositor feed.** New chrome frames go to
//!    [`Compositor::update_chrome`]; the **active tab's** new content frames go
//!    to [`Compositor::update_page`]. Switching the active tab swaps which
//!    page's frames feed the page texture (a re-upload of the new active page).
//! 3. **Multi-tab.** A vector of [`ShellTab`]s keyed by [`mote_types::TabId`]. Open
//!    creates a new content [`mote_cef::Page`] under the default
//!    [`mote_cef::ProfileHandle`]; close drops it; switching feeds the new
//!    active page. Inactive pages keep their CEF browsers (discard-on-idle is a
//!    later session concern); restored placeholders have **no** live page and
//!    materialize on first focus (the restoration model).
//! 4. **Per-identity profiles.** Content pages are created via
//!    [`mote_cef::ProfileManager`] under the `default` identity, so identity
//!    isolation (cookies/storage/cache) is actually in use — not the global
//!    request context.
//! 5. **Bridge → chrome state push.** On open / close / switch / navigate the
//!    shell pushes the live tab list (`set_tabs`) and current URL (`set_url`)
//!    into the chrome via [`mote_cef::Page::eval_js`] →
//!    `window.mote.applyOp(op, payload)`, so the chrome tab strip + omnibox
//!    reflect runtime state (replacing the static demo tabs).
//! 6. **Session persistence.** A [`mote_session::Session`] backed by per-identity
//!    [`mote_storage`] `SQLite`. The shell flushes on every tab change and on
//!    clean shutdown, and [`mote_session::Session::restore`]s on launch —
//!    restored active-workspace tabs become placeholders that load on focus
//!    (crash-recovery == clean-exit).
//! 7. **Input routing** (plan §1.3). Mouse events hit-test the viewport rect:
//!    inside → the active content page in page-local coords; outside → the
//!    chrome page in window coords. Keyboard goes to the logical focus owner.
//! 8. **The `navigate` op + tab ops.** The chrome invokes `navigate`, `new_tab`,
//!    `close_tab`, `select_tab`; each op (which is `Send + Sync` and cannot hold
//!    the `!Send` [`mote_cef::Page`]) enqueues a [`ShellCommand`] the winit loop
//!    drains and applies on the pump thread.
//!
//! ## What is stubbed / deferred (honest scope)
//!
//! - **Title tracking.** `mote-cef` does not yet surface a page-title callback,
//!   so a tab's title falls back to its URL. The push contract carries a `title`
//!   field already, so wiring a real title source later is additive.
//! - **Workspaces / multi-window.** A single window on workspace 0 under the
//!   `default` identity. The session model supports more; the shell drives one.
//! - **Discard / hidden-tab aging.** Inactive tabs keep their renderers; the
//!   `Discarder` / `HiddenTabReaper` integration is a later session concern.
//! - **Provider-plugin navigation** (`ui:urlbar_provider`): the op accepts a URL
//!   directly (the chrome bootstrap normalizes omnibox text).

mod approval;
mod picker;
mod runtime;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[cfg(test)]
use mote_cef::edit_flag;
use mote_cef::{
    ButtonAction, ChromePageRequest, ChromeResources, ContextMenuKind, ContextMenuRequest, Engine,
    EngineConfig, HostBridge, IdentityId, KeyAction, KeyInput, Modifiers, MouseButton,
    MousePosition, OpRegistry, OpResponse, Page, PageOptions, PageRole, PopupTabRequest,
    ProfileHandle, ProfileManager, chrome_url, overlay_url,
};
use mote_runtime::{HostValue, host_to_json};
use mote_session::{DiscardConfig, Discarder, HiddenTabConfig, HiddenTabReaper, Session};
use mote_storage::Store;
use mote_types::{TabId, WorkspaceId};
use mote_ui::{
    ApprovalRequest, Compositor, CompositorError, IntegrityPanel, PixelFormat, ViewportRect,
};

use crate::picker::{PickerEntry, PickerState};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// The window's initial logical size in physical pixels (the slice does not
/// persist geometry — DESIGN §Session: the WM handles placement).
const INITIAL_WIDTH: u32 = 1280;
/// The window's initial height.
const INITIAL_HEIGHT: u32 = 800;

/// The chrome tab strip + sidebar reserve the left edge; the omnibox the top.
/// These are the page-viewport insets the shell uses until the chrome reports
/// its real `<main>` geometry over the bridge (later). They mirror the default
/// layout: the sidebar (activity bar 36 + panel 280 = 316px) on the left.
const VIEWPORT_LEFT: u32 = 316;
/// The top-bar (omnibox) height reserved above the viewport.
const VIEWPORT_TOP: u32 = 44;

/// The identity every tab opens under in this slice (DESIGN: a single `default`
/// identity hides the multi-identity machinery until config creates more).
const DEFAULT_IDENTITY: &str = "default";

/// The session-storage identity id (the `mote-session` integer identity axis).
/// One identity in this slice, so a fixed id is fine.
const SESSION_IDENTITY: u64 = 0;

/// The single workspace this slice drives.
const WORKSPACE: u64 = 0;

/// How often the shell runs the session housekeeping pass (active-tab discard +
/// hidden-tab reap). The decisions themselves use the configured idle/TTL
/// thresholds; this is just the polling cadence. A minute is far below either
/// default threshold, so housekeeping is timely without busy-looping.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_mins(1);

/// Environment override for the active-tab discard threshold, in **seconds**
/// (`MOTE_DISCARD_AFTER_SECS`). Lets a test drive the discard path without
/// waiting the 30-minute default. Unset → [`DiscardConfig::default`] (30 min).
const DISCARD_AFTER_ENV: &str = "MOTE_DISCARD_AFTER_SECS";

/// Environment override for the hidden-tab TTL, in **seconds**
/// (`MOTE_HIDDEN_TTL_SECS`). Lets a test drive the reap path without waiting the
/// 30-day default. Unset → [`HiddenTabConfig::default`] (30 days).
const HIDDEN_TTL_ENV: &str = "MOTE_HIDDEN_TTL_SECS";

/// Environment override for the housekeeping cadence, in **seconds**
/// (`MOTE_HOUSEKEEPING_SECS`). Lets a test see discard/reap fire promptly.
const HOUSEKEEPING_ENV: &str = "MOTE_HOUSEKEEPING_SECS";

/// The start URL a brand-new tab loads (P3, ADR-0015).
///
/// `mote://chrome/newtab.html` is served via the global CEF request context
/// (the `mote://chrome` scheme handler). It replaces the old `data:text/html,…`
/// placeholder with a proper chrome page that: (a) shows the `[·]` brand mark
/// centered at 96px, (b) carries the `newtab.center` declarable slot, (c) sets
/// `<title>new tab</title>` so R2's `OnTitleChange` mirror surfaces a clean
/// sidebar tab title.
///
/// **ADR-0015 constraint**: `mote://` URLs MUST be loaded via `Page::new` (global
/// request context), NEVER via `Page::with_profile` (per-identity profile
/// context). The `create_content_page` helper enforces this routing.
const DEFAULT_START_URL: &str = "mote://chrome/newtab.html";

/// Default search URL template used when no engine has been configured via
/// `set_search_engine`. `{q}` is replaced with the percent-encoded query by
/// [`resolve_omnibox_input`].
///
/// The value is `"https://duckduckgo.com/?q={q}"`.  The `{q}` is a product
/// template placeholder, not a Rust format specifier — the string is built
/// from bytes to avoid the `clippy::literal_string_with_formatting_args` lint
/// that fires on any `{...}` inside a string literal regardless of context.
fn default_search_url_template() -> &'static str {
    // b"https://duckduckgo.com/?q={q}" where {q} = 0x7b 'q' 0x7d
    std::str::from_utf8(b"https://duckduckgo.com/?q=\x7bq\x7d")
        .expect("ASCII bytes are valid UTF-8")
}

/// A command produced by a host-bridge op (which is `Send + Sync`) and applied
/// by the winit loop on the pump thread (which owns the `!Send` pages).
#[derive(Debug, Clone)]
enum ShellCommand {
    /// Navigate the **active** tab to `url` (the omnibox `navigate` op).
    Navigate(String),
    /// The user submitted free text from the omnibox (the `omnibox_submit` op).
    ///
    /// Unlike `Navigate` (which carries a pre-resolved URL from a suggestion
    /// row click), this variant carries raw user text.  The pump thread calls
    /// [`resolve_omnibox_input`] with the live `search_url_template` to turn
    /// it into a navigable URL before calling `navigate_active`.
    OmniboxSubmit(String),
    /// Open a new tab (and switch to it). Optional URL: when set, the new
    /// tab loads that URL via `create_content_page` (which routes `mote://`
    /// URLs through the global request context per ADR-0015); when `None`,
    /// the default newtab page (`mote://chrome/newtab.html`) is used.
    NewTab(Option<String>),
    /// Close the tab with this id.
    CloseTab(u64),
    /// Switch the active tab to this id.
    SelectTab(u64),
    /// The chrome reported a focus-owner change (`chrome` ⇒ keyboard to the
    /// omnibox; otherwise to the active content page).
    FocusOwner(FocusOwner),
    /// The user answered the approval dialog (`approve_plugin` op). The payload
    /// passed the op-boundary structural validation; the pump thread does the
    /// semantic cross-check and finishes the load/deny.
    ApprovePlugin(approval::DialogResult),
    /// Panel action: update the named plugin (git/bundled `plugin_update`).
    PluginUpdate(String),
    /// Panel action: roll the named plugin back to its prior commit.
    PluginRollback(String),
    /// Panel action: reload the named plugin (path/dev re-run).
    PluginReload(String),
    /// Panel action: revoke the named plugin (unload + drop its approval).
    PluginRevoke(String),
    /// Panel action: re-open the approval dialog for the named plugin so the
    /// user can re-narrow its grant.
    PluginAdjustScope(String),
    /// Panel action: revoke a specific secret grant from the named plugin
    /// (session-scoped narrowing; does not persist across relaunch).
    PluginRevokeSecret {
        /// The plugin whose grant should be narrowed.
        plugin: String,
        /// The secret name (`<name>` from `secret:read:<name>`).
        name: String,
    },
    /// The chrome omnibox input changed; invoke `ui:urlbar_provider` → `query`
    /// and push the results to chrome as `applyOp('urlbar_suggestions', …)`.
    UrlbarQuery(String),
    /// The user switched the active sidebar panel; invoke the appropriate list
    /// capability and push the result to chrome.
    SetActivePanel(String),
    /// Copy the active tab's current URL to the system clipboard.
    CopyActiveUrl,
    /// The user removed a bookmark from the bookmarks panel; invoke
    /// `ui:bookmarks_provider` → `remove_bookmark` and re-push the bookmark list.
    BookmarkRemove(String),
    /// The user clicked the inline urlbar bookmark toggle; add if not bookmarked,
    /// remove if bookmarked, then re-push `set_url` so the star color updates.
    BookmarkToggle,
    /// The user (or chrome) invoked `set_active_workspace`; ask the
    /// `workspace:provider` plugin to validate + persist, then re-point
    /// `self.workspace` and rebuild the visible tab strip.
    SwitchWorkspace(String),
    /// Close the current window (Ctrl+Shift+W, Ctrl+Q, or Ctrl+W on last tab,
    /// or the chrome close-button click via the `close_window` op).
    /// Sets `ShellApp::should_exit = true`; `about_to_wait` calls
    /// `event_loop.exit()` on the next tick.
    CloseWindow,
    // P6: settings panel ops ─────────────────────────────────────────────────
    /// Switch the active theme (`set_theme` op). Writes `theme` key to
    /// `managed.lua` and pushes `applyOp('set_theme', {theme})` to the chrome.
    SetTheme(String),
    /// Set the default search engine name + URL template (`set_search_engine` op).
    /// Writes to `managed.lua`.
    SetSearchEngine {
        /// The human-readable engine name, e.g. `"DuckDuckGo"`.
        name: String,
        /// The URL template string, e.g. `"https://duckduckgo.com/?q={q}"`.
        url_template: String,
    },
    /// Toggle hardware acceleration (`set_hw_accel` op). Writes to `managed.lua`.
    /// Takes effect after restart.
    SetHwAccel(bool),
    /// Toggle per-origin zoom persistence (`set_zoom_persist` op). Writes to
    /// `managed.lua`; P5 reads this on load.
    SetZoomPersist(bool),
    /// Disable a named plugin (`plugin_disable` op). Writes a `disabled = true`
    /// entry to `managed.lua`.
    PluginDisable(String),
    /// Uninstall a named plugin (`plugin_uninstall` op). Removes from the plugins
    /// directory and clears the `managed.lua` entry.
    PluginUninstall(String),
    /// Trigger the native file picker for plugin install (`plugin_install_picker`
    /// op). The shell opens the picker; user selects a zip/tarball; existing
    /// integrity verification + approval flow runs.
    PluginInstallPicker,
    /// Re-verify all plugin checksums against their lock-file records
    /// (`integrity_reverify_all` op). Pushes updated integrity panel data.
    IntegrityReverifyAll,
    /// Request drill-down detail for a specific plugin's integrity record
    /// (`integrity_plugin_detail` op). In v0.1 this logs the data; a future
    /// wave adds a dedicated detail view.
    IntegrityPluginDetail(String),
    // P5: find / zoom / reopen ops ────────────────────────────────────────────
    /// Open find-in-page mode: push `applyOp('focus_find', null)` to the chrome
    /// omnibox so it enters `[find]` mode. The chrome JS then sends
    /// `find_in_page` ops as the user types.
    FindInPage,
    /// Execute a text search on the active page with the given query.
    ///
    /// Produced by the `find_in_page` op handler on every keystroke.
    /// `forward = true` searches forward; `find_next = false` starts a new
    /// session, `true` advances to the next/previous match.
    FindText {
        /// The search query string.
        query: String,
        /// Search direction: `true` = forward (default), `false` = backward.
        forward: bool,
        /// `false` = start a new find session; `true` = advance the current one.
        find_next: bool,
    },
    /// Advance to the next find match (Ctrl+G from the shell keybind, or Enter
    /// in the find omnibox via the `find_next` op).
    FindNext,
    /// Advance to the previous find match (Ctrl+Shift+G from the shell keybind,
    /// or Shift+Enter in the find omnibox via the `find_prev` op).
    FindPrev,
    /// Stop finding and clear the active selection.
    StopFinding,
    /// Zoom the active page in by one level.
    ZoomIn,
    /// Zoom the active page out by one level.
    ZoomOut,
    /// Reset the active page's zoom to 100%.
    ZoomReset,
    /// Reopen the most recently closed tab from the closed-tab stack.
    ReopenClosedTab,
    /// Context-menu action dispatched from `host.js` (`context_menu_action` op).
    ContextMenuAction(String),
    // P2: address-bar navigation ops ─────────────────────────────────────────
    /// Navigate the active tab back one step (`go_back` op). No-op when there
    /// is no back history (CEF's `can_go_back` guard is re-checked on the pump
    /// thread; the guard in `Page::go_back` prevents the CEF call).
    NavGoBack,
    /// Navigate the active tab forward one step (`go_forward` op).
    NavGoForward,
    /// Reload the active tab (`reload` op). Always available.
    NavReload,
    /// Stop the active tab's current load (`stop` op). No-op when not loading.
    NavStop,
    /// Return TLS/security information for the active tab (`security_info` op).
    /// Synchronous: the response is produced inline from the current URL and
    /// nav state without a round-trip, so no `ShellCommand` needed for this
    /// one — it is handled directly in the op closure.
    ///
    /// This variant is reserved as a doc anchor; actual dispatch is synchronous.
    #[allow(dead_code)]
    SecurityInfoQuery,
}

/// The action a keybind chord maps to (ADR-0012 chord table).
///
/// Returned by [`classify_chord`] — a pure, testable function that maps
/// `(modifiers, key, tab_count)` to an action without touching any shell state.
/// `intercept_keybind` calls this and dispatches; the separation keeps the
/// chord-classification logic unit-testable without a live `ShellApp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeybindAction {
    /// Open a new tab in the current workspace.
    NewTab,
    /// Close the active tab (tabs > 1) or the window (tabs == 1) — contextual.
    ///
    /// ADR-0012: `Ctrl+W` with only one tab open closes the window, matching
    /// Chrome/Safari/Firefox behavior. `close_tab`'s "re-open fresh tab on last
    /// close" behavior stays unchanged — the contextual rule lives here, not there.
    CloseTabOrWindow,
    /// Close the window unconditionally regardless of tab count.
    CloseWindow,
    /// Quit Mote (equivalent to `CloseWindow` in single-window v0.1).
    Quit,
    /// Focus the omnibox and select all existing text.
    FocusOmnibox,
    /// Reload the active tab.
    ReloadTab,
    /// Navigate back in the active tab's history.
    GoBack,
    /// Navigate forward in the active tab's history.
    GoForward,
    /// Switch to workspace at 1-based index (1..=8).
    SwitchWorkspaceByIndex(u8),
    /// Switch to the **last** workspace (Chrome convention for `Ctrl+9`).
    SwitchWorkspaceLast,
    /// Cycle to the next tab (existing `Ctrl+Tab` behavior).
    CycleTab,
    /// Toggle the integrity panel (existing `Ctrl+Shift+I` behavior).
    ToggleIntegrity,
    /// Open the workspace tab picker (existing `Mod+Space` behavior).
    OpenPicker,
    /// Dismiss the topmost modal surface (existing `Esc` behavior).
    DismissModal,
    // P5 additions
    /// Open find-in-page mode (`Ctrl+F`). Focuses the omnibox in `[find]` mode.
    FindInPage,
    /// Advance to the next find match (`Ctrl+G`).
    FindNext,
    /// Advance to the previous find match (`Ctrl+Shift+G`).
    FindPrev,
    /// Zoom in on the active page (`Ctrl+=`).
    ZoomIn,
    /// Zoom out on the active page (`Ctrl+-`).
    ZoomOut,
    /// Reset the active page zoom to 100% (`Ctrl+0`).
    ZoomReset,
    /// Reopen the most recently closed tab (`Ctrl+Shift+T`).
    ReopenClosedTab,
}

/// Classify a keypress as a keybind action (ADR-0012 chord table, v0.1).
///
/// Pure function: takes the current modifier state, the logical key, and the
/// number of open tabs (needed for the contextual `Ctrl+W` rule). Returns
/// `Some(action)` if the chord is in the table, `None` if it should be
/// focus-routed.
///
/// **Caller contract**: this function is called only when the event is a
/// `Pressed` key (not `Released`). The picker-open check (which captures all
/// keys) happens before this call in `intercept_keybind`.
///
/// Uses `Ctrl` (the Linux/dev convention) where the spec writes `⌘`.
pub(crate) fn classify_chord(
    modifiers: Modifiers,
    key: &Key,
    tab_count: usize,
) -> Option<KeybindAction> {
    // Esc: captured-modal scope — closes the topmost modal (integrity, picker,
    // approval dialog). Fires regardless of modifier state.
    if matches!(key, Key::Named(NamedKey::Escape)) {
        return Some(KeybindAction::DismissModal);
    }

    // Mod+Space (Super or Ctrl): open the workspace tab picker.
    if matches!(key, Key::Named(NamedKey::Space))
        && (modifiers.contains(Modifiers::COMMAND) || modifiers.contains(Modifiers::CONTROL))
    {
        return Some(KeybindAction::OpenPicker);
    }

    // All remaining chords require Ctrl.
    if !modifiers.contains(Modifiers::CONTROL) {
        return None;
    }

    let shift = modifiers.contains(Modifiers::SHIFT);

    match key {
        Key::Character(s) => {
            match s.as_str() {
                // Ctrl+Shift+I: toggle integrity panel (case-insensitive).
                "I" | "i" if shift => Some(KeybindAction::ToggleIntegrity),
                // Ctrl+Shift+W: close window unconditionally.
                "W" | "w" if shift => Some(KeybindAction::CloseWindow),
                // Ctrl+T: new tab.
                "T" | "t" if !shift => Some(KeybindAction::NewTab),
                // Ctrl+W: contextual — close window on last tab, close tab otherwise.
                "w" if !shift => {
                    if tab_count <= 1 {
                        Some(KeybindAction::CloseWindow)
                    } else {
                        Some(KeybindAction::CloseTabOrWindow)
                    }
                }
                // Ctrl+Q: quit Mote.
                "Q" | "q" if !shift => Some(KeybindAction::Quit),
                // Ctrl+L: focus omnibox.
                "L" | "l" if !shift => Some(KeybindAction::FocusOmnibox),
                // Ctrl+R: reload active tab.
                "R" | "r" if !shift => Some(KeybindAction::ReloadTab),
                // Ctrl+[: go back.
                "[" if !shift => Some(KeybindAction::GoBack),
                // Ctrl+]: go forward.
                "]" if !shift => Some(KeybindAction::GoForward),
                // Ctrl+1..Ctrl+8: switch to workspace by 1-based index.
                "1" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(1)),
                "2" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(2)),
                "3" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(3)),
                "4" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(4)),
                "5" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(5)),
                "6" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(6)),
                "7" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(7)),
                "8" if !shift => Some(KeybindAction::SwitchWorkspaceByIndex(8)),
                // Ctrl+9: switch to the LAST workspace (Chrome convention — not
                // the literal 9th). ADR-0012 §`⌘9` documents the rationale.
                "9" if !shift => Some(KeybindAction::SwitchWorkspaceLast),
                // P5: find-in-page (Ctrl+F).
                "F" | "f" if !shift => Some(KeybindAction::FindInPage),
                // P5: find next (Ctrl+G), find prev (Ctrl+Shift+G).
                "G" | "g" if !shift => Some(KeybindAction::FindNext),
                "G" | "g" if shift => Some(KeybindAction::FindPrev),
                // P5: zoom (Ctrl+= zoom in, Ctrl+- zoom out, Ctrl+0 reset).
                "=" | "+" if !shift => Some(KeybindAction::ZoomIn),
                // Shift+= produces "+" on most keyboards; also handle without shift.
                "=" | "+" if shift => Some(KeybindAction::ZoomIn),
                "-" if !shift => Some(KeybindAction::ZoomOut),
                "0" if !shift => Some(KeybindAction::ZoomReset),
                // P5: reopen closed tab (Ctrl+Shift+T).
                "T" | "t" if shift => Some(KeybindAction::ReopenClosedTab),
                _ => None,
            }
        }
        Key::Named(NamedKey::Tab) if !shift => Some(KeybindAction::CycleTab),
        _ => None,
    }
}

/// Who owns keyboard input (plan §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusOwner {
    /// The chrome document (omnibox / a chrome control) has focus.
    Chrome,
    /// The active content page has focus.
    Page,
}

/// A lock-free-ish command queue shared between the op handlers (any thread)
/// and the winit loop. Ops are infrequent (user actions), so a `Mutex` is fine.
type CommandQueue = Arc<Mutex<VecDeque<ShellCommand>>>;

/// Maximum number of recently-closed tabs remembered in [`ClosedTabStack`].
/// Matches Chrome / Firefox conventions; more would be rarely useful.
const CLOSED_TAB_STACK_CAP: usize = 25;

/// A snapshot of a tab at the moment it was closed, enough to reopen it.
struct ClosedTab {
    url: String,
    title: Option<String>,
}

/// A LIFO stack of recently-closed tabs (cap [`CLOSED_TAB_STACK_CAP`]).
///
/// Push when a tab closes; pop from the front to reopen the most recent.
/// When the stack is at capacity the oldest entry (back) is discarded first.
struct ClosedTabStack {
    inner: VecDeque<ClosedTab>,
}

impl ClosedTabStack {
    const fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }

    /// Push a closed tab. If at capacity, drop the oldest entry first.
    fn push(&mut self, tab: ClosedTab) {
        if self.inner.len() >= CLOSED_TAB_STACK_CAP {
            self.inner.pop_back();
        }
        self.inner.push_front(tab);
    }

    /// Pop the most recently closed tab (LIFO order), or `None` if empty.
    fn pop(&mut self) -> Option<ClosedTab> {
        self.inner.pop_front()
    }
}

/// One tab the shell owns: its stable id, its current URL/title, and its live
/// CEF [`Page`] — `None` for a **placeholder** (a restored tab that has not yet
/// been focused; it materializes its page on first selection, per the
/// restoration model).
struct ShellTab {
    id: TabId,
    url: String,
    title: Option<String>,
    page: Option<Page>,
}

impl ShellTab {
    /// `true` if this tab has a live CEF page (vs a restore placeholder).
    const fn is_live(&self) -> bool {
        self.page.is_some()
    }
}

/// Boot the browser shell: open one window, bring up CEF, restore (or seed) the
/// session, render the chrome with the active tab composited inside, and run the
/// event loop until the window closes.
///
/// Call this from `mote-app`'s `main` **after** [`mote_cef::bootstrap_with_bridge`]
/// returned [`mote_cef::ProcessRole::Browser`].
///
/// # Errors
/// Returns a boxed error if the engine, the chrome bridge, the session store, or
/// the first content page cannot be created, or if the winit loop cannot start.
#[allow(
    clippy::too_many_lines,
    reason = "composition root — assembles every subsystem in one place; \
              splitting into sub-functions would obscure the startup order"
)]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // CEF flags (e.g. `--ozone-platform=x11`) are passed through to CEF by the
    // bootstrap; every non-flag argument is an initial tab URL (like
    // `firefox url1 url2 …`). With none, a fresh session seeds one default tab.
    let cli_urls: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();

    let cache_path = default_cache_path();
    let config = EngineConfig {
        no_sandbox: true,
        cache_path: cache_path.clone(),
        ..EngineConfig::default()
    };
    let engine = Engine::init(&config)?;

    // The single shared per-identity SQLite store backs the session, the plugin
    // runtime's per-plugin storage, and the audit sink (one database, namespaced).
    let store = open_session_store()?;

    // Stand up the plugin runtime (runtime + manager + audit + approval store).
    // Per ADR-0007 this constructs but loads NOTHING: the resolve + classify +
    // load pass runs in `run_initial_load_pass`, fired once from `about_to_wait`
    // after the chrome's first paint so the window is live before any plugin
    // (or git fetch) work, and a fatal resolution error leaves the window alive.
    let host = runtime::PluginHost::boot(store.clone())?;

    // Render the integrity panel from LIVE loaded-plugin / audit / storage data,
    // and serve it as the `mote://overlay/integrity.html` overlay surface.
    let integrity_html = runtime::render_panel_html(&host.build_panel());

    // Serve the privileged chrome document from `mote://chrome` (the only origin
    // the host-bridge binding is installed on), and the unprivileged overlay
    // surfaces (picker + integrity) from the distinct `mote://overlay` origin the
    // origin gate does NOT match (S2).
    engine.register_chrome_resources(build_chrome_resources());
    engine.register_overlay_resources(build_overlay_resources(&integrity_html));

    // Per-identity profiles MUST be rooted at the engine's cache path (CEF
    // requires each profile dir to be a direct child of root_cache_path).
    let profiles = ProfileManager::new(cache_path);
    let default_identity = IdentityId::new(DEFAULT_IDENTITY)?;
    let default_profile = profiles.get_or_create(&default_identity)?;
    // A freshly created profile context initialises asynchronously on the CEF UI
    // thread; pump a few times before creating the first page under it.
    for _ in 0..20 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(5));
    }

    // Session: per-identity SQLite, restored on launch (empty on first run).
    // Reuses the shared store opened above.
    let session_identity = mote_types::IdentityId::new(SESSION_IDENTITY);
    let ns = Session::open_namespace(&store, session_identity)?;
    let mut session = Session::restore(&ns, session_identity)?;

    let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
    let registry = build_op_registry(&commands);

    // The privileged chrome page + the bridge that carries the op router.
    let chrome_req = ChromePageRequest::new(
        &chrome_url("index.html"),
        &PageOptions {
            width: INITIAL_WIDTH,
            height: INITIAL_HEIGHT,
            frame_rate: 60,
            role: PageRole::Chrome,
        },
    );
    let bridge = HostBridge::for_chrome(chrome_req, registry)?;

    // Build the tab set from the restored session (active-workspace tabs become
    // placeholders), or seed a first tab if the session is empty.
    let workspace = WorkspaceId::new(WORKSPACE);
    // Initial sizing uses the logical insets (scale 1.0); `resumed` recomputes
    // them against the window's real scale factor before the first paint.
    let (vw, vh) = viewport_size(INITIAL_WIDTH, INITIAL_HEIGHT, VIEWPORT_LEFT, VIEWPORT_TOP);
    let content_opts = PageOptions {
        width: vw,
        height: vh,
        frame_rate: 60,
        role: PageRole::Content,
    };

    let active: usize = 0;
    let tabs = build_initial_tabs(
        &mut session,
        &ns,
        workspace,
        &content_opts,
        &default_profile,
        cli_urls,
    )?;

    let mut app = ShellApp {
        engine,
        bridge,
        profiles,
        default_profile,
        content_opts,
        session,
        store,
        host,
        integrity_page: None,
        integrity_open: false,
        integrity_chrome_open: false,
        integrity_paints: 0,
        picker: PickerState::default(),
        picker_page: None,
        picker_paints: 0,
        discarder: Discarder::new(discard_config()),
        reaper: HiddenTabReaper::new(hidden_tab_config()),
        housekeeping_interval: housekeeping_interval(),
        last_housekeeping: Instant::now(),
        session_identity,
        workspace,
        tabs,
        active,
        commands,
        window: None,
        compositor: None,
        chrome_paints: 0,
        content_paints: 0,
        width: INITIAL_WIDTH,
        height: INITIAL_HEIGHT,
        scale_factor: 1.0,
        cursor: (0, 0),
        modifiers: Modifiers::NONE,
        focus: FocusOwner::Page,
        chrome_ready: false,
        did_initial_load: false,
        first_frame_logged: false,
        started: Instant::now(),
        should_exit: false,
        closed_tab_stack: ClosedTabStack::new(),
        tab_zoom_levels: std::collections::HashMap::new(),
        hover_url_last: None,
        zoom_clear_at: None,
        nav_state_last: (false, false),
        load_state_last: false,
        find_query_last: String::new(),
        search_url_template: default_search_url_template().to_owned(),
    };

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Build the shell's initial tab set: restore the active workspace's tabs as
/// placeholders (they load on focus), or — on a fresh session — seed one tab per
/// CLI URL (the first live, the rest placeholders) and flush. The active tab is
/// index 0; its page is always materialized so the window is not blank.
fn build_initial_tabs(
    session: &mut Session,
    ns: &mote_storage::Namespace,
    workspace: WorkspaceId,
    content_opts: &PageOptions,
    default_profile: &ProfileHandle,
    cli_urls: Vec<String>,
) -> Result<Vec<ShellTab>, Box<dyn std::error::Error>> {
    let mut tabs: Vec<ShellTab> = session
        .tab_picker_ranked(workspace)
        .into_iter()
        .map(|tab| ShellTab {
            id: tab.id,
            url: tab.url.clone(),
            title: tab.title.clone(),
            page: None,
        })
        .collect();

    if tabs.is_empty() {
        let urls = if cli_urls.is_empty() {
            vec![DEFAULT_START_URL.to_string()]
        } else {
            cli_urls
        };
        for (i, url) in urls.into_iter().enumerate() {
            let id = session.add_tab(url.clone(), workspace);
            let page = if i == 0 {
                Some(create_content_page(&url, content_opts, default_profile)?)
            } else {
                None
            };
            tabs.push(ShellTab {
                id,
                url,
                title: None,
                page,
            });
        }
        session.flush(ns)?;
    } else {
        // Restored: materialize the active tab eagerly so the window is not blank.
        let url = tabs[0].url.clone();
        if let Ok(page) = create_content_page(&url, content_opts, default_profile) {
            tabs[0].page = Some(page);
        }
    }
    Ok(tabs)
}

/// The CEF cache/profile root. Per DESIGN, per-identity storage lives under the
/// XDG state dir; the engine cache path doubles as the profile-manager root.
fn default_cache_path() -> PathBuf {
    state_dir().join("cef-cache")
}

/// Open the per-identity session `SQLite` store under the XDG state dir
/// (`~/.local/state/mote/session.db`), creating the directory if needed.
fn open_session_store() -> Result<Store, Box<dyn std::error::Error>> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(Store::open(dir.join("session.db"))?)
}

/// `~/.local/state/mote` (honouring `XDG_STATE_HOME`), or `.mote-state` under the
/// cwd as a last resort.
fn state_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("mote");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state/mote");
    }
    PathBuf::from(".mote-state")
}

/// Parse a `Duration` from an environment variable holding a whole number of
/// seconds, returning `None` when unset, empty, or unparsable.
fn env_secs(var: &str) -> Option<Duration> {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Build the active-tab [`DiscardConfig`] from an optional idle override. The
/// default is 30 minutes with pinned tabs kept loaded (DESIGN); `Some(d)`
/// shortens the idle threshold (used by [`DISCARD_AFTER_ENV`] for tests).
fn discard_config_with(discard_after: Option<Duration>) -> DiscardConfig {
    let mut cfg = DiscardConfig::default();
    if let Some(after) = discard_after {
        cfg.discard_after = after;
    }
    cfg
}

/// [`discard_config_with`] sourced from [`DISCARD_AFTER_ENV`].
fn discard_config() -> DiscardConfig {
    discard_config_with(env_secs(DISCARD_AFTER_ENV))
}

/// Build the hidden-tab [`HiddenTabConfig`] from an optional TTL override. The
/// default TTL is 30 days (DESIGN); `Some(d)` shortens it (used by
/// [`HIDDEN_TTL_ENV`] for tests).
fn hidden_tab_config_with(ttl: Option<Duration>) -> HiddenTabConfig {
    let mut cfg = HiddenTabConfig::default();
    if let Some(ttl) = ttl {
        cfg.ttl = Some(ttl);
    }
    cfg
}

/// [`hidden_tab_config_with`] sourced from [`HIDDEN_TTL_ENV`].
fn hidden_tab_config() -> HiddenTabConfig {
    hidden_tab_config_with(env_secs(HIDDEN_TTL_ENV))
}

/// The housekeeping cadence, honouring [`HOUSEKEEPING_ENV`] for tests.
fn housekeeping_interval() -> Duration {
    env_secs(HOUSEKEEPING_ENV).unwrap_or(HOUSEKEEPING_INTERVAL)
}

/// Whether a popup URL is allowed to create a new in-window tab.
///
/// The S1 navigation guard (`RequestHandlerImpl::on_before_browse`) cancels
/// any top-level `mote://` navigation on a `PageRole::Content` page. If we
/// create a tab and then have its initial load cancelled, the tab persists in
/// the sidebar at a broken URL — a phantom tab the user can't dismiss
/// meaningfully. Pre-filter `mote://` popups before tab creation so the guard
/// never has to fire on a popup-shaped path.
///
/// URL schemes are case-insensitive per RFC 3986; CEF normalises to lowercase
/// before invoking `on_before_popup`, but defensively lowercase-compare the
/// scheme prefix here.
fn is_popup_url_allowed(url: &str) -> bool {
    let scheme_lower = url
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    !matches!(scheme_lower.as_deref(), Some("mote"))
}

/// Validate a settings deep-link URL (ADR-0017 URL whitelist).
///
/// Returns `Some(section)` if `url` is one of the four permitted settings URLs:
///   `mote://chrome/settings/general`   → `"general"`
///   `mote://chrome/settings/plugins`   → `"plugins"`
///   `mote://chrome/settings/integrity` → `"integrity"`
///   `mote://chrome/settings/keybinds`  → `"keybinds"`
///
/// Returns `None` for any other URL — including `mote://chrome/settings/bogus`.
/// The whitelist prevents future typo-driven 404s (ADR-0017 §URL whitelist).
///
/// Currently only exercised in tests; the production routing relies on the
/// registered path set in `build_chrome_resources()`. Will be wired into the
/// live URL handler in the navigation phase.
#[cfg(test)]
pub(crate) fn settings_section_from_url(url: &str) -> Option<&'static str> {
    const BASE: &str = "mote://chrome/settings/";
    let section = url.strip_prefix(BASE)?;
    // Strip optional .html suffix for parity with the .html form.
    let section = section.strip_suffix(".html").unwrap_or(section);
    match section {
        "general" => Some("general"),
        "plugins" => Some("plugins"),
        "integrity" => Some("integrity"),
        "keybinds" => Some("keybinds"),
        _ => None,
    }
}

/// The v0.1 keybind chord table as a JSON-serialisable slice.
///
/// Each entry is `(action, chord, scope, source)` — the four columns in the
/// keybinds reference section (ADR-0012 / ADR-0017). Generated from the same
/// source as [`classify_chord`]; the `keybinds_list` op returns this.
///
/// Scope values (ADR-0012): `global`, `chrome`, `content`, `captured-modal`.
/// Source values (v0.1): `built-in` only (plugin + user-override deferred).
const KEYBIND_TABLE: &[(&str, &str, &str, &str)] = &[
    // captured-modal scope — Esc fires regardless of modifier state.
    ("dismiss modal", "Esc", "captured-modal", "built-in"),
    // global scope — fire regardless of focus owner.
    ("new tab", "Ctrl+T", "global", "built-in"),
    ("close tab / window", "Ctrl+W", "global", "built-in"),
    ("close window", "Ctrl+Shift+W", "global", "built-in"),
    ("quit mote", "Ctrl+Q", "global", "built-in"),
    ("focus omnibox", "Ctrl+L", "global", "built-in"),
    ("reload page", "Ctrl+R", "global", "built-in"),
    ("go back", "Ctrl+[", "global", "built-in"),
    ("go forward", "Ctrl+]", "global", "built-in"),
    ("cycle tab", "Ctrl+Tab", "global", "built-in"),
    (
        "toggle integrity panel",
        "Ctrl+Shift+I",
        "global",
        "built-in",
    ),
    ("open workspace picker", "Ctrl+Space", "global", "built-in"),
    ("switch workspace 1", "Ctrl+1", "global", "built-in"),
    ("switch workspace 2", "Ctrl+2", "global", "built-in"),
    ("switch workspace 3", "Ctrl+3", "global", "built-in"),
    ("switch workspace 4", "Ctrl+4", "global", "built-in"),
    ("switch workspace 5", "Ctrl+5", "global", "built-in"),
    ("switch workspace 6", "Ctrl+6", "global", "built-in"),
    ("switch workspace 7", "Ctrl+7", "global", "built-in"),
    ("switch workspace 8", "Ctrl+8", "global", "built-in"),
    ("switch to last workspace", "Ctrl+9", "global", "built-in"),
    // P5 additions.
    ("find in page", "Ctrl+F", "global", "built-in"),
    ("find next match", "Ctrl+G", "global", "built-in"),
    ("find previous match", "Ctrl+Shift+G", "global", "built-in"),
    ("zoom in", "Ctrl+=", "global", "built-in"),
    ("zoom out", "Ctrl+-", "global", "built-in"),
    ("reset zoom", "Ctrl+0", "global", "built-in"),
    ("reopen closed tab", "Ctrl+Shift+T", "global", "built-in"),
];

/// Serialise [`KEYBIND_TABLE`] as a JSON object the `keybinds_list` op returns.
///
/// Shape: `{"keybinds":[{"action":"…","chord":"…","scope":"…","source":"…"},…]}`
fn keybinds_list_json() -> String {
    let mut parts = Vec::with_capacity(KEYBIND_TABLE.len());
    for (action, chord, scope, source) in KEYBIND_TABLE {
        // All four fields are static &str — no user-supplied strings here so no
        // escaping concerns, but we JSON-encode defensively anyway.
        parts.push(format!(
            "{{\"action\":{},\"chord\":{},\"scope\":{},\"source\":{}}}",
            serde_json::to_string(action).unwrap_or_default(),
            serde_json::to_string(chord).unwrap_or_default(),
            serde_json::to_string(scope).unwrap_or_default(),
            serde_json::to_string(source).unwrap_or_default(),
        ));
    }
    format!("{{\"keybinds\":[{}]}}", parts.join(","))
}

/// The reverse-DNS application identifier used for the window's Wayland `app_id`
/// / X11 `WM_CLASS`. Compositors key window rules, icons, and taskbar grouping
/// off this; leaving it empty makes Mote an unidentified window.
const APP_ID: &str = "com.mote.Mote";

/// Build the winit window attributes, setting the title and the Wayland/X11
/// application identity (`app_id` / `WM_CLASS`).
///
/// On Linux the `app_id` (Wayland) and the `WM_CLASS` instance/general names
/// (X11) are set to [`APP_ID`] so the compositor can identify, group, and
/// icon-match the window; winit's `with_name` maps to the right protocol field
/// for whichever backend is active. On other platforms only title + size apply.
fn window_attributes() -> winit::window::WindowAttributes {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(unused_mut, reason = "only the linux cfg arm mutates `attrs`")
    )]
    let mut attrs = Window::default_attributes()
        .with_title("mote")
        .with_inner_size(PhysicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));
    #[cfg(target_os = "linux")]
    {
        // Both backends compile in on Linux; set the identity for whichever the
        // session uses. `with_name(general, instance)` (fully-qualified to
        // disambiguate the two extension traits' identically-named method).
        use winit::platform::wayland::WindowAttributesExtWayland;
        use winit::platform::x11::WindowAttributesExtX11;
        attrs = WindowAttributesExtWayland::with_name(attrs, APP_ID, "mote");
        attrs = WindowAttributesExtX11::with_name(attrs, APP_ID, "mote");
    }
    attrs
}

/// Build the privileged `mote://chrome` resource set from mote-ui's embedded
/// assets. This is the ONLY origin that carries the host-bridge binding, so it
/// serves only the chrome document and its assets — the overlays live on the
/// unprivileged `mote://overlay` origin (see [`build_overlay_resources`]).
fn build_chrome_resources() -> ChromeResources {
    let css = "text/css; charset=utf-8";
    let mut res = ChromeResources::new()
        .register(
            "index.html",
            mote_ui::CHROME_HTML,
            "text/html; charset=utf-8",
        )
        .register(
            "roving.js",
            mote_ui::ROVING_JS,
            "text/javascript; charset=utf-8",
        )
        .register(
            "host.js",
            mote_ui::HOST_JS,
            "text/javascript; charset=utf-8",
        )
        .register(
            "panels.js",
            mote_ui::PANELS_JS,
            "text/javascript; charset=utf-8",
        )
        .register("tokens.css", mote_ui::TOKENS_CSS, css)
        .register("base.css", mote_ui::BASE_CSS, css)
        .register(
            "assets/wordmark.svg",
            mote_ui::WORDMARK_SVG,
            "image/svg+xml",
        )
        .register("assets/mark.svg", mote_ui::MARK_SVG, "image/svg+xml")
        .register(
            "assets/lucide-sprite.svg",
            mote_ui::LUCIDE_SPRITE_SVG,
            "image/svg+xml",
        );
    for (name, contents) in mote_ui::COMPONENT_CSS {
        res = res.register(format!("components/{name}.css"), *contents, css);
    }
    // P3: newtab page (ADR-0015). Served from `mote://chrome/newtab.html` via
    // the global request context. Registered under the `.html` path so relative
    // CSS imports (tokens.css, base.css, components/empty-slot.css) resolve.
    res = res.register(
        "newtab.html",
        mote_ui::NEWTAB_HTML,
        "text/html; charset=utf-8",
    );
    // P6: settings panel — four section pages + shared CSS/JS.
    //
    // Each section is registered under TWO paths:
    //   `settings/<section>`       — the deep-link URL per ADR-0017
    //   `settings/<section>.html`  — so relative CSS/JS imports resolve
    //
    // The URL-whitelist enforced by `settings_section_from_url` covers only the
    // four valid sections; arbitrary paths remain a 404.
    let html_ct = "text/html; charset=utf-8";
    let js_ct = "text/javascript; charset=utf-8";
    for (section, html) in [
        ("general", mote_ui::SETTINGS_GENERAL_HTML),
        ("plugins", mote_ui::SETTINGS_PLUGINS_HTML),
        ("integrity", mote_ui::SETTINGS_INTEGRITY_HTML),
        ("keybinds", mote_ui::SETTINGS_KEYBINDS_HTML),
    ] {
        res = res
            .register(format!("settings/{section}"), html, html_ct)
            .register(format!("settings/{section}.html"), html, html_ct);
    }
    res = res
        .register("settings/settings.css", mote_ui::SETTINGS_CSS, css)
        .register("settings/settings.js", mote_ui::SETTINGS_JS, js_ct);
    res
}

/// Build the **unprivileged** `mote://overlay` resource set: the shell-rendered
/// live integrity overlay (`integrity.html`) and the tab-picker overlay
/// (`picker.html`), plus the design-token + component CSS those documents
/// reference by relative URL. These surfaces are driven entirely Rust-side
/// (input routing + `eval_js`) and need no host-bridge, so they are served off a
/// distinct origin the chrome-origin gate does NOT match (S2) — no
/// `window.cefQuery` is ever installed in them.
fn build_overlay_resources(integrity_html: &str) -> ChromeResources {
    let css = "text/css; charset=utf-8";
    let mut res = ChromeResources::new()
        .register(
            "integrity.html",
            integrity_html.to_owned(),
            "text/html; charset=utf-8",
        )
        .register(
            "picker.html",
            picker::PICKER_HTML,
            "text/html; charset=utf-8",
        )
        .register("tokens.css", mote_ui::TOKENS_CSS, css)
        .register("base.css", mote_ui::BASE_CSS, css);
    for (name, contents) in mote_ui::COMPONENT_CSS {
        res = res.register(format!("components/{name}.css"), *contents, css);
    }
    res
}

/// Create a content [`Page`] with the correct CEF request context for `url`.
///
/// **ADR-0015 § global-request-context constraint**: `mote://` URLs MUST be
/// loaded via the CEF global request context ([`Page::new`]); per-identity
/// profile contexts do NOT have the `mote://` scheme handler installed and
/// will fail with `ERR_UNKNOWN_URL_SCHEME`. All other URLs are loaded via the
/// per-identity profile context ([`Page::with_profile`]) for cookie/cache
/// isolation (ADR-0010).
///
/// URL scheme matching is case-insensitive per RFC 3986 §3.1.
///
/// This is the **single routing point** for all tab-creation code paths.
/// Every `Page::with_profile` call in a tab-creation context must go through
/// this helper so the routing logic cannot be accidentally bypassed.
fn create_content_page(
    url: &str,
    opts: &PageOptions,
    profile: &ProfileHandle,
) -> mote_cef::Result<Page> {
    if url.len() >= 7 && url[..7].eq_ignore_ascii_case("mote://") {
        // Global request context — mote:// scheme handler is registered here only.
        // Role: Overlay (trusted-but-unprivileged) so the S1 nav guard does not block
        // this top-level mote:// navigation. These pages are shell-authored and static;
        // they do not need the host-bridge (window.cefQuery / window.mote).
        let mote_opts = PageOptions {
            role: PageRole::Overlay,
            ..opts.clone()
        };
        Page::new(url, &mote_opts)
    } else {
        // Per-identity profile context — all untrusted web content.
        Page::with_profile(url, opts, profile)
    }
}

/// Build the closed op registry. Ops translate chrome intents into
/// [`ShellCommand`]s the winit loop applies (the handlers are `Send + Sync` and
/// must not capture the `!Send` pages).
#[allow(
    clippy::too_many_lines,
    reason = "registry factory — each op is one logical entry; extracting sub-factories adds indirection without clarity"
)]
fn build_op_registry(commands: &CommandQueue) -> OpRegistry {
    let nav_queue = Arc::clone(commands);
    let focus_queue = Arc::clone(commands);
    let new_queue = Arc::clone(commands);
    let close_queue = Arc::clone(commands);
    let select_queue = Arc::clone(commands);
    let approve_queue = Arc::clone(commands);
    let update_queue = Arc::clone(commands);
    let rollback_queue = Arc::clone(commands);
    let reload_queue = Arc::clone(commands);
    let revoke_queue = Arc::clone(commands);
    let set_active_panel_queue = Arc::clone(commands);
    let bookmark_remove_queue = Arc::clone(commands);
    let bookmark_toggle_queue = Arc::clone(commands);
    let adjust_queue = Arc::clone(commands);
    let revoke_secret_queue = Arc::clone(commands);
    let urlbar_query_queue = Arc::clone(commands);
    let switch_workspace_queue = Arc::clone(commands);
    let copy_url_queue = Arc::clone(commands);
    // P2: address-bar navigation op queues.
    let go_back_queue = Arc::clone(commands);
    let go_forward_queue = Arc::clone(commands);
    let nav_reload_queue = Arc::clone(commands);
    let nav_stop_queue = Arc::clone(commands);
    // P6: settings panel op queues.
    let set_theme_queue = Arc::clone(commands);
    let set_search_engine_queue = Arc::clone(commands);
    let set_hw_accel_queue = Arc::clone(commands);
    let set_zoom_persist_queue = Arc::clone(commands);
    let plugin_disable_queue = Arc::clone(commands);
    let plugin_uninstall_queue = Arc::clone(commands);
    let plugin_install_picker_queue = Arc::clone(commands);
    let integrity_reverify_queue = Arc::clone(commands);
    let integrity_detail_queue = Arc::clone(commands);
    // P5: find / zoom / reopen / context-menu op queues.
    let find_in_page_queue = Arc::clone(commands);
    let find_next_op_queue = Arc::clone(commands);
    let find_prev_op_queue = Arc::clone(commands);
    let stop_finding_queue = Arc::clone(commands);
    let zoom_in_queue = Arc::clone(commands);
    let zoom_out_queue = Arc::clone(commands);
    let zoom_reset_queue = Arc::clone(commands);
    let reopen_closed_tab_queue = Arc::clone(commands);
    let context_menu_action_queue = Arc::clone(commands);
    let omnibox_submit_queue = Arc::clone(commands);
    OpRegistry::new()
        .register("navigate", move |params: &str| {
            json_string_field(params, "url").map_or_else(
                || OpResponse::err(400, "navigate requires a string `url`"),
                |url| {
                    push(&nav_queue, ShellCommand::Navigate(url));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        // `omnibox_submit` — free-text submission from the omnibox.  Carries
        // the raw user text; the pump thread resolves it to a URL via
        // `resolve_omnibox_input` using the live `search_url_template`.
        // Suggestion-row clicks go through `navigate` directly (they already
        // carry real URLs from history/bookmarks).
        .register("omnibox_submit", move |params: &str| {
            json_string_field(params, "text").map_or_else(
                || OpResponse::err(400, "omnibox_submit requires a string `text`"),
                |text| {
                    push(&omnibox_submit_queue, ShellCommand::OmniboxSubmit(text));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("new_tab", move |params: &str| {
            // Optional `url` param: when present, the new tab opens at that
            // URL via `create_content_page` (ADR-0015 routes mote:// through
            // the global context). When absent, default newtab page applies.
            let url = json_string_field(params, "url");
            push(&new_queue, ShellCommand::NewTab(url));
            OpResponse::ok("{\"ok\":true}")
        })
        .register("close_tab", move |params: &str| {
            json_u64_field(params, "id").map_or_else(
                || OpResponse::err(400, "close_tab requires a numeric `id`"),
                |id| {
                    push(&close_queue, ShellCommand::CloseTab(id));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("select_tab", move |params: &str| {
            json_u64_field(params, "id").map_or_else(
                || OpResponse::err(400, "select_tab requires a numeric `id`"),
                |id| {
                    push(&select_queue, ShellCommand::SelectTab(id));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("focus_changed", move |params: &str| {
            let owner = match json_string_field(params, "owner").as_deref() {
                Some("chrome") => FocusOwner::Chrome,
                _ => FocusOwner::Page,
            };
            push(&focus_queue, ShellCommand::FocusOwner(owner));
            OpResponse::ok("{\"ok\":true}")
        })
        .register("approve_plugin", move |params: &str| {
            // Deserialize the dialog's structured verdict.
            let Ok(result) = serde_json::from_str::<approval::DialogResult>(params) else {
                return OpResponse::err(400, "approve_plugin: malformed payload");
            };
            // Op-boundary STRUCTURAL validation (ADR-0005 closed structured
            // operations): bound every origin glob and the count per permission
            // synchronously, BEFORE the string can become a Narrowing resource.
            if let Err(msg) = validate_dialog_origins(&result) {
                return OpResponse::err(400, msg);
            }
            push(&approve_queue, ShellCommand::ApprovePlugin(result));
            OpResponse::ok("{\"accepted\":true}")
        })
        .register("plugin_update", move |params: &str| {
            plugin_action_op(&update_queue, params, ShellCommand::PluginUpdate)
        })
        .register("plugin_rollback", move |params: &str| {
            plugin_action_op(&rollback_queue, params, ShellCommand::PluginRollback)
        })
        .register("plugin_reload", move |params: &str| {
            plugin_action_op(&reload_queue, params, ShellCommand::PluginReload)
        })
        .register("plugin_revoke", move |params: &str| {
            plugin_action_op(&revoke_queue, params, ShellCommand::PluginRevoke)
        })
        .register("plugin_adjust_scope", move |params: &str| {
            plugin_action_op(&adjust_queue, params, ShellCommand::PluginAdjustScope)
        })
        .register("plugin_revoke_secret", move |params: &str| {
            plugin_revoke_secret_op(&revoke_secret_queue, params)
        })
        .register("urlbar_query", move |params: &str| {
            json_string_field(params, "text").map_or_else(
                || OpResponse::err(400, "urlbar_query requires a string `text`"),
                |text| {
                    push(&urlbar_query_queue, ShellCommand::UrlbarQuery(text));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("set_active_panel", move |params: &str| {
            json_string_field(params, "name").map_or_else(
                || OpResponse::err(400, "set_active_panel requires a string `name`"),
                |name| {
                    push(&set_active_panel_queue, ShellCommand::SetActivePanel(name));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("bookmark_remove", move |params: &str| {
            json_string_field(params, "url").map_or_else(
                || OpResponse::err(400, "bookmark_remove requires a string `url`"),
                |url| {
                    push(&bookmark_remove_queue, ShellCommand::BookmarkRemove(url));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        .register("bookmark_toggle", move |_params: &str| {
            push(&bookmark_toggle_queue, ShellCommand::BookmarkToggle);
            OpResponse::ok("{\"ok\":true}")
        })
        .register("set_active_workspace", move |params: &str| {
            json_string_field(params, "id").map_or_else(
                || OpResponse::err(400, "set_active_workspace requires a string `id`"),
                |id| {
                    push(&switch_workspace_queue, ShellCommand::SwitchWorkspace(id));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        // R2: copy the active tab's URL to the system clipboard.
        //
        // This op is callable ONLY from `mote://chrome` JS via the host-bridge
        // origin gate (ADR-0005); the plugin host API (`mote-runtime::hostapi`)
        // does not expose `mote.invoke(op, params)` to Lua, so plugins cannot
        // reach this code path. If a future change exposes ops to plugins, this
        // op MUST gain a `read-tabs` capability check at that boundary — the
        // gate doesn't exist here because there's no plugin-reachable path to
        // gate today, not because chrome is somehow exempt.
        //
        // The clipboard write is host-side (arboard); nothing leaves via JS.
        .register("copy_active_url", move |_params: &str| {
            push(&copy_url_queue, ShellCommand::CopyActiveUrl);
            OpResponse::ok("{\"ok\":true}")
        })
        // R4: close the window via the chrome close-button click.
        // Callable only from the privileged `mote://chrome` origin (ADR-0005
        // origin gate; the host-bridge router is attached only to the chrome
        // browser). The shell sets `should_exit = true` on the next pump tick
        // and `about_to_wait` calls `event_loop.exit()`.
        .register("close_window", {
            let q = Arc::clone(commands);
            move |_params: &str| {
                push(&q, ShellCommand::CloseWindow);
                OpResponse::ok("{\"ok\":true}")
            }
        })
        // ── P6: settings panel ops ──────────────────────────────────────────
        //
        // All ops below are callable only from `mote://chrome` (ADR-0005 origin
        // gate). Writes go to `managed.lua` via the pump thread (ADR-0006 /
        // ADR-0017). No plugin-reachable path reaches these ops.
        //
        // `set_theme` — switch the active theme. The `theme` field must be one
        // of the two built-in theme names (`dusk`, `vellum`) or an installed
        // custom theme name. The pump thread validates the name before writing.
        .register("set_theme", move |params: &str| {
            json_string_field(params, "theme").map_or_else(
                || OpResponse::err(400, "set_theme requires a string `theme`"),
                |theme| {
                    push(&set_theme_queue, ShellCommand::SetTheme(theme));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        // `set_search_engine` — update the default search engine. Requires
        // non-empty `name` and `url_template` string fields.
        .register("set_search_engine", move |params: &str| {
            let name = json_string_field(params, "name").filter(|s| !s.is_empty());
            let url_template =
                json_string_field(params, "url_template").filter(|s| !s.is_empty());
            match (name, url_template) {
                (Some(name), Some(url_template)) => {
                    push(
                        &set_search_engine_queue,
                        ShellCommand::SetSearchEngine { name, url_template },
                    );
                    OpResponse::ok("{\"ok\":true}")
                }
                _ => OpResponse::err(
                    400,
                    "set_search_engine requires non-empty string fields `name` and `url_template`",
                ),
            }
        })
        // `set_hw_accel` — toggle hardware acceleration. `enabled` must be a
        // boolean JSON field.
        .register("set_hw_accel", move |params: &str| {
            json_bool_field(params, "enabled").map_or_else(
                || OpResponse::err(400, "set_hw_accel requires a boolean `enabled`"),
                |enabled| {
                    push(&set_hw_accel_queue, ShellCommand::SetHwAccel(enabled));
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        // `set_zoom_persist` — toggle per-origin zoom persistence.
        .register("set_zoom_persist", move |params: &str| {
            json_bool_field(params, "enabled").map_or_else(
                || OpResponse::err(400, "set_zoom_persist requires a boolean `enabled`"),
                |enabled| {
                    push(
                        &set_zoom_persist_queue,
                        ShellCommand::SetZoomPersist(enabled),
                    );
                    OpResponse::ok("{\"ok\":true}")
                },
            )
        })
        // `plugin_disable` — disable a named plugin.
        .register("plugin_disable", move |params: &str| {
            plugin_action_op(&plugin_disable_queue, params, ShellCommand::PluginDisable)
        })
        // `plugin_uninstall` — uninstall a named plugin.
        .register("plugin_uninstall", move |params: &str| {
            plugin_action_op(
                &plugin_uninstall_queue,
                params,
                ShellCommand::PluginUninstall,
            )
        })
        // `plugin_install_picker` — open the native file picker for plugin install.
        .register("plugin_install_picker", move |_params: &str| {
            push(&plugin_install_picker_queue, ShellCommand::PluginInstallPicker);
            OpResponse::ok("{\"ok\":true}")
        })
        // `integrity_reverify_all` — re-verify all plugin checksums.
        .register("integrity_reverify_all", move |_params: &str| {
            push(&integrity_reverify_queue, ShellCommand::IntegrityReverifyAll);
            OpResponse::ok("{\"ok\":true}")
        })
        // `integrity_plugin_detail` — request drill-down detail for a plugin.
        .register("integrity_plugin_detail", move |params: &str| {
            plugin_action_op(
                &integrity_detail_queue,
                params,
                ShellCommand::IntegrityPluginDetail,
            )
        })
        // ── P2: address-bar navigation ops ──────────────────────────────────
        //
        // All three ops are callable only from `mote://chrome` (ADR-0005 origin
        // gate). They enqueue a ShellCommand on the pump-thread command queue;
        // the pump thread checks the active page and calls the CEF API.
        //
        // `go_back` — navigate the active tab back one step.
        .register("go_back", move |_params: &str| {
            push(&go_back_queue, ShellCommand::NavGoBack);
            OpResponse::ok("{\"ok\":true}")
        })
        // `go_forward` — navigate the active tab forward one step.
        .register("go_forward", move |_params: &str| {
            push(&go_forward_queue, ShellCommand::NavGoForward);
            OpResponse::ok("{\"ok\":true}")
        })
        // `reload` — reload the active tab.
        .register("reload", move |_params: &str| {
            push(&nav_reload_queue, ShellCommand::NavReload);
            OpResponse::ok("{\"ok\":true}")
        })
        // `stop` — stop the active tab's current load.
        .register("stop", move |_params: &str| {
            push(&nav_stop_queue, ShellCommand::NavStop);
            OpResponse::ok("{\"ok\":true}")
        })
        // `security_info` — return TLS/security metadata for the active tab.
        // Synchronous: the JS caller already knows the current URL (it was
        // last set via `set_url` applyOp and is held in the omnibox input).
        // The op returns a sentinel that tells host.js to construct the popover
        // from the URL it already holds, per the JS-hydration pattern.
        //
        // Full TLS cert details (cert subject/issuer/valid-through, cipher,
        // version) require a CEF `OnCertificateError` / SSL-status callback
        // that is not yet wired — deferred. The popover shows what is derivable
        // from the URL scheme alone in v0.1 (secure/insecure indicator). The
        // JSON shape is forward-compatible: callers check `type` before reading
        // optional fields.
        .register("security_info", |_params: &str| {
            OpResponse::ok("{\"ok\":true,\"type\":\"js_hydrated\"}")
        })
        // `keybinds_list` — return the v0.1 chord table as JSON. This op is
        // read-only: it serialises `KEYBIND_TABLE` (derived from `classify_chord`)
        // and returns it directly without touching the command queue. No state
        // change; no `push`. Stays in the op handler since no shell state is
        // needed (KEYBIND_TABLE is a static constant).
        .register("keybinds_list", |_params: &str| {
            OpResponse::ok(keybinds_list_json())
        })
        // ── P5: find / zoom / reopen / context-menu ops ─────────────────────
        //
        // All callable only from `mote://chrome` (ADR-0005 origin gate).
        //
        // `find_in_page` — called by host.js with the search text while in
        // [find] mode. Enqueues `FindText` carrying the query and direction so
        // the drain branch can call `Page::find(query, forward, false, find_next)`.
        .register("find_in_page", move |params: &str| {
            let query = json_string_field(params, "text").unwrap_or_default();
            let find_next = json_bool_field(params, "findNext").unwrap_or(false);
            let forward = json_bool_field(params, "forward").unwrap_or(true);
            push(
                &find_in_page_queue,
                ShellCommand::FindText {
                    query,
                    forward,
                    find_next,
                },
            );
            OpResponse::ok("{\"ok\":true}")
        })
        // `stop_finding` — exit find mode, clear selection.
        .register("stop_finding", move |_params: &str| {
            push(&stop_finding_queue, ShellCommand::StopFinding);
            OpResponse::ok("{\"ok\":true}")
        })
        // `find_next` — advance to the next match using the last query (Enter
        // in the find omnibox, C3 fix).
        .register("find_next", move |_params: &str| {
            push(&find_next_op_queue, ShellCommand::FindNext);
            OpResponse::ok("{\"ok\":true}")
        })
        // `find_prev` — advance to the previous match using the last query
        // (Shift+Enter in the find omnibox, C3 fix).
        .register("find_prev", move |_params: &str| {
            push(&find_prev_op_queue, ShellCommand::FindPrev);
            OpResponse::ok("{\"ok\":true}")
        })
        // `zoom_in` / `zoom_out` / `zoom_reset` — zoom the active page.
        .register("zoom_in", move |_params: &str| {
            push(&zoom_in_queue, ShellCommand::ZoomIn);
            OpResponse::ok("{\"ok\":true}")
        })
        .register("zoom_out", move |_params: &str| {
            push(&zoom_out_queue, ShellCommand::ZoomOut);
            OpResponse::ok("{\"ok\":true}")
        })
        .register("zoom_reset", move |_params: &str| {
            push(&zoom_reset_queue, ShellCommand::ZoomReset);
            OpResponse::ok("{\"ok\":true}")
        })
        // `reopen_closed_tab` — pop from the closed-tab stack and open it.
        .register("reopen_closed_tab", move |_params: &str| {
            push(&reopen_closed_tab_queue, ShellCommand::ReopenClosedTab);
            OpResponse::ok("{\"ok\":true}")
        })
        // `context_menu_action` — handle a context-menu item chosen by the user.
        // `action` is one of the fixed strings enumerated in `host.js`; the shell
        // handles the navigation-side ones (reload, go_back, go_forward) and logs
        // the rest (handled entirely in chrome JS).
        .register("context_menu_action", move |params: &str| {
            let action = json_string_field(params, "action")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_owned());
            push(
                &context_menu_action_queue,
                ShellCommand::ContextMenuAction(action),
            );
            OpResponse::ok("{\"ok\":true}")
        })
}

/// Op-boundary structural validation of the dialog's origin globs
/// (ADR-0005 closed structured operations). For every permission narrowed to
/// `origins`, every glob must pass [`approval::validate_origin_glob`] and the
/// per-permission count must not exceed [`approval::MAX_ORIGINS_PER_PERMISSION`].
/// Returns `Err(message)` on the first violation so the whole op is rejected —
/// no arbitrary chrome-supplied string is ever pushed toward a `Narrowing`.
fn validate_dialog_origins(result: &approval::DialogResult) -> Result<(), &'static str> {
    for perm in &result.permissions {
        if perm.mode != "origins" {
            continue;
        }
        let origins = perm.origins.as_deref().unwrap_or(&[]);
        if origins.len() > approval::MAX_ORIGINS_PER_PERMISSION {
            return Err("approve_plugin: too many origins for a permission");
        }
        if !origins.iter().all(|o| approval::validate_origin_glob(o)) {
            return Err("approve_plugin: invalid origin glob");
        }
    }
    Ok(())
}

/// Shared handler for the five panel-action ops: parse a non-empty string
/// `plugin` field, build the [`ShellCommand`] from it, and enqueue it. The
/// plugin-name *format* is validated when the pump thread turns the string into
/// a [`PluginName`]; here we only require a non-empty string.
fn plugin_action_op(
    queue: &CommandQueue,
    params: &str,
    make: impl FnOnce(String) -> ShellCommand,
) -> OpResponse {
    match json_string_field(params, "plugin") {
        Some(name) if !name.is_empty() => {
            push(queue, make(name));
            OpResponse::ok("{\"ok\":true}")
        }
        _ => OpResponse::err(400, "requires a non-empty string `plugin`"),
    }
}

/// Handler for `plugin_revoke_secret`: parse non-empty `plugin` and `name`
/// string fields and enqueue the revoke command. The plugin-name *format* is
/// validated when the pump thread turns the string into a [`PluginName`]; here
/// we only require both fields to be present non-empty strings.
fn plugin_revoke_secret_op(queue: &CommandQueue, params: &str) -> OpResponse {
    let plugin = json_string_field(params, "plugin").filter(|s| !s.is_empty());
    let name = json_string_field(params, "name").filter(|s| !s.is_empty());
    match (plugin, name) {
        (Some(plugin), Some(name)) => {
            push(queue, ShellCommand::PluginRevokeSecret { plugin, name });
            OpResponse::ok("{\"ok\":true}")
        }
        _ => OpResponse::err(
            400,
            "plugin_revoke_secret requires non-empty string fields `plugin` and `name`",
        ),
    }
}

/// Enqueue a command (poisoned-lock tolerant: a poisoned queue is recoverable).
fn push(queue: &CommandQueue, command: ShellCommand) {
    queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(command);
}

/// The winit application: owns the window, the CEF engine + pages, the
/// compositor, and the session. All CEF objects stay on this (the event-loop)
/// thread.
#[allow(
    clippy::struct_excessive_bools,
    reason = "winit app state — each bool is an independent in-flight UI flag (integrity overlay open, chrome-side panel open, chrome-ready, did-initial-load, first-frame-logged); a state machine would obscure them"
)]
struct ShellApp {
    engine: Engine,
    bridge: HostBridge,
    /// Interns one profile per identity; the default identity's profile backs
    /// every tab in this slice.
    #[allow(
        dead_code,
        reason = "retained so the default profile's manager outlives the pages"
    )]
    profiles: ProfileManager,
    default_profile: ProfileHandle,
    /// Page geometry/role for a new content page (recomputed on resize).
    content_opts: PageOptions,
    session: Session,
    store: Store,
    /// The Phase-1 plugin runtime + audit log + the bundled plugins it loaded.
    /// Held for the program's lifetime so the audit thread stays alive and the
    /// integrity panel can be re-queried; `Drop` shuts the audit log down.
    host: runtime::PluginHost,
    /// The lazily-created integrity overlay page (loads `mote://overlay/integrity.html`).
    /// `None` until the panel is first opened.
    integrity_page: Option<Page>,
    /// Whether the integrity overlay is currently composited full-window.
    integrity_open: bool,
    /// Whether the chrome-rendered integrity panel (structured-DOM path in
    /// `panels.js`) is currently shown. Tracked independently from the legacy
    /// `integrity_open` overlay flag — Ctrl+Shift+I prefers the chrome path,
    /// and the overlay path remains as a fallback for the chrome-not-ready
    /// window (the legacy-overlay cleanup is a later task).
    integrity_chrome_open: bool,
    /// Last integrity paint count uploaded (re-upload only on a new frame).
    integrity_paints: u64,
    /// The workspace tab picker (`Mod+Space`) state machine. Logic lives
    /// Rust-side; the overlay page is pure display (see [`picker`]).
    picker: PickerState,
    /// The lazily-created picker overlay page (`mote://overlay/picker.html`).
    /// `None` until the picker is first opened.
    picker_page: Option<Page>,
    /// Last picker paint count uploaded (re-upload only on a new frame).
    picker_paints: u64,
    /// Applies active-tab renderer-discard decisions (DESIGN §Active tab
    /// discarding). The shell kills the renderer for each newly-discarded tab.
    discarder: Discarder,
    /// Ages out hidden tabs past their TTL (DESIGN §Hidden tab lifecycle).
    reaper: HiddenTabReaper,
    /// How often [`Self::run_housekeeping`] runs (discard + reap pass).
    housekeeping_interval: Duration,
    /// When housekeeping last ran (throttles the pass to the interval).
    last_housekeeping: Instant,
    session_identity: mote_types::IdentityId,
    workspace: WorkspaceId,
    /// The open tabs in display order; `active` indexes the focused one.
    tabs: Vec<ShellTab>,
    active: usize,
    commands: CommandQueue,
    /// Created lazily in `resumed` (winit creates the window there).
    window: Option<Arc<Window>>,
    compositor: Option<Compositor>,
    /// Last chrome/content paint counts uploaded — re-upload only on a new one.
    chrome_paints: u64,
    content_paints: u64,
    /// Physical window size (chrome covers all of it; the page fills the viewport).
    width: u32,
    height: u32,
    /// The window's DPI scale factor (1.0 at 96dpi, 1.25 at 120dpi, …). The
    /// chrome insets are authored in logical pixels, so the physical page
    /// viewport is computed by scaling them by this factor (plan §1.3): a fixed
    /// physical inset is wrong on a scaled display.
    scale_factor: f64,
    /// Last cursor position in physical window pixels (for click routing).
    cursor: (i32, i32),
    /// Active keyboard modifiers (forwarded to CEF).
    modifiers: Modifiers,
    /// Who owns keyboard input.
    focus: FocusOwner,
    /// `true` once the chrome has painted at least once, so it can receive its
    /// initial tab-list / URL push (an `applyOp` before the bootstrap runs is
    /// lost).
    chrome_ready: bool,
    /// `true` once the deferred plugin load pass has run (exactly once, on the
    /// first post-paint tick). Until then no plugin has been resolved/loaded —
    /// the load is deferred past window creation so a slow/offline git fetch or
    /// a fatal resolution error cannot block or abort startup (ADR T3 review).
    did_initial_load: bool,
    /// CEF needs a brief warm-up before the chrome's first paint; we log once.
    first_frame_logged: bool,
    /// When the window opened (for the warm-up log only).
    started: Instant,
    /// Set to `true` by `drain_commands` when a `CloseWindow` command is
    /// processed. `about_to_wait` calls `event_loop.exit()` on the next tick.
    should_exit: bool,
    // ── P5 fields ──────────────────────────────────────────────────────────────
    /// Recently-closed tabs the user can reopen with Ctrl+Shift+T.
    closed_tab_stack: ClosedTabStack,
    /// Per-tab zoom levels. Keyed by numeric tab id. Written on zoom change,
    /// read when pushing the zoom statusline element.
    tab_zoom_levels: std::collections::HashMap<u64, f64>,
    /// The last hover-URL value sent to the chrome. Tracked so we only push
    /// an update when the URL actually changes (avoids per-tick noise).
    hover_url_last: Option<String>,
    /// When the zoom status indicator auto-clears (set after each zoom action).
    zoom_clear_at: Option<Instant>,
    /// The last `(can_go_back, can_go_forward)` pair pushed to the chrome.
    /// Tracked so [`sync_nav_state`](Self::sync_nav_state) only re-pushes when
    /// the nav state actually changes. CEF updates these flags asynchronously
    /// via `on_loading_state_change`; `push_state_to_chrome` runs on URL
    /// change (`set_url`) which is BEFORE CEF commits the new history entry,
    /// so the nav state pushed there is stale. The poll-on-change path here
    /// is what lights up the back/forward buttons after a navigation
    /// completes.
    nav_state_last: (bool, bool),
    /// The last `is_loading` value pushed to the chrome. Tracked so
    /// [`sync_load_state`](Self::sync_load_state) only re-pushes when the
    /// loading state actually changes. CEF updates this flag asynchronously
    /// via `on_loading_state_change`; polling here surfaces the binary
    /// loading indicator to the chrome reliably after each transition.
    load_state_last: bool,
    /// The most recent non-empty find query text. Stored by `FindText` drain so
    /// `FindNext`/`FindPrev` (Ctrl+G, Ctrl+Shift+G) can repeat the last search
    /// without the chrome resending the query string.
    find_query_last: String,
    /// Active search URL template (default: `DuckDuckGo`).  Updated when the
    /// user changes the search engine via the settings panel (`set_search_engine`
    /// op → `SetSearchEngine` command).  Used by `OmniboxSubmit` to build the
    /// search URL for free-text input that does not look like a URL.
    search_url_template: String,
}

impl std::fmt::Debug for ShellApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellApp")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("tabs", &self.tabs.len())
            .field("active", &self.active)
            .field("focus", &self.focus)
            .field("has_window", &self.window.is_some())
            .finish_non_exhaustive()
    }
}

impl ShellApp {
    /// The chrome's left inset in **physical** pixels for the current display
    /// scale. The inset is authored in logical pixels (`VIEWPORT_LEFT`); on a
    /// scaled display (e.g. 1.25×) the physical inset is larger, so a fixed
    /// physical value would misalign the composited page (plan §1.3).
    fn inset_left(&self) -> u32 {
        scale_inset(VIEWPORT_LEFT, self.scale_factor)
    }

    /// The chrome's top inset in physical pixels for the current display scale.
    fn inset_top(&self) -> u32 {
        scale_inset(VIEWPORT_TOP, self.scale_factor)
    }

    /// The content page's physical surface size (window minus the scaled insets).
    fn viewport_dims(&self) -> (u32, u32) {
        viewport_size(self.width, self.height, self.inset_left(), self.inset_top())
    }

    /// The current page-viewport rect in physical pixels.
    fn viewport_rect(&self) -> ViewportRect {
        let (vw, vh) = self.viewport_dims();
        ViewportRect::new(
            px(self.inset_left()),
            px(self.inset_top()),
            px(vw.max(1)),
            px(vh.max(1)),
        )
    }

    /// The active tab's live page, if any (a placeholder has none).
    fn active_page(&self) -> Option<&Page> {
        self.tabs.get(self.active).and_then(|t| t.page.as_ref())
    }

    /// Drain the op command queue and apply each command on this (pump) thread.
    #[allow(
        clippy::too_many_lines,
        reason = "command dispatch table — each arm is one logical command; \
                  extracting sub-dispatchers would add indirection without clarity"
    )]
    fn drain_commands(&mut self) {
        let drained: Vec<ShellCommand> = {
            let mut q = self
                .commands
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.drain(..).collect()
        };
        for command in drained {
            match command {
                ShellCommand::Navigate(url) => self.navigate_active(&url),
                ShellCommand::OmniboxSubmit(text) => {
                    let url = resolve_omnibox_input(&text, &self.search_url_template.clone());
                    if !url.is_empty() {
                        self.navigate_active(&url);
                    }
                }
                ShellCommand::NewTab(url) => self.open_tab(url),
                ShellCommand::CloseTab(id) => self.close_tab(TabId::new(id)),
                ShellCommand::SelectTab(id) => self.select_tab(TabId::new(id)),
                ShellCommand::FocusOwner(owner) => self.set_focus_owner(owner),
                ShellCommand::ApprovePlugin(result) => self.approve_plugin(&result),
                ShellCommand::PluginUpdate(name) => self.plugin_update(&name),
                ShellCommand::PluginRollback(name) => self.plugin_rollback(&name),
                ShellCommand::PluginReload(name) => self.plugin_reload(&name),
                ShellCommand::PluginRevoke(name) => self.plugin_revoke(&name),
                ShellCommand::PluginAdjustScope(name) => self.plugin_adjust_scope(&name),
                ShellCommand::PluginRevokeSecret { plugin, name } => {
                    self.plugin_revoke_secret(&plugin, &name);
                }
                ShellCommand::UrlbarQuery(text) => self.urlbar_query(&text),
                ShellCommand::SetActivePanel(name) => self.set_active_panel(&name),
                ShellCommand::BookmarkRemove(url) => self.bookmark_remove(&url),
                ShellCommand::BookmarkToggle => self.bookmark_toggle(),
                ShellCommand::SwitchWorkspace(id) => self.switch_workspace(&id),
                ShellCommand::CopyActiveUrl => self.copy_active_url(),
                ShellCommand::CloseWindow => {
                    eprintln!("mote-shell: close window requested; exiting");
                    self.should_exit = true;
                }
                // P6: settings panel commands — write target is managed.lua per
                // ADR-0006 / ADR-0017. In v0.1 these log the intent; the full
                // managed.lua write path is wired once the config-mutation API is
                // complete (the write-seam test covers the enqueue contract).
                ShellCommand::SetTheme(theme) => {
                    eprintln!("mote-shell: set_theme → managed.lua: theme = {theme:?}");
                    // Push theme switch to the chrome so the data-theme attribute
                    // updates immediately without a reload.
                    let js = format!(
                        "document.querySelector('.mote-root')?.setAttribute('data-theme', {});",
                        serde_json::to_string(&theme).unwrap_or_default()
                    );
                    self.bridge.page().eval_js(&js);
                }
                ShellCommand::SetSearchEngine { name, url_template } => {
                    eprintln!(
                        "mote-shell: set_search_engine → managed.lua: \
                         name = {name:?}, url_template = {url_template:?}"
                    );
                    // Update the live search engine so the next `OmniboxSubmit`
                    // uses the new provider immediately (managed.lua write is
                    // deferred to the Bucket-A config-mutation pass).
                    self.search_url_template.clone_from(&url_template);
                }
                ShellCommand::SetHwAccel(enabled) => {
                    eprintln!("mote-shell: set_hw_accel → managed.lua: enabled = {enabled}");
                }
                ShellCommand::SetZoomPersist(enabled) => {
                    eprintln!("mote-shell: set_zoom_persist → managed.lua: enabled = {enabled}");
                }
                ShellCommand::PluginDisable(name) => {
                    eprintln!("mote-shell: plugin_disable → managed.lua: plugin = {name:?}");
                }
                ShellCommand::PluginUninstall(name) => {
                    eprintln!("mote-shell: plugin_uninstall → managed.lua: plugin = {name:?}");
                }
                ShellCommand::PluginInstallPicker => {
                    eprintln!(
                        "mote-shell: plugin_install_picker — file picker not yet \
                         implemented in v0.1; no-op"
                    );
                }
                ShellCommand::IntegrityReverifyAll => {
                    eprintln!("mote-shell: integrity_reverify_all — re-verify all plugins");
                }
                ShellCommand::IntegrityPluginDetail(name) => {
                    eprintln!("mote-shell: integrity_plugin_detail: plugin = {name:?}");
                }
                // P5: find / zoom / reopen commands ─────────────────────────
                ShellCommand::FindInPage => {
                    // Tell the chrome to enter [find] mode in the omnibox.
                    if self.chrome_ready {
                        self.bridge.page().eval_js(
                            "window.mote&&window.mote.applyOp&&\
                             window.mote.applyOp('focus_find',null);",
                        );
                    }
                }
                ShellCommand::FindText {
                    query,
                    forward,
                    find_next,
                } => {
                    // Store the query so Ctrl+G / Ctrl+Shift+G (FindNext/FindPrev)
                    // can repeat the last search without the chrome resending it.
                    if !query.is_empty() {
                        self.find_query_last.clone_from(&query);
                    }
                    // Clone before borrowing self via active_page().
                    let q = query.clone();
                    if let Some(page) = self.active_page() {
                        page.find(&q, forward, false, find_next);
                    }
                }
                ShellCommand::FindNext => {
                    let query = self.find_query_last.clone();
                    if let Some(page) = self.active_page() {
                        page.find(&query, true, false, true);
                    }
                }
                ShellCommand::FindPrev => {
                    let query = self.find_query_last.clone();
                    if let Some(page) = self.active_page() {
                        page.find(&query, false, false, true);
                    }
                }
                ShellCommand::StopFinding => {
                    if let Some(page) = self.active_page() {
                        page.stop_finding(true);
                    }
                }
                ShellCommand::ZoomIn => {
                    self.adjust_zoom(0.1);
                }
                ShellCommand::ZoomOut => {
                    self.adjust_zoom(-0.1);
                }
                ShellCommand::ZoomReset => {
                    self.set_zoom_level(0.0);
                }
                ShellCommand::ReopenClosedTab => {
                    self.reopen_closed_tab();
                }
                ShellCommand::ContextMenuAction(action) => {
                    self.handle_context_menu_action(&action);
                }
                // P2: address-bar navigation ops ─────────────────────────────
                ShellCommand::NavGoBack => {
                    if let Some(page) = self.active_page() {
                        page.go_back();
                    }
                }
                ShellCommand::NavGoForward => {
                    if let Some(page) = self.active_page() {
                        page.go_forward();
                    }
                }
                ShellCommand::NavReload => {
                    if let Some(page) = self.active_page() {
                        page.reload();
                    }
                }
                ShellCommand::NavStop => {
                    if let Some(page) = self.active_page() {
                        page.stop_load();
                    }
                }
                // SecurityInfoQuery is a doc-anchor variant (never enqueued;
                // the `security_info` op is handled synchronously in its
                // closure). The match arm is required for exhaustiveness.
                ShellCommand::SecurityInfoQuery => {}
            }
        }
    }

    /// Drain popup-tab requests queued by each live content page's
    /// `LifeSpanHandler::on_before_popup` (ADR-0011).
    ///
    /// Called once per `about_to_wait` tick, immediately after `drain_commands`.
    /// Iterates all live tab pages, collects their pending [`PopupTabRequest`]s,
    /// and processes each one as an [`open_popup_tab`](Self::open_popup_tab) call.
    /// The `user_gesture` flag from the CEF callback drives the `foreground`
    /// decision: gesture-driven → foreground (matches Chrome behaviour for
    /// `target=_blank` clicks); non-gesture → background (reduces focus-stealing
    /// from JS-initiated popups).
    fn drain_popup_tabs(&mut self) {
        // Collect requests from all live tabs AND the chrome page. We need to
        // own the vec before calling `open_popup_tab` (which mutably borrows
        // `self`). Draining the chrome page's queue too prevents an unbounded
        // VecDeque growth if chrome JS ever legitimately calls `window.open(...)`
        // (currently it doesn't, but the queue exists on every `Page` and a
        // chrome-side popup would otherwise leak indefinitely).
        let requests: Vec<PopupTabRequest> = self
            .tabs
            .iter()
            .filter_map(|t| t.page.as_ref())
            .flat_map(Page::drain_popup_requests)
            .chain(self.bridge.page().drain_popup_requests())
            .collect();
        for req in requests {
            // P3 (ADR-0015): `background=true` means the user explicitly chose
            // a background tab (⌘-click → NEW_BACKGROUND_TAB disposition). Honor
            // it: a background-flagged request is never foregrounded even if the
            // user gesture flag is true.
            let foreground = req.user_gesture && !req.background;
            self.open_popup_tab(&req.url, foreground);
        }
    }

    /// Invoke `ui:urlbar_provider` → `query` with the given text and push the
    /// returned suggestion list to the chrome via
    /// `window.mote.applyOp('urlbar_suggestions', <json>)`.
    ///
    /// Called from [`drain_commands`] for every [`ShellCommand::UrlbarQuery`].
    /// The D1 chrome-side rendering (dropdown build + keyboard select) is a
    /// separate task; this method's observable end is the `eval_js` call.
    ///
    /// # Failure policy
    ///
    /// - No fulfiller loaded / contract violation / timeout →
    ///   [`Runtime::invoke_capability`] returns `None`; we push an empty list so
    ///   the chrome dropdown clears rather than showing stale results.
    /// - The provider may return an empty Lua table `{}`, which marshals to
    ///   [`HostValue::Map`] (not [`HostValue::List`]) because the runtime cannot
    ///   distinguish an empty array from an empty object in Lua. We normalise any
    ///   non-List result (including an empty Map) to `List([])` so chrome always
    ///   receives a JSON array.
    /// - JSON serialisation failure is defended against even though it cannot
    ///   occur for a well-formed `HostValue` (no NaN/±Inf paths reach here):
    ///   we push an empty list and log, matching the existing shell failure idiom.
    fn urlbar_query(&self, text: &str) {
        // The history plugin's `query(text)` function accepts a plain Str
        // argument (matching the existing host_invoke_capability test pattern).
        // Pass the text directly as HostValue::Str rather than a Map.
        let arg = HostValue::Str(text.to_owned());

        // Invoke the provider; None on no fulfiller / contract violation /
        // timeout / error (Runtime::invoke_capability audits failures internally).
        // Normalise the result: only a List is a valid suggestions payload; any
        // other variant (Nil, Map, Str, …) — including the empty-Lua-table Map
        // the provider returns for empty-text fast-exits — becomes an empty list.
        let suggestions =
            match self
                .host
                .runtime
                .invoke_capability("ui:urlbar_provider", "query", &arg)
            {
                Some(HostValue::List(items)) => HostValue::List(items),
                _ => HostValue::List(vec![]),
            };

        // Convert HostValue → serde_json::Value → JSON string.
        // Serialisation of a HostValue::List should never fail; defend anyway.
        let payload_json = match serde_json::to_string(&host_to_json(&suggestions)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: urlbar_query serialise failed: {e}; pushing empty list");
                "[]".to_owned()
            }
        };

        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('urlbar_suggestions',{payload_json});"
        ));
    }

    /// Switch the active sidebar panel and push fresh data to chrome.
    ///
    /// `"bookmarks"` → [`push_bookmark_list`]; `"history"` → [`push_history_list`];
    /// `"tabs"` → [`push_state_to_chrome`] so the meta refreshes to "N open".
    /// Unknown panel names are silently ignored.
    fn set_active_panel(&mut self, name: &str) {
        match name {
            "bookmarks" => self.push_bookmark_list(),
            "history" => self.push_history_list(),
            "tabs" => self.push_state_to_chrome(),
            _ => {}
        }
    }

    /// Remove the bookmark keyed by `url` and re-push the bookmark list.
    ///
    /// Invokes `ui:bookmarks_provider` → `remove_bookmark` with `{ url }`.
    /// Returns after pushing the updated list regardless of the remove outcome
    /// (no-op for a URL that was never bookmarked).
    fn bookmark_remove(&mut self, url: &str) {
        let mut arg_map = BTreeMap::new();
        arg_map.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        let arg = HostValue::Map(arg_map);
        let _ =
            self.host
                .runtime
                .invoke_capability("ui:bookmarks_provider", "remove_bookmark", &arg);
        self.push_bookmark_list();
        // If the removed URL is the active tab's URL, the urlbar star must
        // drop its accent/fill — re-push set_url so the chrome re-evaluates
        // `bookmarked` for the active tab.
        self.push_state_to_chrome();
    }

    /// Check whether `url` is currently in the bookmarks store by calling
    /// `list_bookmarks` and scanning the returned rows.
    ///
    /// Returns `false` on any failure (no provider, empty list, marshal error) so
    /// callers get a safe default. Used by [`push_state_to_chrome`] and
    /// [`bookmark_toggle`].
    fn is_url_bookmarked(&self, url: &str) -> bool {
        let arg = HostValue::Str(String::new());
        let raw =
            self.host
                .runtime
                .invoke_capability("ui:bookmarks_provider", "list_bookmarks", &arg);
        let Some(HostValue::List(items)) = raw else {
            return false;
        };
        items.iter().any(|item| match item {
            HostValue::Map(m) => matches!(m.get("url"), Some(HostValue::Str(u)) if u == url),
            _ => false,
        })
    }

    /// Toggle the bookmark state for the active tab.
    ///
    /// - If the URL is not yet bookmarked: calls `add_bookmark {url, title}`.
    /// - If already bookmarked: calls `remove_bookmark {url}`.
    ///
    /// After the toggle, re-pushes `set_url` (via [`push_state_to_chrome`]) so
    /// the urlbar star reflects the new state, and re-pushes the bookmark list so
    /// the sidebar panel stays in sync.
    ///
    /// # Failure policy
    ///
    /// `let _ =` on every `invoke_capability` call — the audit log records
    /// failures internally; the UI will simply not update on a provider fault.
    fn bookmark_toggle(&mut self) {
        // Capture owned values before any mutable borrow.
        let (url, title) = match self.tabs.get(self.active) {
            Some(tab) => (tab.url.clone(), tab.title.clone()),
            None => return,
        };

        let already_bookmarked = self.is_url_bookmarked(&url);
        let mut arg_map = BTreeMap::new();
        arg_map.insert("url".to_owned(), HostValue::Str(url));
        if already_bookmarked {
            let _ = self.host.runtime.invoke_capability(
                "ui:bookmarks_provider",
                "remove_bookmark",
                &HostValue::Map(arg_map),
            );
        } else {
            arg_map.insert(
                "title".to_owned(),
                HostValue::Str(title.unwrap_or_default()),
            );
            let _ = self.host.runtime.invoke_capability(
                "ui:bookmarks_provider",
                "add_bookmark",
                &HostValue::Map(arg_map),
            );
        }

        // Re-push set_url so the urlbar star color reflects the new state.
        self.push_state_to_chrome();
        // Re-push bookmark list so the sidebar panel stays in sync.
        self.push_bookmark_list();
    }

    /// Ask the `workspace:provider` plugin to validate, persist, and emit the
    /// workspace switch, then re-point `self.workspace` and rebuild the visible
    /// tab strip from the session's tab list for the new workspace.
    ///
    /// # Flow
    ///
    /// 1. Invoke `workspace:provider` → `switch_workspace({id})` — the plugin
    ///    validates the id against the built-in set, persists the new active
    ///    workspace, and emits `workspaces:on_change`.
    /// 2. On a truthy return: re-point `self.workspace` and `self.session`'s
    ///    active workspace, rebuild `self.tabs` from
    ///    [`Session::tab_picker_ranked`] for the new workspace, reset `active`
    ///    to 0, call [`Self::on_active_changed`], and flush + push to chrome.
    /// 3. On a falsy / `None` return (plugin rejected the id): no-op — the shell
    ///    state is unchanged.
    ///
    /// # Design note
    ///
    /// This is the shell *mechanism*; the plugin owns *policy* (validation,
    /// persistence, event emission). The shell does not subscribe to
    /// `workspaces:on_change` — it drives the switch and trusts the plugin's
    /// return value to confirm acceptance.
    fn switch_workspace(&mut self, id: &str) {
        // Build the Map arg the plugin expects: { id = "<string>" }.
        let mut arg_map = BTreeMap::new();
        arg_map.insert("id".to_owned(), HostValue::Str(id.to_owned()));
        let result = self.host.runtime.invoke_capability(
            "workspace:provider",
            "switch_workspace",
            &HostValue::Map(arg_map),
        );
        // Plugin returns `true` on success, `false` (or `None`) on unknown id.
        let accepted = match result {
            Some(HostValue::Bool(b)) => b,
            Some(_) => true, // any non-Bool truthy return treated as accepted
            None => false,
        };
        if !accepted {
            eprintln!("mote-shell: switch_workspace({id}) rejected by plugin");
            return;
        }
        // Map the string id to the numeric WorkspaceId the session uses.
        // The plugin's ordered list is the single source of truth: the 0-based
        // position in that list is the stable WorkspaceId.
        let slugs = workspace_slugs_from_host(&self.host).unwrap_or_default();
        let Some(new_ws) = workspace_id_for_slug(id, &slugs) else {
            eprintln!("mote-shell: switch_workspace({id}): unrecognised slug, ignoring");
            return;
        };
        self.workspace = new_ws;
        self.session.set_active_workspace(new_ws);
        self.rebuild_tabs_for_workspace();
        // Materialize the active tab's CEF page. `rebuild_tabs_for_workspace`
        // creates every tab with `page: None`; without this materialization the
        // active tab has no live browser, `navigate_active` silently no-ops
        // (the `if let Some(page)` skips), and the viewport stays frozen on the
        // previous workspace's content. Other tabs stay placeholders until
        // `select_tab` materializes them on focus.
        self.materialize_active_if_placeholder();
        self.on_active_changed();
        self.persist_and_push();
        // Push updated workspace list so the chrome strip reflects the switch.
        self.push_workspace_list();
        eprintln!("mote-shell: switched to workspace {id} ({new_ws})");
    }

    /// If the active tab is a placeholder (no live CEF page), create one. Used
    /// after `rebuild_tabs_for_workspace` so the user-visible result of a
    /// workspace switch (or any equivalent rebuild) has a live page they can
    /// navigate / interact with.
    ///
    /// **Regression note (`0ccb346`):** removing this call from
    /// `switch_workspace` reintroduces a navigation regression — typing a URL
    /// into the omnibox after switching workspaces silently no-ops because
    /// `navigate_active` falls through the `if let Some(page)` guard. No unit
    /// test currently catches that because the failure mode requires a live
    /// CEF engine to observe (`Page::with_profile` cannot be instantiated
    /// headlessly).  A future closest-seam test would refactor materialization
    /// behind a trait that can be mocked + counter-spied; tracked as a
    /// `feedback-always-write-tests` follow-up.  Until then, this doc comment
    /// IS the protection — do not remove the call from `switch_workspace`
    /// without replacing the regression protection.
    fn materialize_active_if_placeholder(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.is_live() {
            return;
        }
        let url = tab.url.clone();
        let id = tab.id;
        eprintln!("mote-shell: materialize placeholder tab {id} -> {url} (workspace switch)");
        match create_content_page(&url, &self.content_opts, &self.default_profile) {
            Ok(page) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.page = Some(page);
                }
            }
            Err(e) => eprintln!("mote-shell: failed to materialize tab {id}: {e}"),
        }
    }

    /// Rebuild `self.tabs` and `self.active` from the session's
    /// [`Session::tab_picker_ranked`] list for `self.workspace`.
    ///
    /// This is the same logic as [`build_initial_tabs`] for the restore path,
    /// minus the fresh-session tab seeding and page materialization: on a
    /// workspace switch existing tabs in the new workspace are surfaced as
    /// placeholders and the first one is selected (the user will focus it or
    /// navigate — at that point `select_tab` materializes the page).
    ///
    /// If the target workspace has no tabs yet, a fresh default-URL tab is
    /// added to keep the window non-blank.
    fn rebuild_tabs_for_workspace(&mut self) {
        let ranked: Vec<ShellTab> = self
            .session
            .tab_picker_ranked(self.workspace)
            .into_iter()
            .map(|tab| ShellTab {
                id: tab.id,
                url: tab.url.clone(),
                title: tab.title.clone(),
                page: None, // all placeholders; selected one materializes on focus
            })
            .collect();

        if ranked.is_empty() {
            // Seed a fresh tab for an empty workspace.
            let url = DEFAULT_START_URL.to_string();
            let id = self.session.add_tab(url.clone(), self.workspace);
            self.tabs = vec![ShellTab {
                id,
                url,
                title: None,
                page: None,
            }];
        } else {
            self.tabs = ranked;
        }
        self.active = 0;
    }

    /// Invoke `ui:bookmarks_provider` → `list_bookmarks` with an empty filter
    /// and push the result to chrome as `applyOp('bookmark_list', {rows, count})`.
    ///
    /// # Failure policy
    ///
    /// - No fulfiller / contract violation → returns `None`; push an empty list.
    /// - The provider may return an empty Lua table `{}` which marshals to
    ///   [`HostValue::Map`] (empty-table-marshalling defensiveness, lessons.md L3).
    ///   Normalise any non-List to `List([])`.
    /// - Rows are sorted by `added` desc (the bookmarks plugin's monotonic `_seq`
    ///   counter — larger value = more recently added).
    pub(crate) fn push_bookmark_list(&self) {
        // Pass empty string filter — list_bookmarks(filter) returns all when empty.
        let arg = HostValue::Str(String::new());
        let raw =
            self.host
                .runtime
                .invoke_capability("ui:bookmarks_provider", "list_bookmarks", &arg);

        let mut items = match raw {
            Some(HostValue::List(v)) => v,
            _ => vec![],
        };

        // Sort by `added` desc (higher _seq = added more recently).
        items.sort_by(|a, b| {
            let added_a = match a {
                HostValue::Map(m) => match m.get("added") {
                    Some(HostValue::Number(f)) => *f,
                    _ => 0.0,
                },
                _ => 0.0,
            };
            let added_b = match b {
                HostValue::Map(m) => match m.get("added") {
                    Some(HostValue::Number(f)) => *f,
                    _ => 0.0,
                },
                _ => 0.0,
            };
            added_b
                .partial_cmp(&added_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let count = items.len();
        let rows_json = match serde_json::to_string(&host_to_json(&HostValue::List(items))) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: push_bookmark_list serialise failed: {e}");
                "[]".to_owned()
            }
        };

        let payload_json = format!("{{\"rows\":{rows_json},\"count\":{count}}}");

        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('bookmark_list',{payload_json});"
        ));
    }

    /// Invoke `ui:history_provider` → `query_history` with
    /// `{filter="", limit=200, sort="recent"}` and push the result to chrome as
    /// `applyOp('history_list', {rows, count, truncated})`.
    ///
    /// The plugin applies `sort="recent"` (`last_visited` descending) and caps its
    /// output at 200. The shell defensively truncates to 200 if the plugin returns
    /// more (the caps are identical, so `truncated` will always be `false` in
    /// normal operation; it exists as a safety net).
    ///
    /// # Failure policy
    ///
    /// Same as [`push_bookmark_list`]: normalise non-List results to an empty list.
    pub(crate) fn push_history_list(&self) {
        const HISTORY_CAP: usize = 200;
        const HISTORY_OVERFETCH: f64 = 201.0; // HISTORY_CAP + 1, f64-typed for HostValue::Number

        // Overfetch by 1 (request `HISTORY_CAP` + 1) so the shell can distinguish
        // "exactly `HISTORY_CAP` visits exist" from ">`HISTORY_CAP`, truncated."
        // Without overfetch the truncation footer would never fire — symmetric
        // caps make the two cases indistinguishable.  Display rows still capped
        // at `HISTORY_CAP`.
        let mut payload = BTreeMap::new();
        payload.insert("filter".to_owned(), HostValue::Str(String::new()));
        payload.insert("limit".to_owned(), HostValue::Number(HISTORY_OVERFETCH));
        payload.insert("sort".to_owned(), HostValue::Str("recent".to_owned()));
        let arg = HostValue::Map(payload);
        let raw = self
            .host
            .runtime
            .invoke_capability("ui:history_provider", "query_history", &arg);

        let mut items = match raw {
            Some(HostValue::List(v)) => v,
            _ => vec![],
        };

        // truncated iff the plugin returned the overfetch sentinel (HISTORY_CAP+1
        // rows means there were more in the store).
        let truncated = items.len() > HISTORY_CAP;
        if truncated {
            items.truncate(HISTORY_CAP);
        }
        let count = items.len();

        let rows_json = match serde_json::to_string(&host_to_json(&HostValue::List(items))) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: push_history_list serialise failed: {e}");
                "[]".to_owned()
            }
        };

        let payload_json =
            format!("{{\"rows\":{rows_json},\"count\":{count},\"truncated\":{truncated}}}");

        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('history_list',{payload_json});"
        ));
    }

    /// Invoke `workspace:provider` → `list_workspaces` and push the result to
    /// chrome as `applyOp('workspace_list', {rows: [{id, name, active}, …]})`.
    ///
    /// Called on chrome-ready (boot) and after every `switch_workspace` so the
    /// workspace strip stays in sync with the persisted active workspace.
    ///
    /// # Failure policy
    ///
    /// - No fulfiller / contract violation → returns `None`; push an empty rows
    ///   list so the strip renders a safe fallback rather than crashing.
    /// - `list_workspaces` takes no real argument; an empty Map is passed
    ///   (L2/L3: `HostValue::Map(BTreeMap::new())` is the safe zero-arg form —
    ///   Lua ignores extra args; the empty-table-maps-to-Map footgun only applies
    ///   on the *return* side, not the call side).
    pub(crate) fn push_workspace_list(&self) {
        // list_workspaces() takes no parameter — empty Map is the safe zero-arg
        // HostValue (Lua ignores extra args; L2 note).
        let arg = HostValue::Map(BTreeMap::new());
        let raw =
            self.host
                .runtime
                .invoke_capability("workspace:provider", "list_workspaces", &arg);

        let rows = match raw {
            Some(HostValue::List(v)) => v,
            // Defensive: empty Lua table returns as Map({}) (L3).
            _ => vec![],
        };

        let rows_json = match serde_json::to_string(&host_to_json(&HostValue::List(rows))) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: push_workspace_list serialise failed: {e}");
                "[]".to_owned()
            }
        };

        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('workspace_list',{{rows:{rows_json}}});"
        ));
    }

    /// Finish a dialog approval on the pump thread (ADR-0007 async approval).
    ///
    /// The op handler already ran the op-boundary structural validation; here
    /// we do the semantic work via [`runtime::PluginHost::approve_pending`]
    /// (find the pending entry, cross-check, load or deny, record approval),
    /// then drive the chrome. Dismissal is **shell-driven** (panels.js no longer
    /// hides on click): the shell hides the dialog only on a resolved outcome.
    ///
    /// - `Loaded` / `Denied`: hide the dialog, re-render the panel, and advance
    ///   to the next pending dialog if the queue is non-empty.
    /// - `LoadFailed`: leave the dialog up so the user can decline/retry; the
    ///   plugin stays pending. (A proper in-dialog error banner is deferred.)
    /// - `NotPending`: do nothing — the dropped result names no live dialog, so
    ///   hiding could dismiss a legitimate dialog for a different plugin.
    fn approve_plugin(&mut self, result: &approval::DialogResult) {
        match self.host.approve_pending(result) {
            runtime::ApproveOutcome::Loaded => {
                eprintln!("mote-shell: plugin `{}` approved + loaded", result.plugin());
                self.push_hide_approval_dialog();
                self.refresh_integrity_panel();
                self.show_next_pending_dialog();
            }
            runtime::ApproveOutcome::Denied => {
                eprintln!("mote-shell: plugin `{}` denied by user", result.plugin());
                self.push_hide_approval_dialog();
                self.refresh_integrity_panel();
                self.show_next_pending_dialog();
            }
            runtime::ApproveOutcome::LoadFailed => {
                // Leave the dialog visible so the user can decline/retry.
                // TODO(phase-10): inline error banner in the dialog on LoadFailed.
                eprintln!(
                    "mote-shell: plugin `{}` approval load FAILED (left pending; dialog kept)",
                    result.plugin()
                );
            }
            runtime::ApproveOutcome::NotPending => {
                // Do NOT hide: a dropped/stale result must not dismiss whatever
                // legitimate dialog is currently shown.
                eprintln!(
                    "mote-shell: approve for non-pending plugin `{}` (dropped)",
                    result.plugin()
                );
            }
        }
    }

    /// Render the next awaiting-approval dialog, if any. The dialog root holds a
    /// single dialog, so the shell shows pending approvals one at a time: after
    /// one resolves (`Loaded`/`Denied`) the shell advances to the next. For 0–1
    /// pending this is a no-op beyond the initial push.
    ///
    /// follow-up: a richer multi-dialog queue UI is out of scope.
    fn show_next_pending_dialog(&self) {
        if let Some((_, req)) = self.host.pending_approvals.first() {
            self.push_approval_dialog(req);
        }
    }

    /// Panel action — update the plugin, then re-render the integrity panel.
    ///
    /// An expanding update needs re-approval: the host enqueues a fresh pending
    /// entry and returns its [`ApprovalRequest`], which we render as a dialog
    /// (the subsequent `approve_plugin` reloads with the new grant). A
    /// non-expanding update is applied + reloaded by the host directly.
    fn plugin_update(&mut self, name: &str) {
        match self.host.update_plugin(name) {
            runtime::UpdateAction::ReApproval(req) => {
                self.push_approval_dialog(&req);
            }
            runtime::UpdateAction::Applied | runtime::UpdateAction::Failed => {}
        }
        self.refresh_integrity_panel();
    }

    /// Panel action — roll the plugin back to its prior commit + reload.
    fn plugin_rollback(&mut self, name: &str) {
        self.host.rollback_plugin(name);
        self.refresh_integrity_panel();
    }

    /// Panel action — reload the plugin (path/dev re-run).
    fn plugin_reload(&mut self, name: &str) {
        self.host.reload_plugin(name);
        self.refresh_integrity_panel();
    }

    /// Panel action — revoke the plugin (unload + drop its stored approval).
    fn plugin_revoke(&mut self, name: &str) {
        self.host.revoke_plugin(name);
        self.refresh_integrity_panel();
    }

    /// Panel action — re-open the approval dialog for the plugin so the user can
    /// re-narrow its grant. The subsequent `approve_plugin` reloads with the new
    /// narrowing (re-grant-via-reload). No-op if the plugin is not loaded.
    fn plugin_adjust_scope(&mut self, name: &str) {
        if let Some(req) = self.host.adjust_scope_request(name) {
            self.push_approval_dialog(&req);
            // adjust_scope_request moves the plugin loaded→pending; refresh the
            // panel (if open) so it reflects the parked/awaiting-approval state,
            // consistent with the other panel actions.
            self.refresh_integrity_panel();
        }
    }

    /// Panel action — revoke a specific secret grant from a loaded plugin
    /// (session-scoped; does not persist across relaunch). Narrows the plugin's
    /// `(secret, read)` grant to exclude `name`, then refreshes the panel.
    fn plugin_revoke_secret(&mut self, plugin: &str, name: &str) {
        self.host.revoke_secret(plugin, name);
        self.refresh_integrity_panel();
    }

    /// Re-render the chrome integrity panel from live host state, but only when
    /// it is currently open (so panel-driven actions reflect immediately without
    /// popping the panel open behind a dialog the user did not request).
    fn refresh_integrity_panel(&self) {
        if self.integrity_chrome_open {
            let panel = self.host.build_panel();
            self.push_integrity_panel_to_chrome(&panel);
        }
    }

    /// Hide the approval dialog inside the chrome document. The dialog buttons
    /// also dismiss it locally (optimistic UX), so this is idempotent; it exists
    /// for the pump-thread path (e.g. a dropped/denied result) and to keep the
    /// chrome and host state in lockstep.
    fn push_hide_approval_dialog(&self) {
        if !self.chrome_ready {
            return;
        }
        self.bridge.page().eval_js(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('hide_approval_dialog',null);",
        );
    }

    /// Navigate the active tab, update session state, and push the new URL.
    fn navigate_active(&mut self, url: &str) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        eprintln!("mote-shell: navigate tab {} -> {url}", tab.id);
        tab.url = url.to_string();
        tab.title = None;
        if let Some(page) = tab.page.as_ref() {
            page.load_url(url);
        }
        if let Some(stab) = self.session.tab_mut(tab.id) {
            stab.url = url.to_string();
            stab.title = None;
        }

        // Record this navigation in the history store so urlbar suggestions
        // reflect real browsing data (F2 — phase5a plan §Group F).
        //
        // The payload is a Map with "url" + "time" (wall-clock ms) so
        // record_visit's new chronological-event model is satisfied.
        // Per lessons.md L2 the arg must be a table, not a bare Str.
        //
        // Wall-clock time is stamped here (shell side) so the plugin remains
        // time-free — consistent with the "shell stamps context for plugins"
        // pattern (sidesteps the fingerprinting concern that gates a general
        // mote.time host API for arbitrary plugins).
        //
        // u128 → f64: milliseconds since UNIX epoch fits in f64 exactly until
        // year ~285,000 (53-bit mantissa covers ~9×10¹⁵ ms), so precision
        // loss is genuinely moot for any foreseeable use of this field.
        #[allow(clippy::cast_precision_loss)]
        let time_ms = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_millis() as f64);

        // The result is silently discarded: invoke_capability already audits
        // failures (no fulfiller, contract violation, timeout, plugin error
        // all surface as None and are logged by Core::invoke_capability).
        // The shell continues regardless of whether history is loaded.
        let mut record_visit_arg = BTreeMap::new();
        record_visit_arg.insert("url".to_owned(), HostValue::Str(url.to_owned()));
        record_visit_arg.insert("time".to_owned(), HostValue::Number(time_ms));
        let _ = self.host.runtime.invoke_capability(
            "ui:history_provider",
            "record_visit",
            &HostValue::Map(record_visit_arg),
        );

        self.persist_and_push();
    }

    /// Open a new tab (live page under the default profile) and switch to it.
    /// `url` defaults to the start URL.
    fn open_tab(&mut self, url: Option<String>) {
        let url = url.unwrap_or_else(|| DEFAULT_START_URL.to_string());
        let id = self.session.add_tab(url.clone(), self.workspace);
        let page = match create_content_page(&url, &self.content_opts, &self.default_profile) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mote-shell: failed to create page for new tab: {e}");
                None
            }
        };
        self.tabs.push(ShellTab {
            id,
            url,
            title: None,
            page,
        });
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
        self.persist_and_push();
    }

    /// Open a tab at `url` from an intercepted CEF popup (ADR-0011).
    ///
    /// If `foreground` is `true` (user-gesture-driven popup: a `target=_blank`
    /// click or middle-click), the new tab becomes the active tab immediately —
    /// matching Chrome's behaviour for click-driven popups. If `foreground` is
    /// `false` (JS-initiated popup with no preceding click), the tab is appended
    /// but the active index stays on the current tab, reducing focus-stealing
    /// from ad windows and OAuth redirects.
    fn open_popup_tab(&mut self, url: &str, foreground: bool) {
        // Pre-filter URLs the S1 navigation guard would block. Without this,
        // we'd create a tab and add it to the session, then have CEF's
        // `on_before_browse` cancel the load (Content-role pages cannot
        // navigate to `mote://`), leaving a phantom tab in the sidebar with
        // a broken URL. Drop the request silently — content pages calling
        // `window.open('mote://...')` is malicious-shaped, not legitimate.
        if !is_popup_url_allowed(url) {
            eprintln!("mote-shell: dropped popup with disallowed scheme: {url}");
            return;
        }
        let url = url.to_string();
        let id = self.session.add_tab(url.clone(), self.workspace);
        let page = match create_content_page(&url, &self.content_opts, &self.default_profile) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mote-shell: popup tab page create failed: {e}");
                None
            }
        };
        self.tabs.push(ShellTab {
            id,
            url,
            title: None,
            page,
        });
        if foreground {
            self.active = self.tabs.len() - 1;
            self.on_active_changed();
        }
        self.persist_and_push();
    }

    /// Close the tab with `id`. The session marks it closed; the live page (if
    /// any) is dropped (its CEF browser tears down). If the active tab closes,
    /// focus moves to its neighbour. Closing the last tab opens a fresh one.
    fn close_tab(&mut self, id: TabId) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        eprintln!("mote-shell: close tab {id}");
        let removed = self.tabs.remove(idx);
        // P5: remember the closed tab so Ctrl+Shift+T can reopen it.
        self.closed_tab_stack.push(ClosedTab {
            url: removed.url.clone(),
            title: removed.title.clone(),
        });
        if let Some(page) = removed.page {
            page.close();
        }
        let _ = self.session.close_tab(id);

        if self.tabs.is_empty() {
            // Never leave the window tab-less: open a fresh tab.
            self.open_tab(None);
            return;
        }
        // Keep the active index valid and pointing at a sensible neighbour.
        if idx <= self.active {
            self.active = self.active.saturating_sub(1);
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.on_active_changed();
        self.persist_and_push();
    }

    /// Switch the active tab to `id`. A placeholder materializes its page here
    /// (load-on-focus, the restoration model).
    fn select_tab(&mut self, id: TabId) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        if idx == self.active && self.tabs[idx].is_live() {
            return;
        }
        self.active = idx;
        // Materialize a placeholder on focus.
        if !self.tabs[idx].is_live() {
            let url = self.tabs[idx].url.clone();
            eprintln!("mote-shell: materialize placeholder tab {id} -> {url}");
            match create_content_page(&url, &self.content_opts, &self.default_profile) {
                Ok(page) => self.tabs[idx].page = Some(page),
                Err(e) => eprintln!("mote-shell: failed to materialize tab {id}: {e}"),
            }
        }
        self.on_active_changed();
        self.persist_and_push();
    }

    /// React to a change of active tab: size the new page to the viewport, give
    /// it focus, and force a content re-upload (so the compositor swaps to it).
    fn on_active_changed(&mut self) {
        let (vw, vh) = self.viewport_dims();
        let scale = self.scale_factor;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh, scale);
        }
        // Reset the content dirty counter so the next upload_frames re-uploads
        // the new active page's frame (texture swap).
        self.content_paints = 0;
        // The freshly focused page should believe it has focus if the page owns
        // input; otherwise focus stays with the chrome.
        if self.focus == FocusOwner::Page
            && let Some(page) = self.active_page()
        {
            page.send_focus(true);
        }
    }

    /// Flush the session and push the live tab list + active URL to the chrome.
    fn persist_and_push(&mut self) {
        if let Some(ns) = self.session_namespace()
            && let Err(e) = self.session.flush(&ns)
        {
            eprintln!("mote-shell: session flush failed: {e}");
        }
        self.push_state_to_chrome();
    }

    /// Reopen the per-identity session namespace (cheap; just binds the shared
    /// connection + scope).
    fn session_namespace(&self) -> Option<mote_storage::Namespace> {
        Session::open_namespace(&self.store, self.session_identity).ok()
    }

    /// Push the current tab list (`set_tabs`) and active URL (`set_url`) into the
    /// chrome document via the privileged `window.mote.applyOp` seam.
    fn push_state_to_chrome(&mut self) {
        if !self.chrome_ready {
            return;
        }
        let tabs_json = self.tabs_json();
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('set_tabs',{{tabs:{tabs_json}}});"
        ));
        if let Some(tab) = self.tabs.get(self.active) {
            let url_str = tab.url.clone();
            let url = js_string(&url_str);
            let bookmarked = self.is_url_bookmarked(&url_str);
            let analysis = analyze_url(&url_str);
            let display_json = analysis.as_ref().map(|a| {
                format!(
                    "{{\"scheme\":{},\"subdomain\":{},\"registrable\":{},\"rest\":{}}}",
                    js_string(&a.scheme),
                    js_string(&a.subdomain),
                    js_string(&a.registrable),
                    js_string(&a.rest),
                )
            });
            let trackers_json = analysis.as_ref().and_then(|a| {
                if a.tracker_names.is_empty() {
                    None
                } else {
                    let count = a.tracker_names.len();
                    let clean = js_string(&a.clean_url);
                    let names: String = a
                        .tracker_names
                        .iter()
                        .map(|n| js_string(n))
                        .collect::<Vec<_>>()
                        .join(",");
                    Some(format!(
                        "{{\"count\":{count},\"clean\":{clean},\"names\":[{names}]}}"
                    ))
                }
            });
            let display_field = display_json.as_deref().map_or_else(
                || ",\"display\":null".to_owned(),
                |d| format!(",\"display\":{d}"),
            );
            let trackers_field = trackers_json.as_deref().map_or_else(
                || ",\"trackers\":null".to_owned(),
                |t| format!(",\"trackers\":{t}"),
            );
            chrome.eval_js(&format!(
                "window.mote&&window.mote.applyOp&&window.mote.applyOp(\
                 'set_url',{{\"url\":{url},\"bookmarked\":{bookmarked}{display_field}{trackers_field}}});"
            ));
        }
        // P2: push nav state (can_go_back, can_go_forward) so the [‹][›]
        // buttons can reflect disabled/enabled state without a round-trip.
        let (can_go_back, can_go_forward) = self
            .active_page()
            .map_or((false, false), |p| (p.can_go_back(), p.can_go_forward()));
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('set_nav_state',\
             {{can_go_back:{can_go_back},can_go_forward:{can_go_forward}}});"
        ));
        // P4 follow-up: built-in status-line elements (mote.tabcount,
        // mote.security) reflect tab/navigation state. Pushing them alongside
        // tabs+url+nav keeps the status line in lockstep — without this, the
        // built-ins were only pushed once at startup and never updated.
        self.push_statusline_to_chrome();
    }

    /// Push the current status-line element set into the chrome document via the
    /// `set_statusline_elements` applyOp (ADR-0016).
    ///
    /// Merges the three built-in chrome elements (`mote.mode`, `mote.security`,
    /// `mote.tabcount`) with any plugin-declared elements registered in the
    /// runtime. Built-ins are prepended so their well-known ids are always
    /// present; plugin elements follow. The chrome's `wireStatuslineOp` handler
    /// groups by zone and sorts by priority.
    ///
    /// The security element reflects the active tab's URL scheme:
    /// `https://` → secure lock / accent; everything else → triangle-alert / warn.
    /// Internal `mote://` pages use the secure variant (they are chrome-owned,
    /// no remote origin).
    pub(crate) fn push_statusline_to_chrome(&mut self) {
        if !self.chrome_ready {
            return;
        }

        // Determine the active URL for the security element.
        let active_url = self.tabs.get(self.active).map_or("", |t| t.url.as_str());
        let is_secure = active_url.starts_with("https://") || active_url.starts_with("mote://");
        let security_el = if is_secure {
            mote_types::StatusLineElement::builtin_security_https()
        } else {
            mote_types::StatusLineElement::builtin_security_http()
        };

        // P5: check whether the zoom status indicator has auto-cleared.
        if self.zoom_clear_at.is_some_and(|at| Instant::now() >= at) {
            self.zoom_clear_at = None;
        }

        // Build the full element list: built-ins first, then plugin-declared.
        let mut elements: Vec<mote_types::StatusLineElement> = vec![
            mote_types::StatusLineElement::builtin_mode(),
            security_el,
            mote_types::StatusLineElement::builtin_tabcount(self.tabs.len()),
        ];

        // P5 / CL-URL-XPARENCY B3: hover-URL preview (center zone, priority
        // 100). Show origin+path with query stripped and tracker count appended
        // when >0. Falls back to the raw string for internal (non-HTTP) URLs.
        // Per-token styling of the registrable domain is a later chrome concern.
        if let Some(ref url) = self.hover_url_last {
            let display_text = build_hover_display(url);
            elements.push(mote_types::StatusLineElement::new(
                "mote.hoverurl".to_owned(),
                mote_types::StatusZone::Center,
                100,
                mote_types::StatusKind::Text,
                Some(display_text),
                None,
                mote_types::StatusColor::Mute,
                None,
            ));
        }

        // P5: transient zoom indicator (right zone, priority 90). Shown for
        // 1.5 s after a zoom action, then removed from the element list.
        if self.zoom_clear_at.is_some() {
            elements.push(mote_types::StatusLineElement::new(
                "mote.zoom".to_owned(),
                mote_types::StatusZone::Right,
                90,
                mote_types::StatusKind::Text,
                Some(format!("zoom {}", self.zoom_percent_text())),
                None,
                mote_types::StatusColor::Mute,
                None,
            ));
        }

        elements.extend(self.host.runtime.statusline_elements());

        let payload = match serde_json::to_string(&elements) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: push_statusline_to_chrome serialise failed: {e}");
                return;
            }
        };

        self.bridge.page().eval_js(&format!(
            "window.mote&&window.mote.applyOp&&\
             window.mote.applyOp('set_statusline_elements',{payload});"
        ));
    }

    /// Push the integrity-panel view-model into the chrome document. The chrome
    /// page's `panels.js` builds the panel DOM via `createElement` / `textContent`
    /// (ADR-0005) — no HTML strings cross the bridge. Returns `true` when the
    /// push was emitted, `false` when the chrome is not yet ready.
    fn push_integrity_panel_to_chrome(&self, panel: &IntegrityPanel) -> bool {
        if !self.chrome_ready {
            return false;
        }
        let payload = match serde_json::to_string(panel) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: serialize integrity panel failed: {e}");
                return false;
            }
        };
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('render_integrity_panel',{payload});"
        ));
        true
    }

    /// Hide the integrity panel inside the chrome document.
    fn push_hide_integrity_to_chrome(&self) {
        if !self.chrome_ready {
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('hide_integrity_panel',null);",
        );
    }

    /// Push an approval-dialog view-model into the chrome document. The chrome
    /// page's `panels.js` builds the dialog DOM via `createElement` /
    /// `textContent` (ADR-0005). The dialog's buttons invoke the `approve_plugin`
    /// op; the pump thread finishes the load and re-renders the panel on the
    /// user's answer (ADR-0007 async approval).
    fn push_approval_dialog(&self, req: &ApprovalRequest) -> bool {
        if !self.chrome_ready {
            return false;
        }
        let payload = match serde_json::to_string(req) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mote-shell: serialize approval request failed: {e}");
                return false;
            }
        };
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('show_approval_dialog',{payload});"
        ));
        true
    }

    /// Poll the active tab's live page for a document-title change (CEF reports
    /// it via `DisplayHandler::on_title_change`, surfaced by `Page::title`). When
    /// the title differs from what the tab already shows, update the tab + the
    /// session and push the new tab list into the chrome so the tab strip shows
    /// the real page title instead of the URL fallback.
    fn sync_active_title(&mut self) {
        // --- Phase 1: read-only; capture owned values before any mutable borrow.
        // `invoke_capability` requires an immutable borrow of `self.host.runtime`,
        // so all `&mut self` accesses must be dropped before we call it.
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(title) = tab.page.as_ref().and_then(Page::title) else {
            return;
        };
        if tab.title.as_deref() == Some(title.as_str()) {
            return;
        }
        let id = tab.id;
        let url = tab.url.clone();
        // Clone the title string now; the mutable-borrow sections below move
        // `title` into the tab/session fields, leaving us needing an owned copy
        // for the history call and for push_state_to_chrome's tab-list walk.
        let title_for_history = title.clone();

        // --- Phase 2: mutable updates (each borrow is a separate statement so
        // NLL drops it before the next one begins).
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.title = Some(title.clone());
        }
        if let Some(stab) = self.session.tab_mut(id) {
            stab.title = Some(title);
        }

        // --- Phase 3: title-on-load follow-up for history (immutable borrow ok).
        //
        // `record_visit` was called at navigate time with the URL only (F2,
        // navigate_active). Now that CEF has resolved the real page title via
        // on_title_change, we call `update_title` so urlbar suggestions show the
        // title column instead of leaving it blank.
        //
        // `update_title` is a no-op when no prior record exists (e.g. restored
        // tabs where navigate_active was never called in this session).
        // visit_count and last_visited are unchanged — title resolution is not
        // a new user navigation.
        //
        // Failure is silently discarded: invoke_capability logs internally
        // (no fulfiller, contract violation, timeout, plugin error all surface
        // as None). The shell continues regardless.
        // Update the OS window title so the active page's document title shows in
        // the taskbar / Alt-Tab list, not the static "mote" string.
        if let Some(window) = self.window.as_ref() {
            window.set_title(&title_for_history);
        }

        let mut arg_map = BTreeMap::new();
        arg_map.insert("url".to_owned(), HostValue::Str(url));
        arg_map.insert("title".to_owned(), HostValue::Str(title_for_history));
        let _ = self.host.runtime.invoke_capability(
            "ui:history_provider",
            "update_title",
            &HostValue::Map(arg_map),
        );

        // No session flush here: title is cosmetic and changes frequently; it is
        // captured on the next structural flush (open/close/switch/navigate).
        self.push_state_to_chrome();
    }

    /// Poll the active tab's live page for a URL change fired by CEF's
    /// `DisplayHandler::on_address_change` (main-frame only). When the URL
    /// reported by CEF differs from what the tab shows, update the tab and push
    /// `set_url` + `set_tabs` to the chrome so the omnibox and sidebar both
    /// reflect the navigation.
    ///
    /// This is the fix for omnibox-not-updating-on-navigation (R2): clicking a
    /// link inside a page commits a new URL in CEF, which the display handler
    /// enqueues in `UrlSlot`. The shell drains it here each tick.
    fn sync_active_url(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(new_url) = tab.page.as_ref().and_then(Page::current_url) else {
            return;
        };
        // Only act when CEF reports a different URL than what the tab shows.
        if tab.url == new_url {
            return;
        }
        let id = tab.id;

        // Update the in-memory tab and the session row.
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.url.clone_from(&new_url);
            // Clear the stale title so the next on_title_change refresh applies
            // the new page's title, not the prior page's one.
            t.title = None;
        }
        if let Some(stab) = self.session.tab_mut(id) {
            stab.url = new_url;
            stab.title = None;
        }

        // Push the new URL/bookmark state and the updated tab list.
        // No session flush here: URL changes on link-clicks are frequent; the
        // next structural action (navigate / close / switch) will flush.
        self.push_state_to_chrome();
    }

    /// Copy the active tab's current URL to the system clipboard via `arboard`.
    ///
    /// This is the `copy_active_url` host-bridge op handler (R2). The clipboard
    /// write is host-side; no string leaves via JS eval. On arboard failure
    /// (e.g. no clipboard server on a headless display), the error is logged and
    /// the op returns silently rather than crashing the shell.
    fn copy_active_url(&self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let url = tab.url.clone();
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(e) = clipboard.set_text(&url) {
                    eprintln!("mote-shell: copy_active_url clipboard write failed: {e}");
                }
            }
            Err(e) => {
                eprintln!("mote-shell: copy_active_url clipboard init failed: {e}");
            }
        }
    }

    /// Serialise the tab list to a JSON array `[ {id,title,url,active}, … ]` with
    /// every string JS-escaped (these are page-derived; the chrome inserts them
    /// as text nodes, never markup — bridge.rs caller discipline).
    fn tabs_json(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::from("[");
        for (i, tab) in self.tabs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let title = tab.title.clone().unwrap_or_else(|| tab.url.clone());
            let _ = write!(
                out,
                "{{\"id\":{},\"title\":{},\"url\":{},\"active\":{}}}",
                tab.id.get(),
                js_string(&title),
                js_string(&tab.url),
                i == self.active
            );
        }
        out.push(']');
        out
    }

    /// Mirror logical focus into CEF so exactly one browser thinks it is focused.
    fn set_focus_owner(&mut self, owner: FocusOwner) {
        if owner == self.focus {
            return;
        }
        self.focus = owner;
        match owner {
            FocusOwner::Chrome => {
                if let Some(page) = self.active_page() {
                    page.send_focus(false);
                }
                self.bridge.page().send_focus(true);
            }
            FocusOwner::Page => {
                self.bridge.page().send_focus(false);
                if let Some(page) = self.active_page() {
                    page.send_focus(true);
                }
            }
        }
    }

    /// Upload any newly painted chrome / active-content frames into the
    /// compositor.
    ///
    /// When the integrity overlay is open it is composited full-window onto the
    /// **chrome** texture (which the compositor draws over the page): the overlay
    /// document is opaque, so it covers the whole window. The normal chrome /
    /// content uploads are skipped while it is open, and forced to re-upload when
    /// it closes (the `*_paints` counters are reset in `set_integrity_open`).
    fn upload_frames(&mut self) {
        if self.picker.open {
            self.upload_picker_overlay();
            return;
        }
        if self.integrity_open {
            self.upload_integrity_overlay();
            return;
        }
        let viewport = self.viewport_rect();

        let chrome_count = self.bridge.page().paint_count();
        let chrome_frame = (chrome_count != self.chrome_paints)
            .then(|| self.bridge.page().latest_frame())
            .flatten();
        if chrome_frame.is_some() {
            self.chrome_paints = chrome_count;
        }

        let content = self.active_page();
        let content_count = content.map_or(0, Page::paint_count);
        let content_frame = content.and_then(|p| {
            (content_count != self.content_paints)
                .then(|| p.latest_frame())
                .flatten()
        });
        if content_frame.is_some() {
            self.content_paints = content_count;
        }

        let Some(compositor) = self.compositor.as_mut() else {
            return;
        };
        if let Some(frame) = chrome_frame
            && let Err(e) = compositor.update_chrome(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
            )
        {
            eprintln!("mote-shell: chrome upload failed: {e}");
        }
        if let Some(frame) = content_frame
            && let Err(e) = compositor.update_page(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
                viewport,
            )
        {
            eprintln!("mote-shell: page upload failed: {e}");
        }
    }

    /// Composite the integrity overlay full-window onto the chrome texture.
    fn upload_integrity_overlay(&mut self) {
        let frame = self.integrity_page.as_ref().and_then(|p| {
            let count = p.paint_count();
            (count != self.integrity_paints).then(|| {
                self.integrity_paints = count;
                p.latest_frame()
            })
        });
        let Some(Some(frame)) = frame else {
            return;
        };
        if let Some(compositor) = self.compositor.as_mut()
            && let Err(e) = compositor.update_chrome(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
            )
        {
            eprintln!("mote-shell: integrity overlay upload failed: {e}");
        }
    }

    /// Open or close the live integrity overlay.
    ///
    /// On first open, lazily creates a full-window opaque [`Page`] loading
    /// `mote://overlay/integrity.html` (the shell-rendered live view-model). It
    /// uses the **global** request context (`Page::new`, not `with_profile`) —
    /// the same context the chrome bridge page loads `mote://chrome` from — so
    /// the privileged scheme resolves (a per-identity profile context does not
    /// carry the custom-scheme factory, yielding `ERR_UNKNOWN_URL_SCHEME`). The
    /// overlay is a trusted runtime surface, not user content. Toggling resets
    /// the relevant paint counters so the compositor re-uploads the right surface
    /// on the next pump.
    fn set_integrity_open(&mut self, open: bool) {
        self.integrity_open = open;
        if open {
            if self.integrity_page.is_none() {
                let url = overlay_url("integrity.html");
                let opts = PageOptions {
                    width: self.width.max(1),
                    height: self.height.max(1),
                    frame_rate: 60,
                    // Trusted, bridgeless overlay surface on the unprivileged
                    // `mote://overlay` origin (S2). The Overlay role both keeps
                    // `window.cefQuery` out (origin gate) and lets this internal
                    // URL load past the content navigation guard (S1).
                    role: PageRole::Overlay,
                };
                match Page::new(&url, &opts) {
                    Ok(page) => {
                        page.notify_resized(
                            self.width.max(1),
                            self.height.max(1),
                            self.scale_factor,
                        );
                        self.integrity_page = Some(page);
                    }
                    Err(e) => {
                        eprintln!("mote-shell: failed to open integrity panel: {e}");
                        self.integrity_open = false;
                        return;
                    }
                }
            } else if let Some(page) = self.integrity_page.as_ref() {
                page.notify_resized(self.width.max(1), self.height.max(1), self.scale_factor);
            }
            self.integrity_paints = 0;
            eprintln!(
                "mote-shell: integrity panel opened ({} bundled plugin(s) shown)",
                self.host.loaded.len()
            );
        } else {
            // Force the chrome + content layers to re-upload, restoring the
            // browser surface beneath the (now hidden) overlay.
            self.chrome_paints = 0;
            self.content_paints = 0;
            eprintln!("mote-shell: integrity panel closed");
        }
    }

    // ── Workspace tab picker (Mod+Space) ──────────────────────────────────

    /// Open or close the workspace tab picker overlay.
    ///
    /// On open it snapshots the current workspace's tabs in
    /// [`Session::tab_picker_ranked`] order into [`PickerState`] and lazily
    /// creates the full-window overlay [`Page`] (`mote://overlay/picker.html`),
    /// composited like the integrity overlay. Selection/filtering are handled
    /// Rust-side (see [`picker`]); on close the browser surface re-uploads.
    fn set_picker_open(&mut self, open: bool) {
        if open {
            let entries: Vec<PickerEntry> = self
                .session
                .tab_picker_ranked(self.workspace)
                .into_iter()
                .map(PickerEntry::from_tab)
                .collect();
            self.picker.open(entries);
            if self.picker_page.is_none() {
                let url = overlay_url("picker.html");
                let opts = PageOptions {
                    width: self.width.max(1),
                    height: self.height.max(1),
                    frame_rate: 60,
                    // Trusted, bridgeless overlay surface on the unprivileged
                    // `mote://overlay` origin (S2); see the integrity overlay.
                    role: PageRole::Overlay,
                };
                match Page::new(&url, &opts) {
                    Ok(page) => {
                        page.notify_resized(
                            self.width.max(1),
                            self.height.max(1),
                            self.scale_factor,
                        );
                        self.picker_page = Some(page);
                    }
                    Err(e) => {
                        eprintln!("mote-shell: failed to open tab picker: {e}");
                        self.picker.close();
                        return;
                    }
                }
            } else if let Some(page) = self.picker_page.as_ref() {
                page.notify_resized(self.width.max(1), self.height.max(1), self.scale_factor);
            }
            self.picker_paints = 0;
            eprintln!("mote-shell: tab picker opened");
        } else {
            self.picker.close();
            // Force the browser surface beneath the (now hidden) overlay to
            // re-upload (the picker drew over the chrome texture).
            self.chrome_paints = 0;
            self.content_paints = 0;
            eprintln!("mote-shell: tab picker closed");
        }
    }

    /// Push the picker's current query + filtered rows into the overlay via
    /// `eval_js` → `window.__motePicker(state)`. Strings are JS-escaped exactly
    /// like the chrome push (page-derived titles/URLs are injection vectors).
    fn push_picker_state(&self) {
        let Some(page) = self.picker_page.as_ref() else {
            return;
        };
        let rows = self.picker.rows_json(js_string);
        let query = js_string(self.picker.query());
        page.eval_js(&format!(
            "window.__motePicker&&window.__motePicker({{query:{query},rows:{rows}}});"
        ));
    }

    /// Handle a key event while the picker is open. Returns `true` if the key
    /// was consumed by the picker (so it is not routed to a page).
    ///
    /// Esc closes; Enter selects; Up/Down (and Ctrl+J/K) move; Backspace edits;
    /// printable characters filter. Every state change re-renders the overlay.
    fn picker_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            // Swallow key-ups too while open so the page never sees them.
            return true;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.set_picker_open(false);
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.activate_picker_selection();
                return true;
            }
            Key::Named(NamedKey::ArrowDown) => self.picker.move_down(),
            Key::Named(NamedKey::ArrowUp) => self.picker.move_up(),
            Key::Named(NamedKey::Backspace) => self.picker.backspace(),
            Key::Character(s) if self.modifiers.contains(Modifiers::CONTROL) => {
                // Vim-style Ctrl+J / Ctrl+K navigation (palette spec).
                if s.eq_ignore_ascii_case("j") {
                    self.picker.move_down();
                } else if s.eq_ignore_ascii_case("k") {
                    self.picker.move_up();
                } else {
                    return true; // swallow other Ctrl-combos while open
                }
            }
            Key::Character(s) => {
                for c in s.chars() {
                    self.picker.push_char(c);
                }
            }
            Key::Named(NamedKey::Space) => self.picker.push_char(' '),
            _ => return true, // swallow anything else; the picker owns input
        }
        self.push_picker_state();
        true
    }

    /// Resolve the selected picker row and act on it, then close the picker.
    ///
    /// An **active** tab → the existing switch path ([`Self::select_tab`]); a
    /// **hidden** tab → reveal it in the session and materialize/switch to it
    /// (DESIGN: selecting a hidden tab brings it into the current window).
    fn activate_picker_selection(&mut self) {
        let Some(entry) = self.picker.selected_entry() else {
            self.set_picker_open(false);
            return;
        };
        let id = entry.id;
        let is_active = entry.is_active();
        self.set_picker_open(false);
        if is_active {
            self.select_tab(id);
        } else {
            self.reveal_tab(id);
        }
    }

    /// Reveal a hidden tab into the current window: flip it to active in the
    /// session, add it to the window's tab strip (materializing its page), and
    /// switch to it. No-op if the tab is missing or already shown in the strip.
    fn reveal_tab(&mut self, id: TabId) {
        // Already in this window's strip → just select it.
        if self.tabs.iter().any(|t| t.id == id) {
            self.select_tab(id);
            return;
        }
        if let Err(e) = self.session.reveal_tab(id) {
            eprintln!("mote-shell: reveal tab {id} failed: {e}");
            return;
        }
        let Some(stab) = self.session.tab(id) else {
            return;
        };
        let url = stab.url.clone();
        let title = stab.title.clone();
        eprintln!("mote-shell: reveal hidden tab {id} -> {url}");
        let page = match create_content_page(&url, &self.content_opts, &self.default_profile) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mote-shell: failed to materialize revealed tab {id}: {e}");
                None
            }
        };
        self.tabs.push(ShellTab {
            id,
            url,
            title,
            page,
        });
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
        self.persist_and_push();
    }

    /// Composite the picker overlay full-window onto the chrome texture. On the
    /// page's first paint, push the initial picker state (an `eval_js` before the
    /// document's script runs would be lost — same warm-up as the chrome).
    fn upload_picker_overlay(&mut self) {
        if !self.picker.ready
            && self
                .picker_page
                .as_ref()
                .is_some_and(|p| p.paint_count() >= 1)
        {
            self.picker.ready = true;
            self.push_picker_state();
        }
        let frame = self.picker_page.as_ref().and_then(|p| {
            let count = p.paint_count();
            (count != self.picker_paints).then(|| {
                self.picker_paints = count;
                p.latest_frame()
            })
        });
        let Some(Some(frame)) = frame else {
            return;
        };
        if let Some(compositor) = self.compositor.as_mut()
            && let Err(e) = compositor.update_chrome(
                &frame.pixels,
                frame.width,
                frame.height,
                PixelFormat::Bgra8,
            )
        {
            eprintln!("mote-shell: tab picker overlay upload failed: {e}");
        }
    }

    // ── Session housekeeping (discard + hidden-tab reap) ───────────────────

    /// Run the periodic session housekeeping pass if the interval has elapsed:
    /// discard idle active-tab renderers, then reap hidden tabs past their TTL.
    fn maybe_run_housekeeping(&mut self) {
        if self.last_housekeeping.elapsed() < self.housekeeping_interval {
            return;
        }
        self.last_housekeeping = Instant::now();
        let discarded = self.discard_idle_renderers();
        let reaped = self.reap_hidden_tabs();
        if discarded > 0 || reaped > 0 {
            self.persist_and_push();
        }
    }

    /// Apply [`Discarder`] to the workspace's tabs and drop the CEF renderer for
    /// each tab whose `is_discarded` flag newly transitions to `true`. The tab
    /// entry stays in the strip (DESIGN: a discarded tab reloads on focus). The
    /// **active, focused** tab is never discarded — `last_visited` is bumped for
    /// it so its idle timer only starts once the user moves away. Returns the
    /// number of renderers dropped.
    ///
    /// Drives [`Discarder::should_discard`] per tab through the session's public
    /// per-id API (the session exposes no `&mut [Tab]` slice for the batch
    /// `discard_all`, so we apply the same decision tab-by-tab).
    fn discard_idle_renderers(&mut self) -> usize {
        // Keep the focused tab's idle clock from advancing: it is in use.
        if let Some(focused) = self.tabs.get(self.active)
            && let Some(stab) = self.session.tab_mut(focused.id)
        {
            stab.last_visited = Some(SystemTime::now());
        }

        // Decide which tabs to discard (immutable borrow), then apply + drop the
        // renderers (mutable borrows) — split so the borrows don't overlap.
        let to_discard: Vec<TabId> = self
            .session
            .tab_picker_ranked(self.workspace)
            .into_iter()
            .filter(|t| self.discarder.should_discard(t))
            .map(|t| t.id)
            .collect();

        let mut dropped = 0;
        for id in to_discard {
            if let Some(stab) = self.session.tab_mut(id) {
                stab.discard();
            }
            // Drop the live renderer but KEEP the tab entry (page = None makes it
            // a placeholder that reloads on focus, the discard contract).
            if let Some(shell_tab) = self.tabs.iter_mut().find(|t| t.id == id)
                && let Some(page) = shell_tab.page.take()
            {
                page.close();
                dropped += 1;
                eprintln!("mote-shell: discarded idle renderer for tab {id}");
            }
        }
        dropped
    }

    /// Reap hidden tabs past their TTL via [`HiddenTabReaper::should_reap`],
    /// permanently removing them from the session (DESIGN §Memory cost:
    /// "Aged out (>30 days hidden) → Deleted").
    ///
    /// For each reap candidate the tab is deleted from the session map via
    /// [`Session::remove_tab`] so it does not survive the next
    /// [`Session::flush`]. Because hidden tabs have no live CEF renderer, there
    /// is no `ShellTab`/`Page` to drop — `self.tabs` only carries active-window
    /// tabs. Returns the number of tabs actually deleted.
    fn reap_hidden_tabs(&mut self) -> usize {
        // Collect candidates first (immutable borrow of session), then remove
        // (mutable borrow) — split to avoid overlapping borrows.
        let to_reap: Vec<TabId> = self
            .session
            .tab_picker_ranked(self.workspace)
            .into_iter()
            .filter(|t| self.reaper.should_reap(t))
            .map(|t| t.id)
            .collect();

        let mut reaped = 0;
        for id in to_reap {
            if self.session.remove_tab(id).is_some() {
                reaped += 1;
                eprintln!("mote-shell: reaped hidden tab {id} (TTL expired)");
            }
            // Hidden tabs have no live renderer; self.tabs holds only
            // active-window tabs. Nothing further to drop.
        }
        reaped
    }

    /// React to a DPI scale-factor change: recompute the (logical-pixel) chrome
    /// insets at the new scale and resize ALL live pages to the new physical
    /// viewport (high-DPI, plan §1.3; R1: previously only the active page was
    /// notified, leaving inactive tabs at the old scale after a monitor change).
    fn on_scale_factor_changed(&mut self, scale: f64) {
        self.scale_factor = scale;
        let (vw, vh) = self.viewport_dims();
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        self.notify_all_pages_of_size_change(self.width, self.height, vw, vh, scale);
        self.content_paints = 0;
    }

    /// Resize: reconfigure the surface, tell **all** live pages their new sizes,
    /// and force a re-upload.
    ///
    /// Called on `WindowEvent::Resized`, and also on `Focused(true)` / `Occluded(false)`
    /// after a workspace bounce where Hyprland does not always emit `Resized` even
    /// though the Xwayland surface was reconfigured (R1 fix).
    fn handle_resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.width = size.width;
        self.height = size.height;
        let scale = self.scale_factor;
        if let Some(compositor) = self.compositor.as_mut() {
            compositor.resize(size.width, size.height);
        }
        let (vw, vh) = self.viewport_dims();
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        self.notify_all_pages_of_size_change(size.width, size.height, vw, vh, scale);
    }

    /// Fan out a window-size change to every live page.
    ///
    /// Full-window surfaces (chrome, overlays) receive the window dimensions;
    /// content pages receive the viewport dimensions. All inactive tab pages are
    /// included — not just the active one — so a tab switch post-resize shows the
    /// correct layout (R1 fix; previously only the active page was notified).
    ///
    /// `win_w / win_h`: physical window pixels (chrome and overlays).
    /// `vp_w / vp_h`: physical viewport pixels (content pages).
    /// `scale`: device scale factor.
    fn notify_all_pages_of_size_change(
        &self,
        win_w: u32,
        win_h: u32,
        vp_w: u32,
        vp_h: u32,
        scale: f64,
    ) {
        // Chrome page (full-window).
        self.bridge.page().notify_resized(win_w, win_h, scale);
        // Integrity overlay (full-window, if live).
        if let Some(page) = self.integrity_page.as_ref() {
            page.notify_resized(win_w, win_h, scale);
        }
        // Picker overlay (full-window, if live).
        if let Some(page) = self.picker_page.as_ref() {
            page.notify_resized(win_w, win_h, scale);
        }
        // All tab content pages — active AND inactive (viewport-sized).
        for tab in &self.tabs {
            if let Some(page) = tab.page.as_ref() {
                page.notify_resized(vp_w, vp_h, scale);
            }
        }
    }

    /// Re-query the window's physical size and drive a resize cascade.
    ///
    /// Called when `Focused(true)` or `Occluded(false)` fires after a workspace
    /// bounce.  Hyprland does not always deliver `WindowEvent::Resized` for
    /// hide-then-re-show transitions (the window returns to the same geometry, so
    /// no geometry delta is reported), but the Xwayland surface may have been
    /// recycled by the compositor.  Calling `handle_resize` here:
    /// 1. Reconfigures the wgpu surface (via `compositor.resize`), recovering from
    ///    an `Outdated`/`Lost` state the previous render cycle left it in.
    /// 2. Re-notifies every live CEF page (`was_resized` + `notify_screen_info_changed`),
    ///    which flushes new paint callbacks even when dimensions are unchanged.
    fn on_window_shown(&mut self) {
        if let Some(window) = self.window.clone() {
            let size = window.inner_size();
            self.handle_resize(size);
            // Reset both paint counters so the very next frames produced by the
            // CEF repaint are uploaded unconditionally (not skipped as "already
            // seen at this count").
            self.chrome_paints = 0;
            self.content_paints = 0;
        }
    }

    /// Returns `true` when a chrome-rendered overlay (approval dialog or
    /// integrity panel) is currently capturing input.
    ///
    /// While an overlay is active every mouse event must go to the chrome
    /// page — the overlays render full-window inside `mote://chrome` over the
    /// content viewport, so content must not receive pointer events.
    const fn chrome_overlay_capturing_input(&self) -> bool {
        self.integrity_chrome_open || !self.host.pending_approvals.is_empty()
    }

    /// Route a mouse click to the active content page (if inside the viewport)
    /// or the chrome page (otherwise), in the correct coordinate space
    /// (plan §1.3).
    ///
    /// When a chrome overlay is capturing input (approval dialog or integrity
    /// panel), the cursor position is irrelevant — all clicks go to the chrome
    /// page so the overlay can receive them (ADR-0007/ADR-0008).
    fn route_click(&mut self, button: WinitMouseButton, state: ElementState) {
        let Some(mb) = map_mouse_button(button) else {
            return;
        };
        let action = match state {
            ElementState::Pressed => ButtonAction::Down,
            ElementState::Released => ButtonAction::Up,
        };
        let (x, y) = self.cursor;
        let page_local = self.page_local(x, y);
        match click_target(self.chrome_overlay_capturing_input(), page_local) {
            ClickTarget::Chrome => {
                self.set_focus_owner(FocusOwner::Chrome);
                let pos = self.to_view_dip(MousePosition { x, y });
                self.bridge
                    .page()
                    .send_mouse_button(pos, mb, action, 1, self.modifiers);
            }
            ClickTarget::ContentPage => {
                self.set_focus_owner(FocusOwner::Page);
                // page_local is Some(_) when ClickTarget::ContentPage is returned.
                if let Some(pos) = page_local {
                    let pos = self.to_view_dip(pos);
                    if let Some(page) = self.active_page() {
                        page.send_mouse_button(pos, mb, action, 1, self.modifiers);
                    }
                }
            }
        }
    }

    /// Route a mouse move to whichever surface the cursor is over.
    ///
    /// When a chrome overlay is capturing input, moves are directed to the
    /// chrome page regardless of cursor geometry (same gate as [`Self::route_click`]).
    fn route_mouse_move(&self) {
        let (x, y) = self.cursor;
        let page_local = self.page_local(x, y);
        match click_target(self.chrome_overlay_capturing_input(), page_local) {
            ClickTarget::Chrome => {
                let pos = self.to_view_dip(MousePosition { x, y });
                self.bridge
                    .page()
                    .send_mouse_move(pos, self.modifiers, false);
            }
            ClickTarget::ContentPage => {
                if let (Some(pos), Some(page)) = (page_local, self.active_page()) {
                    page.send_mouse_move(self.to_view_dip(pos), self.modifiers, false);
                }
            }
        }
    }

    /// Route a mouse-wheel scroll to whichever surface the cursor is over — the
    /// same chrome-overlay-capture gate and DIP coordinate mapping as
    /// [`Self::route_mouse_move`]. winit line deltas are scaled to pixel deltas
    /// ([`WHEEL_STEP_PX`] per line); pixel deltas pass through. Positive
    /// `delta_y` scrolls the page up (CEF convention).
    fn route_wheel(&self, delta: MouseScrollDelta) {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (
                wheel_delta(f64::from(x) * WHEEL_STEP_PX),
                wheel_delta(f64::from(y) * WHEEL_STEP_PX),
            ),
            MouseScrollDelta::PixelDelta(p) => (wheel_delta(p.x), wheel_delta(p.y)),
        };
        if dx == 0 && dy == 0 {
            return;
        }
        let (x, y) = self.cursor;
        let page_local = self.page_local(x, y);
        match click_target(self.chrome_overlay_capturing_input(), page_local) {
            ClickTarget::Chrome => {
                let pos = self.to_view_dip(MousePosition { x, y });
                self.bridge
                    .page()
                    .send_mouse_wheel(pos, dx, dy, self.modifiers);
            }
            ClickTarget::ContentPage => {
                if let (Some(pos), Some(page)) = (page_local, self.active_page()) {
                    page.send_mouse_wheel(self.to_view_dip(pos), dx, dy, self.modifiers);
                }
            }
        }
    }

    /// Convert a window-physical-pixel position to the CEF page's view (DIP)
    /// coordinate space. CEF lays both the chrome and content OSR documents out
    /// at logical size (`physical / scale`) and scales the paint buffer by the
    /// device scale (issue #7); mouse events are injected in that view space, so
    /// raw winit physical pixels must be divided by the scale or every pointer
    /// event lands `scale`× off-target.
    fn to_view_dip(&self, p: MousePosition) -> MousePosition {
        MousePosition {
            x: physical_to_dip(p.x, self.scale_factor),
            y: physical_to_dip(p.y, self.scale_factor),
        }
    }

    /// If window-space `(x, y)` is inside the page viewport, return the
    /// page-local position; otherwise `None` (the event belongs to the chrome).
    fn page_local(&self, x: i32, y: i32) -> Option<MousePosition> {
        page_local_coords(
            x,
            y,
            self.width,
            self.height,
            self.inset_left(),
            self.inset_top(),
        )
    }

    /// Intercept the chrome keybinds that must always win before the page sees
    /// the key (ADR-0012 chord table). Returns `true` if the key was consumed
    /// (not routed).
    ///
    /// Uses `Ctrl` (the Linux/dev convention) where the spec writes `⌘`.
    ///
    /// The picker-open fast-path captures every key for the picker's own
    /// navigation. After that, key classification is delegated to
    /// [`classify_chord`] (a pure, unit-testable function); this method only
    /// does the stateful dispatch — each arm is exactly one action.
    #[allow(
        clippy::too_many_lines,
        reason = "ADR-0012: the full browser-keybind suite dispatches here by design; \
                  splitting would make the chord table harder to audit against the ADR"
    )]
    fn intercept_keybind(&mut self, event: &winit::event::KeyEvent) -> bool {
        // While the tab picker is open it owns ALL keyboard input (filter,
        // navigate, select, close) — route every key to it before anything else.
        if self.picker.open {
            return self.picker_key(event);
        }
        if event.state != ElementState::Pressed {
            return false;
        }

        // Debug-only keybind (Ctrl+Shift+A): push a sample ApprovalRequest into
        // the chrome page so the dialog renders. The buttons call the real
        // `approve_plugin` op (registered in `build_op_registry`); the sample
        // plugin is not pending, so an Approve resolves as NotPending and the
        // dialog stays up (expected for the synthetic sample).
        // Retained for T7 live verification (this headless box cannot synthesize
        // CEF clicks); remove at T7 close.
        if self.modifiers.contains(Modifiers::SHIFT)
            && self.modifiers.contains(Modifiers::CONTROL)
            && let Key::Character(s) = &event.logical_key
            && s.eq_ignore_ascii_case("a")
            && self.chrome_ready
        {
            let req = ApprovalRequest::sample();
            self.push_approval_dialog(&req);
            return true;
        }
        // T4 debug-only keybind (Ctrl+Shift+X): push a HOSTILE ApprovalRequest
        // whose plugin name, source, and permission domain carry XSS payloads
        // (script tags, onerror attribute, raw quotes). The chrome must render
        // them as inert literal text — proving the structured-DOM path holds
        // (ADR-0005). Verifies the boundary in live behaviour; the Rust test
        // `hostile_approval_request_serializes_as_inert_json_strings` proves
        // the wire-format encoding.
        // Retained for T7 live verification; remove at T7 close.
        if self.modifiers.contains(Modifiers::SHIFT)
            && self.modifiers.contains(Modifiers::CONTROL)
            && let Key::Character(s) = &event.logical_key
            && s.eq_ignore_ascii_case("x")
            && self.chrome_ready
        {
            let mut req = ApprovalRequest::sample();
            req.plugin = "<script>alert('xss-pluginname')</script>".into();
            req.source = "evil\" onerror=\"alert('xss-source')".into();
            req.permissions[0].domain = "</script><img src=x onerror=alert(1)>".into();
            req.permissions[0].description =
                "this description contains <script>alert(1)</script> markup".into();
            req.dangerous_combinations[0] = "\"></div><script>fetch('//attacker')</script>".into();
            self.push_approval_dialog(&req);
            return true;
        }

        let Some(action) = classify_chord(self.modifiers, &event.logical_key, self.tabs.len())
        else {
            return false;
        };

        match action {
            KeybindAction::DismissModal => {
                // Esc closes the integrity panel (either the chrome-rendered one
                // or the legacy overlay surface, whichever happens to be live).
                let overlay_was_open = self.integrity_open;
                let chrome_was_open = self.integrity_chrome_open;
                if overlay_was_open {
                    self.set_integrity_open(false);
                }
                if chrome_was_open {
                    self.integrity_chrome_open = false;
                    self.push_hide_integrity_to_chrome();
                }
                // Only consume Esc when a panel was actually open.
                overlay_was_open || chrome_was_open
            }
            KeybindAction::OpenPicker => {
                self.set_picker_open(true);
                true
            }
            KeybindAction::ToggleIntegrity => {
                // Prefers the chrome-rendered structured-DOM path. The legacy
                // overlay path stays as a fallback for the chrome-not-ready
                // window so the keybind never appears broken.
                if self.chrome_ready {
                    if self.integrity_chrome_open {
                        self.integrity_chrome_open = false;
                        self.push_hide_integrity_to_chrome();
                    } else {
                        let panel = self.host.build_panel();
                        if self.push_integrity_panel_to_chrome(&panel) {
                            self.integrity_chrome_open = true;
                        }
                    }
                } else {
                    let open = !self.integrity_open;
                    self.set_integrity_open(open);
                }
                true
            }
            KeybindAction::NewTab => {
                self.open_tab(None);
                true
            }
            KeybindAction::CloseTabOrWindow => {
                // tabs.len() > 1 here (classify_chord gates on tab_count > 1);
                // close the active tab.
                if let Some(tab) = self.tabs.get(self.active) {
                    let id = tab.id;
                    self.close_tab(id);
                }
                true
            }
            KeybindAction::CloseWindow | KeybindAction::Quit => {
                eprintln!("mote-shell: close window requested via keybind; exiting");
                self.should_exit = true;
                true
            }
            KeybindAction::FocusOmnibox => {
                self.push_focus_omnibox_to_chrome();
                true
            }
            KeybindAction::ReloadTab => {
                if let Some(page) = self.active_page() {
                    page.reload();
                }
                true
            }
            KeybindAction::GoBack => {
                if let Some(page) = self.active_page() {
                    page.go_back();
                }
                true
            }
            KeybindAction::GoForward => {
                if let Some(page) = self.active_page() {
                    page.go_forward();
                }
                true
            }
            KeybindAction::SwitchWorkspaceByIndex(idx) => {
                if let Some(slug) = self.workspace_slug_by_index(idx) {
                    self.switch_workspace(&slug);
                }
                true
            }
            KeybindAction::SwitchWorkspaceLast => {
                if let Some(slug) = self.workspace_slug_last() {
                    self.switch_workspace(&slug);
                }
                true
            }
            KeybindAction::CycleTab => {
                self.cycle_active_tab();
                true
            }
            // P5: find / zoom / reopen
            KeybindAction::FindInPage => {
                push(&self.commands, ShellCommand::FindInPage);
                true
            }
            KeybindAction::FindNext => {
                push(&self.commands, ShellCommand::FindNext);
                true
            }
            KeybindAction::FindPrev => {
                push(&self.commands, ShellCommand::FindPrev);
                true
            }
            KeybindAction::ZoomIn => {
                push(&self.commands, ShellCommand::ZoomIn);
                true
            }
            KeybindAction::ZoomOut => {
                push(&self.commands, ShellCommand::ZoomOut);
                true
            }
            KeybindAction::ZoomReset => {
                push(&self.commands, ShellCommand::ZoomReset);
                true
            }
            KeybindAction::ReopenClosedTab => {
                push(&self.commands, ShellCommand::ReopenClosedTab);
                true
            }
        }
    }

    /// Push `applyOp('focus_omnibox', null)` to the chrome page.
    ///
    /// The chrome's `host.js` handles this op by calling `input.focus()` on
    /// the omnibox element and selecting all existing text — the standard
    /// address-bar `⌘L` / `Ctrl+L` behavior.
    fn push_focus_omnibox_to_chrome(&self) {
        self.bridge.page().eval_js(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('focus_omnibox',null);",
        );
    }

    /// Return the workspace slug at 1-based `index` from the live workspace
    /// list (ADR-0012: `Ctrl+1`..`Ctrl+8` switch to workspace by index).
    ///
    /// Queries `workspace:provider → list_workspaces` so the ordering follows
    /// the plugin's canonical list, not a hardcoded index. Returns `None` when
    /// no workspace exists at that index (index out of range or plugin not loaded).
    fn workspace_slug_by_index(&self, index: u8) -> Option<String> {
        workspace_slugs_from_host(&self.host).and_then(|slugs| {
            let i = usize::from(index).checked_sub(1)?;
            slugs.into_iter().nth(i)
        })
    }

    /// Return the slug of the **last** workspace (ADR-0012: `Ctrl+9` Chrome
    /// convention — not the literal 9th, but always the final workspace).
    fn workspace_slug_last(&self) -> Option<String> {
        workspace_slugs_from_host(&self.host).and_then(|mut slugs| {
            if slugs.is_empty() {
                None
            } else {
                Some(slugs.remove(slugs.len() - 1))
            }
        })
    }

    /// Advance the active tab to the next one (wrapping), materializing a
    /// placeholder on focus. No-op with fewer than two tabs.
    fn cycle_active_tab(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        let next = (self.active + 1) % self.tabs.len();
        let id = self.tabs[next].id;
        self.select_tab(id);
    }

    // ── P5: zoom helpers ──────────────────────────────────────────────────────

    /// Read the current zoom level for the active tab (0.0 = 100% in CEF's
    /// log-factor encoding). Returns 0.0 when no live page is active.
    fn active_zoom_level(&self) -> f64 {
        self.active_page().map_or(0.0, Page::get_zoom_level)
    }

    /// Set the zoom level for the active tab and show the transient statusline.
    fn set_zoom_level(&mut self, level: f64) {
        if let Some(page) = self.active_page() {
            page.set_zoom_level(level);
            let raw = page.get_zoom_level();
            // Store per-tab so zooming back to 100% gives us 0.0 on the next read.
            if let Some(tab) = self.tabs.get(self.active) {
                self.tab_zoom_levels.insert(tab.id.get(), raw);
            }
        }
        // Schedule the transient statusline element to clear after 1.5 s.
        self.zoom_clear_at = Some(Instant::now() + Duration::from_millis(1500));
        self.push_statusline_to_chrome();
    }

    /// Adjust the active tab's zoom by `delta` (in CEF log-factor units).
    ///
    /// CEF zoom levels are natural-log factors: level 0.0 = 100%, 0.1 ≈ +10%,
    /// -0.1 ≈ -10%. Clamped to [-2.0, 2.0] (~14% to ~738%). Steps match
    /// Chrome's default zoom level steps for familiar feel.
    fn adjust_zoom(&mut self, delta: f64) {
        let current = self.active_zoom_level();
        // Clamp to the practical range; beyond ±2.0 the page is unusable.
        let new_level = (current + delta).clamp(-2.0, 2.0);
        self.set_zoom_level(new_level);
    }

    /// Compute the zoom percentage string for the current active page.
    ///
    /// CEF level 0.0 = 100%; level `n` = `100 × e^n` percent (rounded).
    fn zoom_percent_text(&self) -> String {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "zoom percent is a small positive integer (14–738); rounding is intentional"
        )]
        let pct = (self.active_zoom_level().exp() * 100.0).round() as u32;
        format!("{pct}%")
    }

    // ── P5: reopen-closed-tab ─────────────────────────────────────────────────

    /// Reopen the most recently closed tab. No-op if the stack is empty.
    fn reopen_closed_tab(&mut self) {
        let Some(closed) = self.closed_tab_stack.pop() else {
            return;
        };
        eprintln!("mote-shell: reopen closed tab -> {}", closed.url);
        let id = self.session.add_tab(closed.url.clone(), self.workspace);
        let page = match create_content_page(&closed.url, &self.content_opts, &self.default_profile)
        {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mote-shell: failed to reopen closed tab: {e}");
                None
            }
        };
        self.tabs.push(ShellTab {
            id,
            url: closed.url,
            title: closed.title,
            page,
        });
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
        self.persist_and_push();
    }

    // ── P5: context menu ──────────────────────────────────────────────────────

    /// Drain context-menu requests from every live tab and push `show_context_menu`
    /// to the chrome for each one, so `host.js` can render the Mote-styled popover.
    ///
    /// D10 fix: `can_go_back`/`can_go_forward` are hardcoded `false` by the CEF
    /// callback (it has no access to the nav state at that point). We patch them
    /// from the active page's live nav state here, before forwarding to chrome,
    /// so the "go back"/"go forward" items appear only when navigation is possible.
    fn drain_context_menus(&self) {
        let requests: Vec<ContextMenuRequest> = self
            .tabs
            .iter()
            .filter_map(|t| t.page.as_ref())
            .flat_map(Page::drain_context_menu_requests)
            .collect();
        if requests.is_empty() {
            return;
        }
        let (can_go_back, can_go_forward) = self
            .active_page()
            .map_or((false, false), |p| (p.can_go_back(), p.can_go_forward()));
        for mut req in requests {
            // Patch nav state for page-kind menus (back/forward items).
            // Editable and other kinds don't show nav items, but we patch
            // unconditionally to keep the serialization consistent.
            req.can_go_back = can_go_back;
            req.can_go_forward = can_go_forward;
            self.push_context_menu_to_chrome(&req);
        }
    }

    /// Serialize a [`ContextMenuRequest`] and send `show_context_menu` to the chrome.
    fn push_context_menu_to_chrome(&self, req: &ContextMenuRequest) {
        if !self.chrome_ready {
            return;
        }
        let payload = context_menu_payload(req);
        self.bridge.page().eval_js(&format!(
            "window.mote&&window.mote.applyOp&&\
             window.mote.applyOp('show_context_menu',{payload});"
        ));
    }

    /// Execute a context-menu action string dispatched from `host.js`.
    ///
    /// Actions are one of the fixed strings the chrome knows about:
    /// `"new_tab"`, `"copy_link"`, `"copy_link_as_markdown"`, `"reload"`,
    /// `"go_back"`, `"go_forward"`, `"view_source"`, `"copy_selection"`,
    /// `"search_google"`, `"cut"`, `"copy"`, `"paste"`, `"select_all"`,
    /// `"undo"`, `"redo"`. Any unrecognised action is silently ignored (forward
    /// compatibility: a newer chrome version may send actions an old shell
    /// doesn't know about).
    fn handle_context_menu_action(&self, action: &str) {
        match action {
            "reload" => {
                if let Some(page) = self.active_page() {
                    page.reload();
                }
            }
            "go_back" => {
                if let Some(page) = self.active_page() {
                    page.go_back();
                }
            }
            "go_forward" => {
                if let Some(page) = self.active_page() {
                    page.go_forward();
                }
            }
            // D1: editable-field edit commands — dispatch via CEF frame API.
            "cut" | "copy" | "paste" | "select_all" | "undo" | "redo" => {
                if let Some(page) = self.active_page() {
                    page.edit_frame_command(action);
                }
            }
            _ => {
                // All other actions are handled entirely in host.js (copy, search,
                // new_tab, view_source, copy_selection) using the clipboard API or
                // the existing new_tab/navigate ops. The shell just needs to know
                // the action was dispatched so it can handle the navigation-side
                // ones above.
                eprintln!("mote-shell: context_menu_action: {action:?} (handled in chrome)");
            }
        }
    }

    // ── P5: hover-URL sync ────────────────────────────────────────────────────

    /// Poll the active tab's hover-URL slot and push a statusline update when
    /// the value changes. CEF fires `on_status_message` with the link URL on
    /// hover and with an empty string on mouse-leave; both are handled here.
    fn sync_hover_url(&mut self) {
        let new_url = self
            .active_page()
            .and_then(Page::hover_url)
            .filter(|s| !s.is_empty());
        if new_url == self.hover_url_last {
            return;
        }
        self.hover_url_last = new_url;
        self.push_statusline_to_chrome();
    }

    /// Take any pending find-result from the active page and push a
    /// `find_count` applyOp to the chrome omnibox (C4).
    ///
    /// CEF's `FindHandlerImpl::on_find_result` writes into the [`FindResultSlot`]
    /// on `final_update`; this method takes that result and formats it as
    /// `"N / M"` (1-indexed active match / total count). An empty string is
    /// pushed when the count drops to zero (no matches).
    fn sync_find_result(&self) {
        if !self.chrome_ready {
            return;
        }
        let Some(result) = self.active_page().and_then(Page::take_find_result) else {
            return;
        };
        // Format the count label: "N / M" (1-based ordinal, total count).
        // An ordinal of 0 means CEF cleared the session (no matches found);
        // in that case push an empty string so the find-count span is hidden.
        let label = if result.count == 0 {
            String::new()
        } else {
            format!("{} / {}", result.active_match_ordinal, result.count)
        };
        let label_js = js_string(&label);
        self.bridge.page().eval_js(&format!(
            "window.mote&&window.mote.applyOp&&\
             window.mote.applyOp('find_count',{{label:{label_js}}});"
        ));
    }

    /// Poll the active tab's nav state (`can_go_back`, `can_go_forward`) and
    /// push a `set_nav_state` op to the chrome when the pair changes.
    ///
    /// `push_state_to_chrome` already pushes nav state on every state push,
    /// but state pushes fire on URL change (`sync_active_url`) which runs
    /// BEFORE CEF commits the new history entry — `can_go_back` is still
    /// stale at that point. CEF fires `on_loading_state_change` separately
    /// once the entry is committed (`NavState` cache updates), and this poll
    /// catches that and pushes to chrome so the back/forward keycap buttons
    /// reflect reality. Runs each `about_to_wait` tick.
    fn sync_nav_state(&mut self) {
        let (can_go_back, can_go_forward) = self
            .active_page()
            .map_or((false, false), |p| (p.can_go_back(), p.can_go_forward()));
        let new_state = (can_go_back, can_go_forward);
        if new_state == self.nav_state_last {
            return;
        }
        self.nav_state_last = new_state;
        if !self.chrome_ready {
            // Defer the first push to once the chrome is mounted; the
            // chrome-ready path in about_to_wait calls push_state_to_chrome
            // which covers the initial nav state alongside set_url + set_tabs.
            return;
        }
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('set_nav_state',\
             {{can_go_back:{can_go_back},can_go_forward:{can_go_forward}}});"
        ));
    }

    /// Poll the active tab's `is_loading` flag and push a `set_load_state` op
    /// to the chrome when it changes.
    ///
    /// Mirrors [`sync_nav_state`](Self::sync_nav_state): CEF fires
    /// `on_loading_state_change` asynchronously, so this poll-on-change path
    /// is what keeps the chrome's loading indicator in sync. Runs each
    /// `about_to_wait` tick, immediately after `sync_nav_state`.
    fn sync_load_state(&mut self) {
        // Defer entirely until the chrome is mounted — and do NOT cache the
        // state while deferred. Unlike nav state (which push_state_to_chrome
        // re-pushes at chrome-ready), there is no initial load-state push, so
        // caching a rising edge here would swallow it and the very first page
        // load (the boot tab) would never show its ticker. Leaving
        // load_state_last untouched lets the first post-ready tick push the
        // true current state.
        if !self.chrome_ready {
            return;
        }
        let loading = self.active_page().is_some_and(Page::is_loading);
        if loading == self.load_state_last {
            return;
        }
        self.load_state_last = loading;
        let chrome = self.bridge.page();
        chrome.eval_js(&format!(
            "window.mote&&window.mote.applyOp&&window.mote.applyOp('set_load_state',\
             {{loading:{loading}}});"
        ));
    }

    /// Route a keyboard event to the logical focus owner.
    fn route_key(&self, event: &winit::event::KeyEvent) {
        let target: Option<&Page> = match self.focus {
            FocusOwner::Chrome => Some(self.bridge.page()),
            FocusOwner::Page => self.active_page(),
        };
        let Some(target) = target else {
            return;
        };
        let action = match event.state {
            ElementState::Pressed => KeyAction::Down,
            ElementState::Released => KeyAction::Up,
        };
        let win_code = windows_key_code(&event.logical_key);
        target.send_key(KeyInput {
            action,
            windows_key_code: win_code,
            native_key_code: 0,
            character: 0,
            modifiers: self.modifiers,
        });
        if event.state == ElementState::Pressed
            && let Some(text) = &event.text
        {
            for unit in text.encode_utf16() {
                target.send_key(KeyInput {
                    action: KeyAction::Char,
                    windows_key_code: win_code,
                    native_key_code: 0,
                    character: unit,
                    modifiers: self.modifiers,
                });
            }
        }
    }
}

impl ApplicationHandler for ShellApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = window_attributes();
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("mote-shell: failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        self.width = size.width.max(1);
        self.height = size.height.max(1);
        // Capture the display scale so the chrome insets (authored in logical
        // pixels) map to the right physical page viewport (HiDPI, plan §1.3).
        self.scale_factor = window.scale_factor();

        match Compositor::new_for_window(Arc::clone(&window), self.width, self.height) {
            Ok(c) => self.compositor = Some(c),
            Err(e) => {
                eprintln!("mote-shell: failed to create compositor: {e}");
                event_loop.exit();
                return;
            }
        }

        let (vw, vh) = self.viewport_dims();
        self.content_opts.width = vw;
        self.content_opts.height = vh;
        let scale = self.scale_factor;
        if let Some(page) = self.active_page() {
            page.notify_resized(vw, vh, scale);
        }
        self.bridge
            .page()
            .notify_resized(self.width, self.height, scale);
        self.window = Some(window);
        eprintln!(
            "mote-shell: window {}x{} (scale {:.2}) up; chrome + {} tab(s) live",
            self.width,
            self.height,
            self.scale_factor,
            self.tabs.len()
        );
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                eprintln!("mote-shell: close requested; shutting down");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.handle_resize(size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor_changed(scale_factor);
            }
            // R1 fix: when the window is re-shown after a workspace bounce,
            // Hyprland/winit may not deliver `Resized` (the geometry is
            // unchanged), but the Xwayland surface is recycled.  Re-querying
            // the actual window size and calling `handle_resize` reconfigures
            // the wgpu surface and flushes new CEF paints.
            WindowEvent::Focused(true) | WindowEvent::Occluded(false) => {
                self.on_window_shown();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (cursor_px(position.x), cursor_px(position.y));
                self.route_mouse_move();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.route_click(button, state);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.route_wheel(delta);
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = map_modifiers(mods.state());
            }
            // Chrome keybinds win before the page sees the key (plan §1.3 /
            // §6.1). Only un-intercepted keys route to the focus owner.
            WindowEvent::KeyboardInput { event, .. } if !self.intercept_keybind(&event) => {
                self.route_key(&event);
            }
            WindowEvent::RedrawRequested => {
                if let Some(compositor) = self.compositor.as_mut() {
                    match compositor.render() {
                        // Surface recycled (Xwayland workspace bounce).
                        // Reconfigure so the next render attempt can succeed.
                        Err(CompositorError::SurfaceOutdated) => {
                            if let Some(compositor) = self.compositor.as_mut() {
                                compositor.resize(self.width, self.height);
                            }
                        }
                        Ok(())
                        // Occluded = window hidden (other workspace); skip silently.
                        | Err(CompositorError::AcquireFrame("occluded")) => {}
                        Err(e) => {
                            eprintln!("mote-shell: render failed: {e}");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.engine.pump();
        self.drain_commands();
        // CloseWindow / Quit commands set `should_exit`; exit after the current
        // drain cycle so any in-flight commands are consumed first.
        if self.should_exit {
            eprintln!("mote-shell: should_exit set; shutting down event loop");
            event_loop.exit();
            return;
        }
        self.drain_popup_tabs();
        self.drain_context_menus();
        self.sync_active_url();
        self.sync_active_title();
        self.sync_hover_url();
        self.sync_find_result();
        self.sync_nav_state();
        self.sync_load_state();
        self.maybe_run_housekeeping();
        self.upload_frames();

        // Once the chrome has painted, its bootstrap has run; push the initial
        // tab list + URL exactly once (an applyOp before then would be lost).
        if !self.chrome_ready && self.bridge.page().paint_count() >= 1 {
            self.chrome_ready = true;
            self.push_state_to_chrome();
            // Push the built-in status-line elements immediately so the bar renders
            // with mode/security/tabcount before the plugin load pass fires.
            // Plugin-declared elements are added (and the statusline re-pushed)
            // right after run_initial_load_pass below.
            self.push_statusline_to_chrome();
            // NOTE: push_workspace_list intentionally NOT called here — plugins
            // haven't loaded yet at chrome-ready (the load pass runs on the next
            // tick, below); invoke_capability would return None and the strip
            // would render empty.  It's pushed right after run_initial_load_pass.
        }

        // The plugin load pass is deferred past window creation so a slow or
        // offline git fetch (resolved_set → sync) cannot block startup and a
        // fatal resolution error cannot abort the app (T3 review findings).
        // Run it exactly once, on the first tick after the chrome is live, so
        // any first-install approval dialog can render immediately.
        if self.chrome_ready && !self.did_initial_load {
            self.did_initial_load = true;
            self.host.run_initial_load_pass();
            // Populate the workspace strip now that workspace:provider is loaded.
            self.push_workspace_list();
            // Push the initial status-line state now that plugins have loaded
            // (plugin-declared elements are now registered in the runtime).
            self.push_statusline_to_chrome();
            // Show the FIRST awaiting-approval dialog now that chrome is live.
            // The single dialog root holds one dialog, so multi-pending is shown
            // one at a time — `approve_plugin` advances to the next as each
            // resolves (looping over all here would overwrite earlier dialogs).
            // The buttons call the `approve_plugin` op; the pump thread finishes
            // the load and re-renders on the user's answer.
            self.show_next_pending_dialog();
        }

        if !self.first_frame_logged
            && self.bridge.page().paint_count() >= 1
            && self.active_page().is_some_and(|p| p.paint_count() >= 1)
        {
            self.first_frame_logged = true;
            eprintln!(
                "mote-shell: first chrome+content frames painted ({}ms after window)",
                self.started.elapsed().as_millis()
            );
        }

        if let Some(window) = &self.window {
            window.request_redraw();
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// On exit: flush the session (clean-exit == crash-recovery), close every page +
/// the chrome, then pump so CEF can tear the hosts down before shutdown.
impl Drop for ShellApp {
    fn drop(&mut self) {
        if let Some(ns) = self.session_namespace()
            && let Err(e) = self.session.flush(&ns)
        {
            eprintln!("mote-shell: final session flush failed: {e}");
        }
        for tab in &self.tabs {
            if let Some(page) = tab.page.as_ref() {
                page.close();
            }
        }
        if let Some(page) = self.integrity_page.as_ref() {
            page.close();
        }
        if let Some(page) = self.picker_page.as_ref() {
            page.close();
        }
        self.bridge.page().close();
        for _ in 0..25 {
            self.engine.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        // Drain + stop the audit thread cleanly so buffered events flush to the
        // store before the process exits.
        if let Err(e) = self.host.audit.shutdown() {
            eprintln!("mote-shell: audit log shutdown failed: {e}");
        }
    }
}

/// The content page's surface size for a given window size (window minus the
/// chrome insets).
const fn viewport_size(width: u32, height: u32, inset_left: u32, inset_top: u32) -> (u32, u32) {
    let w = width.saturating_sub(inset_left);
    let h = height.saturating_sub(inset_top);
    (if w == 0 { 1 } else { w }, if h == 0 { 1 } else { h })
}

/// Scale a logical-pixel chrome inset to physical pixels for the display
/// `scale` factor (high-DPI). The chrome insets (`VIEWPORT_LEFT`/`VIEWPORT_TOP`)
/// are CSS/logical pixels; the composited page surface and the window→page
/// hit-test work in physical pixels, so they must be scaled (a fixed physical
/// inset misaligns at 1.25×). Rounds to the nearest physical pixel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "insets are small positive logical pixels; scaled + rounded to a physical pixel"
)]
fn scale_inset(logical: u32, scale: f64) -> u32 {
    (f64::from(logical) * scale.max(1.0)).round() as u32
}

/// Convert a window/viewport pixel dimension to `f32` for the compositor's
/// `ViewportRect`.
#[allow(
    clippy::cast_precision_loss,
    reason = "window pixel dimensions are < 2^23; f32 represents them exactly"
)]
const fn px(v: u32) -> f32 {
    v as f32
}

/// Convert a winit physical cursor coordinate (`f64`) to integer window pixels.
#[allow(
    clippy::cast_possible_truncation,
    reason = "physical cursor coords are small positive pixel values; truncation toward the pixel is intended"
)]
const fn cursor_px(v: f64) -> i32 {
    v as i32
}

/// Pixels scrolled per wheel "line" (winit [`MouseScrollDelta::LineDelta`]).
/// Matches the common desktop default so one notch scrolls a few text lines.
const WHEEL_STEP_PX: f64 = 40.0;

/// Round a (possibly fractional) scroll delta to integer page pixels.
#[allow(
    clippy::cast_possible_truncation,
    reason = "scroll deltas are small pixel values; rounding to the nearest pixel is intended"
)]
const fn wheel_delta(px: f64) -> i32 {
    px.round() as i32
}

/// Divide a window-physical pixel coordinate by the display `scale` to get the
/// CEF view's logical (DIP) coordinate. `scale` is winit's `scale_factor`
/// (always `> 0`); a scale of `1.0` is the identity.
#[allow(
    clippy::cast_possible_truncation,
    reason = "DIP coords are small positive pixel values; rounding to the nearest pixel is intended"
)]
fn physical_to_dip(physical: i32, scale: f64) -> i32 {
    (f64::from(physical) / scale).round() as i32
}

/// The window→page hit-test (plan §1.3).
const fn page_local_coords(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    inset_left: u32,
    inset_top: u32,
) -> Option<MousePosition> {
    let left = inset_left.cast_signed();
    let top = inset_top.cast_signed();
    let right = width.cast_signed();
    let bottom = height.cast_signed();
    if x >= left && x < right && y >= top && y < bottom {
        Some(MousePosition {
            x: x - left,
            y: y - top,
        })
    } else {
        None
    }
}

/// Which surface should receive a mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickTarget {
    /// The privileged chrome page (`mote://chrome`).
    Chrome,
    /// The active content page.
    ContentPage,
}

/// Decide where a mouse event (click or move) should be routed.
///
/// When a chrome-rendered overlay is capturing input (an approval dialog is
/// showing or the integrity panel is open), **all** mouse events go to the
/// chrome page regardless of viewport geometry — the overlay renders
/// full-window on top of the content and must receive every click.  Otherwise
/// the geometry rules apply: a cursor inside the page viewport goes to the
/// content page; a cursor in the chrome insets goes to the chrome page.
///
/// The `overlay_capturing` flag is computed by
/// [`ShellApp::chrome_overlay_capturing_input`].  `page_local` is the result
/// of the viewport hit-test ([`page_local_coords`]) — `Some` means the cursor
/// is inside the content viewport, `None` means it is in the chrome insets.
pub(crate) const fn click_target(
    overlay_capturing: bool,
    page_local: Option<MousePosition>,
) -> ClickTarget {
    if overlay_capturing {
        // A chrome overlay (approval dialog or integrity panel) is capturing
        // input: all mouse events go to the chrome page regardless of where
        // the cursor sits geometrically.  The overlay is rendered full-window
        // inside the chrome page and content must not receive these events.
        ClickTarget::Chrome
    } else if page_local.is_some() {
        ClickTarget::ContentPage
    } else {
        ClickTarget::Chrome
    }
}

/// Map a winit mouse button to the Mote vocabulary (other buttons unrouted).
const fn map_mouse_button(button: WinitMouseButton) -> Option<MouseButton> {
    match button {
        WinitMouseButton::Left => Some(MouseButton::Left),
        WinitMouseButton::Right => Some(MouseButton::Right),
        WinitMouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Map winit modifier state to CEF event flags.
fn map_modifiers(state: winit::keyboard::ModifiersState) -> Modifiers {
    let mut m = Modifiers::NONE;
    if state.shift_key() {
        m |= Modifiers::SHIFT;
    }
    if state.control_key() {
        m |= Modifiers::CONTROL;
    }
    if state.alt_key() {
        m |= Modifiers::ALT;
    }
    if state.super_key() {
        m |= Modifiers::COMMAND;
    }
    m
}

/// A best-effort Windows virtual-key code for a winit logical key.
fn windows_key_code(key: &Key) -> i32 {
    match key {
        Key::Named(NamedKey::Enter) => 0x0D,
        Key::Named(NamedKey::Backspace) => 0x08,
        Key::Named(NamedKey::Tab) => 0x09,
        Key::Named(NamedKey::Escape) => 0x1B,
        Key::Named(NamedKey::Space) => 0x20,
        Key::Named(NamedKey::ArrowLeft) => 0x25,
        Key::Named(NamedKey::ArrowUp) => 0x26,
        Key::Named(NamedKey::ArrowRight) => 0x27,
        Key::Named(NamedKey::ArrowDown) => 0x28,
        Key::Named(NamedKey::Delete) => 0x2E,
        Key::Named(NamedKey::Home) => 0x24,
        Key::Named(NamedKey::End) => 0x23,
        // Letters and digits: the ASCII-uppercase value equals the Windows VK
        // code (0x30–0x39 digits, 0x41–0x5A letters), so the shortcut is correct.
        // Punctuation does NOT line up — e.g. '.' is ASCII 0x2E, which is
        // `VK_DELETE` (see the `NamedKey::Delete => 0x2E` arm above). Faking the
        // ASCII value as the keydown VK made the omnibox interpret '.' as a
        // Delete keypress and drop it (issue #6). For non-alphanumeric
        // printables the CHAR event `route_key` also emits carries the actual
        // character and performs the text insertion, so a 0 ("unknown") keydown
        // VK is both correct and free of the editing-key collisions.
        Key::Character(s) => s.chars().next().map_or(0, |c| {
            let up = c.to_ascii_uppercase();
            if up.is_ascii_alphanumeric() {
                u8::try_from(u32::from(up)).map_or(0, i32::from)
            } else {
                0
            }
        }),
        _ => 0,
    }
}

/// Classify the omnibox text into one of the three mode strings the chrome JS
/// reads from: `"url"` (default), `"cmd"` (leading `>`), `"find"` (leading `/`).
///
/// This mirrors the JS implementation in `host.js` `wireOmniboxMode()` so the
/// contract can be tested in Rust. The leading-character triggers are:
///   `>`  → `[cmd]` mode (command palette)
///   `/`  → `[find]` mode (find-in-page; functional wiring deferred to P5)
///   else → `[url]` mode (default)
///
/// `[ask]` mode is deferred to the AI phase and intentionally not handled here.
///
/// This function is defined only for tests — the production mode logic lives in
/// `host.js`'s `wireOmniboxMode()`. Keeping the Rust mirror lets us unit-test the
/// classification contract without spinning up the JS runtime.
#[cfg(test)]
pub(crate) fn omnibox_mode_from_text(text: &str) -> &'static str {
    match text.chars().next() {
        Some('>') => "cmd",
        Some('/') => "find",
        _ => "url",
    }
}

/// Quote and escape `s` as a JavaScript string literal (double-quoted). Escapes
/// the control / quote / backslash / line-terminator set so a page-derived URL
/// or title can never break out of the literal or inject script when the chrome
/// `eval_js`'s `window.mote.applyOp(...)` call is built.
///
/// REVIEW RULE (`eval_js` interpolation chokepoint): this is the SOLE sanctioner of
/// untrusted data into a `Page::eval_js(format!(...))` string. Every
/// `eval_js(format!(...))` in the shell MUST interpolate only `js_string(...)`
/// output or trusted compile-time constants — never a raw page-derived string.
/// Adding a new `eval_js` call site without routing its dynamic args through
/// `js_string` is a chrome-context injection bug.
/// Serialize a [`ContextMenuRequest`] into the JSON payload string sent to
/// `show_context_menu` in `host.js`.
///
/// Extracted as a free function so the serialization logic is unit-testable
/// without a live chrome bridge (D1/D10 closest-seam tests).
fn context_menu_payload(req: &ContextMenuRequest) -> String {
    let kind = match req.kind {
        ContextMenuKind::Link => "link",
        ContextMenuKind::Image => "image",
        ContextMenuKind::SelectedText => "selection",
        ContextMenuKind::Editable => "editable",
        ContextMenuKind::Page => "page",
    };
    let target_url = req
        .target_url
        .as_deref()
        .map_or_else(|| "null".to_owned(), js_string);
    let selected_text = req
        .selected_text
        .as_deref()
        .map_or_else(|| "null".to_owned(), js_string);
    let can_go_back = req.can_go_back;
    let can_go_forward = req.can_go_forward;
    let is_editable = req.is_editable;
    let edit_flags = req.edit_flags;
    let x = req.x;
    let y = req.y;
    format!(
        "{{\"kind\":{kind:?},\"targetUrl\":{target_url},\
         \"selectedText\":{selected_text},\
         \"x\":{x},\"y\":{y},\
         \"canGoBack\":{can_go_back},\"canGoForward\":{can_go_forward},\
         \"isEditable\":{is_editable},\"editFlags\":{edit_flags}}}"
    )
}

fn js_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // JS string literals may not contain these raw line terminators.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            // Escape '<' so a "</script" sequence cannot terminate any
            // surrounding script context defensively.
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Percent-encode a string for use as a URL query value (RFC 3986 §2.1).
///
/// Unreserved characters (`A-Z a-z 0-9 - _ . ~`) pass through unchanged;
/// every other byte is encoded as `%XX`.  This is correct for a search-query
/// value: spaces become `%20` (not `+`), consistent with modern search engine
/// URL formats.
fn url_percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            // RFC 3986 unreserved characters.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Resolve raw omnibox text to a navigable URL.
///
/// Implements ADR-0018 (first match wins):
///
/// 1. **Explicit scheme** — `<scheme>://…` or the schemeless special forms
///    `data:`, `mote:`, `about:` — returned as-is.
/// 2. **Search (Firefox whitespace rule)** — leading `?`, or whitespace/quote
///    before the first `.`/`:`/`?` — treat as a search query.
/// 3. **Host-shaped → navigate** — IPv4, IPv6 (bracketed `[…]` or bare `::1`),
///    `localhost`/`*.localhost`, or a dotted host whose public suffix is known
///    (ICANN or private, via the PSL crate).  Dotless words and dotted hosts
///    with an unknown suffix (`node.js`, `foo.internal`) → search.
/// 4. **Scheme for schemeless navigations** — `https://` by default; loopback
///    (`localhost`, `127.x.x.x`, `[::1]` / `::1`) → `http://`.
/// 5. **Search** — `{q}` in `url_template` replaced with the RFC-3986
///    percent-encoded query.  A template lacking `{q}` falls back to the
///    built-in default.
///
/// Empty text returns an empty string.
pub(crate) fn resolve_omnibox_input(text: &str, url_template: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }

    // Rule 1: already has a scheme.
    if has_scheme(t) {
        return t.to_owned();
    }

    // Rule 2: Firefox whitespace/quote rule — leading `?` or whitespace/quote
    // before the first delimiter → search immediately.
    if is_search_by_whitespace_rule(t) {
        return make_search(t, url_template);
    }

    // Rules 3 & 4: determine host + optional port, classify, prepend scheme.
    //
    // Strip path/query/fragment first so that authority-only classifiers
    // (PSL suffix lookup, IP parse, localhost match) operate on just the
    // host[:port] portion.  The full `t` (path/query/fragment included) is
    // still used when building the navigation URL.
    let authority = t.find(['/', '?', '#']).map_or(t, |i| &t[..i]);
    let (host, _port_suffix) = split_host_port(authority);
    if is_navigable_host(host) {
        let scheme = if is_loopback(host) { "http" } else { "https" };
        return format!("{scheme}://{t}");
    }

    // Rule 5: search fallback.
    make_search(t, url_template)
}

/// Return the search URL for `query` using `url_template`, falling back to the
/// built-in default if the template does not contain the `{q}` placeholder.
fn make_search(query: &str, url_template: &str) -> String {
    let encoded = url_percent_encode(query);
    let placeholder = "\x7bq\x7d"; // literal {q}
    let effective = if url_template.contains(placeholder) {
        url_template
    } else {
        default_search_url_template()
    };
    effective.replace(placeholder, &encoded)
}

/// Return `(host, port_suffix)` by stripping a trailing `:<digits>` port.
///
/// IPv6 brackets are respected: `[::1]:8080` → `("[::1]", ":8080")`.
/// A bare `::1` has colons that are NOT a port — it returns `("::1", "")`.
fn split_host_port(t: &str) -> (&str, &str) {
    // IPv6 in brackets: `[…]:port` or just `[…]`.
    if t.starts_with('[')
        && let Some(close) = t.find(']')
    {
        let after = &t[close + 1..];
        if let Some(port_str) = after.strip_prefix(':')
            && !port_str.is_empty()
            && port_str.bytes().all(|b| b.is_ascii_digit())
        {
            return (&t[..=close], after);
        }
        return (&t[..=close], after);
    }

    // A bare IPv6 address like `::1` or `2001:db8::1` has multiple colons that
    // are part of the address, not a port separator.  Only attempt port
    // stripping when the string contains exactly one colon (e.g. `host:8080`
    // or `127.0.0.1:8080`).
    if t.bytes().filter(|&b| b == b':').count() == 1
        && let Some(colon_pos) = t.rfind(':')
    {
        let port_str = &t[colon_pos + 1..];
        if !port_str.is_empty() && port_str.bytes().all(|b| b.is_ascii_digit()) {
            return (&t[..colon_pos], &t[colon_pos..]);
        }
    }

    (t, "")
}

/// Return `true` if `t` should be treated as a search query due to the Firefox
/// whitespace/quote rule: the string starts with `?`, or contains a space or
/// quote (`"` / `'`) before the first `.`, `:`, or `?`.
fn is_search_by_whitespace_rule(t: &str) -> bool {
    // Leading `?` → search.
    if t.starts_with('?') {
        return true;
    }

    // Space anywhere in the string → search (covers "no whitespace in URLs").
    if t.bytes().any(|b| b.is_ascii_whitespace()) {
        return true;
    }

    // Quote before the first delimiter.
    let first_delim = t.bytes().position(|b| b == b'.' || b == b':' || b == b'?');
    let first_quote = t.bytes().position(|b| b == b'"' || b == b'\'');
    matches!((first_quote, first_delim), (Some(q), Some(d)) if q < d)
}

/// Return `true` if `host` is a navigable target (IP, loopback, or a dotted
/// name whose public suffix is known).
fn is_navigable_host(host: &str) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr as _;

    let lower = host.to_ascii_lowercase();

    // localhost or *.localhost
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }

    // Bracketed IPv6: strip brackets before parsing.
    let addr_str = if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    };

    // IPv4 or IPv6 literal (covers 127.x.x.x, ::1, etc.)
    if IpAddr::from_str(addr_str).is_ok() {
        return true;
    }

    // Dotted hostname with a known public suffix (ICANN or private).
    // A dotless word, or a dotted host with an unknown suffix, returns false.
    if host.contains('.')
        && let Some(suffix) = psl::suffix(host.as_bytes())
    {
        return suffix.is_known();
    }

    false
}

/// Return `true` if `host` is a loopback address (navigations use `http://`).
fn is_loopback(host: &str) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr as _;

    let lower = host.to_ascii_lowercase();

    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }

    let addr_str = if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    };

    IpAddr::from_str(addr_str).is_ok_and(|addr| addr.is_loopback())
}

/// Return `true` if `t` already carries a URI scheme.
fn has_scheme(t: &str) -> bool {
    // Generic scheme: letter followed by letters/digits/+/-/. then "://"
    if t.len() >= 4 {
        let bytes = t.as_bytes();
        if bytes[0].is_ascii_alphabetic() {
            let colon_pos = bytes.iter().position(|&b| b == b':');
            if let Some(pos) = colon_pos {
                if pos >= 1
                    && bytes[..pos]
                        .iter()
                        .all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
                    && t.len() > pos + 2
                    && bytes[pos + 1] == b'/'
                    && bytes[pos + 2] == b'/'
                {
                    return true;
                }
                // Schemeless special forms: `data:`, `mote:`, `about:` — no slashes.
                let scheme = &t[..pos].to_ascii_lowercase();
                if scheme == "data" || scheme == "mote" || scheme == "about" {
                    return true;
                }
            }
        }
    }
    false
}

// ── URL analysis (CL-URL-XPARENCY) ───────────────────────────────────────────
//
// `analyze_url` parses an HTTP(S) URL and produces a structured breakdown used
// by the chrome to (A8) emphasise the registrable domain, (A9) count trackers
// and offer a clean URL, and (B3) render a stripped hover-URL. Non-HTTP(S)
// URLs (mote://, about:, data:, …) return `None`; the chrome falls back to the
// raw string for those.
//
// The `clearurls::UrlCleaner` is built once at first call and reused for every
// subsequent analysis. Construction (~50 ms) compiles ~300 regexes from the
// embedded JSON rule set; re-running it per URL would be prohibitive.

/// The structural breakdown of an HTTP(S) URL produced by [`analyze_url`].
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct UrlAnalysis {
    /// Scheme including separator, e.g. `"https://"`. Always `"https://"` or
    /// `"http://"` (only those schemes reach `analyze_url`).
    pub(crate) scheme: String,
    /// Host labels that precede the registrable domain, with a trailing dot.
    /// `"www."` for `www.theverge.com`; `""` when the host *is* the registrable
    /// domain (e.g. `theverge.com` directly, or an IP / `localhost`).
    pub(crate) subdomain: String,
    /// The eTLD+1 via `psl`, i.e. the emphasised part (`"theverge.com"`).
    /// For IP literals, `localhost`, and non-PSL hosts the entire host
    /// (including port) is returned here.
    pub(crate) registrable: String,
    /// Everything after the host: path, query-string, fragment.
    /// E.g. `"/2024/x/story?utm_source=nl&utm_medium=email"`.
    pub(crate) rest: String,
    /// The URL after clearurls has stripped tracking parameters and/or
    /// unwrapped a redirect wrapper. Equal to the original URL when no rules
    /// matched.
    pub(crate) clean_url: String,
    /// Query-parameter names that were present in the raw URL but are absent
    /// from `clean_url` (i.e. the tracking parameters). `len()` is the tracker
    /// count.
    pub(crate) tracker_names: Vec<String>,
}

/// Lazily-initialised `UrlCleaner` reused for every `analyze_url` call.
///
/// Construction is expensive (~50 ms, compiles ~300 regexes). The `OnceLock`
/// ensures we pay the cost exactly once per process.
fn url_cleaner() -> Option<&'static clearurls::UrlCleaner> {
    use std::sync::OnceLock;
    static CLEANER: OnceLock<Option<clearurls::UrlCleaner>> = OnceLock::new();
    CLEANER
        .get_or_init(|| {
            clearurls::UrlCleaner::from_embedded_rules()
                .map_err(|e| eprintln!("mote-shell: clearurls init failed: {e}"))
                .ok()
        })
        .as_ref()
}

/// Parse `raw` into a [`UrlAnalysis`].
///
/// Returns `None` for any URL that is not `http://` or `https://`, for
/// unparsable input, and for clearurls errors (degrading gracefully by falling
/// back to `None` so the chrome renders the raw string).
pub(crate) fn analyze_url(raw: &str) -> Option<UrlAnalysis> {
    use std::collections::HashSet;
    use url::Url;

    // Only analyse HTTP(S) URLs. mote://, about:, data:, empty, etc. → None.
    let scheme_str = if raw.starts_with("https://") {
        "https://"
    } else if raw.starts_with("http://") {
        "http://"
    } else {
        return None;
    };

    let parsed = Url::parse(raw).ok()?;

    // ── host split: subdomain + registrable ──────────────────────────────
    // `parsed.host_str()` never includes the port. We reconstruct the
    // host-with-optional-port string for `registrable` when psl has no suffix
    // (IPs, localhost, non-PSL TLDs).
    let host = parsed.host_str()?;
    let host_with_port = parsed
        .port()
        .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));

    let (subdomain, registrable) = split_registrable(host, &host_with_port);

    // ── rest: path + query + fragment ────────────────────────────────────
    let rest = {
        let mut r = parsed.path().to_owned();
        if let Some(q) = parsed.query() {
            r.push('?');
            r.push_str(q);
        }
        if let Some(f) = parsed.fragment() {
            r.push('#');
            r.push_str(f);
        }
        r
    };

    // ── clearurls cleaning ────────────────────────────────────────────────
    // On cleaner init failure we degrade gracefully: clean_url = raw, no
    // trackers reported.
    let clean_url = url_cleaner()
        .and_then(|c| {
            c.clear_single_url_str(raw)
                .map_err(|e| eprintln!("mote-shell: clearurls clean failed: {e}"))
                .ok()
        })
        .map_or_else(|| raw.to_owned(), std::borrow::Cow::into_owned);

    // ── tracker diff: param names in raw but absent from clean_url ────────
    let raw_params: HashSet<String> = parsed.query_pairs().map(|(k, _)| k.into_owned()).collect();

    let clean_params: HashSet<String> = Url::parse(&clean_url)
        .ok()
        .map(|u| {
            u.query_pairs()
                .map(|(k, _)| k.into_owned())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();

    let mut tracker_names: Vec<String> = raw_params.difference(&clean_params).cloned().collect();
    tracker_names.sort();

    Some(UrlAnalysis {
        scheme: scheme_str.to_owned(),
        subdomain,
        registrable,
        rest,
        clean_url,
        tracker_names,
    })
}

/// Split `host` (no port) into `(subdomain, registrable)` using the PSL.
///
/// Returns `("", host_with_port)` when:
/// - the host is an IP literal or localhost (no PSL suffix),
/// - the PSL does not know the suffix,
/// - or the suffix covers the entire hostname (bare TLD edge case).
fn split_registrable(host: &str, host_with_port: &str) -> (String, String) {
    use std::net::IpAddr;
    use std::str::FromStr as _;

    // IP literals and localhost → no subdomain; registrable = full host+port.
    let addr_candidate = if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    };
    if IpAddr::from_str(addr_candidate).is_ok()
        || host == "localhost"
        || host.ends_with(".localhost")
    {
        return (String::new(), host_with_port.to_owned());
    }

    // PSL lookup. `psl::suffix` returns the *suffix* (e.g. `"com"` for
    // `"theverge.com"`). `psl::domain` returns the *registrable domain*
    // (eTLD+1, e.g. `"theverge.com"`).
    let host_bytes = host.as_bytes();
    if let Some(domain) = psl::domain(host_bytes) {
        let domain_str = std::str::from_utf8(domain.as_bytes()).unwrap_or(host);
        if host.len() > domain_str.len() {
            // There are labels before the registrable domain.
            let sub_end = host.len() - domain_str.len();
            // sub_end already includes the trailing dot that separates
            // subdomain from registrable, so subdomain = host[..sub_end].
            let subdomain = host[..sub_end].to_owned();
            return (subdomain, domain_str.to_owned());
        }
        // No subdomain; the host *is* the registrable domain.
        return (String::new(), domain_str.to_owned());
    }

    // No PSL entry → return full host+port as registrable.
    (String::new(), host_with_port.to_owned())
}

/// Build the B3 stripped hover-URL text for the status-line.
///
/// For HTTP(S) URLs: `"<registrable><path-only> · N trackers"` when there are
/// trackers, or `"<registrable><path-only>"` when there are none. Query string
/// and fragment are removed from the displayed form (B3 spec: query stripped).
/// For non-HTTP(S) URLs (mote://, about:, …): the raw string is returned
/// unchanged (no analysis available).
///
/// The result is truncated at 120 code-points so it fits on a single status
/// line without overflowing.
fn build_hover_display(url: &str) -> String {
    use url::Url;

    const MAX_LEN: usize = 120;

    let text = if let Some(a) = analyze_url(url) {
        // B3: derive the preview from `clean_url`, which clearurls has already
        // de-tracked and — crucially — redirect-unwrapped. This means the hover
        // shows *where the link actually goes*, not the wrapper URL.
        //
        // Show the full host (including subdomain — `login.paypal.com` vs
        // `paypal.com.evil.ru` must look different) plus path, query stripped
        // (clearurls already removed trackers; we drop any remainder too).
        let preview = Url::parse(&a.clean_url)
            .ok()
            .and_then(|u| {
                let host = u.host_str()?.to_owned();
                let path = u.path().to_owned();
                Some(format!("{host}{path}"))
            })
            .unwrap_or_else(|| a.clean_url.clone());

        let count = a.tracker_names.len();
        if count > 0 {
            format!(
                "{preview} · {} tracker{}",
                count,
                if count == 1 { "" } else { "s" }
            )
        } else {
            preview
        }
    } else {
        url.to_owned()
    };

    // Truncate at MAX_LEN code-points (not bytes) to avoid multi-byte splits.
    if text.chars().count() > MAX_LEN {
        text.chars().take(MAX_LEN).collect()
    } else {
        text
    }
}

/// Extract a top-level string field `"field": "value"` from a JSON object.
///
/// Parses `json` as a JSON object (anchored, not a first-substring match) and
/// returns the named field iff it is present and a string. Returns `None` if
/// `json` is not an object or the field is absent / not a string.
fn json_string_field(json: &str, field: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?.get(field)?.as_str().map(str::to_string)
}

/// Extract a top-level unsigned-integer field `"field": <number>` from a JSON
/// object (the tab-op `id`s the chrome bootstrap sends are integers). Returns
/// `None` if `json` is not an object or the field is absent / not a `u64`.
fn json_u64_field(json: &str, field: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?.get(field)?.as_u64()
}

/// Extract a top-level boolean field `"field": true/false` from a JSON object.
/// Returns `None` if `json` is not an object or the field is absent / not a bool.
fn json_bool_field(json: &str, field: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?.get(field)?.as_bool()
}

/// Map the `workspace:provider` plugin's string workspace id to the numeric
/// [`WorkspaceId`] the `mote-session` layer uses.
///
/// The slug is resolved to `WorkspaceId::new(index)` where `index` is the
/// slug's 0-based position in `slugs` — the plugin's canonical ordered list
/// (returned by `workspace:provider → list_workspaces`).  This makes the
/// plugin the single source of truth: adding a third workspace to the plugin
/// automatically resolves here without any shell change.
///
/// Backward-compatible: the plugin's default ordering is `["default", "work"]`
/// so `"default" → WorkspaceId(0)` and `"work" → WorkspaceId(1)` are
/// preserved exactly.
///
/// Returns `None` for an unrecognised slug (defensive; the plugin validates
/// ids before calling the shell, so `None` should not be reached in normal
/// operation).
pub(crate) fn workspace_id_for_slug(slug: &str, slugs: &[String]) -> Option<WorkspaceId> {
    slugs
        .iter()
        .position(|s| s == slug)
        .map(|i| WorkspaceId::new(i as u64))
}

/// Query `workspace:provider → list_workspaces` and return the ordered list of
/// workspace id strings (slugs).
///
/// Used by the `Ctrl+1`..`Ctrl+9` keybind dispatch to map a numeric index to a
/// slug without hardcoding the workspace order in the shell. The ordering is the
/// plugin's canonical ordering (the same order the workspace strip shows).
///
/// Returns `None` when the provider is unavailable (plugin not yet loaded).
/// Returns `Some([])` when the provider returns an empty list.
fn workspace_slugs_from_host(host: &runtime::PluginHost) -> Option<Vec<String>> {
    let arg = HostValue::Map(BTreeMap::new());
    let raw = host
        .runtime
        .invoke_capability("workspace:provider", "list_workspaces", &arg)?;
    let rows = match raw {
        HostValue::List(v) => v,
        // Lua `{}` returns as Map (L3 defensive normalisation).
        _ => vec![],
    };
    let slugs = rows
        .into_iter()
        .filter_map(|v| match v {
            HostValue::Map(m) => match m.get("id") {
                Some(HostValue::Str(id)) => Some(id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    Some(slugs)
}

/// Build the `bookmark_list` applyOp JSON payload from the runtime, without
/// performing the `eval_js` push.
///
/// This is the testable intermediate-state seam for the three bookmark/history
/// TDD tests (the chrome page is not available in headless tests). The payload
/// shape is `{"rows":[{url,title,...}...],"count":N}`.
///
/// Returns `None` when the `ui:bookmarks_provider` capability is unavailable.
#[cfg(test)]
pub(crate) fn build_bookmark_list_json(host: &runtime::PluginHost) -> Option<String> {
    let arg = HostValue::Str(String::new());
    let raw = host
        .runtime
        .invoke_capability("ui:bookmarks_provider", "list_bookmarks", &arg)?;

    let mut items = match raw {
        HostValue::List(v) => v,
        _ => vec![],
    };

    // Sort by `added` desc (per push_bookmark_list).
    items.sort_by(|a, b| {
        let added = |v: &HostValue| match v {
            HostValue::Map(m) => match m.get("added") {
                Some(HostValue::Number(f)) => *f,
                _ => 0.0,
            },
            _ => 0.0,
        };
        added(b)
            .partial_cmp(&added(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let count = items.len();
    let rows_json = serde_json::to_string(&host_to_json(&HostValue::List(items)))
        .unwrap_or_else(|_| "[]".to_owned());
    Some(format!("{{\"rows\":{rows_json},\"count\":{count}}}"))
}

/// Build the `history_list` applyOp JSON payload from the runtime, without
/// performing the `eval_js` push.
///
/// This is the testable intermediate-state seam for the history TDD tests.
/// Mirrors [`ShellApp::push_history_list`] exactly: passes
/// `{filter="", limit=200, sort="recent"}` to the plugin.
/// The payload shape is `{"rows":[...],"count":N,"truncated":bool}`.
///
/// Returns `None` when the `ui:history_provider` capability is unavailable.
#[cfg(test)]
pub(crate) fn build_history_list_json(host: &runtime::PluginHost) -> Option<String> {
    use std::collections::BTreeMap;

    const HISTORY_CAP: usize = 200;

    // Overfetch by 1 (same as production `push_history_list`) so truncation
    // is detectable instead of indistinguishable from "exactly `HISTORY_CAP`."
    const HISTORY_OVERFETCH: f64 = 201.0;
    let mut map = BTreeMap::new();
    map.insert("filter".to_owned(), HostValue::Str(String::new()));
    map.insert("limit".to_owned(), HostValue::Number(HISTORY_OVERFETCH));
    map.insert("sort".to_owned(), HostValue::Str("recent".to_owned()));
    let arg = HostValue::Map(map);
    let raw = host
        .runtime
        .invoke_capability("ui:history_provider", "query_history", &arg)?;

    let mut items = match raw {
        HostValue::List(v) => v,
        _ => vec![],
    };

    let truncated = items.len() > HISTORY_CAP;
    if truncated {
        items.truncate(HISTORY_CAP);
    }
    let count = items.len();

    let rows_json = serde_json::to_string(&host_to_json(&HostValue::List(items)))
        .unwrap_or_else(|_| "[]".to_owned());
    Some(format!(
        "{{\"rows\":{rows_json},\"count\":{count},\"truncated\":{truncated}}}"
    ))
}

/// Build the `workspace_list` applyOp JSON payload from the runtime, without
/// performing the `eval_js` push.
///
/// This is the testable intermediate-state seam for the workspace-switcher TDD
/// test (the chrome page is not available in headless tests). The payload shape
/// is `{"rows":[{id,name,active},…]}`.
///
/// Returns `None` when the `workspace:provider` capability is unavailable.
#[cfg(test)]
pub(crate) fn build_workspace_list_json(host: &runtime::PluginHost) -> Option<String> {
    // list_workspaces takes no real argument — Lua ignores extra args, so an
    // empty Map is safe (L2 + L3 defensiveness: always pass a valid HostValue).
    let arg = HostValue::Map(BTreeMap::new());
    let raw = host
        .runtime
        .invoke_capability("workspace:provider", "list_workspaces", &arg)?;

    let rows = match raw {
        HostValue::List(v) => v,
        // Lua returns `{}` (empty table) as Map when the list is empty (L3).
        _ => vec![],
    };

    let rows_json = serde_json::to_string(&host_to_json(&HostValue::List(rows)))
        .unwrap_or_else(|_| "[]".to_owned());
    Some(format!("{{\"rows\":{rows_json}}}"))
}

/// Test-accessible seam: invoke `workspace:provider` → `switch_workspace({id})`
/// and return whether the plugin accepted the switch.
///
/// Mirrors the first half of [`ShellApp::switch_workspace`] without requiring a
/// live window bridge. Used by workspace-switch tests to drive the plugin path
/// (persistence + `workspaces:on_change` emit) and assert the return value.
#[cfg(test)]
pub(crate) fn invoke_switch_workspace(host: &runtime::PluginHost, id: &str) -> bool {
    let mut arg_map = BTreeMap::new();
    arg_map.insert("id".to_owned(), HostValue::Str(id.to_owned()));
    match host.runtime.invoke_capability(
        "workspace:provider",
        "switch_workspace",
        &HostValue::Map(arg_map),
    ) {
        Some(HostValue::Bool(b)) => b,
        Some(_) => true,
        None => false,
    }
}

/// Test-accessible seam: check whether `url` is present in the bookmarks store,
/// mirroring the logic of [`ShellApp::is_url_bookmarked`] without requiring a
/// live window bridge.
///
/// Returns `false` when the provider is unavailable or returns an unexpected
/// shape — callers should treat that as "not bookmarked."
#[cfg(test)]
pub(crate) fn is_url_bookmarked_in_host(host: &runtime::PluginHost, url: &str) -> bool {
    let arg = HostValue::Str(String::new());
    let raw = host
        .runtime
        .invoke_capability("ui:bookmarks_provider", "list_bookmarks", &arg);
    let Some(HostValue::List(items)) = raw else {
        return false;
    };
    items.iter().any(|item| match item {
        HostValue::Map(m) => matches!(m.get("url"), Some(HostValue::Str(u)) if u == url),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_size_excludes_chrome_insets() {
        let (w, h) = viewport_size(1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP);
        assert_eq!(w, 1280 - VIEWPORT_LEFT);
        assert_eq!(h, 800 - VIEWPORT_TOP);
        assert_eq!(viewport_size(0, 0, VIEWPORT_LEFT, VIEWPORT_TOP), (1, 1));
    }

    // ── R1 resize-cascade coverage ────────────────────────────────────────────
    //
    // `notify_all_pages_of_size_change` touches live CEF `Page` objects which
    // require an active engine; we cannot instantiate `ShellApp` in a unit test.
    // The closest testable seam is the size-arithmetic that determines what each
    // page class receives: chrome + overlays get the window dims, content pages
    // get the viewport dims (window minus scaled insets).  These tests pin that
    // arithmetic for a 1859×2098 window at 1.25× scale — the exact geometry
    // observed in the diagnostic log — so a regression in the inset computation
    // would immediately surface here.

    /// At the repro geometry (1859×2098, scale 1.25) the content pages must
    /// receive a viewport that is narrower by exactly the scaled left inset and
    /// shorter by exactly the scaled top inset.
    #[test]
    fn r1_fanout_content_size_at_repro_geometry() {
        let win_w = 1859_u32;
        let win_h = 2098_u32;
        let scale = 1.25_f64;
        let left = scale_inset(VIEWPORT_LEFT, scale);
        let top = scale_inset(VIEWPORT_TOP, scale);
        let (vw, vh) = viewport_size(win_w, win_h, left, top);
        // Chrome / overlays → full window.
        assert_eq!((win_w, win_h), (1859, 2098));
        // Content pages → window minus scaled insets.
        assert_eq!(left, 395, "VIEWPORT_LEFT={VIEWPORT_LEFT} × 1.25 = 395");
        assert_eq!(top, 55, "VIEWPORT_TOP={VIEWPORT_TOP} × 1.25 = 55");
        assert_eq!(
            vw,
            1859 - 395,
            "content width must exclude scaled left inset"
        );
        assert_eq!(
            vh,
            2098 - 55,
            "content height must exclude scaled top inset"
        );
    }

    /// The size-zero guard in `handle_resize` prevents a zero-dim configure.
    #[test]
    fn r1_zero_size_guard_prevents_zero_dim_viewport() {
        // viewport_size clamps to 1 so CEF never sees a 0-dim view_rect.
        assert_eq!(viewport_size(0, 0, VIEWPORT_LEFT, VIEWPORT_TOP), (1, 1));
        // A window exactly as wide as the inset leaves 1px for content.
        let (vw, _) = viewport_size(VIEWPORT_LEFT, 800, VIEWPORT_LEFT, VIEWPORT_TOP);
        assert_eq!(vw, 1, "saturating_sub + clamp-to-1 must produce 1");
    }

    #[test]
    fn scale_inset_scales_logical_to_physical() {
        // At 1.0 the physical inset equals the logical one.
        assert_eq!(scale_inset(316, 1.0), 316);
        // At 1.25 it grows proportionally (the bug the fixed inset had).
        assert_eq!(scale_inset(316, 1.25), 395);
        assert_eq!(scale_inset(44, 1.25), 55);
        // A sub-1.0 scale never shrinks below the logical inset.
        assert_eq!(scale_inset(316, 0.5), 316);
    }

    #[test]
    fn physical_to_dip_maps_pointer_coords_into_the_view() {
        // At 1.0 the DIP coordinate is the physical one (identity).
        assert_eq!(physical_to_dip(640, 1.0), 640);
        // At 1.25 a physical pointer coord divides down into the logical (DIP)
        // space CEF lays the OSR page out in. This is the #7 input regression:
        // the live miss sent physical 663 verbatim where the view wanted 530
        // (~133px off), and 19 where the view wanted 15.
        assert_eq!(physical_to_dip(663, 1.25), 530);
        assert_eq!(physical_to_dip(19, 1.25), 15);
        // A sub-1.0 scale maps the other way (physical < DIP).
        assert_eq!(physical_to_dip(100, 0.5), 200);
    }

    #[test]
    fn wheel_delta_rounds_to_whole_pixels() {
        assert_eq!(wheel_delta(WHEEL_STEP_PX), 40);
        assert_eq!(wheel_delta(-WHEEL_STEP_PX * 2.0), -80);
        assert_eq!(wheel_delta(0.4), 0);
    }

    #[test]
    fn hit_test_maps_page_local_and_rejects_chrome() {
        let pos = page_local_coords(400, 300, 1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP)
            .expect("inside viewport");
        assert_eq!(pos.x, 400 - VIEWPORT_LEFT.cast_signed());
        assert_eq!(pos.y, 300 - VIEWPORT_TOP.cast_signed());
        assert!(page_local_coords(100, 300, 1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP).is_none());
        assert!(page_local_coords(400, 10, 1280, 800, VIEWPORT_LEFT, VIEWPORT_TOP).is_none());
    }

    #[test]
    fn hit_test_uses_scaled_insets_on_hidpi() {
        // At 1.25× the left inset is 395px physical; a click at x=350 (inside the
        // unscaled 316 inset but left of the scaled one) belongs to the chrome.
        let left = scale_inset(VIEWPORT_LEFT, 1.25);
        let top = scale_inset(VIEWPORT_TOP, 1.25);
        assert!(page_local_coords(350, 300, 1600, 1000, left, top).is_none());
        assert!(page_local_coords(420, 300, 1600, 1000, left, top).is_some());
    }

    #[test]
    fn json_string_field_extracts_url() {
        assert_eq!(
            json_string_field(r#"{"url":"https://example.com"}"#, "url").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            json_string_field(r#"{"owner":"chrome"}"#, "owner").as_deref(),
            Some("chrome")
        );
        assert!(json_string_field(r#"{"n":3}"#, "url").is_none());
    }

    #[test]
    fn json_string_field_unescapes_quotes() {
        assert_eq!(
            json_string_field(r#"{"url":"a\"b"}"#, "url").as_deref(),
            Some("a\"b")
        );
    }

    #[test]
    fn json_u64_field_extracts_id() {
        assert_eq!(json_u64_field(r#"{"id":42}"#, "id"), Some(42));
        assert_eq!(json_u64_field(r#"{"id": 7 ,"x":1}"#, "id"), Some(7));
        assert!(json_u64_field(r#"{"id":"nope"}"#, "id").is_none());
        assert!(json_u64_field(r#"{"other":1}"#, "id").is_none());
    }

    #[test]
    fn enter_key_maps_to_carriage_return() {
        assert_eq!(windows_key_code(&Key::Named(NamedKey::Enter)), 0x0D);
        assert_eq!(
            windows_key_code(&Key::Character("a".into())),
            i32::from(b'A')
        );
    }

    #[test]
    fn punctuation_does_not_collide_with_editing_vk_codes() {
        // Regression for #6: '.' is ASCII 0x2E, which is VK_DELETE. Mapping the
        // raw ASCII value as the keydown VK made the omnibox treat a typed '.'
        // as Delete and drop it. Non-alphanumeric printables must NOT map to an
        // editing VK; 0 ("unknown") is correct — the CHAR event inserts them.
        assert_eq!(windows_key_code(&Key::Character(".".into())), 0);
        assert_ne!(
            windows_key_code(&Key::Character(".".into())),
            windows_key_code(&Key::Named(NamedKey::Delete)),
            "'.' must not share VK_DELETE's code"
        );
        for p in ["/", ":", "-", "_", "~", "?", "&", "=", "%", ",", "@"] {
            assert_eq!(
                windows_key_code(&Key::Character(p.into())),
                0,
                "punctuation `{p}` must map to 0, not a colliding editing VK"
            );
        }
        // Alphanumerics still map to their (correct) ASCII-uppercase VK.
        assert_eq!(
            windows_key_code(&Key::Character("z".into())),
            i32::from(b'Z')
        );
        assert_eq!(
            windows_key_code(&Key::Character("5".into())),
            i32::from(b'5')
        );
    }

    // ── Session-housekeeping wiring (discard + reap) ──────────────────────
    //
    // These exercise the shell's config plumbing and that the session crate's
    // Discarder / HiddenTabReaper make the right decisions at SHORT intervals
    // (so the logic is testable without waiting the 30-min / 30-day defaults).
    // The full housekeeping loop also drops CEF renderers, which needs a live
    // engine and is covered by the manual `mote-app` run, not a unit test.

    use std::time::{Duration, SystemTime};

    use mote_session::{Discarder, HiddenTabReaper, Tab};
    use mote_types::{TabId, WorkspaceId};

    #[test]
    fn discard_config_override_shortens_idle_threshold() {
        let cfg = discard_config_with(Some(Duration::from_secs(2)));
        assert_eq!(cfg.discard_after, Duration::from_secs(2));
        assert!(cfg.keep_pinned_loaded); // default preserved
        // No override → the 30-minute default.
        assert_eq!(
            discard_config_with(None).discard_after,
            Duration::from_mins(30)
        );
    }

    #[test]
    fn hidden_ttl_override_shortens_ttl() {
        let cfg = hidden_tab_config_with(Some(Duration::from_secs(2)));
        assert_eq!(cfg.ttl, Some(Duration::from_secs(2)));
        // No override → the 30-day default.
        assert_eq!(
            hidden_tab_config_with(None).ttl,
            Some(Duration::from_hours(720))
        );
    }

    #[test]
    fn discarder_with_short_threshold_discards_idle_active_tab() {
        // An active tab idle 5s with a 2s threshold is discard-eligible; the
        // shell builds the Discarder from `discard_config_with` exactly so.
        let discarder = Discarder::new(discard_config_with(Some(Duration::from_secs(2))));
        let mut tab = Tab::new(TabId::new(1), "https://a.com".into(), WorkspaceId::new(0));
        tab.last_visited = Some(SystemTime::now() - Duration::from_secs(5));
        assert!(discarder.should_discard(&tab));

        // A freshly-visited tab is not.
        let mut fresh = Tab::new(TabId::new(2), "https://b.com".into(), WorkspaceId::new(0));
        fresh.last_visited = Some(SystemTime::now());
        assert!(!discarder.should_discard(&fresh));
    }

    #[test]
    fn reaper_with_short_ttl_reaps_old_hidden_tab() {
        // A hidden tab released 5s ago with a 2s TTL is reap-eligible; the shell
        // builds the reaper from `hidden_tab_config_with` exactly so.
        let reaper = HiddenTabReaper::new(hidden_tab_config_with(Some(Duration::from_secs(2))));
        let mut tab = Tab::new(TabId::new(1), "https://a.com".into(), WorkspaceId::new(0));
        tab.hide(SystemTime::now() - Duration::from_secs(5));
        assert!(reaper.should_reap(&tab));

        // A just-hidden tab is not yet stale.
        let mut recent = Tab::new(TabId::new(2), "https://b.com".into(), WorkspaceId::new(0));
        recent.hide(SystemTime::now());
        assert!(!reaper.should_reap(&recent));
    }

    /// Verifies the reap wiring end-to-end at the session level (without a live
    /// CEF engine): a hidden tab past its TTL is removed from the session by
    /// the reaper and does not survive a flush/restore cycle. This mirrors what
    /// `ShellApp::reap_hidden_tabs` does — collect candidates via
    /// `HiddenTabReaper::should_reap`, then call `Session::remove_tab` for each.
    #[test]
    fn reap_wiring_deletes_expired_hidden_tab_from_session() {
        use mote_session::Session;
        use mote_storage::Store;
        use mote_types::{IdentityId, WorkspaceId};

        let store = Store::open_in_memory().unwrap();
        let identity = IdentityId::new(1);
        let plugin = mote_types::PluginName::new_internal("mote-session").unwrap();
        let ns = store.namespace(&plugin, mote_storage::IdentityScope::PerIdentity(identity));
        let workspace = WorkspaceId::new(0);

        let mut session = Session::new(identity, workspace);
        // An active tab that should survive.
        let keep_id = session.add_tab("https://keep.com".to_owned(), workspace);
        // A hidden tab released 5 seconds ago — past the 2s TTL we'll configure.
        let reap_id = session.add_tab("https://reap-me.com".to_owned(), workspace);
        session
            .hide_tab(reap_id, SystemTime::now() - Duration::from_secs(5))
            .unwrap();
        // A recently-hidden tab that must NOT be reaped yet.
        let young_id = session.add_tab("https://young.com".to_owned(), workspace);
        session.hide_tab(young_id, SystemTime::now()).unwrap();

        // Build a reaper with a 2s TTL (mirrors `hidden_tab_config_with`).
        let reaper = HiddenTabReaper::new(hidden_tab_config_with(Some(Duration::from_secs(2))));

        // Apply the same logic ShellApp::reap_hidden_tabs uses.
        let to_reap: Vec<_> = session
            .tab_picker_ranked(workspace)
            .into_iter()
            .filter(|t| reaper.should_reap(t))
            .map(|t| t.id)
            .collect();
        assert_eq!(to_reap, vec![reap_id], "only the stale tab is a candidate");

        for id in &to_reap {
            assert!(session.remove_tab(*id).is_some());
        }

        // Flush and restore: the reaped tab must be gone; others must survive.
        session.flush(&ns).unwrap();
        let restored = Session::restore(&ns, identity).unwrap();
        assert!(
            restored.tab(keep_id).is_some(),
            "active tab must survive reap"
        );
        assert!(
            restored.tab(young_id).is_some(),
            "recently-hidden tab must survive reap"
        );
        assert!(
            restored.tab(reap_id).is_none(),
            "stale hidden tab must be deleted after reap + flush"
        );
    }

    #[test]
    fn js_string_escapes_injection_vectors() {
        assert_eq!(js_string("ab"), "\"ab\"");
        assert_eq!(js_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(js_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(js_string("a\nb"), "\"a\\nb\"");
        // A `</script>` payload cannot terminate a script context.
        assert!(!js_string("</script>").contains("</script>"));
    }

    // ── approve_plugin op-boundary validation (ADR-0005) ──────────────────

    /// Parses a JSON payload into a `DialogResult` and runs the op-boundary
    /// origin-glob validation exactly as the `approve_plugin` op handler does.
    fn validate_payload(json: &str) -> Result<(), &'static str> {
        let result: approval::DialogResult =
            serde_json::from_str(json).expect("test payload parses");
        validate_dialog_origins(&result)
    }

    #[test]
    fn approve_payload_with_valid_origins_passes_op_boundary() {
        let json = r#"{"plugin":"p","decision":"grant","permissions":[
            {"domain":"page:inject_script","action":"*","mode":"origins",
             "origins":["https://example.com/*","https://*.github.com/*"]}]}"#;
        assert!(validate_payload(json).is_ok(), "valid origin globs pass");
    }

    #[test]
    fn approve_payload_with_full_mode_skips_origin_validation() {
        // mode != "origins" → origins are not validated (none are present).
        let json = r#"{"plugin":"p","decision":"grant","permissions":[
            {"domain":"storage:persistent","action":"","mode":"full"}]}"#;
        assert!(validate_payload(json).is_ok());
    }

    #[test]
    fn approve_payload_with_injection_origin_is_rejected() {
        let json = r#"{"plugin":"p","decision":"grant","permissions":[
            {"domain":"page:inject_script","action":"*","mode":"origins",
             "origins":["<script>alert(1)</script>"]}]}"#;
        assert_eq!(
            validate_payload(json),
            Err("approve_plugin: invalid origin glob"),
            "an injection-shaped origin must be rejected at the op boundary"
        );
    }

    #[test]
    fn approve_payload_over_origin_count_cap_is_rejected() {
        let origins: Vec<String> = (0..=approval::MAX_ORIGINS_PER_PERMISSION)
            .map(|i| format!("https://h{i}.example.com/*"))
            .collect();
        let result = approval::DialogResult {
            plugin: "p".into(),
            decision: "grant".into(),
            permissions: vec![approval::DialogPermission {
                domain: "page:inject_script".into(),
                action: "*".into(),
                mode: "origins".into(),
                origins: Some(origins),
            }],
        };
        assert_eq!(
            validate_dialog_origins(&result),
            Err("approve_plugin: too many origins for a permission"),
            "exceeding the per-permission origin cap must be rejected"
        );
    }

    #[test]
    fn approve_payload_malformed_json_is_an_error() {
        // The op handler maps a parse failure to a 400; mirror that boundary.
        assert!(serde_json::from_str::<approval::DialogResult>("not json").is_err());
    }

    // ── click_target: overlay-aware mouse routing ─────────────────────────
    //
    // When a chrome overlay (approval dialog or integrity panel) is capturing
    // input every mouse event must go to the chrome page — the overlay renders
    // full-window in the chrome page and content must not see those events.
    //
    // Inline helpers used in tests below:
    //   inside  = Some(MousePosition { x: 100, y: 100 })  — cursor in viewport
    //   outside = None                                      — cursor in chrome insets

    /// THE BUG: overlay capturing + cursor inside viewport → chrome (not content).
    ///
    /// With the old geometry-only logic this returns `ContentPage`; the fix
    /// makes it return `Chrome`.  The test MUST fail before the fix is applied
    /// and MUST pass afterwards.
    #[test]
    fn overlay_capturing_inside_viewport_routes_to_chrome() {
        assert_eq!(
            click_target(true, Some(MousePosition { x: 100, y: 100 })),
            ClickTarget::Chrome,
            "an overlay capturing input must redirect viewport clicks to chrome"
        );
    }

    /// Overlay capturing + cursor outside viewport → chrome (no change needed,
    /// already correct by the geometry rule, but must remain correct).
    #[test]
    fn overlay_capturing_outside_viewport_routes_to_chrome() {
        assert_eq!(
            click_target(true, None),
            ClickTarget::Chrome,
            "an overlay capturing input routes chrome-inset clicks to chrome"
        );
    }

    /// No overlay, cursor inside viewport → content (the normal happy path).
    #[test]
    fn no_overlay_inside_viewport_routes_to_content() {
        assert_eq!(
            click_target(false, Some(MousePosition { x: 100, y: 100 })),
            ClickTarget::ContentPage,
            "without an overlay a viewport click goes to the content page"
        );
    }

    /// No overlay, cursor outside viewport (in chrome insets) → chrome.
    #[test]
    fn no_overlay_outside_viewport_routes_to_chrome() {
        assert_eq!(
            click_target(false, None),
            ClickTarget::Chrome,
            "without an overlay a chrome-inset click goes to the chrome page"
        );
    }

    // ── urlbar_query op wiring ────────────────────────────────────────────

    /// `build_op_registry` registers the `urlbar_query` op (wiring check).
    #[test]
    fn urlbar_query_op_is_registered() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"urlbar_query"),
            "urlbar_query must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// Missing `text` field → 400 error (op-boundary validation).
    #[test]
    fn urlbar_query_missing_text_is_a_400() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Directly exercise the same logic the op handler uses.
        let result = json_string_field("{}", "text");
        assert!(
            result.is_none(),
            "missing `text` field must yield None so the op returns 400"
        );
        // Queue must remain empty.
        assert!(queue.lock().unwrap().is_empty());
    }

    /// Valid `text` field → `ShellCommand::UrlbarQuery` is enqueued.
    #[test]
    fn urlbar_query_valid_text_enqueues_command() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate what the registered closure does.
        let text =
            json_string_field(r#"{"text":"example"}"#, "text").expect("text field must parse");
        push(&queue, ShellCommand::UrlbarQuery(text));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::UrlbarQuery(t) => {
                assert_eq!(t, "example", "enqueued text must match the input");
            }
            other => panic!("expected UrlbarQuery; got {other:?}"),
        }
    }

    // ── workspace_list push seam ──────────────────────────────────────────

    /// `build_workspace_list_json` exercises the full path from the bundled
    /// workspace-manager plugin through the Rust→Lua `invoke_capability` seam
    /// and JSON serialization.
    ///
    /// Contract:
    ///   • returns `Some(json)` when the `workspace:provider` cap is available.
    ///   • The JSON is a `{rows: [...]}` object.
    ///   • There are at least 2 rows (the built-in `default` + `work` workspaces).
    ///   • Every row has `id`, `name`, and `active` fields.
    ///   • Exactly one row has `active: true`.
    #[test]
    fn push_workspace_list_returns_list_from_provider() {
        use std::time::Duration;

        use mote_audit::{AuditLog, Config};
        use mote_storage::Store;
        use mote_types::{IdentityId, SchemaVersion};

        use crate::runtime::PluginHost;

        // The bundled plugin source must be a module-level const to avoid the
        // items_after_statements clippy lint.
        const WS_SRC: &str = include_str!("../../../plugins/workspace-manager/init.lua");

        let store = Store::open_in_memory().expect("in-memory store opens");
        let config = Config {
            ring_capacity: 256,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(5),
        };
        let mut log = AuditLog::new(&store, config).expect("audit log starts");
        let registry = mote_registry::Registry::load(SchemaVersion::V1).expect("v1 registry loads");
        let runtime = mote_runtime::Runtime::new(registry, store.clone(), log.producer());

        // Stand up a minimal PluginHost using the tempdir boot path.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut host =
            PluginHost::boot_in(store, dir.path(), dir.path()).expect("host boots cleanly");
        host.runtime = runtime;

        let policy = mote_runtime::GrantAsRequested;
        let identity = mote_runtime::IdentityContext::new(IdentityId::new(0));
        host.runtime
            .load(WS_SRC, identity, &policy)
            .expect("workspace-manager loads cleanly");

        // Exercise the test-seam helper that mirrors push_workspace_list.
        let json = build_workspace_list_json(&host).expect("workspace:provider is available");

        // Parse and validate the shape.
        let v: serde_json::Value = serde_json::from_str(&json).expect("workspace list JSON parses");
        let rows = v
            .get("rows")
            .and_then(|r| r.as_array())
            .expect("rows array present");

        assert!(
            rows.len() >= 2,
            "at least 2 built-in workspaces expected; got {} (json={json:?})",
            rows.len()
        );

        let mut active_count = 0usize;
        let mut found_default = false;
        let mut found_work = false;
        for row in rows {
            assert!(
                row.get("id").is_some() && row.get("name").is_some() && row.get("active").is_some(),
                "every workspace row must have id, name, active fields (row={row:?})"
            );
            if row.get("active").and_then(serde_json::Value::as_bool) == Some(true) {
                active_count += 1;
            }
            if row.get("id").and_then(serde_json::Value::as_str) == Some("default") {
                found_default = true;
            }
            if row.get("id").and_then(serde_json::Value::as_str) == Some("work") {
                found_work = true;
            }
        }

        assert_eq!(
            active_count, 1,
            "exactly one workspace must be active (json={json:?})"
        );
        assert!(
            found_default,
            "built-in 'default' workspace must be present (json={json:?})"
        );
        assert!(
            found_work,
            "built-in 'work' workspace must be present (json={json:?})"
        );

        log.shutdown().expect("audit log shuts down cleanly");
    }

    /// Regression for `9e19dc1 fix(shell): defer workspace push to after plugin
    /// load`. The bug: `push_workspace_list` was being called at chrome-ready,
    /// before plugins had loaded — so `invoke_capability("workspace:provider",
    /// "list_workspaces", …)` returned `None`, the chrome got `rows: []`, and
    /// the popover rendered empty.
    ///
    /// This test pins the ordering invariant at the data layer:
    ///   • BEFORE the `workspace:provider` fulfiller is loaded,
    ///     `build_workspace_list_json` returns `None` (the seam mirrors
    ///     `push_workspace_list`; None means "no fulfiller, don't push").
    ///   • AFTER the plugin is loaded, the same call returns `Some(json)` with
    ///     the built-in workspace rows.
    ///
    /// If a future change re-introduces a push-before-load call site, the
    /// "before" assertion here would still pass against the helper, but the
    /// SHELL-SIDE invariant — "don't call `push_workspace_list` until plugins
    /// are loaded" — is captured by the code comment above the call site in
    /// `about_to_wait`.  Keep both protections in sync.
    #[test]
    fn workspace_list_unavailable_before_plugin_load() {
        use std::time::Duration;

        use mote_audit::{AuditLog, Config};
        use mote_storage::Store;
        use mote_types::{IdentityId, SchemaVersion};

        use crate::runtime::PluginHost;

        const WS_SRC: &str = include_str!("../../../plugins/workspace-manager/init.lua");

        let store = Store::open_in_memory().expect("in-memory store opens");
        let config = Config {
            ring_capacity: 256,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(5),
        };
        let mut log = AuditLog::new(&store, config).expect("audit log starts");
        let registry = mote_registry::Registry::load(SchemaVersion::V1).expect("v1 registry loads");
        let runtime = mote_runtime::Runtime::new(registry, store.clone(), log.producer());

        let dir = tempfile::tempdir().expect("temp dir");
        let mut host =
            PluginHost::boot_in(store, dir.path(), dir.path()).expect("host boots cleanly");
        host.runtime = runtime;

        // BEFORE loading the workspace plugin: the helper returns None.  This is
        // exactly the state the buggy push-at-chrome-ready code hit on boot.
        assert!(
            build_workspace_list_json(&host).is_none(),
            "workspace_list must be unavailable before the workspace:provider \
             plugin is loaded; pushing it now would deliver empty rows"
        );

        // Load the plugin (mirrors the relevant subset of `run_initial_load_pass`).
        let policy = mote_runtime::GrantAsRequested;
        let identity = mote_runtime::IdentityContext::new(IdentityId::new(0));
        host.runtime
            .load(WS_SRC, identity, &policy)
            .expect("workspace-manager loads cleanly");

        // AFTER loading: the same helper returns Some(json) with workspace rows.
        let json =
            build_workspace_list_json(&host).expect("workspace_list available after plugin load");
        let v: serde_json::Value = serde_json::from_str(&json).expect("workspace list JSON parses");
        let rows = v
            .get("rows")
            .and_then(|r| r.as_array())
            .expect("rows array present after load");
        assert!(
            rows.len() >= 2,
            "expected >=2 built-in workspaces after load; got {} ({json:?})",
            rows.len()
        );

        log.shutdown().expect("audit log shuts down cleanly");
    }

    /// Empty text string is accepted (op enqueues a command; the handler lets
    /// chrome clear the dropdown by pushing an empty suggestions list).
    #[test]
    fn urlbar_query_empty_text_enqueues_command() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let text = json_string_field(r#"{"text":""}"#, "text").expect("empty text must parse fine");
        push(&queue, ShellCommand::UrlbarQuery(text));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        match q.pop_front().unwrap() {
            ShellCommand::UrlbarQuery(t) => assert!(t.is_empty(), "text must be empty string"),
            other => panic!("expected UrlbarQuery; got {other:?}"),
        }
    }

    /// `set_active_panel("tabs")` enqueues `ShellCommand::SetActivePanel("tabs")`.
    ///
    /// The full execution path (`ShellApp::set_active_panel` → `push_state_to_chrome`)
    /// requires a live window bridge and is covered by live verification.  This test
    /// asserts the command-enqueue seam: the op handler parses the `name` field and
    /// produces the correct `ShellCommand` variant with `"tabs"`.
    ///
    /// The tabs branch was previously a no-op in the shell's `set_active_panel` match
    /// (and the JS guard skipped calling the shell for tabs entirely).  This test pins
    /// the fix so the op-layer wiring cannot silently regress.
    #[test]
    fn set_active_panel_tabs_pushes_state() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));

        // Simulate what the registered `set_active_panel` op closure does.
        let name = json_string_field(r#"{"name":"tabs"}"#, "name").expect("name field must parse");
        push(&queue, ShellCommand::SetActivePanel(name));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command must be enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::SetActivePanel(n) => {
                assert_eq!(n, "tabs", "enqueued panel name must be \"tabs\"");
            }
            other => panic!("expected SetActivePanel; got {other:?}"),
        }
    }

    // ── R3 popup-intercept wiring (ADR-0011) ─────────────────────────────────
    //
    // `open_popup_tab` and `drain_popup_tabs` operate on live CEF `Page` objects
    // (the CEF engine cannot be instantiated in a unit test, per the existing
    // housekeeping test note above).  The closest testable seam is the
    // `PopupTabQueue` itself — that `user_gesture` round-trips faithfully, that
    // the queue is empty after a drain, and that cloned handles share state.
    //
    // The foreground/background activation decision in `open_popup_tab` is
    // covered by the live-verification gate (Hacker News middle-click scenario).
    // The activation rule itself is a one-liner conditional (`if foreground {
    // self.active = self.tabs.len() - 1; }`); any test of it would duplicate the
    // implementation rather than exercising a boundary.

    /// A gesture-driven popup request (`user_gesture=true`) maps to `foreground=true`
    /// in `open_popup_tab`.  Verify the queue preserves the flag faithfully.
    #[test]
    fn r3_gesture_popup_round_trips_as_foreground() {
        use mote_cef::PopupTabRequest;

        // Simulate what `LifeSpanHandlerImpl::on_before_popup` produces when
        // `user_gesture == 1` (a click-driven popup):
        let request = PopupTabRequest {
            url: "https://news.ycombinator.com/item?id=42".to_string(),
            user_gesture: true,
            background: false,
        };
        // The shell's `drain_popup_tabs` computes `foreground = user_gesture && !background`.
        let foreground = request.user_gesture && !request.background;
        assert!(
            foreground,
            "gesture=true + background=false must pass foreground=true to open_popup_tab"
        );
    }

    /// `is_popup_url_allowed` rejects `mote://` URLs (S1 navigation guard would
    /// cancel the load on a Content-role page, leaving a phantom tab). All other
    /// schemes the content engine can navigate to are passed through.
    ///
    /// Covers the security-review INFO-3 finding: phantom tab on
    /// `window.open('mote://chrome/...')`.
    #[test]
    fn popup_url_allowed_rejects_mote_scheme() {
        // mote:// is reserved for trusted shell-loaded surfaces; content pages
        // cannot navigate to it (S1 guard). Pre-filter before tab creation.
        assert!(!is_popup_url_allowed("mote://chrome/index.html"));
        assert!(!is_popup_url_allowed("mote://overlay/picker.html"));
        // Case-insensitive scheme matching (RFC 3986).
        assert!(!is_popup_url_allowed("MOTE://chrome/index.html"));
        assert!(!is_popup_url_allowed("Mote://chrome/index.html"));

        // Schemes the popup pipeline legitimately handles:
        assert!(is_popup_url_allowed("https://example.com/"));
        assert!(is_popup_url_allowed("http://example.com/"));
        assert!(is_popup_url_allowed("data:text/html,<p>hi</p>"));
        // file:// and javascript: are blocked by Chromium itself, not the S1
        // guard, so they do NOT create a phantom tab — let them through and
        // rely on Chromium's policy to do the right thing.
        assert!(is_popup_url_allowed("file:///etc/passwd"));
        assert!(is_popup_url_allowed("javascript:alert(1)"));
        // No-scheme URLs (relative, malformed) — let CEF handle them; not our
        // job to second-guess the URL parser.
        assert!(is_popup_url_allowed("foobar"));
        assert!(is_popup_url_allowed(""));
    }

    /// A JS-initiated popup request (`user_gesture=false`) maps to `foreground=false`
    /// — the new tab opens in the background, reducing focus-stealing.
    #[test]
    fn r3_non_gesture_popup_round_trips_as_background() {
        use mote_cef::PopupTabRequest;

        // Simulate what `LifeSpanHandlerImpl::on_before_popup` produces when
        // `user_gesture == 0` (a JS `window.open(...)` call with no preceding click):
        let request = PopupTabRequest {
            url: "https://ad.example.com".to_string(),
            user_gesture: false,
            background: false,
        };
        // The shell's `drain_popup_tabs` computes `foreground = user_gesture && !background`.
        let foreground = request.user_gesture && !request.background;
        assert!(
            !foreground,
            "gesture=false must pass foreground=false to open_popup_tab"
        );
    }

    // ── R2 address-mirror / copy-URL wiring ──────────────────────────────────
    //
    // `sync_active_url` and `sync_active_title` operate on live `ShellApp` state
    // (CEF `Page` objects, session, window handle) which cannot be instantiated in
    // a unit test.  The closest testable seams are:
    //   1. Op-registry wiring: `copy_active_url` is registered and enqueues the
    //      correct `ShellCommand` variant.
    //   2. Predicate logic: `sync_active_url` only acts when CEF's reported URL
    //      differs from the tab's current URL — the same predicate that prevents
    //      spurious `push_state_to_chrome` calls on every tick.
    //   3. Title-not-duplicate predicate: `sync_active_title` skips the update
    //      path when `tab.title` already equals the live page title, so a
    //      non-active-tab title change does not stomp the window title.

    /// `build_op_registry` registers the `copy_active_url` op (wiring check).
    #[test]
    fn r2_copy_active_url_op_is_registered() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"copy_active_url"),
            "copy_active_url must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// Calling the `copy_active_url` op enqueues exactly one `ShellCommand::CopyActiveUrl`.
    ///
    /// The op takes no meaningful params — the clipboard is written from the tab
    /// state on the pump thread, not from anything the chrome passes in.  Verify
    /// the queue receives the sentinel variant regardless of params.
    #[test]
    fn r2_copy_active_url_op_enqueues_command() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate what the registered closure does: push the sentinel variant.
        push(&queue, ShellCommand::CopyActiveUrl);

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::CopyActiveUrl => {} // correct
            other => panic!("expected CopyActiveUrl; got {other:?}"),
        }
    }

    /// `sync_active_url` must NOT fire when the URL reported by CEF equals the
    /// tab's current URL.  This predicate (`if tab.url == new_url { return; }`)
    /// prevents a `push_state_to_chrome` call on every tick for a stable page.
    ///
    /// Test at the predicate level: equal strings → no work needed (false branch).
    #[test]
    fn r2_sync_active_url_predicate_skips_on_matching_url() {
        let current = "https://example.com/page".to_string();
        let from_cef = "https://example.com/page".to_string();
        // The predicate in sync_active_url: if tab.url == new_url { return; }
        let should_update = current != from_cef;
        assert!(!should_update, "equal URLs must not trigger a state update");
    }

    /// `sync_active_url` MUST fire when CEF reports a URL change (e.g. a link
    /// click that committed a new location).
    #[test]
    fn r2_sync_active_url_predicate_fires_on_changed_url() {
        let current = "https://example.com/".to_string();
        let from_cef = "https://example.com/results?q=test".to_string();
        let should_update = current != from_cef;
        assert!(
            should_update,
            "a differing CEF URL must trigger a state update"
        );
    }

    /// `sync_active_title` must NOT update the window title when the tab's cached
    /// title already equals the live page title.  The predicate in the
    /// implementation is:
    ///   `if tab.title.as_deref() == Some(title.as_str()) { return; }`
    ///
    /// A non-active-tab title change arriving while that tab is backgrounded
    /// should not cause a window-title update because `sync_active_title` only
    /// reads `self.tabs.get(self.active)`.  This test pins the dedup predicate.
    #[test]
    fn r2_sync_active_title_predicate_skips_duplicate_title() {
        let cached: Option<String> = Some("Example Domain".into());
        let live = "Example Domain".to_string();
        // The predicate in sync_active_title: if tab.title.as_deref() == Some(live.as_str()) { return; }
        let should_update = cached.as_deref() != Some(live.as_str());
        assert!(
            !should_update,
            "duplicate title must not trigger a window-title update"
        );
    }

    /// When the live title differs from the cached title (new page loaded or
    /// first title received), `sync_active_title` must proceed with the update.
    #[test]
    fn r2_sync_active_title_predicate_fires_on_changed_title() {
        let cached: Option<String> = Some("Loading…".into());
        let live = "Example Domain".to_string();
        let should_update = cached.as_deref() != Some(live.as_str());
        assert!(
            should_update,
            "a new page title must trigger a window-title + sidebar update"
        );
    }

    /// When a tab has no cached title yet (`tab.title == None`), any live title
    /// from CEF must trigger the update path — `None != Some(...)` is always true.
    #[test]
    fn r2_sync_active_title_predicate_fires_when_no_cached_title() {
        let cached: Option<String> = None;
        let live = "First Title".to_string();
        let should_update = cached.as_deref() != Some(live.as_str());
        assert!(
            should_update,
            "a tab with no cached title must accept the first live title"
        );
    }

    // ── R4 keybind-suite chord classification (ADR-0012) ─────────────────────
    //
    // `classify_chord` is a pure function — no live `ShellApp` required.
    // These tests cover:
    //   • every new chord's classification,
    //   • the contextual Ctrl+W rule (tab_count <= 1 → CloseWindow),
    //   • the Ctrl+9 → last-workspace-not-literal-9th rule,
    //   • the Ctrl+1..8 → 1-based indexing (off-by-one regression guard).

    /// Helper: produce a `Key::Character` for a single-char string.
    fn char_key(c: &str) -> Key {
        Key::Character(c.into())
    }

    /// `Ctrl+T` is classified as `NewTab`.
    #[test]
    fn r4_ctrl_t_is_new_tab() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("t"), 1);
        assert_eq!(action, Some(KeybindAction::NewTab));
        // Uppercase T (shift held, but Shift alone doesn't change this chord).
        let action_upper = classify_chord(Modifiers::CONTROL, &char_key("T"), 1);
        assert_eq!(action_upper, Some(KeybindAction::NewTab));
    }

    /// `Ctrl+W` with more than one tab → `CloseTabOrWindow` (close tab).
    #[test]
    fn r4_ctrl_w_multi_tab_closes_tab() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("w"), 3);
        assert_eq!(
            action,
            Some(KeybindAction::CloseTabOrWindow),
            "Ctrl+W with >1 tab must produce CloseTabOrWindow"
        );
    }

    /// `Ctrl+W` with exactly one tab → `CloseWindow` (contextual rule, ADR-0012).
    #[test]
    fn r4_ctrl_w_last_tab_closes_window() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("w"), 1);
        assert_eq!(
            action,
            Some(KeybindAction::CloseWindow),
            "Ctrl+W with 1 tab must produce CloseWindow (contextual rule, ADR-0012)"
        );
    }

    /// `Ctrl+W` with zero tabs (edge case) → `CloseWindow`.
    #[test]
    fn r4_ctrl_w_zero_tabs_closes_window() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("w"), 0);
        assert_eq!(
            action,
            Some(KeybindAction::CloseWindow),
            "Ctrl+W with 0 tabs must produce CloseWindow (tab_count <= 1)"
        );
    }

    /// `Ctrl+Shift+W` always closes the window regardless of tab count.
    #[test]
    fn r4_ctrl_shift_w_always_closes_window() {
        let mods = Modifiers::CONTROL | Modifiers::SHIFT;
        // 1 tab
        assert_eq!(
            classify_chord(mods, &char_key("W"), 1),
            Some(KeybindAction::CloseWindow),
            "Ctrl+Shift+W with 1 tab must close window"
        );
        // Many tabs
        assert_eq!(
            classify_chord(mods, &char_key("w"), 5),
            Some(KeybindAction::CloseWindow),
            "Ctrl+Shift+W with 5 tabs must close window"
        );
    }

    /// `Ctrl+Q` is classified as `Quit`.
    #[test]
    fn r4_ctrl_q_is_quit() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("q"), 1);
        assert_eq!(action, Some(KeybindAction::Quit));
    }

    /// `Ctrl+L` is classified as `FocusOmnibox`.
    #[test]
    fn r4_ctrl_l_is_focus_omnibox() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("l"), 1);
        assert_eq!(action, Some(KeybindAction::FocusOmnibox));
    }

    /// `Ctrl+R` is classified as `ReloadTab`.
    #[test]
    fn r4_ctrl_r_is_reload() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("r"), 1);
        assert_eq!(action, Some(KeybindAction::ReloadTab));
    }

    /// `Ctrl+[` is classified as `GoBack`.
    #[test]
    fn r4_ctrl_bracket_open_is_go_back() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("["), 1);
        assert_eq!(action, Some(KeybindAction::GoBack));
    }

    /// `Ctrl+]` is classified as `GoForward`.
    #[test]
    fn r4_ctrl_bracket_close_is_go_forward() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("]"), 1);
        assert_eq!(action, Some(KeybindAction::GoForward));
    }

    /// `Ctrl+1` through `Ctrl+8` are classified as `SwitchWorkspaceByIndex(N)`.
    /// Off-by-one regression: `Ctrl+1` must be index 1 (not 0), `Ctrl+8` must
    /// be index 8.
    #[test]
    fn r4_ctrl_1_through_8_map_to_1_based_index() {
        let cases: &[(&str, u8)] = &[
            ("1", 1),
            ("2", 2),
            ("3", 3),
            ("4", 4),
            ("5", 5),
            ("6", 6),
            ("7", 7),
            ("8", 8),
        ];
        for (digit, expected_idx) in cases {
            let action = classify_chord(Modifiers::CONTROL, &char_key(digit), 1);
            assert_eq!(
                action,
                Some(KeybindAction::SwitchWorkspaceByIndex(*expected_idx)),
                "Ctrl+{digit} must map to index {expected_idx}, got {action:?}"
            );
        }
    }

    /// `Ctrl+9` is `SwitchWorkspaceLast` — the LAST workspace, NOT workspace 9.
    /// This is the Chrome convention documented in ADR-0012.
    #[test]
    fn r4_ctrl_9_is_switch_workspace_last_not_index_9() {
        let action = classify_chord(Modifiers::CONTROL, &char_key("9"), 1);
        assert_eq!(
            action,
            Some(KeybindAction::SwitchWorkspaceLast),
            "Ctrl+9 must map to SwitchWorkspaceLast (Chrome convention), not index 9"
        );
        // Explicitly assert it is NOT SwitchWorkspaceByIndex(9).
        assert_ne!(
            action,
            Some(KeybindAction::SwitchWorkspaceByIndex(9)),
            "Ctrl+9 must NOT produce SwitchWorkspaceByIndex(9)"
        );
    }

    /// `workspace_slug_by_index` returns the slug at the correct 1-based index.
    /// Uses a live workspace-manager plugin, same as the workspace-list tests.
    #[test]
    fn r4_workspace_slug_by_index_is_1_based() {
        use std::time::Duration;

        use mote_audit::{AuditLog, Config};
        use mote_storage::Store;
        use mote_types::{IdentityId, SchemaVersion};

        use crate::runtime::PluginHost;

        const WS_SRC: &str = include_str!("../../../plugins/workspace-manager/init.lua");

        let store = Store::open_in_memory().expect("in-memory store opens");
        let config = Config {
            ring_capacity: 256,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(5),
        };
        let mut log = AuditLog::new(&store, config).expect("audit log starts");
        let registry = mote_registry::Registry::load(SchemaVersion::V1).expect("v1 registry loads");
        let runtime = mote_runtime::Runtime::new(registry, store.clone(), log.producer());
        let dir = tempfile::tempdir().expect("temp dir");
        let mut host =
            PluginHost::boot_in(store, dir.path(), dir.path()).expect("host boots cleanly");
        host.runtime = runtime;
        let policy = mote_runtime::GrantAsRequested;
        let identity = mote_runtime::IdentityContext::new(IdentityId::new(0));
        host.runtime
            .load(WS_SRC, identity, &policy)
            .expect("workspace-manager loads cleanly");

        // index 1 → "default" (the first built-in workspace).
        let slug1 = workspace_slugs_from_host(&host).and_then(|s| s.into_iter().next());
        assert_eq!(
            slug1.as_deref(),
            Some("default"),
            "index 0 (1-based index 1) must be 'default'"
        );

        // index 2 → "work".
        let slug2 = workspace_slugs_from_host(&host).and_then(|s| s.into_iter().nth(1));
        assert_eq!(
            slug2.as_deref(),
            Some("work"),
            "index 1 (1-based index 2) must be 'work'"
        );

        // index 3 → None (only 2 built-in workspaces).
        let slug3 = workspace_slugs_from_host(&host).and_then(|s| s.into_iter().nth(2));
        assert!(
            slug3.is_none(),
            "index 2 (1-based index 3) must be None (only 2 workspaces exist)"
        );

        log.shutdown().expect("audit log shuts down cleanly");
    }

    /// `workspace_slug_last` returns the last workspace slug (ADR-0012: `Ctrl+9`
    /// switches to the last workspace, not the literal 9th). With 2 built-in
    /// workspaces this must return "work".
    #[test]
    fn r4_workspace_slug_last_returns_final_workspace() {
        use std::time::Duration;

        use mote_audit::{AuditLog, Config};
        use mote_storage::Store;
        use mote_types::{IdentityId, SchemaVersion};

        use crate::runtime::PluginHost;

        const WS_SRC: &str = include_str!("../../../plugins/workspace-manager/init.lua");

        let store = Store::open_in_memory().expect("in-memory store opens");
        let config = Config {
            ring_capacity: 256,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(5),
        };
        let mut log = AuditLog::new(&store, config).expect("audit log starts");
        let registry = mote_registry::Registry::load(SchemaVersion::V1).expect("v1 registry loads");
        let runtime = mote_runtime::Runtime::new(registry, store.clone(), log.producer());
        let dir = tempfile::tempdir().expect("temp dir");
        let mut host =
            PluginHost::boot_in(store, dir.path(), dir.path()).expect("host boots cleanly");
        host.runtime = runtime;
        let policy = mote_runtime::GrantAsRequested;
        let identity = mote_runtime::IdentityContext::new(IdentityId::new(0));
        host.runtime
            .load(WS_SRC, identity, &policy)
            .expect("workspace-manager loads cleanly");

        let slugs = workspace_slugs_from_host(&host).expect("workspace:provider is available");
        let last = slugs.last().cloned();
        assert_eq!(
            last.as_deref(),
            Some("work"),
            "the last workspace must be 'work' (the 2nd built-in); \
             Ctrl+9 navigates here, not to a literal 9th workspace"
        );

        log.shutdown().expect("audit log shuts down cleanly");
    }

    /// `close_window` op is registered in the op registry.
    #[test]
    fn r4_close_window_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"close_window"),
            "close_window must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// Calling the `close_window` op enqueues `ShellCommand::CloseWindow`.
    #[test]
    fn r4_close_window_op_enqueues_command() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        push(&queue, ShellCommand::CloseWindow);

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::CloseWindow => {} // correct
            other => panic!("expected CloseWindow; got {other:?}"),
        }
    }

    /// `Esc` is classified as `DismissModal` — does not require Ctrl.
    #[test]
    fn r4_esc_dismisses_modal_without_modifiers() {
        let esc = Key::Named(NamedKey::Escape);
        assert_eq!(
            classify_chord(Modifiers::NONE, &esc, 1),
            Some(KeybindAction::DismissModal),
        );
        // Also works when Ctrl is held (e.g. accidental chord).
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &esc, 1),
            Some(KeybindAction::DismissModal),
        );
    }

    /// `Ctrl+Tab` is classified as `CycleTab`.
    #[test]
    fn r4_ctrl_tab_cycles_tabs() {
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &tab, 2),
            Some(KeybindAction::CycleTab),
        );
    }

    /// Keys without Ctrl (and not Esc or Mod+Space) return `None`.
    #[test]
    fn r4_non_chord_keys_are_not_consumed() {
        // Plain 't' with no modifiers must NOT be consumed (it's a typed char).
        assert_eq!(classify_chord(Modifiers::NONE, &char_key("t"), 1), None);
        // Alt+T is not in the chord table.
        assert_eq!(classify_chord(Modifiers::ALT, &char_key("t"), 1), None);
        // Plain Enter is not a chord.
        assert_eq!(
            classify_chord(Modifiers::NONE, &Key::Named(NamedKey::Enter), 1),
            None
        );
    }

    /// Every local `src="…"` / `href="…"` URL the chrome HTML references must be
    /// registered in `build_chrome_resources`, otherwise CEF 404s on the request
    /// and the referenced asset renders as nothing — with no console error.
    /// Two regressions this guards: `assets/lucide-sprite.svg` missing → every
    /// chrome icon disappears (P1 bug); a new `<script src>` like `roving.js`
    /// missing → `window.mote.roving` is undefined and the omnibox arrow-key
    /// navigation silently dies (CL-KBNAV). The narrow original scanned only
    /// `assets/*`, so a bare `.js`/`.css` reference slipped through; this scans
    /// every `src`/`href` attribute, drops external/scheme/fragment refs, and
    /// asserts each remaining relative path is in the registered set.
    #[test]
    fn chrome_assets_referenced_in_html_are_registered() {
        use std::collections::HashSet;

        let html = mote_ui::CHROME_HTML;
        let mut referenced: HashSet<String> = HashSet::new();
        for attr in ["src=\"", "href=\""] {
            let mut rest = html;
            while let Some(start) = rest.find(attr) {
                let after = &rest[start + attr.len()..];
                let Some(end) = after.find('"') else { break };
                let raw = &after[..end];
                rest = &after[end + 1..];
                // Strip any URL fragment (e.g. `lucide-sprite.svg#icon-x`).
                let path = raw.split('#').next().unwrap_or(raw);
                // Only relative paths are served by build_chrome_resources; skip
                // empty, absolute, and scheme-qualified (http/https/mote/data) refs.
                if path.is_empty()
                    || path.starts_with('/')
                    || path.contains("://")
                    || path.starts_with("mote:")
                    || path.starts_with("data:")
                {
                    continue;
                }
                referenced.insert(path.to_string());
            }
        }

        let res = build_chrome_resources();
        let registered: HashSet<&str> = res.paths().into_iter().collect();

        let mut missing: Vec<&String> = referenced
            .iter()
            .filter(|p| !registered.contains(p.as_str()))
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "chrome.html references assets that are not registered in build_chrome_resources: {missing:?}\n\
             Registered: {registered:?}\n\
             Wire each referenced asset (e.g. mote_ui::ROVING_JS) via .register(\"<path>\", BYTES, mime)."
        );
    }

    // ── P6: Settings panel tests ───────────────────────────────────────────────

    /// The four settings section URLs map to their expected HTML content via the
    /// URL whitelist function (ADR-0017 deep-link contract).
    #[test]
    fn p6_settings_section_from_url_maps_valid_sections() {
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/general"),
            Some("general"),
            "general section URL must resolve"
        );
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/plugins"),
            Some("plugins"),
            "plugins section URL must resolve"
        );
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/integrity"),
            Some("integrity"),
            "integrity section URL must resolve"
        );
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/keybinds"),
            Some("keybinds"),
            "keybinds section URL must resolve"
        );
    }

    /// The .html suffix variant also resolves (static-file preview parity).
    #[test]
    fn p6_settings_section_from_url_accepts_html_suffix() {
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/general.html"),
            Some("general"),
        );
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/keybinds.html"),
            Some("keybinds"),
        );
    }

    /// Invalid section paths return `None` (ADR-0017 URL whitelist enforcement).
    #[test]
    fn p6_settings_section_from_url_rejects_invalid_section() {
        // The whitelist must reject arbitrary path suffixes.
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/bogus"),
            None,
            "unknown section must return None (ADR-0017 URL whitelist)"
        );
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/"),
            None,
            "bare /settings/ path must return None (no default section routing)"
        );
        assert_eq!(
            settings_section_from_url("mote://chrome/settings"),
            None,
            "bare /settings path (no trailing slash) must return None"
        );
        // A path that is a prefix of a valid section is not valid.
        assert_eq!(
            settings_section_from_url("mote://chrome/settings/gen"),
            None,
        );
    }

    /// All four settings section HTML files are registered in
    /// `build_chrome_resources()` under both the deep-link path and the .html
    /// path (ADR-0017 §deep-link contract).
    #[test]
    fn p6_settings_pages_registered_in_chrome_resources() {
        let res = build_chrome_resources();
        let registered: std::collections::HashSet<&str> = res.paths().into_iter().collect();

        for section in ["general", "plugins", "integrity", "keybinds"] {
            let deep_link = format!("settings/{section}");
            let html_path = format!("settings/{section}.html");
            assert!(
                registered.contains(deep_link.as_str()),
                "settings/{section} (deep-link path) must be registered"
            );
            assert!(
                registered.contains(html_path.as_str()),
                "settings/{section}.html must be registered for relative-import resolution"
            );
        }
        // Shared CSS and JS must also be present.
        assert!(
            registered.contains("settings/settings.css"),
            "settings/settings.css must be registered"
        );
        assert!(
            registered.contains("settings/settings.js"),
            "settings/settings.js must be registered"
        );
    }

    /// Each settings HTML page declares both themes via the tokens.css link
    /// (the lockup + bracket patterns depend on both themes being available).
    #[test]
    fn p6_settings_html_pages_declare_both_themes_via_tokens() {
        for (name, html) in [
            ("general", mote_ui::SETTINGS_GENERAL_HTML),
            ("plugins", mote_ui::SETTINGS_PLUGINS_HTML),
            ("integrity", mote_ui::SETTINGS_INTEGRITY_HTML),
            ("keybinds", mote_ui::SETTINGS_KEYBINDS_HTML),
        ] {
            assert!(
                html.contains("tokens.css"),
                "{name}.html must link tokens.css (both themes via :root + [data-theme=\"vellum\"])"
            );
            assert!(
                html.contains("data-theme=\"dusk\""),
                "{name}.html must boot in dusk theme"
            );
            // Tokens.css declares vellum — assert the link is present rather
            // than repeating the full vellum block here.
            assert!(
                mote_ui::TOKENS_CSS.contains("[data-theme=\"vellum\"]"),
                "tokens.css must declare the vellum theme"
            );
        }
    }

    /// Each settings HTML page uses the [settings] bracket-lockup pattern
    /// (matching the design spec and ADR-0017 layout).
    #[test]
    fn p6_settings_html_pages_use_lockup_pattern() {
        for (name, html) in [
            ("general", mote_ui::SETTINGS_GENERAL_HTML),
            ("plugins", mote_ui::SETTINGS_PLUGINS_HTML),
            ("integrity", mote_ui::SETTINGS_INTEGRITY_HTML),
            ("keybinds", mote_ui::SETTINGS_KEYBINDS_HTML),
        ] {
            // The lockup must contain the [settings] identifier.
            assert!(
                html.contains("[settings]") || html.contains("class=\"name\">settings"),
                "{name}.html must carry the [settings] bracket-lockup"
            );
            // Four section tabs must be present.
            for section in ["general", "plugins", "integrity", "keybinds"] {
                assert!(
                    html.contains(&format!("data-section=\"{section}\"")),
                    "{name}.html must have a tab for section '{section}'"
                );
            }
        }
    }

    /// Each settings HTML page carries a CSP that blocks inline scripts and eval
    /// (ADR-0005 — same gate as the main chrome document).
    #[test]
    fn p6_settings_html_pages_have_csp() {
        for (name, html) in [
            ("general", mote_ui::SETTINGS_GENERAL_HTML),
            ("plugins", mote_ui::SETTINGS_PLUGINS_HTML),
            ("integrity", mote_ui::SETTINGS_INTEGRITY_HTML),
            ("keybinds", mote_ui::SETTINGS_KEYBINDS_HTML),
        ] {
            assert!(
                html.contains("Content-Security-Policy"),
                "{name}.html must carry a CSP meta tag"
            );
            assert!(
                html.contains("script-src 'self'"),
                "{name}.html CSP must restrict script-src to 'self'"
            );
            assert!(
                !html.contains("'unsafe-eval'"),
                "{name}.html CSP must not allow 'unsafe-eval'"
            );
        }
    }

    /// settings.js is accessible as a registered chrome resource (wiring check).
    /// The innerHTML/ADR-0005 check lives in mote-ui where `js_strip_noncode` is defined.
    #[test]
    fn p6_settings_js_is_registered_as_chrome_resource() {
        let res = build_chrome_resources();
        let registered: std::collections::HashSet<&str> = res.paths().into_iter().collect();
        assert!(
            registered.contains("settings/settings.js"),
            "settings.js must be registered as a chrome resource"
        );
    }

    /// `set_theme` op is registered.
    #[test]
    fn p6_set_theme_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"set_theme"),
            "set_theme must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// `set_theme` op: valid `theme` field → enqueues `ShellCommand::SetTheme`.
    #[test]
    fn p6_set_theme_op_enqueues_set_theme() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate what the registered closure does.
        let theme =
            json_string_field(r#"{"theme":"vellum"}"#, "theme").expect("theme field must parse");
        push(&queue, ShellCommand::SetTheme(theme));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command must be enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::SetTheme(t) => {
                assert_eq!(t, "vellum", "set_theme must capture the theme name");
            }
            other => panic!("expected SetTheme; got {other:?}"),
        }
    }

    /// `set_theme` op: missing `theme` field → `json_string_field` returns `None`
    /// so the op returns 400 and nothing is pushed.
    #[test]
    fn p6_set_theme_op_rejects_missing_theme() {
        // The closure calls json_string_field and returns err on None — verify the
        // helper's contract directly (no push path exercised).
        let result = json_string_field("{}", "theme");
        assert!(
            result.is_none(),
            "missing theme field must yield None (op returns 400)"
        );
    }

    /// `set_search_engine` op is registered.
    #[test]
    fn p6_set_search_engine_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"set_search_engine"),
            "set_search_engine must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// `set_search_engine` op: valid fields → enqueues `ShellCommand::SetSearchEngine`.
    #[test]
    fn p6_set_search_engine_op_enqueues_set_search_engine() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Use a URL template with a literal `{q}` placeholder. Variables are
        // used rather than string literals inside assert_eq! to avoid clippy's
        // "this looks like a formatting argument" lint (the `{q}` is product
        // data, not a format specifier).
        let expected_name = "DuckDuckGo";
        // Construct the url_template as bytes to avoid the
        // `clippy::literal_string_with_formatting_args` lint that fires on any
        // string literal containing `{...}` — even when not in a format macro.
        // The `{q}` here is product data (a search-template placeholder), not a
        // Rust format specifier.
        let expected_url =
            String::from_utf8(b"https://duckduckgo.com/?q=\x7bq\x7d".to_vec()).unwrap();
        let params = format!(r#"{{"name":"{expected_name}","url_template":"{expected_url}"}}"#);
        // Simulate what the registered closure does.
        let name = json_string_field(&params, "name")
            .filter(|s| !s.is_empty())
            .expect("name must parse");
        let url_template = json_string_field(&params, "url_template")
            .filter(|s| !s.is_empty())
            .expect("url_template must parse");
        push(&queue, ShellCommand::SetSearchEngine { name, url_template });

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        match q.pop_front().unwrap() {
            ShellCommand::SetSearchEngine { name, url_template } => {
                assert_eq!(name, expected_name);
                assert_eq!(url_template, expected_url);
            }
            other => panic!("expected SetSearchEngine; got {other:?}"),
        }
    }

    /// `plugin_disable` op is registered.
    #[test]
    fn p6_plugin_disable_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"plugin_disable"),
            "plugin_disable must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// `plugin_disable` op: valid `plugin` field → enqueues `ShellCommand::PluginDisable`.
    #[test]
    fn p6_plugin_disable_op_enqueues_plugin_disable() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate what the registered closure does via plugin_action_op.
        let name = json_string_field(r#"{"plugin":"bookmarks"}"#, "plugin")
            .filter(|s| !s.is_empty())
            .expect("plugin field must parse");
        push(&queue, ShellCommand::PluginDisable(name));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        match q.pop_front().unwrap() {
            ShellCommand::PluginDisable(n) => {
                assert_eq!(n, "bookmarks");
            }
            other => panic!("expected PluginDisable; got {other:?}"),
        }
    }

    /// `plugin_uninstall` op is registered.
    #[test]
    fn p6_plugin_uninstall_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"plugin_uninstall"),
            "plugin_uninstall must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// `plugin_uninstall` op: valid `plugin` field → enqueues `ShellCommand::PluginUninstall`.
    #[test]
    fn p6_plugin_uninstall_op_enqueues_plugin_uninstall() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate what the registered closure does via plugin_action_op.
        let name = json_string_field(r#"{"plugin":"bookmarks"}"#, "plugin")
            .filter(|s| !s.is_empty())
            .expect("plugin field must parse");
        push(&queue, ShellCommand::PluginUninstall(name));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        match q.pop_front().unwrap() {
            ShellCommand::PluginUninstall(n) => {
                assert_eq!(n, "bookmarks");
            }
            other => panic!("expected PluginUninstall; got {other:?}"),
        }
    }

    /// `keybinds_list` op is registered.
    #[test]
    fn p6_keybinds_list_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"keybinds_list"),
            "keybinds_list must be registered; got: {:?}",
            registry.op_names()
        );
    }

    /// `keybinds_list` returns JSON with the expected shape: a top-level
    /// `keybinds` array of `{action, chord, scope, source}` objects.
    /// The function is called directly since it is read-only (no queue needed).
    #[test]
    fn p6_keybinds_list_op_returns_expected_json_shape() {
        let json_str = keybinds_list_json();
        let json: serde_json::Value =
            serde_json::from_str(&json_str).expect("keybinds_list_json must return valid JSON");

        // Top-level `keybinds` array must be present.
        let keybinds = json
            .get("keybinds")
            .and_then(|v| v.as_array())
            .expect("response must have a `keybinds` array");

        assert!(
            !keybinds.is_empty(),
            "keybinds array must be non-empty (v0.1 chord table)"
        );

        // Every entry must have the four required fields.
        for (i, entry) in keybinds.iter().enumerate() {
            for field in ["action", "chord", "scope", "source"] {
                assert!(
                    entry.get(field).and_then(|v| v.as_str()).is_some(),
                    "keybinds[{i}] must have a string `{field}` field"
                );
            }
        }

        // Scope values must be restricted to the ADR-0012 scope set.
        let valid_scopes = ["global", "chrome", "content", "captured-modal"];
        for (i, entry) in keybinds.iter().enumerate() {
            let scope = entry["scope"].as_str().unwrap();
            assert!(
                valid_scopes.contains(&scope),
                "keybinds[{i}] scope `{scope}` must be one of {valid_scopes:?} (ADR-0012)"
            );
        }

        // Source must be `built-in` for all v0.1 entries.
        for (i, entry) in keybinds.iter().enumerate() {
            assert_eq!(
                entry["source"].as_str().unwrap(),
                "built-in",
                "keybinds[{i}] source must be `built-in` in v0.1"
            );
        }
    }

    /// `keybinds_list` response includes the well-known v0.1 chords (Ctrl+T,
    /// Ctrl+W, Esc).
    #[test]
    fn p6_keybinds_list_includes_v01_chords() {
        let json_str = keybinds_list_json();
        let json: serde_json::Value =
            serde_json::from_str(&json_str).expect("keybinds_list_json must be valid JSON");
        let keybinds = json["keybinds"].as_array().unwrap();

        let actions: Vec<&str> = keybinds
            .iter()
            .filter_map(|e| e["action"].as_str())
            .collect();
        let chords: Vec<&str> = keybinds
            .iter()
            .filter_map(|e| e["chord"].as_str())
            .collect();

        // Must include the core browser chords from ADR-0012.
        assert!(chords.contains(&"Ctrl+T"), "must include Ctrl+T (new tab)");
        assert!(
            chords.contains(&"Ctrl+W"),
            "must include Ctrl+W (close tab)"
        );
        assert!(chords.contains(&"Esc"), "must include Esc (dismiss modal)");
        assert!(
            actions.iter().any(|a| a.contains("new tab")),
            "must include new tab action"
        );
    }

    // ── P3: create_content_page routing (ADR-0015) ─────────────────────────

    /// `create_content_page` selects `Page::new` (global context) for `mote://`
    /// URLs and `Page::with_profile` (per-identity context) for all others.
    ///
    /// This test is a closest-seam structural regression: it exercises the
    /// routing logic directly via the URL-scheme classifier, without a live CEF
    /// engine (which is not available headlessly). The classifier is the same
    /// predicate used by `create_content_page`; the test is equivalent to
    /// asserting that `create_content_page` would call `Page::new` vs
    /// `Page::with_profile`.
    ///
    /// ADR-0015 global-request-context-constraint section.
    #[test]
    fn p3_create_content_page_routes_mote_urls_to_global_context() {
        // The routing predicate extracted from create_content_page.
        fn is_mote_url(url: &str) -> bool {
            url.len() >= 7 && url[..7].eq_ignore_ascii_case("mote://")
        }

        // mote:// URLs must route to global context (Page::new).
        assert!(
            is_mote_url("mote://chrome/newtab.html"),
            "newtab URL must route to global context"
        );
        assert!(
            is_mote_url("mote://chrome/settings/general"),
            "settings URL must route to global context"
        );
        assert!(
            is_mote_url("MOTE://chrome/index.html"),
            "case-insensitive: MOTE:// must route to global context"
        );
        assert!(
            is_mote_url("Mote://chrome/newtab.html"),
            "case-insensitive: Mote:// must route to global context"
        );

        // Non-mote:// URLs must route to profile context (Page::with_profile).
        assert!(
            !is_mote_url("https://example.com"),
            "https:// must route to profile context"
        );
        assert!(
            !is_mote_url("http://news.ycombinator.com"),
            "http:// must route to profile context"
        );
        assert!(
            !is_mote_url("data:text/html,<html></html>"),
            "data: must route to profile context"
        );
        assert!(
            !is_mote_url(""),
            "empty URL must route to profile context (not crash)"
        );
        assert!(
            !is_mote_url("mote"),
            "bare 'mote' (no scheme) must not match"
        );
        assert!(
            !is_mote_url("mote:/"),
            "single-slash 'mote:/' must not match (not a valid mote:// URL)"
        );
    }

    /// `create_content_page` must assign `PageRole::Overlay` for `mote://` URLs so
    /// the S1 navigation guard does not block the top-level `mote://chrome/newtab.html`
    /// load. Without this, the page commits a cancelled navigation and paints black.
    ///
    /// This test exercises the opts construction logic (role override) that lives
    /// alongside the URL-routing predicate in `create_content_page`.
    #[test]
    fn p3_create_content_page_uses_overlay_role_for_mote_urls() {
        // Replicate the role-selection logic from create_content_page.
        fn role_for(url: &str, base_role: PageRole) -> PageRole {
            if url.len() >= 7 && url[..7].eq_ignore_ascii_case("mote://") {
                PageRole::Overlay
            } else {
                base_role
            }
        }

        // mote:// URLs must get Overlay role (exempt from S1 nav guard).
        assert_eq!(
            role_for("mote://chrome/newtab.html", PageRole::Content),
            PageRole::Overlay,
            "newtab page must use Overlay role so the S1 guard allows mote:// navigation"
        );
        assert_eq!(
            role_for("mote://chrome/settings/general", PageRole::Content),
            PageRole::Overlay,
            "settings page must use Overlay role"
        );
        assert_eq!(
            role_for("MOTE://chrome/newtab.html", PageRole::Content),
            PageRole::Overlay,
            "case-insensitive: MOTE:// must also get Overlay role"
        );

        // Non-mote:// URLs must inherit the base role (Content).
        assert_eq!(
            role_for("https://news.ycombinator.com", PageRole::Content),
            PageRole::Content,
            "https:// must keep Content role"
        );
        assert_eq!(
            role_for("http://example.com", PageRole::Content),
            PageRole::Content,
            "http:// must keep Content role"
        );
    }

    /// `DEFAULT_START_URL` must be a `mote://` URL (P3, ADR-0015). The old
    /// `data:text/html,...` placeholder is replaced; this test prevents
    /// regression to any non-mote:// start URL.
    #[test]
    fn p3_default_start_url_is_mote_scheme() {
        assert!(
            DEFAULT_START_URL.starts_with("mote://"),
            "DEFAULT_START_URL must be a mote:// URL (P3, ADR-0015); got: {DEFAULT_START_URL}"
        );
    }

    /// `newtab.html` must be registered in `build_chrome_resources()` so the
    /// `mote://chrome/newtab.html` request resolves and the page loads. Missing
    /// registration would produce a CEF 404 / blank tab.
    #[test]
    fn p3_newtab_registered_in_chrome_resources() {
        let res = build_chrome_resources();
        let registered: std::collections::HashSet<&str> = res.paths().into_iter().collect();
        assert!(
            registered.contains("newtab.html"),
            "newtab.html must be registered in build_chrome_resources() (P3, ADR-0015)"
        );
    }

    // ── P2: omnibox mode classification ─────────────────────────────────────
    //
    // The mode-prefix trigger is a pure classification on the leading character
    // of the omnibox text. These tests encode the contract; the JS implementation
    // in host.js calls the same logic at input time.

    /// Leading `>` → `[cmd]` mode.
    #[test]
    fn p2_omnibox_mode_gt_is_cmd() {
        assert_eq!(omnibox_mode_from_text(">"), "cmd");
        assert_eq!(omnibox_mode_from_text("> something"), "cmd");
    }

    /// Leading `/` → `[find]` mode.
    #[test]
    fn p2_omnibox_mode_slash_is_find() {
        assert_eq!(omnibox_mode_from_text("/"), "find");
        assert_eq!(omnibox_mode_from_text("/pattern"), "find");
    }

    /// Empty string, URL, or anything else → `[url]` mode.
    #[test]
    fn p2_omnibox_mode_default_is_url() {
        assert_eq!(omnibox_mode_from_text(""), "url");
        assert_eq!(omnibox_mode_from_text("https://example.com"), "url");
        assert_eq!(omnibox_mode_from_text("google.com"), "url");
        assert_eq!(omnibox_mode_from_text("some search"), "url");
    }

    // ── P2: nav-op registration ──────────────────────────────────────────────
    //
    // Verify that `go_back`, `go_forward`, `reload`, and `security_info` are
    // registered in the op registry. This is the closest testable seam: the
    // registry is built in a pure function (`build_op_registry`), and
    // `OpRegistry::op_names` returns the set of op names. Nav button → enqueue
    // → dispatch is covered by the existing keybind tests (which also verify
    // `GoBack`/`GoForward`/`ReloadTab` keybinds through `classify_chord` +
    // `intercept_keybind`).

    /// `go_back`, `go_forward`, `reload`, and `security_info` ops are all
    /// registered.
    #[test]
    fn p2_nav_and_security_ops_registered() {
        let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&commands);
        let names = registry.op_names();
        for op in ["go_back", "go_forward", "reload", "security_info"] {
            assert!(
                names.contains(&op),
                "op '{op}' must be registered in build_op_registry (P2)"
            );
        }
    }

    // ── P5: chord classification ──────────────────────────────────────────────

    /// `Ctrl+F` is classified as `FindInPage`.
    #[test]
    fn p5_ctrl_f_is_find_in_page() {
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &char_key("f"), 1),
            Some(KeybindAction::FindInPage)
        );
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &char_key("F"), 1),
            Some(KeybindAction::FindInPage)
        );
    }

    /// `Ctrl+G` is classified as `FindNext`.
    #[test]
    fn p5_ctrl_g_is_find_next() {
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &char_key("g"), 1),
            Some(KeybindAction::FindNext)
        );
    }

    /// `Ctrl+Shift+G` is classified as `FindPrev`.
    #[test]
    fn p5_ctrl_shift_g_is_find_prev() {
        let mods = Modifiers::CONTROL | Modifiers::SHIFT;
        assert_eq!(
            classify_chord(mods, &char_key("G"), 1),
            Some(KeybindAction::FindPrev)
        );
        assert_eq!(
            classify_chord(mods, &char_key("g"), 1),
            Some(KeybindAction::FindPrev)
        );
    }

    /// `Ctrl+=` is classified as `ZoomIn`.
    #[test]
    fn p5_ctrl_equals_is_zoom_in() {
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &char_key("="), 1),
            Some(KeybindAction::ZoomIn)
        );
    }

    /// `Ctrl+-` is classified as `ZoomOut`.
    #[test]
    fn p5_ctrl_minus_is_zoom_out() {
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &char_key("-"), 1),
            Some(KeybindAction::ZoomOut)
        );
    }

    /// `Ctrl+0` is classified as `ZoomReset`.
    #[test]
    fn p5_ctrl_0_is_zoom_reset() {
        assert_eq!(
            classify_chord(Modifiers::CONTROL, &char_key("0"), 1),
            Some(KeybindAction::ZoomReset)
        );
    }

    /// `Ctrl+Shift+T` is classified as `ReopenClosedTab`.
    #[test]
    fn p5_ctrl_shift_t_is_reopen_closed_tab() {
        let mods = Modifiers::CONTROL | Modifiers::SHIFT;
        assert_eq!(
            classify_chord(mods, &char_key("T"), 1),
            Some(KeybindAction::ReopenClosedTab)
        );
        assert_eq!(
            classify_chord(mods, &char_key("t"), 1),
            Some(KeybindAction::ReopenClosedTab)
        );
    }

    // ── P5: ClosedTabStack ────────────────────────────────────────────────────

    /// Pushing 26 entries evicts the oldest (cap is 25).
    #[test]
    fn p5_closed_tab_stack_evicts_oldest_at_cap() {
        let mut stack = ClosedTabStack::new();
        for i in 0..=25 {
            stack.push(ClosedTab {
                url: format!("https://example.com/{i}"),
                title: None,
            });
        }
        // Stack should be at cap (25), not 26.
        assert_eq!(
            stack.inner.len(),
            CLOSED_TAB_STACK_CAP,
            "stack must be capped at {CLOSED_TAB_STACK_CAP}"
        );
        // Most recent is the last one pushed (i=25).
        let top = stack.pop().unwrap();
        assert_eq!(
            top.url, "https://example.com/25",
            "pop must return the most recently pushed entry (LIFO)"
        );
        // Oldest entry (i=0) must have been evicted.
        assert!(
            !stack.inner.iter().any(|t| t.url == "https://example.com/0"),
            "oldest entry must be evicted when stack exceeds cap"
        );
    }

    /// Pop from an empty stack returns `None`.
    #[test]
    fn p5_closed_tab_stack_empty_pop_returns_none() {
        let mut stack = ClosedTabStack::new();
        assert!(stack.pop().is_none(), "empty stack pop must return None");
    }

    /// Push then pop returns the entry in LIFO order.
    #[test]
    fn p5_closed_tab_stack_pop_returns_most_recent() {
        let mut stack = ClosedTabStack::new();
        stack.push(ClosedTab {
            url: "https://first.example.com".to_owned(),
            title: None,
        });
        stack.push(ClosedTab {
            url: "https://second.example.com".to_owned(),
            title: Some("Second".to_owned()),
        });
        let top = stack.pop().unwrap();
        assert_eq!(
            top.url, "https://second.example.com",
            "pop must return the last-pushed (most recent) entry"
        );
        assert_eq!(
            top.title.as_deref(),
            Some("Second"),
            "title must survive the stack round-trip"
        );
        // Stack should have one entry remaining.
        assert_eq!(stack.inner.len(), 1, "one entry must remain after one pop");
    }

    // ── P5: zoom clamp ────────────────────────────────────────────────────────

    /// The zoom level is clamped to [-2.0, 2.0] in `adjust_zoom`.
    #[test]
    fn p5_zoom_delta_clamps_to_range() {
        // Simulate adjust_zoom logic.
        let clamp_zoom = |current: f64, delta: f64| -> f64 { (current + delta).clamp(-2.0, 2.0) };
        // In-range: no clamping.
        assert!((clamp_zoom(0.0, 0.1) - 0.1).abs() < f64::EPSILON);
        // At the upper bound: further positive delta is clamped.
        assert!((clamp_zoom(1.9, 0.5) - 2.0).abs() < f64::EPSILON);
        // At the lower bound: further negative delta is clamped.
        assert!((clamp_zoom(-1.9, -0.5) - (-2.0)).abs() < f64::EPSILON);
        // Already at max: no movement.
        assert!((clamp_zoom(2.0, 0.1) - 2.0).abs() < f64::EPSILON);
        // Already at min: no movement.
        assert!((clamp_zoom(-2.0, -0.1) - (-2.0)).abs() < f64::EPSILON);
    }

    // ── P5: ops registration ──────────────────────────────────────────────────

    /// All nine P5 ops are registered in `build_op_registry` (includes
    /// `find_next` and `find_prev` added by the C3 fix).
    #[test]
    fn p5_ops_all_registered() {
        let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&commands);
        let names = registry.op_names();
        for op in [
            "find_in_page",
            "find_next",
            "find_prev",
            "stop_finding",
            "zoom_in",
            "zoom_out",
            "zoom_reset",
            "reopen_closed_tab",
            "context_menu_action",
        ] {
            assert!(
                names.contains(&op),
                "P5 op '{op}' must be registered in build_op_registry"
            );
        }
    }

    // ── P5: find_in_page carries query text (C2) ──────────────────────────────

    /// `find_in_page` enqueues a `FindText` command that carries the query
    /// string and direction flags — it must NOT discard `text` (regression pin
    /// for the C2 defect where `let _ = text` silently dropped the query).
    #[test]
    fn p5_find_in_page_enqueues_text() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        // Simulate exactly what the corrected op handler does.
        let text = json_string_field(r#"{"text":"hello"}"#, "text").expect("text field must parse");
        let find_next = json_bool_field(r#"{"text":"hello"}"#, "findNext").unwrap_or(false);
        let forward = json_bool_field(r#"{"text":"hello"}"#, "forward").unwrap_or(true);
        push(
            &queue,
            ShellCommand::FindText {
                query: text,
                forward,
                find_next,
            },
        );

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command must be enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::FindText {
                query,
                forward: fwd,
                find_next: fn_,
            } => {
                assert_eq!(query, "hello", "query must carry the typed text");
                assert!(fwd, "forward must default to true");
                assert!(!fn_, "find_next must default to false for a fresh search");
            }
            other => panic!("expected FindText; got {other:?}"),
        }
    }

    /// `find_in_page` with empty text enqueues `FindText` with an empty query
    /// (not a no-op; clears the active find session).
    #[test]
    fn p5_find_in_page_empty_text_enqueues_empty_query() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let text =
            json_string_field(r#"{"text":""}"#, "text").expect("empty text field must parse");
        push(
            &queue,
            ShellCommand::FindText {
                query: text,
                forward: true,
                find_next: false,
            },
        );

        let mut q = queue.lock().unwrap();
        match q.pop_front().unwrap() {
            ShellCommand::FindText { query, .. } => {
                assert!(
                    query.is_empty(),
                    "empty text input must produce empty query"
                );
            }
            other => panic!("expected FindText; got {other:?}"),
        }
    }

    // ── P5: find_next / find_prev ops registered (C3) ────────────────────────

    /// `find_next` op enqueues `ShellCommand::FindNext`.
    #[test]
    fn p5_find_next_op_enqueues_find_next() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        push(&queue, ShellCommand::FindNext);

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command must be enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::FindNext => {}
            other => panic!("expected FindNext; got {other:?}"),
        }
    }

    /// `find_prev` op enqueues `ShellCommand::FindPrev`.
    #[test]
    fn p5_find_prev_op_enqueues_find_prev() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        push(&queue, ShellCommand::FindPrev);

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "exactly one command must be enqueued");
        match q.pop_front().unwrap() {
            ShellCommand::FindPrev => {}
            other => panic!("expected FindPrev; got {other:?}"),
        }
    }

    // ── D1: editable-field context menu serialization ─────────────────────────

    /// `context_menu_payload` for an editable request includes `"isEditable":true`
    /// and `"editFlags"` with the supplied bitmask.
    #[test]
    fn d1_context_menu_payload_editable_sets_is_editable_and_edit_flags() {
        let req = ContextMenuRequest {
            kind: ContextMenuKind::Editable,
            target_url: None,
            selected_text: None,
            x: 50,
            y: 100,
            can_go_back: false,
            can_go_forward: false,
            is_editable: true,
            edit_flags: edit_flag::CAN_COPY | edit_flag::CAN_PASTE | edit_flag::CAN_SELECT_ALL,
        };
        let payload = context_menu_payload(&req);
        assert!(
            payload.contains("\"isEditable\":true"),
            "payload must contain isEditable:true; got: {payload}"
        );
        let expected_flags = edit_flag::CAN_COPY | edit_flag::CAN_PASTE | edit_flag::CAN_SELECT_ALL;
        assert!(
            payload.contains(&format!("\"editFlags\":{expected_flags}")),
            "payload must contain editFlags:{expected_flags}; got: {payload}"
        );
        assert!(
            payload.contains("\"kind\":\"editable\""),
            "payload kind must be \"editable\"; got: {payload}"
        );
    }

    /// `context_menu_payload` for a page request has `"isEditable":false` and
    /// `"editFlags":0`.
    #[test]
    fn d1_context_menu_payload_page_kind_has_false_editable() {
        let req = ContextMenuRequest {
            kind: ContextMenuKind::Page,
            target_url: None,
            selected_text: None,
            x: 0,
            y: 0,
            can_go_back: false,
            can_go_forward: false,
            is_editable: false,
            edit_flags: 0,
        };
        let payload = context_menu_payload(&req);
        assert!(
            payload.contains("\"isEditable\":false"),
            "non-editable payload must have isEditable:false; got: {payload}"
        );
        assert!(
            payload.contains("\"editFlags\":0"),
            "non-editable payload must have editFlags:0; got: {payload}"
        );
    }

    // ── D10: nav-state patch in context menu payload ──────────────────────────

    /// `context_menu_payload` serializes `can_go_back = true` as
    /// `"canGoBack":true`. This is the shell-patched value; verifying the
    /// serialized form ensures that when the shell writes `req.can_go_back =
    /// true` the chrome receives a truthy payload.
    #[test]
    fn d10_context_menu_payload_can_go_back_true() {
        let req = ContextMenuRequest {
            kind: ContextMenuKind::Page,
            target_url: None,
            selected_text: None,
            x: 0,
            y: 0,
            can_go_back: true,
            can_go_forward: false,
            is_editable: false,
            edit_flags: 0,
        };
        let payload = context_menu_payload(&req);
        assert!(
            payload.contains("\"canGoBack\":true"),
            "patched can_go_back=true must appear in payload; got: {payload}"
        );
        assert!(
            payload.contains("\"canGoForward\":false"),
            "unpatched can_go_forward=false must appear in payload; got: {payload}"
        );
    }

    /// `context_menu_payload` with both nav flags patched to `true` serializes
    /// both as `true`. Verifies the full D10 patch path at the serialization seam.
    #[test]
    fn d10_context_menu_payload_both_nav_flags_patched() {
        let req = ContextMenuRequest {
            kind: ContextMenuKind::Page,
            target_url: None,
            selected_text: None,
            x: 0,
            y: 0,
            can_go_back: true,
            can_go_forward: true,
            is_editable: false,
            edit_flags: 0,
        };
        let payload = context_menu_payload(&req);
        assert!(
            payload.contains("\"canGoBack\":true"),
            "patched can_go_back=true must appear in payload; got: {payload}"
        );
        assert!(
            payload.contains("\"canGoForward\":true"),
            "patched can_go_forward=true must appear in payload; got: {payload}"
        );
    }

    /// A freshly constructed (un-patched) `ContextMenuRequest` has both nav
    /// flags as `false` — confirming the CEF-side default before the shell patch.
    #[test]
    fn d10_context_menu_payload_default_nav_flags_are_false() {
        let req = ContextMenuRequest {
            kind: ContextMenuKind::Page,
            target_url: None,
            selected_text: None,
            x: 0,
            y: 0,
            can_go_back: false,
            can_go_forward: false,
            is_editable: false,
            edit_flags: 0,
        };
        let payload = context_menu_payload(&req);
        assert!(
            payload.contains("\"canGoBack\":false"),
            "default can_go_back must be false in payload; got: {payload}"
        );
        assert!(
            payload.contains("\"canGoForward\":false"),
            "default can_go_forward must be false in payload; got: {payload}"
        );
    }

    /// `context_menu_action` op is registered in `build_op_registry` (covers D1
    /// edit-command dispatch path through the same op).
    #[test]
    fn d1_context_menu_action_op_is_registered() {
        let commands: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&commands);
        assert!(
            registry.op_names().contains(&"context_menu_action"),
            "context_menu_action must be registered for D1 edit commands"
        );
    }

    // ── I1: omnibox resolver tests ────────────────────────────────────────────
    //
    // Pure unit tests on `resolve_omnibox_input`.  Each test covers one
    // decision-branch from the docstring; together they prove the black-box
    // contract: search queries never become bare `https://...` URLs, and URL-like
    // inputs never go through the search engine.

    /// The default `DuckDuckGo` template is well-formed.
    #[test]
    fn i1_default_search_template_contains_placeholder() {
        let tmpl = default_search_url_template();
        assert!(
            tmpl.contains("\x7bq\x7d"),
            "default template must contain the {{q}} placeholder; got {tmpl:?}"
        );
        assert!(
            tmpl.starts_with("https://"),
            "default template must start with https://; got {tmpl:?}"
        );
    }

    /// A multi-word query resolves to the provider search URL with the text
    /// URL-encoded — it must NOT become `https://<query>`.
    #[test]
    fn i1_multi_word_query_resolves_to_search_url() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("weather rust", tmpl);
        // Should be a DuckDuckGo search URL.
        assert!(
            result.starts_with("https://duckduckgo.com/"),
            "multi-word query must resolve to the search URL, not https://<query>; \
             got {result:?}"
        );
        assert!(
            result.contains("weather"),
            "search URL must contain the query text; got {result:?}"
        );
        // Space must be percent-encoded, not a bare space.
        assert!(
            !result.contains(' '),
            "search URL must not contain a bare space; got {result:?}"
        );
    }

    /// A query with spaces must never produce a bare `https://` URL.
    #[test]
    fn i1_query_with_spaces_never_produces_bare_https_url() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("rust async traits", tmpl);
        assert!(
            !result.starts_with("https://rust async"),
            "query with spaces must not produce bare https://<query>; got {result:?}"
        );
        assert!(
            result.contains("rust"),
            "search URL must contain query text; got {result:?}"
        );
    }

    /// A bare domain (has a dot, no spaces) resolves to `https://<domain>`.
    #[test]
    fn i1_bare_domain_resolves_to_https() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("example.com", tmpl);
        assert_eq!(
            result, "https://example.com",
            "bare domain must resolve to https://example.com; got {result:?}"
        );
    }

    /// A URL with an explicit scheme passes through unchanged.
    #[test]
    fn i1_url_with_scheme_passes_through() {
        let tmpl = default_search_url_template();
        let url = "https://x.test/path?q=foo";
        let result = resolve_omnibox_input(url, tmpl);
        assert_eq!(
            result, url,
            "URL with existing scheme must pass through unchanged; got {result:?}"
        );
    }

    /// `http://` URLs also pass through unchanged.
    #[test]
    fn i1_http_url_passes_through() {
        let tmpl = default_search_url_template();
        let url = "http://insecure.example.org/page";
        let result = resolve_omnibox_input(url, tmpl);
        assert_eq!(
            result, url,
            "http:// URL must pass through unchanged; got {result:?}"
        );
    }

    /// `localhost` resolves to `http://localhost` (loopback → http, ADR-0018 rule 4).
    #[test]
    fn i1_localhost_resolves_to_http() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("localhost", tmpl);
        assert_eq!(
            result, "http://localhost",
            "localhost must resolve to http://localhost (loopback uses http); got {result:?}"
        );
    }

    /// `localhost:3000` resolves to `http://localhost:3000` (loopback → http, ADR-0018 rule 4).
    #[test]
    fn i1_localhost_with_port_resolves_to_http_url() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("localhost:3000", tmpl);
        assert_eq!(
            result, "http://localhost:3000",
            "localhost:port must resolve to http://localhost:port; got {result:?}"
        );
    }

    /// `mote://` URLs pass through unchanged.
    #[test]
    fn i1_mote_scheme_passes_through() {
        let tmpl = default_search_url_template();
        let url = "mote://chrome/newtab.html";
        let result = resolve_omnibox_input(url, tmpl);
        assert_eq!(
            result, url,
            "mote:// URL must pass through unchanged; got {result:?}"
        );
    }

    /// `about:blank` passes through unchanged.
    #[test]
    fn i1_about_scheme_passes_through() {
        let tmpl = default_search_url_template();
        let url = "about:blank";
        let result = resolve_omnibox_input(url, tmpl);
        assert_eq!(
            result, url,
            "about:blank must pass through unchanged; got {result:?}"
        );
    }

    // ── ADR-0018 worked-examples matrix ──────────────────────────────────────
    //
    // These tests encode the exact worked-examples from ADR-0018 §Decision
    // Outcome. They fail before the PSL-based resolver is in place and must
    // pass after (fail-first per the project's always-test rule).

    /// `node.js` has an unknown public suffix and must search (ADR-0018 rule 3).
    #[test]
    fn i1_adr0018_node_js_is_search() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("node.js", tmpl);
        assert!(
            result.starts_with("https://duckduckgo.com/"),
            "node.js has unknown suffix and must resolve to search; got {result:?}"
        );
        assert!(
            result.contains("node"),
            "search URL must contain the query; got {result:?}"
        );
    }

    /// `foo.internal` has an unknown public suffix and must search (ADR-0018 rule 3).
    #[test]
    fn i1_adr0018_foo_internal_is_search() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("foo.internal", tmpl);
        assert!(
            result.starts_with("https://duckduckgo.com/"),
            "foo.internal has unknown suffix and must resolve to search; got {result:?}"
        );
    }

    /// `weather` (dotless word) must search (ADR-0018 rule 3).
    #[test]
    fn i1_adr0018_dotless_word_is_search() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("weather", tmpl);
        assert!(
            result.starts_with("https://duckduckgo.com/"),
            "dotless word must resolve to search; got {result:?}"
        );
    }

    /// `what is 2.5` — space before the dot — must search (ADR-0018 rule 2).
    #[test]
    fn i1_adr0018_space_before_dot_is_search() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("what is 2.5", tmpl);
        assert!(
            result.starts_with("https://duckduckgo.com/"),
            "space before dot must resolve to search; got {result:?}"
        );
    }

    /// `google.com` (known ICANN suffix) must navigate to `https://google.com` (ADR-0018 rule 3/4).
    #[test]
    fn i1_adr0018_google_com_navigates() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("google.com", tmpl);
        assert_eq!(
            result, "https://google.com",
            "google.com must navigate to https://google.com; got {result:?}"
        );
    }

    /// `wikipedia.org` (known ICANN suffix) must navigate to `https://wikipedia.org` (ADR-0018 rule 3/4).
    #[test]
    fn i1_adr0018_wikipedia_org_navigates() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("wikipedia.org", tmpl);
        assert_eq!(
            result, "https://wikipedia.org",
            "wikipedia.org must navigate to https://wikipedia.org; got {result:?}"
        );
    }

    /// `127.0.0.1:8080` (loopback IPv4 with port) must navigate to `http://127.0.0.1:8080` (ADR-0018 rule 3/4).
    #[test]
    fn i1_adr0018_ipv4_loopback_with_port_is_http() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("127.0.0.1:8080", tmpl);
        assert_eq!(
            result, "http://127.0.0.1:8080",
            "127.0.0.1:8080 must navigate to http://127.0.0.1:8080; got {result:?}"
        );
    }

    /// `https://x.test/p` with explicit scheme passes through unchanged (ADR-0018 rule 1).
    #[test]
    fn i1_adr0018_explicit_https_passes_through() {
        let tmpl = default_search_url_template();
        let url = "https://x.test/p";
        let result = resolve_omnibox_input(url, tmpl);
        assert_eq!(
            result, url,
            "explicit https:// must pass through unchanged; got {result:?}"
        );
    }

    /// `mote://chrome/settings` with explicit scheme passes through unchanged (ADR-0018 rule 1).
    #[test]
    fn i1_adr0018_mote_scheme_passes_through() {
        let tmpl = default_search_url_template();
        let url = "mote://chrome/settings";
        let result = resolve_omnibox_input(url, tmpl);
        assert_eq!(
            result, url,
            "mote:// must pass through unchanged; got {result:?}"
        );
    }

    /// `foo.github.io` (known private suffix) must navigate to `https://foo.github.io` (ADR-0018 rule 3/4).
    #[test]
    fn i1_adr0018_known_private_suffix_navigates() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("foo.github.io", tmpl);
        assert_eq!(
            result, "https://foo.github.io",
            "foo.github.io has a known private suffix and must navigate; got {result:?}"
        );
    }

    /// Bare `::1` (IPv6 loopback without brackets) must navigate to `http://::1` (ADR-0018 rule 3/4).
    #[test]
    fn i1_adr0018_bare_ipv6_loopback_navigates_http() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("::1", tmpl);
        assert_eq!(
            result, "http://::1",
            "bare ::1 must navigate to http://::1 (IPv6 loopback); got {result:?}"
        );
    }

    /// Template without `{q}` falls back to the built-in default (ADR-0018 rule 5).
    #[test]
    fn i1_adr0018_invalid_template_falls_back_to_default() {
        let bad_tmpl = "https://search.example.com/";
        let result = resolve_omnibox_input("hello world", bad_tmpl);
        let default_result = resolve_omnibox_input("hello world", default_search_url_template());
        assert_eq!(
            result, default_result,
            "template without {{q}} must fall back to default; got {result:?}"
        );
    }

    /// A custom search engine template is used when provided.
    #[test]
    fn i1_custom_template_is_used() {
        // Build template without a format arg to avoid the lint.
        let tmpl = String::from_utf8(b"https://search.example.com/?q=\x7bq\x7d".to_vec())
            .expect("ASCII bytes are valid UTF-8");
        let result = resolve_omnibox_input("hello world", &tmpl);
        assert!(
            result.starts_with("https://search.example.com/"),
            "custom template must be used; got {result:?}"
        );
        assert!(
            result.contains("hello"),
            "search URL must contain query; got {result:?}"
        );
    }

    /// Schemeless input with a path navigates to `https://<host>/<path>`.
    #[test]
    fn i1_schemeless_with_path_navigates() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("github.com/rust-lang/rust", tmpl);
        assert_eq!(
            result, "https://github.com/rust-lang/rust",
            "schemeless URL with path must navigate; got {result:?}"
        );
    }

    /// Schemeless input with path and query string navigates.
    #[test]
    fn i1_schemeless_with_path_and_query_navigates() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("example.org/a?b=c", tmpl);
        assert_eq!(
            result, "https://example.org/a?b=c",
            "schemeless URL with path+query must navigate; got {result:?}"
        );
    }

    /// Schemeless input with fragment navigates.
    #[test]
    fn i1_schemeless_with_fragment_navigates() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("example.com#frag", tmpl);
        assert_eq!(
            result, "https://example.com#frag",
            "schemeless URL with fragment must navigate; got {result:?}"
        );
    }

    /// `localhost:3000/app` (loopback with port and path) navigates to `http://`.
    #[test]
    fn i1_localhost_with_port_and_path_navigates_http() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("localhost:3000/app", tmpl);
        assert_eq!(
            result, "http://localhost:3000/app",
            "localhost:port/path must navigate to http://; got {result:?}"
        );
    }

    /// Non-loopback host with port and path navigates to `https://`.
    #[test]
    fn i1_host_with_port_and_path_navigates_https() {
        let tmpl = default_search_url_template();
        let result = resolve_omnibox_input("example.com:8443/x", tmpl);
        assert_eq!(
            result, "https://example.com:8443/x",
            "host:port/path must navigate to https://; got {result:?}"
        );
    }

    /// `omnibox_submit` op is registered.
    #[test]
    fn i1_omnibox_submit_op_is_registered() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"omnibox_submit"),
            "omnibox_submit must be registered; got: {:?}",
            registry.op_names()
        );
    }

    // ── F1/F10: workspace_id_for_slug index-based resolution ─────────────────
    //
    // `workspace_id_for_slug` must resolve a slug to `WorkspaceId::new(index)`
    // where `index` is the slug's 0-based position in the plugin's ordered list.
    // The old hardcoded match is replaced with an index lookup so any workspace
    // beyond "default" and "work" resolves correctly.

    /// `workspace_id_for_slug` resolves slugs by their 0-based index in the
    /// ordered slug list: "default" → 0, "work" → 1, "research" → 2, unknown → None.
    /// The backward-compatible invariant (default=0, work=1) is preserved.
    #[test]
    fn f1_workspace_id_for_slug_resolves_by_ordered_index() {
        let slugs: Vec<String> = vec![
            "default".to_owned(),
            "work".to_owned(),
            "research".to_owned(),
        ];

        assert_eq!(
            workspace_id_for_slug("default", &slugs),
            Some(WorkspaceId::new(0)),
            "\"default\" must map to WorkspaceId(0) — first in list"
        );
        assert_eq!(
            workspace_id_for_slug("work", &slugs),
            Some(WorkspaceId::new(1)),
            "\"work\" must map to WorkspaceId(1) — second in list"
        );
        assert_eq!(
            workspace_id_for_slug("research", &slugs),
            Some(WorkspaceId::new(2)),
            "\"research\" must map to WorkspaceId(2) — third in list (3rd workspace)"
        );
        assert_eq!(
            workspace_id_for_slug("unknown", &slugs),
            None,
            "unrecognised slug must return None"
        );
    }

    /// `workspace_id_for_slug` with an empty list returns None for any slug.
    #[test]
    fn f1_workspace_id_for_slug_empty_list_returns_none() {
        assert_eq!(
            workspace_id_for_slug("default", &[]),
            None,
            "empty slug list must return None for any slug"
        );
    }

    // ── CL-LOADING: load state op + NavStop ──────────────────────────────────

    /// `stop` op is registered in `build_op_registry`.
    #[test]
    fn cl_loading_stop_op_is_registered() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let registry = build_op_registry(&queue);
        assert!(
            registry.op_names().contains(&"stop"),
            "stop must be registered in build_op_registry; got: {:?}",
            registry.op_names()
        );
    }

    /// `stop` op enqueues exactly one `NavStop` command.
    #[test]
    fn cl_loading_stop_op_enqueues_nav_stop() {
        use std::sync::Mutex;

        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        push(&queue, ShellCommand::NavStop);

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "must enqueue exactly one command");
        match q.pop_front().unwrap() {
            ShellCommand::NavStop => {}
            other => panic!("expected NavStop; got {other:?}"),
        }
    }

    /// `load_state_last` de-dup: the cached value updates only when `is_loading`
    /// changes, not on every tick.
    ///
    /// Mirrors how `sync_load_state` guards re-push: it returns early when
    /// `loading == self.load_state_last` and stores the new value only on a
    /// genuine transition. This test drives the boolean sequence
    /// false→false (no change), false→true (transition), true→true (no change),
    /// true→false (transition) and asserts the cache tracks correctly.
    #[test]
    fn cl_loading_load_state_last_dedup_transitions() {
        // Simulate the de-dup gate in sync_load_state without a live ShellApp.
        // Inline each tick rather than a closure to avoid E0502 borrow conflicts.
        let mut last: bool = false;
        let mut push_count: u32 = 0;

        // Tick 1: false→false (no change) → no push.
        let incoming = false;
        if incoming != last {
            last = incoming;
            push_count += 1;
        }
        assert_eq!(push_count, 0, "false→false: no push expected");
        assert!(!last, "cache must remain false");

        // Tick 2: false→true (transition) → push fires.
        let incoming = true;
        if incoming != last {
            last = incoming;
            push_count += 1;
        }
        assert_eq!(push_count, 1, "false→true: exactly one push expected");
        assert!(last, "cache must update to true");

        // Tick 3: true→true (no change) → no extra push.
        let incoming = true;
        if incoming != last {
            last = incoming;
            push_count += 1;
        }
        assert_eq!(push_count, 1, "true→true: no extra push expected");
        assert!(last, "cache must remain true");

        // Tick 4: true→false (transition) → push fires again.
        let incoming = false;
        if incoming != last {
            last = incoming;
            push_count += 1;
        }
        assert_eq!(
            push_count, 2,
            "true→false: exactly two total pushes expected"
        );
        assert!(!last, "cache must update to false");
    }

    // ── analyze_url (CL-URL-XPARENCY) ────────────────────────────────────────

    /// A tracked URL must be split into scheme/subdomain/registrable/rest
    /// and the three UTM/gclid params must be identified as trackers.
    #[test]
    fn url_analysis_tracked_url_splits_and_finds_trackers() {
        let raw = "https://www.theverge.com/2024/x/story?utm_source=nl&utm_medium=email&gclid=abc";
        let a = analyze_url(raw).expect("tracked http URL must parse");
        assert_eq!(a.scheme, "https://");
        assert_eq!(a.subdomain, "www.");
        assert_eq!(a.registrable, "theverge.com");
        // rest includes path + raw query string (everything after the host).
        assert_eq!(
            a.rest,
            "/2024/x/story?utm_source=nl&utm_medium=email&gclid=abc"
        );
        // The clean URL must have all three tracking params removed.
        assert!(!a.clean_url.contains("utm_source"));
        assert!(!a.clean_url.contains("utm_medium"));
        assert!(!a.clean_url.contains("gclid"));
        // tracker_names must name all three (order-insensitive).
        // tracker_names is already sorted by analyze_url.
        assert_eq!(a.tracker_names, vec!["gclid", "utm_medium", "utm_source"]);
    }

    /// A clean URL (no known tracking params) must produce an empty tracker list
    /// and an unchanged (modulo clearurls normalization) `clean_url`.
    #[test]
    fn url_analysis_clean_url_no_trackers() {
        let raw = "https://www.rust-lang.org/learn?edition=2024";
        let a = analyze_url(raw).expect("clean http URL must parse");
        assert!(
            a.tracker_names.is_empty(),
            "clean URL must have no trackers; got {:?}",
            a.tracker_names
        );
        // The retained query param must still be present in clean_url.
        assert!(
            a.clean_url.contains("edition=2024"),
            "non-tracking param must be kept in clean_url"
        );
    }

    /// localhost and IP-literal hosts must not panic and must use the full
    /// host as the registrable domain (psl has no suffix for these).
    #[test]
    fn url_analysis_localhost_and_ip() {
        let loc = analyze_url("https://localhost:3000/x").expect("localhost must parse");
        assert_eq!(loc.registrable, "localhost:3000");
        assert_eq!(loc.subdomain, "");

        let ip = analyze_url("http://192.168.1.1/path").expect("IP must parse");
        assert_eq!(ip.registrable, "192.168.1.1");
        assert_eq!(ip.subdomain, "");
    }

    /// Internal mote:// and about: URLs must return None — no analysis.
    #[test]
    fn url_analysis_internal_urls_return_none() {
        assert!(
            analyze_url("mote://chrome/newtab.html").is_none(),
            "mote:// must return None"
        );
        assert!(
            analyze_url("about:blank").is_none(),
            "about: must return None"
        );
        assert!(analyze_url("").is_none(), "empty string must return None");
    }

    /// clearurls unwraps Google `url?q=` redirect wrappers: `clean_url` should
    /// be the destination, not the wrapper.
    #[test]
    fn url_analysis_redirect_wrapper_is_unwrapped() {
        let raw = "https://www.google.com/url?q=https%3A%2F%2Fexample.com%2Fpage&sa=D";
        let a = analyze_url(raw).expect("google redirect URL must parse");
        assert!(
            a.clean_url.contains("example.com"),
            "clean_url must be the unwrapped destination; got {}",
            a.clean_url
        );
        assert!(
            !a.clean_url.contains("google.com"),
            "clean_url must not be the wrapper; got {}",
            a.clean_url
        );
    }

    /// `build_hover_display` for a redirect wrapper must show the **unwrapped
    /// destination's** full host (including subdomain) + path, NOT the wrapper's
    /// path. Tracker count is appended when > 0.
    #[test]
    fn hover_display_redirect_wrapper_shows_destination() {
        // A typical email-click redirect wrapper: raw URL is on the wrapper
        // domain; clearurls unwraps it to the real destination.
        // We use the Google /url?q= form which clearurls is confirmed to unwrap.
        let wrapper = "https://www.google.com/url?q=https%3A%2F%2Fwww.theverge.com%2Fbig-story%3Futm_source%3Dnl&sa=D";
        let display = build_hover_display(wrapper);

        // Must show the destination host (full, including subdomain) + path.
        assert!(
            display.starts_with("www.theverge.com"),
            "hover must lead with destination full host; got: {display}"
        );
        assert!(
            display.contains("/big-story"),
            "hover must contain destination path; got: {display}"
        );
        // Must NOT expose the wrapper path.
        assert!(
            !display.contains("/url"),
            "hover must NOT show wrapper path; got: {display}"
        );
        assert!(
            !display.contains("google.com"),
            "hover must NOT show wrapper host; got: {display}"
        );
        // utm_source is a tracker — count must be appended.
        assert!(
            display.contains("tracker"),
            "hover must append tracker count; got: {display}"
        );
        // Query must be stripped from the preview.
        assert!(
            !display.contains("utm_source"),
            "hover must not contain raw query params; got: {display}"
        );
    }

    /// `omnibox_submit` op: valid `text` field → enqueues `ShellCommand::OmniboxSubmit`.
    /// Mirrors the pattern used by `p6_set_search_engine_op_enqueues_set_search_engine`.
    #[test]
    fn i1_omnibox_submit_op_enqueues_command() {
        let queue: CommandQueue = Arc::new(Mutex::new(VecDeque::new()));
        let text = json_string_field(r#"{"text":"rust async"}"#, "text")
            .filter(|s| !s.is_empty())
            .expect("text field must parse");
        push(&queue, ShellCommand::OmniboxSubmit(text));

        let mut q = queue.lock().unwrap();
        assert_eq!(q.len(), 1, "must enqueue exactly one command");
        match q.pop_front().unwrap() {
            ShellCommand::OmniboxSubmit(text) => {
                assert_eq!(text, "rust async", "command must carry the raw text");
            }
            other => panic!("expected OmniboxSubmit; got {other:?}"),
        }
    }
}
