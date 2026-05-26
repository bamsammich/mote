//! The session axis — what's currently open.
//!
//! [`Session`] holds the canonical, in-memory runtime state for all tabs
//! belonging to one identity. It is populated from `SQLite` on launch
//! ([`Session::restore`]) and flushed back continuously ([`Session::flush`]).
//!
//! # Tab-picker ranking
//!
//! [`Session::tab_picker_ranked`] returns the tabs for a workspace ordered per
//! `DESIGN.md` §The workspace tab picker:
//!
//! 1. Active, non-discarded, pinned tabs (highest priority).
//! 2. Active, non-discarded, non-pinned tabs (most-recently-visited first).
//! 3. Active, discarded tabs.
//! 4. Held hidden tabs (pinned held above non-pinned held).
//! 5. Un-held hidden tabs (most-recently-released first).
//! 6. Closed tabs are excluded from the picker entirely.
//!
//! # Persistence
//!
//! The shell drives timing. Call [`Session::flush`] roughly every 5 seconds
//! and on clean shutdown. Call [`Session::restore`] on launch. A hard crash
//! loses at most the last flush interval — this is the "crash recovery ==
//! clean exit" invariant.

use std::collections::HashMap;
use std::time::SystemTime;

use mote_storage::{IdentityScope, Namespace};
use mote_types::{IdentityId, PluginName, TabId, WorkspaceId};

use crate::error::SessionError;
use crate::tab::{Tab, TabState};

/// How eagerly to hydrate tabs from persisted state on restore.
///
/// The shell uses this to decide which tabs to materialize as placeholders
/// immediately vs. deferring until the workspace is switched to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorationMode {
    /// Materialize as placeholders immediately (active workspace).
    Eager,
    /// Defer materialization until the workspace is switched to.
    Lazy,
}

/// Runtime session state for one identity.
///
/// Holds all tabs across all workspaces, the currently-active workspace, and
/// a monotonic counter for assigning new tab IDs. One `Session` instance per
/// identity — two `Session` instances for different identities are completely
/// independent (no shared state).
#[derive(Debug)]
pub struct Session {
    /// The identity this session belongs to.
    pub identity_id: IdentityId,
    /// All tabs, keyed by stable [`TabId`].
    tabs: HashMap<TabId, Tab>,
    /// Monotonically increasing counter for assigning new [`TabId`]s.
    ///
    /// Persisted through flush/restore to guarantee globally-unique tab IDs
    /// across restarts.
    next_tab_id: u64,
    /// The currently-active workspace.
    pub active_workspace: WorkspaceId,
}

impl Session {
    /// Creates a new, empty session for the given identity and initial
    /// active workspace.
    #[must_use]
    pub fn new(identity_id: IdentityId, initial_workspace: WorkspaceId) -> Self {
        Self {
            identity_id,
            tabs: HashMap::new(),
            next_tab_id: 1,
            active_workspace: initial_workspace,
        }
    }

    // ── Tab management ────────────────────────────────────────────────────────

    /// Allocates a new active tab with the given URL in the given workspace and
    /// returns its [`TabId`].
    pub fn add_tab(&mut self, url: String, workspace_id: WorkspaceId) -> TabId {
        let id = TabId::new(self.next_tab_id);
        self.next_tab_id += 1;
        let tab = Tab::new(id, url, workspace_id);
        self.tabs.insert(id, tab);
        id
    }

    /// Returns a reference to the tab with the given ID, or `None` if absent.
    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.get(&id)
    }

    /// Returns a mutable reference to the tab with the given ID, or `None` if
    /// absent.
    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.get_mut(&id)
    }

    /// Transitions an active tab to hidden.
    ///
    /// `released_at` is the moment the window closed (or `⌘⇧H` was pressed).
    /// The tab picker uses this timestamp for recency ranking.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TabNotFound`] if no tab with `id` exists.
    pub fn hide_tab(&mut self, id: TabId, released_at: SystemTime) -> Result<(), SessionError> {
        let tab = self
            .tabs
            .get_mut(&id)
            .ok_or(SessionError::TabNotFound(id))?;
        tab.hide(released_at);
        Ok(())
    }

    /// Transitions an active tab to closed (`Ctrl+W` / middle-click).
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TabNotFound`] if no tab with `id` exists.
    pub fn close_tab(&mut self, id: TabId) -> Result<(), SessionError> {
        let tab = self
            .tabs
            .get_mut(&id)
            .ok_or(SessionError::TabNotFound(id))?;
        tab.close();
        Ok(())
    }

    /// Reveals a hidden tab into the active window.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TabNotFound`] if no tab with `id` exists.
    pub fn reveal_tab(&mut self, id: TabId) -> Result<(), SessionError> {
        let tab = self
            .tabs
            .get_mut(&id)
            .ok_or(SessionError::TabNotFound(id))?;
        tab.reveal();
        Ok(())
    }

    /// Sets the active workspace.
    pub const fn set_active_workspace(&mut self, workspace_id: WorkspaceId) {
        self.active_workspace = workspace_id;
    }

    // ── Tab-picker ranking ────────────────────────────────────────────────────

    /// Returns tabs for `workspace_id` in tab-picker ranking order.
    ///
    /// Ranking per `DESIGN.md` §The workspace tab picker:
    ///
    /// 1. Active, non-discarded, pinned tabs.
    /// 2. Active, non-discarded, non-pinned tabs (most-recently-visited first).
    /// 3. Active, discarded tabs.
    /// 4. Held hidden tabs (pinned above non-pinned).
    /// 5. Un-held hidden tabs (most-recently-released first).
    ///
    /// Closed tabs are excluded entirely.
    #[must_use]
    pub fn tab_picker_ranked(&self, workspace_id: WorkspaceId) -> Vec<&Tab> {
        let mut tabs: Vec<&Tab> = self
            .tabs
            .values()
            .filter(|t| t.workspace_id == workspace_id && t.state != TabState::Closed)
            .collect();

        tabs.sort_by(|a, b| {
            picker_rank(a)
                .cmp(&picker_rank(b))
                .then_with(|| picker_secondary(a, b))
        });
        tabs
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    /// Flushes the current session state to the storage namespace.
    ///
    /// This is the **only** write path. The shell calls this on a ~5 s interval
    /// and on clean shutdown. A hard crash loses at most the last flush interval
    /// — this is the crash-recovery-equals-clean-exit invariant.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if serialization or storage fails.
    pub fn flush(&self, ns: &Namespace) -> Result<(), SessionError> {
        // Active workspace.
        let aw = serde_json::to_vec(&self.active_workspace.get()).map_err(|e| {
            SessionError::Corrupt {
                key: "active_workspace".into(),
                source: e,
            }
        })?;
        ns.set("active_workspace", &aw)?;

        // Next tab ID counter (keeps IDs unique across restarts).
        let next = serde_json::to_vec(&self.next_tab_id).map_err(|e| SessionError::Corrupt {
            key: "next_tab_id".into(),
            source: e,
        })?;
        ns.set("next_tab_id", &next)?;

        // Tab ID manifest.
        let ids: Vec<u64> = self.tabs.keys().map(|t| t.get()).collect();
        let ids_bytes = serde_json::to_vec(&ids).map_err(|e| SessionError::Corrupt {
            key: "tab_ids".into(),
            source: e,
        })?;
        ns.set("tab_ids", &ids_bytes)?;

        // Each tab's full state.
        for (id, tab) in &self.tabs {
            let key = format!("tab:{}", id.get());
            let bytes = serde_json::to_vec(tab).map_err(|e| SessionError::Corrupt {
                key: key.clone(),
                source: e,
            })?;
            ns.set(&key, &bytes)?;
        }

        Ok(())
    }

    /// Restores a session from the storage namespace.
    ///
    /// Returns an empty session if no state has been flushed yet (first launch).
    /// On success the session is fully populated; the shell is responsible for
    /// deciding which tabs to hydrate eagerly vs. lazily using
    /// [`Session::restoration_mode`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if stored data is corrupt or the storage
    /// operation fails.
    pub fn restore(ns: &Namespace, identity_id: IdentityId) -> Result<Self, SessionError> {
        // Active workspace.
        let active_workspace = match ns.get("active_workspace")? {
            None => {
                // No persisted state → fresh session.
                return Ok(Self::new(identity_id, WorkspaceId::new(0)));
            }
            Some(bytes) => {
                let raw: u64 =
                    serde_json::from_slice(&bytes).map_err(|e| SessionError::Corrupt {
                        key: "active_workspace".into(),
                        source: e,
                    })?;
                WorkspaceId::new(raw)
            }
        };

        // Next tab ID counter.
        let next_tab_id = match ns.get("next_tab_id")? {
            None => 1u64,
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| SessionError::Corrupt {
                key: "next_tab_id".into(),
                source: e,
            })?,
        };

        // Tab ID manifest.
        let ids: Vec<u64> = match ns.get("tab_ids")? {
            None => Vec::new(),
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| SessionError::Corrupt {
                key: "tab_ids".into(),
                source: e,
            })?,
        };

        let mut tabs = HashMap::with_capacity(ids.len());
        for raw_id in ids {
            let id = TabId::new(raw_id);
            let key = format!("tab:{raw_id}");
            if let Some(bytes) = ns.get(&key)? {
                let tab: Tab =
                    serde_json::from_slice(&bytes).map_err(|e| SessionError::Corrupt {
                        key: key.clone(),
                        source: e,
                    })?;
                tabs.insert(id, tab);
            }
        }

        Ok(Self {
            identity_id,
            tabs,
            next_tab_id,
            active_workspace,
        })
    }

    /// Returns [`RestorationMode::Eager`] if `workspace_id` matches the
    /// active workspace, otherwise [`RestorationMode::Lazy`].
    ///
    /// The shell uses this to decide which tabs to materialize as placeholders
    /// immediately vs. deferring until the workspace is switched to.
    #[must_use]
    pub fn restoration_mode(&self, workspace_id: WorkspaceId) -> RestorationMode {
        if workspace_id == self.active_workspace {
            RestorationMode::Eager
        } else {
            RestorationMode::Lazy
        }
    }

    /// Returns the plugin name used to open the session namespace.
    ///
    /// Convenience helper so callers don't need to hard-code the plugin name
    /// string. The shell uses this when constructing the [`Namespace`] to pass
    /// to [`Session::flush`] and [`Session::restore`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Internal`] if the plugin name is somehow invalid
    /// (should never happen in practice — the name is a compile-time constant).
    pub fn plugin_name() -> Result<PluginName, SessionError> {
        PluginName::new("mote-session")
            .map_err(|e| SessionError::Internal(format!("invalid plugin name: {e}")))
    }

    /// Convenience: opens the per-identity session namespace from `store`.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// let plugin = PluginName::new("mote-session")?;
    /// store.namespace(&plugin, IdentityScope::PerIdentity(identity_id))
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Internal`] if the plugin name constant is
    /// somehow invalid.
    pub fn open_namespace(
        store: &mote_storage::Store,
        identity_id: IdentityId,
    ) -> Result<Namespace, SessionError> {
        let plugin = Self::plugin_name()?;
        Ok(store.namespace(&plugin, IdentityScope::PerIdentity(identity_id)))
    }
}

/// Primary sort key for tab-picker ordering (lower = higher in the list).
fn picker_rank(tab: &Tab) -> u8 {
    match tab.state {
        TabState::Active if !tab.is_discarded && tab.is_pinned => 0,
        TabState::Active if !tab.is_discarded => 1,
        TabState::Active => 2, // discarded active tabs
        TabState::Hidden if tab.hidden_meta.as_ref().is_some_and(|m| m.hold) && tab.is_pinned => 3,
        TabState::Hidden if tab.hidden_meta.as_ref().is_some_and(|m| m.hold) => 4,
        TabState::Hidden if tab.is_pinned => 5,
        TabState::Hidden => 6,
        TabState::Closed => 7,
    }
}

/// Secondary sort: recency (most-recent first).
///
/// For active tabs: most-recently-visited first.
/// For hidden tabs: most-recently-released first.
fn picker_secondary(a: &Tab, b: &Tab) -> std::cmp::Ordering {
    use std::time::UNIX_EPOCH;

    let ts = |tab: &Tab| -> u64 {
        match tab.state {
            TabState::Active => tab
                .last_visited
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs()),
            TabState::Hidden => tab
                .hidden_meta
                .as_ref()
                .and_then(|m| m.released_at.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs()),
            TabState::Closed => 0,
        }
    };

    // Higher timestamp = more recent = comes first (descending).
    ts(b).cmp(&ts(a))
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use mote_types::{IdentityId, WorkspaceId};

    use super::*;
    use crate::tab::TabState;

    fn make_session() -> Session {
        Session::new(IdentityId::new(0), WorkspaceId::new(1))
    }

    // ── Session tab management ────────────────────────────────────────────────

    #[test]
    fn add_and_get_tab() {
        let mut s = make_session();
        let id = s.add_tab("https://rust-lang.org".to_owned(), WorkspaceId::new(1));
        assert!(s.tab(id).is_some());
        assert_eq!(s.tab(id).unwrap().url, "https://rust-lang.org");
    }

    #[test]
    fn hide_tab_changes_state() {
        let mut s = make_session();
        let id = s.add_tab("https://a.com".to_owned(), WorkspaceId::new(1));
        s.hide_tab(id, SystemTime::now()).unwrap();
        assert_eq!(s.tab(id).unwrap().state, TabState::Hidden);
    }

    #[test]
    fn close_tab_changes_state() {
        let mut s = make_session();
        let id = s.add_tab("https://b.com".to_owned(), WorkspaceId::new(1));
        s.close_tab(id).unwrap();
        assert_eq!(s.tab(id).unwrap().state, TabState::Closed);
    }

    #[test]
    fn reveal_hidden_tab() {
        let mut s = make_session();
        let id = s.add_tab("https://c.com".to_owned(), WorkspaceId::new(1));
        s.hide_tab(id, SystemTime::now()).unwrap();
        s.reveal_tab(id).unwrap();
        assert_eq!(s.tab(id).unwrap().state, TabState::Active);
    }

    #[test]
    fn hide_nonexistent_tab_returns_error() {
        let mut s = make_session();
        let err = s.hide_tab(TabId::new(999), SystemTime::now()).unwrap_err();
        assert!(matches!(err, SessionError::TabNotFound(_)));
    }

    #[test]
    fn per_identity_isolation_different_session_instances() {
        // Two Session instances for different identities are completely separate:
        // no shared in-memory state, no shared tab storage.
        let mut s1 = Session::new(IdentityId::new(1), WorkspaceId::new(1));
        let mut s2 = Session::new(IdentityId::new(2), WorkspaceId::new(1));
        let id1 = s1.add_tab("https://identity1.com".to_owned(), WorkspaceId::new(1));
        // Add two tabs to s2 so its second tab gets a different ID than s1's tab.
        s2.add_tab(
            "https://identity2-first.com".to_owned(),
            WorkspaceId::new(1),
        );
        let id2 = s2.add_tab(
            "https://identity2-second.com".to_owned(),
            WorkspaceId::new(1),
        );

        // s1 has only its own tab; s2's second tab ID does not exist in s1.
        assert!(s1.tab(id1).is_some());
        assert!(s1.tab(id2).is_none());
        // s2 has its own tabs; s1's tab URL is not reachable from s2 at id1.
        assert!(s2.tab(id1).is_none() || s2.tab(id1).unwrap().url != "https://identity1.com");
    }

    #[test]
    fn active_workspace_tracking() {
        let mut s = make_session();
        assert_eq!(s.active_workspace, WorkspaceId::new(1));
        s.set_active_workspace(WorkspaceId::new(2));
        assert_eq!(s.active_workspace, WorkspaceId::new(2));
    }

    // ── Tab-picker ranking ────────────────────────────────────────────────────

    #[test]
    fn tab_picker_active_before_hidden() {
        let mut s = Session::new(IdentityId::new(0), WorkspaceId::new(1));
        let active_id = s.add_tab("https://active.com".to_owned(), WorkspaceId::new(1));
        let hidden_id = s.add_tab("https://hidden.com".to_owned(), WorkspaceId::new(1));
        s.hide_tab(hidden_id, SystemTime::now()).unwrap();

        let ranked = s.tab_picker_ranked(WorkspaceId::new(1));
        let positions: HashMap<_, _> = ranked.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        assert!(positions[&active_id] < positions[&hidden_id]);
    }

    #[test]
    fn tab_picker_older_hidden_ranked_lower() {
        use std::time::Duration;

        let mut s = Session::new(IdentityId::new(0), WorkspaceId::new(1));
        let newer_id = s.add_tab("https://newer.com".to_owned(), WorkspaceId::new(1));
        let older_id = s.add_tab("https://older.com".to_owned(), WorkspaceId::new(1));

        let now = SystemTime::now();
        s.hide_tab(newer_id, now - Duration::from_hours(1)).unwrap(); // 1h ago
        s.hide_tab(older_id, now - Duration::from_hours(168))
            .unwrap(); // 7d ago

        let ranked = s.tab_picker_ranked(WorkspaceId::new(1));
        let positions: HashMap<_, _> = ranked.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        assert!(positions[&newer_id] < positions[&older_id]);
    }

    #[test]
    fn tab_picker_excludes_other_workspaces() {
        let mut s = Session::new(IdentityId::new(0), WorkspaceId::new(1));
        let ws1_tab = s.add_tab("https://ws1.com".to_owned(), WorkspaceId::new(1));
        let ws2_tab = s.add_tab("https://ws2.com".to_owned(), WorkspaceId::new(2));

        let ranked = s.tab_picker_ranked(WorkspaceId::new(1));
        let ids: Vec<_> = ranked.iter().map(|t| t.id).collect();
        assert!(ids.contains(&ws1_tab));
        assert!(!ids.contains(&ws2_tab));
    }

    #[test]
    fn tab_picker_excludes_closed_tabs() {
        let mut s = make_session();
        let open_id = s.add_tab("https://open.com".to_owned(), WorkspaceId::new(1));
        let closed_id = s.add_tab("https://closed.com".to_owned(), WorkspaceId::new(1));
        s.close_tab(closed_id).unwrap();

        let ranked = s.tab_picker_ranked(WorkspaceId::new(1));
        let ids: Vec<_> = ranked.iter().map(|t| t.id).collect();
        assert!(ids.contains(&open_id));
        assert!(!ids.contains(&closed_id));
    }

    #[test]
    fn restoration_mode_eager_for_active_workspace() {
        let s = Session::new(IdentityId::new(0), WorkspaceId::new(1));
        assert_eq!(
            s.restoration_mode(WorkspaceId::new(1)),
            RestorationMode::Eager
        );
        assert_eq!(
            s.restoration_mode(WorkspaceId::new(2)),
            RestorationMode::Lazy
        );
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    fn session_ns(store: &mote_storage::Store, identity: IdentityId) -> Namespace {
        let plugin = PluginName::new("mote-session").unwrap();
        store.namespace(&plugin, IdentityScope::PerIdentity(identity))
    }

    #[test]
    fn flush_restore_round_trip() {
        let store = mote_storage::Store::open_in_memory().unwrap();
        let identity = IdentityId::new(1);
        let ns = session_ns(&store, identity);

        let mut session = Session::new(identity, WorkspaceId::new(1));
        let tab1 = session.add_tab("https://rust-lang.org".to_owned(), WorkspaceId::new(1));
        let tab2 = session.add_tab("https://crates.io".to_owned(), WorkspaceId::new(1));
        session.tab_mut(tab1).unwrap().title = Some("Rust".to_owned());
        session.tab_mut(tab1).unwrap().scroll_y = 256;

        session.flush(&ns).unwrap();

        let restored = Session::restore(&ns, identity).unwrap();
        assert_eq!(restored.active_workspace, WorkspaceId::new(1));
        assert!(restored.tab(tab1).is_some());
        assert!(restored.tab(tab2).is_some());
        assert_eq!(restored.tab(tab1).unwrap().title, Some("Rust".to_owned()));
        assert_eq!(restored.tab(tab1).unwrap().scroll_y, 256);
    }

    #[test]
    fn hidden_tab_survives_round_trip() {
        let store = mote_storage::Store::open_in_memory().unwrap();
        let identity = IdentityId::new(1);
        let ns = session_ns(&store, identity);

        let mut session = Session::new(identity, WorkspaceId::new(1));
        let tab = session.add_tab("https://hidden.com".to_owned(), WorkspaceId::new(1));
        session.hide_tab(tab, SystemTime::now()).unwrap();
        session.tab_mut(tab).unwrap().set_hold(true);

        session.flush(&ns).unwrap();

        let restored = Session::restore(&ns, identity).unwrap();
        let rt = restored.tab(tab).unwrap();
        assert_eq!(rt.state, TabState::Hidden);
        assert!(rt.hidden_meta.as_ref().unwrap().hold);
    }

    #[test]
    fn crash_recovery_equals_clean_exit() {
        // Simulate: write session, "crash" (drop without explicit flush),
        // reopen, restore. State is as of the last flush.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.db");
        let identity = IdentityId::new(99);

        let tab_id;
        {
            let store = mote_storage::Store::open(&path).unwrap();
            let ns = session_ns(&store, identity);
            let mut session = Session::new(identity, WorkspaceId::new(1));
            tab_id = session.add_tab("https://example.com".to_owned(), WorkspaceId::new(1));
            session.tab_mut(tab_id).unwrap().title = Some("Example".to_owned());
            session.flush(&ns).unwrap(); // "last flush before crash"
            // store is dropped here without a clean-shutdown flush
        }

        {
            let store = mote_storage::Store::open(&path).unwrap();
            let ns = session_ns(&store, identity);
            let restored = Session::restore(&ns, identity).unwrap();
            assert!(restored.tab(tab_id).is_some());
            assert_eq!(
                restored.tab(tab_id).unwrap().title,
                Some("Example".to_owned())
            );
        }
    }

    #[test]
    fn per_identity_isolation_in_storage() {
        // Per-identity namespaces are completely isolated at the SQL layer.
        // Verify that restoring session A never surfaces a URL written by B.
        const URL_A: &str = "https://identity-a-only.com";
        const URL_B: &str = "https://identity-b-only.com";

        let store = mote_storage::Store::open_in_memory().unwrap();
        let id_a = IdentityId::new(1);
        let id_b = IdentityId::new(2);
        let ns_a = session_ns(&store, id_a);
        let ns_b = session_ns(&store, id_b);

        let mut s_a = Session::new(id_a, WorkspaceId::new(1));
        // Add two tabs so s_a gets tab IDs 1 and 2.
        s_a.add_tab(
            "https://identity-a-first.com".to_owned(),
            WorkspaceId::new(1),
        );
        let tab_a = s_a.add_tab(URL_A.to_owned(), WorkspaceId::new(1));
        s_a.flush(&ns_a).unwrap();

        let mut s_b = Session::new(id_b, WorkspaceId::new(1));
        // s_b starts its own counter from 1; its tab IDs do not overlap with s_a's
        // per-identity namespace.
        let tab_b = s_b.add_tab(URL_B.to_owned(), WorkspaceId::new(1));
        s_b.flush(&ns_b).unwrap();

        // Restore A — must contain URL_A and must not contain URL_B.
        let ra = Session::restore(&ns_a, id_a).unwrap();
        assert_eq!(ra.tab(tab_a).map(|t| t.url.as_str()), Some(URL_A));
        // B's first tab ID (1) exists in A's restored data, but its URL must be A's.
        let a_tab_at_b_id = ra.tab(tab_b);
        assert!(
            a_tab_at_b_id.is_none() || a_tab_at_b_id.unwrap().url != URL_B,
            "session A must not expose identity B's URL"
        );

        // Restore B — must contain URL_B and must not contain URL_A.
        let rb = Session::restore(&ns_b, id_b).unwrap();
        assert_eq!(rb.tab(tab_b).map(|t| t.url.as_str()), Some(URL_B));
        let b_tab_at_a_id = rb.tab(tab_a);
        assert!(
            b_tab_at_a_id.is_none() || b_tab_at_a_id.unwrap().url != URL_A,
            "session B must not expose identity A's URL"
        );
    }

    #[test]
    fn restore_empty_session_succeeds() {
        let store = mote_storage::Store::open_in_memory().unwrap();
        let ns = session_ns(&store, IdentityId::new(0));
        // No prior flush — restore should yield an empty session.
        let session = Session::restore(&ns, IdentityId::new(0)).unwrap();
        assert_eq!(session.tab_picker_ranked(WorkspaceId::new(1)).len(), 0);
    }

    #[test]
    fn open_namespace_helper_produces_working_namespace() {
        let store = mote_storage::Store::open_in_memory().unwrap();
        let identity = IdentityId::new(42);
        let ns = Session::open_namespace(&store, identity).unwrap();

        let mut session = Session::new(identity, WorkspaceId::new(1));
        session.add_tab("https://test.com".to_owned(), WorkspaceId::new(1));
        session.flush(&ns).unwrap();

        let restored = Session::restore(&ns, identity).unwrap();
        assert_eq!(restored.tab_picker_ranked(WorkspaceId::new(1)).len(), 1);
    }
}
