//! Active-tab renderer discarding — kills renderers of idle active tabs.
//!
//! Standard tab-discarding behaviour modeled after Chrome's Memory Saver: active
//! tabs unfocused for >30 minutes have their renderer process killed. The tab
//! remains visible in the window strip and reloads when the user clicks it.
//!
//! Discard decisions are made here (pure logic); the actual renderer kill happens
//! in `mote-shell` which holds the `Page` handles. The shell calls
//! [`Discarder::discard_all`], then kills the renderer for each tab where
//! `is_discarded` transitions from `false` to `true`.

use std::time::{Duration, SystemTime};

use crate::tab::{Tab, TabState};

/// Configuration for active-tab discarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardConfig {
    /// Idle threshold before a renderer is discarded.
    pub discard_after: Duration,
    /// When `true`, pinned tabs are never discarded regardless of idle time.
    ///
    /// Pinned tabs (tabs matching a workspace [`crate::PinnedTab`] entry) are
    /// high-value persistent tabs; killing their renderer causes extra reload
    /// latency on switch and is usually not worth the saved memory.
    pub keep_pinned_loaded: bool,
}

impl Default for DiscardConfig {
    fn default() -> Self {
        Self {
            discard_after: Duration::from_mins(30), // 30 minutes
            keep_pinned_loaded: true,
        }
    }
}

/// Applies renderer-discard decisions to active idle tabs.
///
/// Construct once with the user's [`DiscardConfig`] and call
/// [`Discarder::discard_all`] periodically (shell-driven).
#[derive(Debug, Clone)]
pub struct Discarder {
    config: DiscardConfig,
}

impl Discarder {
    /// Creates a discarder with the given configuration.
    #[must_use]
    pub const fn new(config: DiscardConfig) -> Self {
        Self { config }
    }

    /// Returns `true` if the tab's renderer should be discarded.
    ///
    /// A tab is eligible for discard when **all** of the following are true:
    /// - The tab is [`TabState::Active`].
    /// - The tab is not already discarded.
    /// - The tab is not pinned when `keep_pinned_loaded` is enabled.
    /// - The tab has a `last_visited` timestamp that exceeds `discard_after`.
    ///
    /// A tab with no `last_visited` timestamp is treated as recently active
    /// (i.e., not eligible for discard).
    #[must_use]
    pub fn should_discard(&self, tab: &Tab) -> bool {
        if tab.state != TabState::Active || tab.is_discarded {
            return false;
        }
        if self.config.keep_pinned_loaded && tab.is_pinned {
            return false;
        }
        let Some(last_visited) = tab.last_visited else {
            return false; // no visit timestamp → treat as recently active
        };
        SystemTime::now()
            .duration_since(last_visited)
            .unwrap_or(Duration::ZERO)
            >= self.config.discard_after
    }

    /// Marks all eligible tabs as discarded, returning the count of tabs affected.
    ///
    /// The shell must kill the renderer process for each tab whose `is_discarded`
    /// flag transitions from `false` to `true` after this call.
    pub fn discard_all(&self, tabs: &mut [Tab]) -> usize {
        let mut count = 0;
        for tab in tabs.iter_mut() {
            if self.should_discard(tab) {
                tab.discard();
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use mote_types::{TabId, WorkspaceId};

    use super::*;
    use crate::tab::Tab;

    fn active_tab(id: u64, last_visited_secs_ago: u64) -> Tab {
        let mut t = Tab::new(
            TabId::new(id),
            "https://x.com".to_owned(),
            WorkspaceId::new(1),
        );
        if last_visited_secs_ago > 0 {
            t.last_visited = Some(SystemTime::now() - Duration::from_secs(last_visited_secs_ago));
        }
        t
    }

    fn cfg() -> DiscardConfig {
        DiscardConfig::default()
    }

    #[test]
    fn recently_visited_not_discarded() {
        let d = Discarder::new(cfg());
        let t = active_tab(1, 60); // 1 min ago
        assert!(!d.should_discard(&t));
    }

    #[test]
    fn long_idle_tab_discarded() {
        let d = Discarder::new(cfg());
        let t = active_tab(1, 35 * 60); // 35 min ago
        assert!(d.should_discard(&t));
    }

    #[test]
    fn already_discarded_not_re_discarded() {
        let d = Discarder::new(cfg());
        let mut t = active_tab(1, 35 * 60);
        t.discard();
        assert!(!d.should_discard(&t)); // already discarded
    }

    #[test]
    fn hidden_tab_not_discarded() {
        let d = Discarder::new(cfg());
        let mut t = active_tab(1, 35 * 60);
        t.hide(SystemTime::now());
        assert!(!d.should_discard(&t));
    }

    #[test]
    fn tab_without_last_visited_not_discarded() {
        // A tab that's been opened but never explicitly visited (no last_visited)
        // is treated as recent — we don't know its idle time.
        let d = Discarder::new(cfg());
        let t = active_tab(1, 0); // last_visited = None
        assert!(!d.should_discard(&t));
    }

    #[test]
    fn keep_pinned_loaded_prevents_discard() {
        let d = Discarder::new(DiscardConfig {
            discard_after: Duration::from_mins(30),
            keep_pinned_loaded: true,
        });
        let mut t = active_tab(1, 35 * 60);
        t.is_pinned = true;
        assert!(!d.should_discard(&t));
    }

    #[test]
    fn discard_all_returns_discarded_count() {
        let d = Discarder::new(cfg());
        let mut tabs = vec![
            active_tab(1, 60),      // recent
            active_tab(2, 35 * 60), // idle
            active_tab(3, 40 * 60), // idle
        ];
        let count = d.discard_all(&mut tabs);
        assert_eq!(count, 2);
        assert!(tabs[1].is_discarded);
        assert!(tabs[2].is_discarded);
        assert!(!tabs[0].is_discarded);
    }
}
