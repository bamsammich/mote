//! Hidden-tab TTL aging — removes stale hidden tabs from the session.
//!
//! A hidden tab costs a `SQLite` row, not RAM. The reaper runs periodically
//! (shell-driven) to keep the tab picker uncluttered and disk usage bounded.
//!
//! - Default TTL: **30 days** (see `DESIGN.md` §Hidden tab lifecycle).
//! - Hold flag: a runtime mark on a hidden tab that exempts it from TTL.
//!   See [`crate::Tab::set_hold`].
//! - "Pin" (promoting a tab to a workspace `pinned_tabs` entry in the dotfiles)
//!   is a dotfile concern handled outside this crate — pinned tabs never appear
//!   in the hidden list.

use std::time::{Duration, SystemTime};

use crate::tab::{Tab, TabState};

/// Configuration for hidden-tab aging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenTabConfig {
    /// How long a hidden tab survives before being reaped.
    ///
    /// `None` disables aging entirely (equivalent to "never" in user config).
    pub ttl: Option<Duration>,
    /// Show an indicator when a workspace's hidden-tab count exceeds this
    /// threshold.
    ///
    /// This is a UI hint only; the reaper does not act on it.
    pub soft_warn_at: usize,
}

impl Default for HiddenTabConfig {
    fn default() -> Self {
        Self {
            ttl: Some(Duration::from_hours(720)), // 30 days
            soft_warn_at: 500,
        }
    }
}

/// Determines which hidden tabs have aged past their TTL and removes them.
///
/// Construct once with the user's [`HiddenTabConfig`] and call
/// [`HiddenTabReaper::reap_all`] periodically (shell-driven).
#[derive(Debug, Clone)]
pub struct HiddenTabReaper {
    config: HiddenTabConfig,
}

impl HiddenTabReaper {
    /// Creates a reaper with the given configuration.
    #[must_use]
    pub const fn new(config: HiddenTabConfig) -> Self {
        Self { config }
    }

    /// Returns `true` if `tab` should be deleted.
    ///
    /// A tab is reaped when **all** of the following are true:
    /// - It is [`TabState::Hidden`].
    /// - It has hidden metadata (always true for well-formed hidden tabs).
    /// - The hold flag is not set.
    /// - TTL is enabled and the tab's `released_at` age exceeds the TTL.
    #[must_use]
    pub fn should_reap(&self, tab: &Tab) -> bool {
        if tab.state != TabState::Hidden {
            return false;
        }
        let Some(meta) = &tab.hidden_meta else {
            return false; // malformed — never reap unexpectedly
        };
        if meta.hold {
            return false;
        }
        let Some(ttl) = self.config.ttl else {
            return false; // TTL disabled
        };
        SystemTime::now()
            .duration_since(meta.released_at)
            .unwrap_or(Duration::ZERO)
            >= ttl
    }

    /// Removes all tabs for which [`should_reap`](Self::should_reap) returns
    /// `true`, draining them from `tabs` and returning them.
    pub fn reap_all(&self, tabs: &mut Vec<Tab>) -> Vec<Tab> {
        let mut reaped = Vec::new();
        let mut i = 0;
        while i < tabs.len() {
            if self.should_reap(&tabs[i]) {
                reaped.push(tabs.remove(i));
            } else {
                i += 1;
            }
        }
        reaped
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use mote_types::{TabId, WorkspaceId};

    use super::*;
    use crate::tab::Tab;

    fn hidden_tab(id: u64, released_secs_ago: u64) -> Tab {
        let mut t = Tab::new(
            TabId::new(id),
            "https://example.com".to_owned(),
            WorkspaceId::new(1),
        );
        let released = SystemTime::now() - Duration::from_secs(released_secs_ago);
        t.hide(released);
        t
    }

    fn cfg_30d() -> HiddenTabConfig {
        HiddenTabConfig::default()
    }

    #[test]
    fn recent_tab_not_reaped() {
        let reaper = HiddenTabReaper::new(cfg_30d());
        let tab = hidden_tab(1, 1000); // ~17 min ago
        assert!(!reaper.should_reap(&tab));
    }

    #[test]
    fn old_tab_reaped() {
        let reaper = HiddenTabReaper::new(cfg_30d());
        let tab = hidden_tab(1, 31 * 24 * 3600); // 31 days ago
        assert!(reaper.should_reap(&tab));
    }

    #[test]
    fn held_tab_never_reaped_even_when_old() {
        let reaper = HiddenTabReaper::new(cfg_30d());
        let mut tab = hidden_tab(1, 365 * 24 * 3600); // 1 year ago
        tab.set_hold(true);
        assert!(!reaper.should_reap(&tab));
    }

    #[test]
    fn active_tab_never_reaped() {
        let reaper = HiddenTabReaper::new(cfg_30d());
        let tab = Tab::new(
            TabId::new(1),
            "https://example.com".to_owned(),
            WorkspaceId::new(1),
        );
        assert_eq!(tab.state, TabState::Active);
        assert!(!reaper.should_reap(&tab));
    }

    #[test]
    fn ttl_never_means_nothing_reaped() {
        let reaper = HiddenTabReaper::new(HiddenTabConfig {
            ttl: None,
            soft_warn_at: 500,
        });
        let tab = hidden_tab(1, 365 * 24 * 3600); // 1 year ago
        assert!(!reaper.should_reap(&tab));
    }

    #[test]
    fn reap_batch_removes_expired_tabs() {
        let r = HiddenTabReaper::new(cfg_30d());
        let mut tabs = vec![
            hidden_tab(1, 1000),           // recent → keep
            hidden_tab(2, 31 * 24 * 3600), // 31d → reap
            hidden_tab(3, 29 * 24 * 3600), // 29d → keep
            hidden_tab(4, 60 * 24 * 3600), // 60d → reap
        ];
        let removed = r.reap_all(&mut tabs);
        assert_eq!(removed.len(), 2);
        assert!(removed.iter().any(|t| t.id == TabId::new(2)));
        assert!(removed.iter().any(|t| t.id == TabId::new(4)));
        assert_eq!(tabs.len(), 2);
    }
}
