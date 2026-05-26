//! The cloneable sender handle for the audit pipeline.
//!
//! [`EventProducer`] wraps a [`crossbeam_channel::Sender`] and exposes a
//! single `record` method.  Because senders are intrinsically cheap to clone
//! (an `Arc` bump), many call sites can hold their own handle without any
//! coordination cost.
//!
//! # Shutdown behaviour
//!
//! The channel message type is `Option<AuditEvent>`: producers send
//! `Some(event)` for normal events.  `None` is the shutdown sentinel sent
//! exclusively by [`AuditLog::shutdown`](crate::AuditLog::shutdown).
//!
//! After `shutdown` is called, the underlying channel is disconnected.
//! Subsequent `record` calls are silently dropped — the send fails but is
//! not propagated as an error.  It is safe to hold and drop `EventProducer`
//! handles at any time without affecting the shutdown sequence.
//!
//! # Drop policy (channel capacity)
//!
//! The channel is **unbounded**.  Sends never block and never drop events
//! because a full buffer is full — the channel grows until the audit thread
//! drains it.  This is the correct trade-off for a per-permission-call audit
//! log where the caller must never pay a latency penalty: the audit thread
//! is the back-pressure valve, and in pathological scenarios (audit thread
//! stalled, thousands of events per second) the channel will consume extra
//! memory rather than silently dropping events or blocking the hot path.

use crossbeam_channel::Sender;

use crate::event::AuditEvent;

/// A cloneable, non-blocking sender for [`AuditEvent`]s.
///
/// Obtain one via [`AuditLog::producer`](crate::AuditLog::producer).
/// Clone freely — each clone is an independent sender over the same channel.
#[derive(Debug, Clone)]
pub struct EventProducer {
    tx: Sender<Option<AuditEvent>>,
}

impl EventProducer {
    pub(crate) const fn new(tx: Sender<Option<AuditEvent>>) -> Self {
        Self { tx }
    }

    /// Sends `event` to the audit pipeline.
    ///
    /// This is a single lock-free atomic operation: the event is appended to
    /// the channel without acquiring any mutex.  It never blocks.
    ///
    /// The call is a no-op (silently discarded) if the [`AuditLog`] has been
    /// shut down — this makes it safe to hold `EventProducer` handles after
    /// shutdown without crashing.
    ///
    /// [`AuditLog`]: crate::AuditLog
    pub fn record(&self, event: AuditEvent) {
        // SendError means the channel is disconnected (after shutdown).
        // Silently ignore so that callers holding producers after shutdown
        // do not crash.
        let _ = self.tx.send(Some(event));
    }
}
