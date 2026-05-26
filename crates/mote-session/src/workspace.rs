//! The workspace axis — what the user is doing.
//!
//! Split into two strictly separate parts:
//!
//! - [`WorkspaceConfig`]: dotfile-config-derived (name, icon, accent, pinned
//!   tabs). Loaded from `mote.workspace.define` in user Lua config. Checked
//!   into dotfiles; reproducible across machines.
//! - [`WorkspaceState`]: runtime session state (last-active tab, tab ordering,
//!   resized slot sizes). Persisted to session `SQLite`; machine-specific.
//!
//! A workspace does **not** own cookies or storage — that is the identity's
//! job. See `DESIGN.md` §Workspace.

use std::collections::HashMap;

use mote_types::{IdentityId, TabId, WorkspaceId};
use serde::{Deserialize, Serialize};

use crate::serde_helpers::{opt_identity_id, opt_tab_id, tab_id_vec, workspace_id as serde_ws_id};

/// A pinned tab entry in a workspace definition.
///
/// Pinned tabs are dotfile-configured and present on every machine. They are
/// distinct from the runtime tab list managed in [`WorkspaceState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedTab {
    /// The URL this pinned tab opens.
    pub url: String,
    /// Override the workspace's default identity for this specific tab.
    ///
    /// `None` means the tab inherits the workspace's
    /// [`WorkspaceConfig::default_identity_id`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_identity_id"
    )]
    pub identity_id: Option<IdentityId>,
}

/// Dotfile-config-derived workspace definition.
///
/// Everything in this struct comes from `mote.workspace.define` in the user's
/// Lua config. It is **not** written to session `SQLite` — it lives in the
/// user's dotfiles repo and is loaded fresh on each launch.
///
/// A workspace does not own cookies or storage — that is the identity's job.
/// See `DESIGN.md` §Workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Stable identity for this workspace.
    #[serde(with = "serde_ws_id")]
    pub id: WorkspaceId,
    /// User-visible name (e.g. `"Work"`, `"Personal"`, `"Deep Research"`).
    pub name: String,
    /// Lucide icon name for the workspace switcher (e.g. `"briefcase"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Accent colour override for this workspace (CSS hex, e.g. `"#3b82f6"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// Default identity for tabs opened in this workspace.
    ///
    /// `None` means the global default identity is used.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_identity_id"
    )]
    pub default_identity_id: Option<IdentityId>,
    /// Default new-tab URL for this workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_newtab_url: Option<String>,
    /// Ordered list of pinned tabs for this workspace.
    #[serde(default)]
    pub pinned_tabs: Vec<PinnedTab>,
}

/// Runtime session state for a single workspace.
///
/// Written to session `SQLite` on every flush. Keyed by workspace ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    /// The workspace this state belongs to.
    #[serde(with = "serde_ws_id")]
    pub id: WorkspaceId,
    /// The most recently focused tab in this workspace.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_tab_id")]
    pub last_active_tab: Option<TabId>,
    /// Ordered list of tab IDs belonging to this workspace (non-pinned tabs).
    #[serde(with = "tab_id_vec")]
    pub tab_order: Vec<TabId>,
    /// User-resized slot dimensions, keyed by slot name (e.g. `"left-sidebar"`).
    #[serde(default)]
    pub slot_sizes: HashMap<String, u32>,
}

impl WorkspaceState {
    /// Creates an empty runtime state for the given workspace.
    #[must_use]
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            last_active_tab: None,
            tab_order: Vec::new(),
            slot_sizes: HashMap::new(),
        }
    }

    /// Records `tab` as the most recently focused tab in this workspace.
    pub const fn set_last_active(&mut self, tab: TabId) {
        self.last_active_tab = Some(tab);
    }

    /// Appends `tab` to the end of the workspace's tab ordering.
    ///
    /// No-op if `tab` is already present.
    pub fn push_tab(&mut self, tab: TabId) {
        if !self.tab_order.contains(&tab) {
            self.tab_order.push(tab);
        }
    }

    /// Removes `tab` from the workspace's tab ordering.
    ///
    /// No-op if the tab is not present.
    pub fn remove_tab(&mut self, tab: TabId) {
        self.tab_order.retain(|&t| t != tab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_config_roundtrip_json() {
        let cfg = WorkspaceConfig {
            id: WorkspaceId::new(1),
            name: "Work".to_owned(),
            icon: Some("briefcase".to_owned()),
            accent: Some("#3b82f6".to_owned()),
            default_identity_id: Some(IdentityId::new(1)),
            default_newtab_url: Some("internal://dashboard".to_owned()),
            pinned_tabs: vec![PinnedTab {
                url: "https://linear.app".to_owned(),
                identity_id: None,
            }],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: WorkspaceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, cfg.name);
        assert_eq!(back.pinned_tabs.len(), 1);
        assert_eq!(back.id, cfg.id);
    }

    #[test]
    fn workspace_state_last_active_tab() {
        let mut state = WorkspaceState::new(WorkspaceId::new(1));
        assert_eq!(state.last_active_tab, None);
        state.set_last_active(TabId::new(42));
        assert_eq!(state.last_active_tab, Some(TabId::new(42)));
    }

    #[test]
    fn workspace_state_tab_ordering() {
        let mut state = WorkspaceState::new(WorkspaceId::new(1));
        state.push_tab(TabId::new(1));
        state.push_tab(TabId::new(2));
        state.push_tab(TabId::new(3));
        assert_eq!(
            state.tab_order,
            vec![TabId::new(1), TabId::new(2), TabId::new(3)]
        );
        state.remove_tab(TabId::new(2));
        assert_eq!(state.tab_order, vec![TabId::new(1), TabId::new(3)]);
    }

    #[test]
    fn push_tab_is_idempotent() {
        let mut state = WorkspaceState::new(WorkspaceId::new(1));
        state.push_tab(TabId::new(1));
        state.push_tab(TabId::new(1));
        assert_eq!(state.tab_order.len(), 1);
    }

    #[test]
    fn remove_absent_tab_is_noop() {
        let mut state = WorkspaceState::new(WorkspaceId::new(1));
        state.push_tab(TabId::new(1));
        state.remove_tab(TabId::new(99));
        assert_eq!(state.tab_order.len(), 1);
    }

    #[test]
    fn workspace_state_roundtrip_json() {
        let mut state = WorkspaceState::new(WorkspaceId::new(7));
        state.push_tab(TabId::new(10));
        state.push_tab(TabId::new(20));
        state.set_last_active(TabId::new(10));
        state.slot_sizes.insert("left-sidebar".to_owned(), 280);

        let json = serde_json::to_string(&state).unwrap();
        let back: WorkspaceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, WorkspaceId::new(7));
        assert_eq!(back.last_active_tab, Some(TabId::new(10)));
        assert_eq!(back.tab_order, vec![TabId::new(10), TabId::new(20)]);
        assert_eq!(back.slot_sizes["left-sidebar"], 280);
    }
}
