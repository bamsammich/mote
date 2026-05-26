//! Fixed-capacity in-memory ring buffer for recent [`AuditEvent`]s.
//!
//! The ring buffer is a classic circular queue backed by a `Vec` allocated
//! once at construction time. When the buffer is full the oldest entry is
//! overwritten by the newest — this is the intentional eviction policy for
//! the "recent N" query surface.
//!
//! The ring buffer is **not** thread-safe by itself; it is owned exclusively
//! by the audit thread and accessed through the shared [`QueryHandle`] under a
//! [`std::sync::Mutex`](crate::auditor).

use crate::event::AuditEvent;

/// A fixed-capacity ring buffer that evicts the oldest entry on overflow.
///
/// Capacity must be at least 1. Events are stored in insertion order; iteration
/// yields oldest-first.
#[derive(Debug)]
pub struct RingBuffer {
    buf: Vec<Option<AuditEvent>>,
    /// Index of the slot where the *next* write will land.
    head: usize,
    /// Number of valid entries currently stored (saturates at `capacity`).
    len: usize,
}

impl RingBuffer {
    /// Creates a new ring buffer with the given `capacity`.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be > 0");
        Self {
            buf: (0..capacity).map(|_| None).collect(),
            head: 0,
            len: 0,
        }
    }

    /// Returns the maximum number of events this buffer can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Returns the number of events currently stored.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the ring buffer contains no events.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends `event`.  If the buffer is full, the oldest event is evicted.
    pub fn push(&mut self, event: AuditEvent) {
        self.buf[self.head] = Some(event);
        self.head = (self.head + 1) % self.capacity();
        if self.len < self.capacity() {
            self.len += 1;
        }
    }

    /// Returns a `Vec` of cloned events in oldest-first order, limited to at
    /// most `limit` entries.  If `limit` equals `usize::MAX`, all stored events
    /// are returned.
    #[must_use]
    pub fn recent(&self, limit: usize) -> Vec<AuditEvent> {
        let count = self.len();
        if self.is_empty() {
            return Vec::new();
        }
        // The oldest entry lives at `head` when the buffer is full, or at 0
        // when it is not yet full.
        let start = if count < self.capacity() {
            0
        } else {
            self.head
        };
        let take = count.min(limit);
        let mut out = Vec::with_capacity(take);
        for i in 0..count {
            if out.len() >= take {
                break;
            }
            let idx = (start + i) % self.capacity();
            if let Some(ev) = &self.buf[idx] {
                out.push(ev.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use mote_types::PluginName;

    use super::*;
    use crate::event::Decision;

    fn ev(plugin: &str, op: &str, decision: Decision) -> AuditEvent {
        AuditEvent::new(PluginName::new(plugin).unwrap(), op, decision)
    }

    #[test]
    fn push_and_recent_basic() {
        let mut rb = RingBuffer::new(4);
        rb.push(ev("adblock", "net:intercept_request", Decision::Allow));
        rb.push(ev("vim-mode", "keys:bind", Decision::Allow));
        let r = rb.recent(10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].plugin.as_str(), "adblock");
        assert_eq!(r[1].plugin.as_str(), "vim-mode");
    }

    #[test]
    fn capacity_cap_evicts_oldest() {
        let mut rb = RingBuffer::new(3);
        rb.push(ev("a", "op", Decision::Allow)); // oldest
        rb.push(ev("b", "op", Decision::Allow));
        rb.push(ev("c", "op", Decision::Allow));
        // Now full — next push evicts "a"
        rb.push(ev("d", "op", Decision::Allow));
        let r = rb.recent(10);
        assert_eq!(r.len(), 3);
        let names: Vec<&str> = r.iter().map(|e| e.plugin.as_str()).collect();
        assert!(!names.contains(&"a"), "oldest entry should be evicted");
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
        assert!(names.contains(&"d"));
    }

    #[test]
    fn recent_limit_is_respected() {
        let mut rb = RingBuffer::new(10);
        for i in 0..8u32 {
            rb.push(ev("adblock", &format!("op-{i}"), Decision::Allow));
        }
        let r = rb.recent(3);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn empty_recent_returns_empty() {
        let rb = RingBuffer::new(5);
        assert!(rb.recent(10).is_empty());
    }

    #[test]
    fn len_never_exceeds_capacity() {
        let cap = 4;
        let mut rb = RingBuffer::new(cap);
        for i in 0..10u32 {
            rb.push(ev("adblock", &format!("op-{i}"), Decision::Allow));
            assert!(rb.len() <= cap);
        }
        assert_eq!(rb.len(), cap);
    }

    #[test]
    fn oldest_first_ordering_after_wrap() {
        // With capacity 3, push A B C D E → expect C D E (oldest-first).
        let mut rb = RingBuffer::new(3);
        for name in &["a", "b", "c", "d", "e"] {
            rb.push(ev(name, "op", Decision::Allow));
        }
        let r = rb.recent(10);
        let names: Vec<&str> = r.iter().map(|e| e.plugin.as_str()).collect();
        assert_eq!(names, vec!["c", "d", "e"]);
    }
}
