//! Durable persistence of audit events via [`mote_storage`].
//!
//! Events are serialized as JSON and stored as key-value pairs in a reserved
//! [`mote_storage::Namespace`] identified by:
//!
//! - plugin name: `"__audit__"` (reserved; invalid as a user plugin name
//!   because it contains underscores, so no collision is possible)
//! - scope: [`mote_storage::IdentityScope::Global`]
//!
//! Each event is keyed by its sequence number formatted as a fixed-width
//! decimal string (`"0000000000000001"`, …). The sequence counter is loaded
//! from the store on construction so that flushes across process restarts
//! never collide.
//!
//! **This is a KV-only persistence approach.** It is simple and free of any
//! schema change to `mote-storage`, but has limitations:
//!
//! - Iteration over all keys requires listing them and parsing each one.
//! - There is no native SQL index for time-range or plugin-name queries.
//!
//! These limitations are acceptable at v0.1 because the integrity panel
//! performs post-hoc filtering in Rust over the deserialized result set.
//! A richer query API (native time/plugin index) would require a dedicated
//! audit table in `mote-storage`'s migration sequence — flag as a v0.2
//! candidate when query latency becomes observable.

use mote_storage::{IdentityScope, Namespace, Store};
use mote_types::PluginName;

use crate::error::AuditError;
use crate::event::AuditEvent;

/// Reserved plugin name for the audit namespace.
///
/// `"x-audit"` satisfies `PluginName` validation (lowercase alphanum + hyphen,
/// no leading/trailing/consecutive hyphens) and is deliberately unusual to
/// avoid collision with real plugin names.  The `x-` prefix signals a reserved
/// internal namespace by convention.
const AUDIT_PLUGIN_NAME: &str = "x-audit";

/// Fixed-width decimal key width for sequence numbers (supports 10^16 events).
const KEY_WIDTH: usize = 16;

/// Durable sink: serialises [`AuditEvent`]s and writes them to a
/// [`mote_storage::Namespace`].
#[derive(Debug)]
pub struct AuditSink {
    ns: Namespace,
    /// Monotonically increasing sequence number; the next event written will
    /// use this value, then it is incremented.
    next_seq: u64,
}

impl AuditSink {
    /// Opens (or re-joins) the audit namespace in `store`.
    ///
    /// Scans existing keys to find the highest sequence number so that a
    /// restarted process never overwrites previously flushed events.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] if the namespace cannot be read.
    ///
    /// # Panics
    ///
    /// This function will not panic in practice: the internal audit plugin
    /// name constant is a valid [`mote_types::PluginName`] by construction.
    pub fn open(store: &Store) -> Result<Self, AuditError> {
        let plugin = PluginName::new(AUDIT_PLUGIN_NAME)
            .expect("AUDIT_PLUGIN_NAME is a valid PluginName; this is a compile-time invariant");
        let ns = store.namespace(&plugin, IdentityScope::Global);
        let next_seq = Self::load_next_seq(&ns)?;
        Ok(Self { ns, next_seq })
    }

    /// Flushes `events` to the durable namespace in sequence-number order.
    ///
    /// Each event is serialized to JSON and stored under its sequence key.
    /// Returns the number of events successfully written.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] on the first serialization or storage failure.
    /// Events written before the failure are durable; events after are not.
    pub fn flush(&mut self, events: &[AuditEvent]) -> Result<usize, AuditError> {
        for event in events {
            let key = Self::seq_key(self.next_seq);
            let json = serde_json::to_vec(event)?;
            self.ns.set(&key, &json)?;
            self.next_seq += 1;
        }
        Ok(events.len())
    }

    /// Reads all durably stored events in sequence order.
    ///
    /// This performs a full scan of the namespace and deserializes every value.
    /// Suitable for integrity-panel "full history" queries; not for hot paths.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] if a storage read or deserialization fails.
    pub fn read_all(&self) -> Result<Vec<AuditEvent>, AuditError> {
        let mut keys = self.ns.list_keys()?;
        keys.sort_unstable();
        let mut out = Vec::with_capacity(keys.len());
        for key in &keys {
            if let Some(bytes) = self.ns.get(key)? {
                let ev: AuditEvent = serde_json::from_slice(&bytes)?;
                out.push(ev);
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn seq_key(seq: u64) -> String {
        format!("{seq:0>KEY_WIDTH$}")
    }

    fn load_next_seq(ns: &Namespace) -> Result<u64, AuditError> {
        let keys = ns.list_keys()?;
        if keys.is_empty() {
            return Ok(0);
        }
        // Keys are fixed-width decimal strings; the lexicographically largest
        // is also the numerically largest.
        let max_key = keys.iter().max().expect("keys is non-empty");
        let max_seq: u64 = max_key.parse().unwrap_or(0);
        Ok(max_seq + 1)
    }
}

#[cfg(test)]
mod tests {
    use mote_storage::Store;
    use mote_types::PluginName;

    use super::*;
    use crate::event::Decision;

    fn ev(plugin: &str, op: &str) -> AuditEvent {
        AuditEvent::new(PluginName::new(plugin).unwrap(), op, Decision::Allow)
    }

    fn open_sink() -> (Store, AuditSink) {
        let store = Store::open_in_memory().unwrap();
        let sink = AuditSink::open(&store).unwrap();
        (store, sink)
    }

    #[test]
    fn flush_and_read_all_round_trip() {
        let (_store, mut sink) = open_sink();
        let events = vec![
            ev("adblock", "net:intercept_request"),
            ev("vim-mode", "keys:bind"),
        ];
        let written = sink.flush(&events).unwrap();
        assert_eq!(written, 2);

        let all = sink.read_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].plugin.as_str(), "adblock");
        assert_eq!(all[1].plugin.as_str(), "vim-mode");
    }

    #[test]
    fn flush_is_sequential_across_calls() {
        let (_store, mut sink) = open_sink();
        sink.flush(&[ev("a", "op1")]).unwrap();
        sink.flush(&[ev("b", "op2")]).unwrap();
        let all = sink.read_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].plugin.as_str(), "a");
        assert_eq!(all[1].plugin.as_str(), "b");
    }

    #[test]
    fn seq_key_is_fixed_width() {
        assert_eq!(AuditSink::seq_key(0).len(), KEY_WIDTH);
        assert_eq!(AuditSink::seq_key(999).len(), KEY_WIDTH);
    }

    #[test]
    fn reopen_resumes_sequence() {
        let store = Store::open_in_memory().unwrap();

        // First sink writes two events.
        {
            let mut sink1 = AuditSink::open(&store).unwrap();
            sink1.flush(&[ev("a", "op"), ev("b", "op")]).unwrap();
        }

        // Second sink on the same store should resume from seq 2.
        let mut sink2 = AuditSink::open(&store).unwrap();
        assert_eq!(sink2.next_seq, 2);
        sink2.flush(&[ev("c", "op")]).unwrap();

        let all = sink2.read_all().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].plugin.as_str(), "c");
    }

    #[test]
    fn empty_read_all_returns_empty() {
        let (_store, sink) = open_sink();
        assert!(sink.read_all().unwrap().is_empty());
    }
}
