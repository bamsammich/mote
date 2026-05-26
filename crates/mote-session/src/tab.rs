//! Tab model — the fundamental unit of the session axis.
//!
//! A tab is always in exactly one [`TabState`]:
//!
//! - [`TabState::Active`] — visible in some window's strip; renderer may be
//!   alive or discarded after idle.
//! - [`TabState::Hidden`] — belongs to its workspace but not shown in any
//!   window. Costs a `SQLite` row, not RAM; renderer is destroyed.
//! - [`TabState::Closed`] — explicitly closed. Recoverable via undo-close
//!   for a short window; gone after `HiddenTabReaper` TTL.
//!
//! State transitions are enforced by the methods on [`Tab`]:
//!
//! - active → hidden: [`Tab::hide`] (window closed, or `⌘⇧H`)
//! - active → closed: [`Tab::close`] (`Ctrl+W`, middle-click)
//! - hidden → active: [`Tab::reveal`] (workspace tab-picker)
//! - active (discarded) → active (live): [`Tab::undiscard`] (user focuses tab)

use std::time::SystemTime;

use mote_types::{TabId, WorkspaceId};
use serde::{Deserialize, Serialize};

use crate::serde_helpers::{opt_system_time, system_time, tab_id, workspace_id};

/// The three mutually exclusive states a tab can be in.
///
/// See `DESIGN.md` §Tab Persistence — Three tab states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabState {
    /// Visible in some window's tab strip.
    ///
    /// The renderer process may be alive (focused / recently focused) or
    /// discarded (unfocused for >30 min). The tab remains in the strip and
    /// reloads when clicked.
    Active,
    /// Belongs to the workspace but not shown in any window.
    ///
    /// The renderer is destroyed at the active → hidden transition. The tab
    /// is retrievable via the workspace tab picker (`Mod+Space`).
    Hidden,
    /// Explicitly closed by the user (`Ctrl+W` / middle-click).
    ///
    /// Recoverable via undo-close for a short window (shell-controlled).
    Closed,
}

/// Metadata carried by a tab in the [`TabState::Hidden`] state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenTabMeta {
    /// When the tab transitioned to hidden.
    ///
    /// Used by [`HiddenTabReaper`](crate::HiddenTabReaper) for TTL
    /// computation and by the tab picker for recency ranking.
    #[serde(with = "system_time")]
    pub released_at: SystemTime,
    /// When `true`, this tab is exempt from TTL aging.
    ///
    /// Set via the tab-picker right-click menu; session-only (not dotfiles).
    /// See `DESIGN.md` §Hold.
    pub hold: bool,
}

/// The back/forward navigation history for a single tab.
///
/// Entries are full URLs. The cursor points at the currently-displayed page.
/// Pushing a new URL while the cursor is not at the tail truncates the forward
/// history (matching browser convention).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabHistory {
    /// All visited URLs in order, oldest first.
    entries: Vec<String>,
    /// Index of the currently-displayed page in `entries`.
    ///
    /// Invariant: `cursor < entries.len()` when `entries` is non-empty.
    cursor: usize,
}

impl TabHistory {
    /// Pushes a new URL onto the history stack.
    ///
    /// Any forward history (entries after the current cursor) is discarded.
    pub fn push(&mut self, url: String) {
        // On an empty history the truncate length is 0; otherwise keep
        // everything up to and including the current cursor position.
        let keep = if self.entries.is_empty() {
            0
        } else {
            self.cursor + 1
        };
        self.entries.truncate(keep);
        self.entries.push(url);
        self.cursor = self.entries.len().saturating_sub(1);
    }

    /// Returns the currently-displayed URL, or `None` if history is empty.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(String::as_str)
    }

    /// Returns `true` if the user can navigate backward.
    #[must_use]
    pub const fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    /// Returns `true` if the user can navigate forward.
    #[must_use]
    pub const fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Moves the cursor one step backward, if possible.
    pub const fn go_back(&mut self) {
        if self.can_go_back() {
            self.cursor -= 1;
        }
    }

    /// Moves the cursor one step forward, if possible.
    pub const fn go_forward(&mut self) {
        if self.can_go_forward() {
            self.cursor += 1;
        }
    }
}

/// A single browser tab and its full session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    /// Stable opaque identifier.
    #[serde(with = "tab_id")]
    pub id: TabId,
    /// The workspace this tab belongs to.
    #[serde(with = "workspace_id")]
    pub workspace_id: WorkspaceId,
    /// Current URL.
    pub url: String,
    /// Page title, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Reference to the favicon (URL or data URI), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_ref: Option<String>,
    /// Current state.
    pub state: TabState,
    /// Whether the active renderer has been discarded for memory pressure.
    ///
    /// Only meaningful when `state == Active`. Hidden tabs have no renderer,
    /// so `is_discarded` is always `false` for them.
    #[serde(default)]
    pub is_discarded: bool,
    /// Whether this tab has been promoted to a workspace pinned tab.
    ///
    /// Set by the shell when the tab matches a pinned entry in
    /// [`WorkspaceConfig::pinned_tabs`](crate::WorkspaceConfig::pinned_tabs).
    /// Pinned tabs are exempt from active-tab discarding when
    /// `keep_pinned_loaded` is enabled.
    #[serde(default)]
    pub is_pinned: bool,
    /// Vertical scroll offset (pixels).
    #[serde(default)]
    pub scroll_y: i64,
    /// Back/forward navigation history.
    #[serde(default)]
    pub history: TabHistory,
    /// Timestamp of the last user focus on this tab.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_system_time"
    )]
    pub last_visited: Option<SystemTime>,
    /// Hidden-state metadata; `Some` only when `state == Hidden`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_meta: Option<HiddenTabMeta>,
}

impl Tab {
    /// Creates a new active tab with the given URL in the given workspace.
    #[must_use]
    pub fn new(id: TabId, url: String, workspace_id: WorkspaceId) -> Self {
        Self {
            id,
            workspace_id,
            url,
            title: None,
            favicon_ref: None,
            state: TabState::Active,
            is_discarded: false,
            is_pinned: false,
            scroll_y: 0,
            history: TabHistory::default(),
            last_visited: None,
            hidden_meta: None,
        }
    }

    /// Transitions an active tab to hidden.
    ///
    /// `released_at` is the moment the window closed (or `⌘⇧H` was pressed);
    /// the tab picker ranks hidden tabs by recency using this timestamp.
    ///
    /// No-op if the tab is not currently active.
    pub fn hide(&mut self, released_at: SystemTime) {
        if self.state == TabState::Active {
            self.state = TabState::Hidden;
            self.is_discarded = false; // no renderer to discard when hidden
            self.hidden_meta = Some(HiddenTabMeta {
                released_at,
                hold: false,
            });
        }
    }

    /// Transitions an active tab to closed.
    ///
    /// `close()` only applies to active tabs — hidden tabs are removed by the
    /// reaper, not by a direct close action. No-op for hidden/closed.
    pub fn close(&mut self) {
        if self.state == TabState::Active {
            self.state = TabState::Closed;
        }
    }

    /// Transitions a hidden tab back to active.
    ///
    /// No-op if the tab is not currently hidden.
    pub fn reveal(&mut self) {
        if self.state == TabState::Hidden {
            self.state = TabState::Active;
            self.hidden_meta = None;
        }
    }

    /// Marks an active tab's renderer as discarded (memory pressure).
    ///
    /// The tab remains in its window's strip; clicking it reloads.
    /// No-op if the tab is not currently active.
    pub fn discard(&mut self) {
        if self.state == TabState::Active {
            self.is_discarded = true;
        }
    }

    /// Clears the discarded flag when the tab is reloaded.
    pub const fn undiscard(&mut self) {
        self.is_discarded = false;
    }

    /// Sets or clears the hold flag on a hidden tab.
    ///
    /// A held tab is exempt from TTL aging. No-op if the tab is not hidden.
    pub const fn set_hold(&mut self, hold: bool) {
        if let Some(meta) = self.hidden_meta.as_mut() {
            meta.hold = hold;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn make_tab(id: u64, url: &str, ws: u64) -> Tab {
        Tab::new(TabId::new(id), url.to_owned(), WorkspaceId::new(ws))
    }

    // ── Tab-state transitions ─────────────────────────────────────────────

    #[test]
    fn new_tab_is_active() {
        let t = make_tab(1, "https://example.com", 1);
        assert_eq!(t.state, TabState::Active);
        assert!(!t.is_discarded);
    }

    #[test]
    fn active_to_hidden_on_window_close() {
        let mut t = make_tab(1, "https://example.com", 1);
        let released = SystemTime::now();
        t.hide(released);
        assert_eq!(t.state, TabState::Hidden);
        assert_eq!(t.hidden_meta.as_ref().unwrap().released_at, released);
        assert!(!t.hidden_meta.as_ref().unwrap().hold);
        // Discard flag is cleared on hide — no active renderer to track.
        assert!(!t.is_discarded);
    }

    #[test]
    fn active_to_closed() {
        let mut t = make_tab(1, "https://example.com", 1);
        t.close();
        assert_eq!(t.state, TabState::Closed);
    }

    #[test]
    fn hidden_to_active_reveal() {
        let mut t = make_tab(1, "https://example.com", 1);
        t.hide(SystemTime::now());
        assert_eq!(t.state, TabState::Hidden);
        t.reveal();
        assert_eq!(t.state, TabState::Active);
        assert!(t.hidden_meta.is_none());
    }

    #[test]
    fn close_on_hidden_tab_is_noop() {
        // Closing a hidden tab is not a direct operation — it must be revealed
        // first, or reaped by the TTL reaper. close() only applies to Active.
        let mut t = make_tab(1, "https://example.com", 1);
        t.hide(SystemTime::now());
        t.close();
        assert_eq!(t.state, TabState::Hidden);
    }

    // ── Discard / hold ───────────────────────────────────────────────────

    #[test]
    fn discard_marks_active_tab() {
        let mut t = make_tab(1, "https://example.com", 1);
        t.discard();
        assert!(t.is_discarded);
        assert_eq!(t.state, TabState::Active); // still active, just no renderer
    }

    #[test]
    fn undiscard_clears_flag() {
        let mut t = make_tab(1, "https://example.com", 1);
        t.discard();
        t.undiscard();
        assert!(!t.is_discarded);
    }

    #[test]
    fn hold_exempts_from_ttl() {
        let mut t = make_tab(1, "https://example.com", 1);
        t.hide(SystemTime::now());
        t.set_hold(true);
        assert!(t.hidden_meta.as_ref().unwrap().hold);
    }

    #[test]
    fn hold_on_active_tab_is_noop() {
        let mut t = make_tab(1, "https://example.com", 1);
        // set_hold on an active tab is a no-op (no hidden_meta).
        t.set_hold(true);
        assert!(t.hidden_meta.is_none());
    }

    // ── History ──────────────────────────────────────────────────────────

    #[test]
    fn history_push_and_back() {
        let mut h = TabHistory::default();
        h.push("https://a.com".to_owned());
        h.push("https://b.com".to_owned());
        h.push("https://c.com".to_owned());
        assert_eq!(h.current(), Some("https://c.com"));
        assert!(h.can_go_back());
        assert!(!h.can_go_forward());
        h.go_back();
        assert_eq!(h.current(), Some("https://b.com"));
        assert!(h.can_go_forward());
    }

    #[test]
    fn history_forward_branch_cleared_on_push() {
        let mut h = TabHistory::default();
        h.push("https://a.com".to_owned());
        h.push("https://b.com".to_owned());
        h.go_back(); // cursor at a
        h.push("https://c.com".to_owned()); // new navigation from a
        assert!(!h.can_go_forward());
        assert_eq!(h.current(), Some("https://c.com"));
    }

    #[test]
    fn history_push_on_empty() {
        let mut h = TabHistory::default();
        assert_eq!(h.current(), None);
        h.push("https://a.com".to_owned());
        assert_eq!(h.current(), Some("https://a.com"));
        assert!(!h.can_go_back());
        assert!(!h.can_go_forward());
    }

    #[test]
    fn history_go_back_at_start_is_noop() {
        let mut h = TabHistory::default();
        h.push("https://a.com".to_owned());
        h.go_back();
        assert_eq!(h.current(), Some("https://a.com"));
    }

    #[test]
    fn history_go_forward_at_end_is_noop() {
        let mut h = TabHistory::default();
        h.push("https://a.com".to_owned());
        h.go_forward();
        assert_eq!(h.current(), Some("https://a.com"));
    }

    // ── Serialization round-trip ─────────────────────────────────────────

    #[test]
    fn tab_roundtrip_json() {
        let mut t = make_tab(7, "https://rust-lang.org", 2);
        t.title = Some("Rust".to_owned());
        t.scroll_y = 512;
        let json = serde_json::to_string(&t).unwrap();
        let back: Tab = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id.get(), 7);
        assert_eq!(back.url, "https://rust-lang.org");
        assert_eq!(back.title, Some("Rust".to_owned()));
        assert_eq!(back.scroll_y, 512);
    }

    #[test]
    fn hidden_tab_roundtrip_json() {
        let mut t = make_tab(3, "https://hidden.com", 1);
        t.hide(SystemTime::now() - Duration::from_hours(1));
        t.set_hold(true);
        let json = serde_json::to_string(&t).unwrap();
        let back: Tab = serde_json::from_str(&json).unwrap();
        assert_eq!(back.state, TabState::Hidden);
        assert!(back.hidden_meta.as_ref().unwrap().hold);
    }
}
