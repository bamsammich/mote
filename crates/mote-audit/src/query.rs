//! Query API for the Browser Integrity Panel.
//!
//! [`QueryHandle`] is a cheap-to-clone handle that grants read access to the
//! audit log's in-memory ring buffer and durable storage sink.  All ring-buffer
//! queries are served under a single `Mutex` guard; the lock is held only for
//! the duration of the copy, so contention is negligible.
//!
//! # Durable queries
//!
//! Queries that need history beyond the ring buffer — e.g. "all events in the
//! last 24 h" — call [`QueryHandle::history`], which reads from the
//! [`mote_storage`] namespace.  This involves disk I/O and is not suitable for
//! real-time use; it is intended for the integrity panel's "history" tab.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mote_types::PluginName;

use crate::AuditError;
use crate::event::{AuditEvent, Decision};
use crate::ring::RingBuffer;
use crate::sink::AuditSink;

/// A shared, read-only view into the audit log's ring buffer and durable sink.
///
/// Obtain one via [`AuditLog::query`](crate::AuditLog::query).
/// Cheap to clone — all clones share the same underlying state.
#[derive(Debug, Clone)]
pub struct QueryHandle {
    ring: Arc<Mutex<RingBuffer>>,
    sink: Arc<Mutex<AuditSink>>,
}

impl QueryHandle {
    pub(crate) const fn new(ring: Arc<Mutex<RingBuffer>>, sink: Arc<Mutex<AuditSink>>) -> Self {
        Self { ring, sink }
    }

    /// Returns at most `limit` recent events from the ring buffer,
    /// oldest-first.
    ///
    /// This is the primary query for the integrity panel's live view — served
    /// entirely from memory, sub-microsecond.
    ///
    /// # Panics
    ///
    /// Panics if the internal ring buffer mutex is poisoned (indicates an
    /// audit-thread panic, which is already a fatal condition).
    #[must_use]
    pub fn recent(&self, limit: usize) -> Vec<AuditEvent> {
        self.ring
            .lock()
            .expect("ring buffer mutex poisoned")
            .recent(limit)
    }

    /// Returns recent events from the ring buffer filtered to `plugin`,
    /// oldest-first.
    ///
    /// At most `limit` events are returned after filtering.
    ///
    /// # Panics
    ///
    /// Panics if the internal ring buffer mutex is poisoned.
    #[must_use]
    pub fn recent_for_plugin(&self, plugin: &PluginName, limit: usize) -> Vec<AuditEvent> {
        self.ring
            .lock()
            .expect("ring buffer mutex poisoned")
            .recent(usize::MAX)
            .into_iter()
            .filter(|ev| &ev.plugin == plugin)
            .take(limit)
            .collect()
    }

    /// Returns at most `limit` recent [`Decision::Deny`] events from the ring
    /// buffer, oldest-first.
    ///
    /// # Panics
    ///
    /// Panics if the internal ring buffer mutex is poisoned.
    #[must_use]
    pub fn recent_denials(&self, limit: usize) -> Vec<AuditEvent> {
        self.ring
            .lock()
            .expect("ring buffer mutex poisoned")
            .recent(usize::MAX)
            .into_iter()
            .filter(|ev| ev.decision == Decision::Deny)
            .take(limit)
            .collect()
    }

    /// Returns the total number of events recorded per plugin across the
    /// current ring buffer contents.
    ///
    /// Note: counts reflect only what is in the ring buffer right now. For
    /// all-time counts across flushed history, use [`history`](Self::history)
    /// and aggregate manually.
    ///
    /// # Panics
    ///
    /// Panics if the internal ring buffer mutex is poisoned.
    #[must_use]
    pub fn counts_per_plugin(&self) -> HashMap<PluginName, usize> {
        let events = self
            .ring
            .lock()
            .expect("ring buffer mutex poisoned")
            .recent(usize::MAX);
        let mut map = HashMap::new();
        for ev in events {
            *map.entry(ev.plugin).or_insert(0) += 1;
        }
        map
    }

    /// Returns the number of [`Decision::Allow`] events per plugin across the
    /// current ring buffer contents.
    ///
    /// Only events with `decision == Allow` are counted; denials are excluded.
    /// Plugins whose every event was denied will not appear in the returned map.
    ///
    /// Note: counts reflect only what is in the ring buffer right now.
    ///
    /// # Panics
    ///
    /// Panics if the internal ring buffer mutex is poisoned.
    #[must_use]
    pub fn allowed_counts_per_plugin(&self) -> HashMap<PluginName, usize> {
        let events = self
            .ring
            .lock()
            .expect("ring buffer mutex poisoned")
            .recent(usize::MAX);
        let mut map = HashMap::new();
        for ev in events.into_iter().filter(|e| e.decision == Decision::Allow) {
            *map.entry(ev.plugin).or_insert(0) += 1;
        }
        map
    }

    /// Returns the number of [`Decision::Deny`] events per plugin across the
    /// current ring buffer contents.
    ///
    /// Only events with `decision == Deny` are counted; allowed events are
    /// excluded. Plugins with no denials will not appear in the returned map.
    ///
    /// Note: counts reflect only what is in the ring buffer right now.
    ///
    /// # Panics
    ///
    /// Panics if the internal ring buffer mutex is poisoned.
    #[must_use]
    pub fn denied_counts_per_plugin(&self) -> HashMap<PluginName, usize> {
        let events = self
            .ring
            .lock()
            .expect("ring buffer mutex poisoned")
            .recent(usize::MAX);
        let mut map = HashMap::new();
        for ev in events.into_iter().filter(|e| e.decision == Decision::Deny) {
            *map.entry(ev.plugin).or_insert(0) += 1;
        }
        map
    }

    /// Reads **all** durably flushed events from the `mote-storage` namespace
    /// in sequence order.
    ///
    /// This performs disk I/O (reading from `SQLite` via `mote-storage`) and
    /// should not be called on the hot path. It is intended for the integrity
    /// panel's history view.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] if the storage read or deserialization fails.
    ///
    /// # Panics
    ///
    /// Panics if the sink mutex is poisoned.
    pub fn history(&self) -> Result<Vec<AuditEvent>, AuditError> {
        let sink = self.sink.lock().expect("sink mutex poisoned");
        sink.read_all()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use mote_storage::Store;
    use mote_types::PluginName;

    use super::*;
    use crate::ring::RingBuffer;
    use crate::sink::AuditSink;

    fn plugin(name: &str) -> PluginName {
        PluginName::new(name).unwrap()
    }

    fn ev(plugin_name: &str, op: &str, decision: Decision) -> AuditEvent {
        AuditEvent::new(plugin(plugin_name), op, decision)
    }

    fn make_query(cap: usize, events: Vec<AuditEvent>) -> (QueryHandle, Store) {
        let mut ring = RingBuffer::new(cap);
        for e in events {
            ring.push(e);
        }
        let store = Store::open_in_memory().unwrap();
        let sink = AuditSink::open(&store).unwrap();
        let handle = QueryHandle::new(Arc::new(Mutex::new(ring)), Arc::new(Mutex::new(sink)));
        (handle, store)
    }

    #[test]
    fn recent_returns_oldest_first() {
        let (q, _) = make_query(
            10,
            vec![
                ev("adblock", "op1", Decision::Allow),
                ev("vim-mode", "op2", Decision::Allow),
            ],
        );
        let r = q.recent(10);
        assert_eq!(r[0].plugin.as_str(), "adblock");
        assert_eq!(r[1].plugin.as_str(), "vim-mode");
    }

    #[test]
    fn recent_for_plugin_filters_correctly() {
        let (q, _) = make_query(
            10,
            vec![
                ev("adblock", "net:intercept_request", Decision::Allow),
                ev("vim-mode", "keys:bind", Decision::Allow),
                ev("adblock", "net:intercept_request", Decision::Deny),
            ],
        );
        let r = q.recent_for_plugin(&plugin("adblock"), 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| e.plugin.as_str() == "adblock"));
    }

    #[test]
    fn recent_denials_filters_correctly() {
        let (q, _) = make_query(
            10,
            vec![
                ev("adblock", "op", Decision::Allow),
                ev("vim-mode", "op", Decision::Deny),
                ev("history", "op", Decision::Deny),
                ev("bookmarks", "op", Decision::Allow),
            ],
        );
        let denials = q.recent_denials(10);
        assert_eq!(denials.len(), 2);
        assert!(denials.iter().all(|e| e.decision == Decision::Deny));
    }

    #[test]
    fn counts_per_plugin_aggregates_correctly() {
        let (q, _) = make_query(
            10,
            vec![
                ev("adblock", "op", Decision::Allow),
                ev("adblock", "op", Decision::Allow),
                ev("vim-mode", "op", Decision::Allow),
            ],
        );
        let counts = q.counts_per_plugin();
        assert_eq!(counts[&plugin("adblock")], 2);
        assert_eq!(counts[&plugin("vim-mode")], 1);
    }

    #[test]
    fn history_reads_flushed_events() {
        let store = Store::open_in_memory().unwrap();
        let mut sink = AuditSink::open(&store).unwrap();
        sink.flush(&[ev("adblock", "op", Decision::Allow)]).unwrap();

        let ring = RingBuffer::new(10);
        let q = QueryHandle::new(Arc::new(Mutex::new(ring)), Arc::new(Mutex::new(sink)));
        let hist = q.history().unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].plugin.as_str(), "adblock");
    }

    #[test]
    fn recent_limit_respected() {
        let events: Vec<_> = (0..10)
            .map(|i| ev("adblock", &format!("op-{i}"), Decision::Allow))
            .collect();
        let (q, _) = make_query(20, events);
        assert_eq!(q.recent(3).len(), 3);
    }

    #[test]
    fn allowed_counts_per_plugin_counts_only_allowed() {
        let (q, _) = make_query(
            20,
            vec![
                ev("adblock", "op", Decision::Allow),
                ev("adblock", "op", Decision::Allow),
                ev("adblock", "op", Decision::Deny),
                ev("vim-mode", "op", Decision::Deny),
                ev("history", "op", Decision::Allow),
            ],
        );
        let counts = q.allowed_counts_per_plugin();
        // adblock: 2 allowed (1 denial excluded)
        assert_eq!(counts[&plugin("adblock")], 2);
        // history: 1 allowed
        assert_eq!(counts[&plugin("history")], 1);
        // vim-mode: only denials — must be absent from the map
        assert!(!counts.contains_key(&plugin("vim-mode")));
    }

    #[test]
    fn denied_counts_per_plugin_counts_only_denied() {
        let (q, _) = make_query(
            20,
            vec![
                ev("adblock", "op", Decision::Deny),
                ev("adblock", "op", Decision::Deny),
                ev("adblock", "op", Decision::Allow),
                ev("vim-mode", "op", Decision::Allow),
                ev("history", "op", Decision::Deny),
            ],
        );
        let counts = q.denied_counts_per_plugin();
        // adblock: 2 denied (1 allow excluded)
        assert_eq!(counts[&plugin("adblock")], 2);
        // history: 1 denied
        assert_eq!(counts[&plugin("history")], 1);
        // vim-mode: only allows — must be absent from the map
        assert!(!counts.contains_key(&plugin("vim-mode")));
    }
}
