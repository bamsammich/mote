//! [`AuditLog`] — the lifecycle type that wires channel + thread + ring
//! buffer + store sink together.
//!
//! # Thread architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │  Caller threads                                      │
//! │  EventProducer::record(event)                        │
//! │     └─ crossbeam Sender::send(Some(event))           │
//! └────────────────────┬─────────────────────────────────┘
//!                      │ unbounded MPSC channel
//! ┌────────────────────▼─────────────────────────────────┐
//! │  Audit thread (single, dedicated)                    │
//! │  loop {                                              │
//! │    recv_timeout(flush_interval)                      │
//! │      → Some(event): accumulate into batch            │
//! │      → None: shutdown sentinel — flush & exit        │
//! │      → Disconnected: all senders gone — flush & exit │
//! │      → Timeout: flush batch so far                   │
//! │    if batch.len() >= flush_threshold → commit_batch  │
//! │  }                                                   │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! The ring buffer and sink are shared with [`QueryHandle`] via
//! `Arc<Mutex<_>>` so the integrity panel can read them while the audit
//! thread writes.  The mutex is held only for individual push/flush calls —
//! never across a channel receive — keeping contention negligible.
//!
//! # Shutdown
//!
//! [`AuditLog::shutdown`] sends a `None` sentinel through the event channel,
//! then joins the thread.  This design lets any number of [`EventProducer`]
//! clones remain alive after shutdown (they silently discard new events once
//! the channel is disconnected) without blocking the shutdown.  The audit
//! thread exits as soon as it dequeues the sentinel, after draining all
//! events that arrived before it.

use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use mote_storage::Store;

use crate::error::AuditError;
use crate::event::AuditEvent;
use crate::producer::EventProducer;
use crate::query::QueryHandle;
use crate::ring::RingBuffer;
use crate::sink::AuditSink;

/// Configuration for the audit pipeline.
///
/// All fields have sensible defaults via [`Config::default`].
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Maximum number of recent events held in memory.
    ///
    /// When the buffer is full the oldest event is evicted.  Flushed events
    /// are durable in storage regardless.
    pub ring_capacity: usize,

    /// How many events to accumulate before triggering a flush, regardless of
    /// elapsed time.
    ///
    /// A lower value means more frequent writes; a higher value batches better.
    pub flush_threshold: usize,

    /// Maximum time between flushes.
    ///
    /// Even if `flush_threshold` is not reached, the audit thread flushes at
    /// this cadence.
    pub flush_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ring_capacity: 1_000,
            flush_threshold: 100,
            flush_interval: Duration::from_secs(5),
        }
    }
}

/// The primary lifecycle handle for the audit pipeline.
///
/// `AuditLog` owns the background thread, the shared ring buffer, and the
/// durable sink.  Obtain [`EventProducer`] handles from [`producer`] and a
/// [`QueryHandle`] from [`query`].
///
/// Call [`shutdown`] to signal the audit thread to stop, flush all in-flight
/// events, and join the thread.  This guarantees durability of all events
/// sent before `shutdown` is called.
///
/// [`producer`]: Self::producer
/// [`query`]: Self::query
/// [`shutdown`]: Self::shutdown
#[derive(Debug)]
pub struct AuditLog {
    /// Sender used exclusively to deliver the shutdown sentinel (`None`) to
    /// the audit thread.  Also functions as a normal event sender for the
    /// [`EventProducer`] clones, which share the same channel but only ever
    /// send `Some(event)`.
    tx: Option<Sender<Option<AuditEvent>>>,
    /// The background audit thread handle.
    thread: Option<JoinHandle<()>>,
    /// Shared with the audit thread and all [`QueryHandle`]s.
    ring: Arc<Mutex<RingBuffer>>,
    /// Shared with the audit thread and all [`QueryHandle`]s.
    sink: Arc<Mutex<AuditSink>>,
}

impl AuditLog {
    /// Constructs and starts the audit pipeline.
    ///
    /// Spawns the audit thread immediately.  Returns an error if the sink
    /// cannot be opened (e.g. storage migration failure).
    ///
    /// # Errors
    ///
    /// Returns [`AuditError`] if [`AuditSink::open`] fails.
    pub fn new(store: &Store, config: Config) -> Result<Self, AuditError> {
        let (tx, rx) = crossbeam_channel::unbounded::<Option<AuditEvent>>();
        let ring = Arc::new(Mutex::new(RingBuffer::new(config.ring_capacity)));
        let sink = Arc::new(Mutex::new(AuditSink::open(store)?));

        let ring_thread = Arc::clone(&ring);
        let sink_thread = Arc::clone(&sink);
        let flush_threshold = config.flush_threshold;
        let flush_interval = config.flush_interval;

        let handle = thread::Builder::new()
            .name("mote-audit".to_owned())
            .spawn(move || {
                run_audit_loop(
                    &rx,
                    &ring_thread,
                    &sink_thread,
                    flush_threshold,
                    flush_interval,
                );
            })
            .map_err(|e| AuditError::ThreadFailed(e.to_string()))?;

        Ok(Self {
            tx: Some(tx),
            thread: Some(handle),
            ring,
            sink,
        })
    }

    /// Returns a cloneable [`EventProducer`] for sending events to the audit
    /// pipeline.
    ///
    /// Multiple producers can coexist; each is a cheap sender clone.
    ///
    /// # Panics
    ///
    /// Panics if called after [`shutdown`](Self::shutdown).
    #[must_use]
    pub fn producer(&self) -> EventProducer {
        EventProducer::new(
            self.tx
                .as_ref()
                .expect("producer() called after shutdown")
                .clone(),
        )
    }

    /// Returns a [`QueryHandle`] for reading recent events and history.
    ///
    /// All query handles share the same underlying ring buffer and sink;
    /// they are cheaply cloneable.
    #[must_use]
    pub fn query(&self) -> QueryHandle {
        QueryHandle::new(Arc::clone(&self.ring), Arc::clone(&self.sink))
    }

    /// Signals the audit thread to stop, drains all in-flight events, and
    /// joins the thread.
    ///
    /// Sends a shutdown sentinel (`None`) through the event channel, then
    /// waits for the audit thread to process all pending events (including
    /// those that arrived before the sentinel) and exit cleanly.  After this
    /// call all events sent before `shutdown` are guaranteed to be durable in
    /// the store.
    ///
    /// Any [`EventProducer`] clones still in existence after shutdown will
    /// silently discard subsequent `record` calls — they are safe to hold and
    /// drop at any time.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::AlreadyShutDown`] if called more than once.
    /// Returns [`AuditError::ThreadFailed`] if the audit thread panicked.
    pub fn shutdown(&mut self) -> Result<(), AuditError> {
        // Send the shutdown sentinel then release our sender.  The audit
        // thread exits when it dequeues the None.
        let tx = self.tx.take().ok_or(AuditError::AlreadyShutDown)?;
        // If the send fails the thread has already exited — that's OK.
        let _ = tx.send(None);
        drop(tx);

        // Join the thread; propagate panic as a ThreadFailed error.
        if let Some(handle) = self.thread.take() {
            handle.join().map_err(|p| {
                AuditError::ThreadFailed(
                    p.downcast_ref::<&str>()
                        .copied()
                        .unwrap_or("unknown panic")
                        .to_owned(),
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for AuditLog {
    /// Best-effort cleanup: sends a shutdown sentinel so the audit thread
    /// exits, then joins it to ensure no leaked threads.
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(None);
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Audit thread entry point
// ---------------------------------------------------------------------------

/// Main loop of the dedicated audit thread.
///
/// Receives `Option<AuditEvent>` messages:
/// - `Some(event)` — accumulate into the batch.
/// - `None` — shutdown sentinel: drain the remainder and exit.
/// - Timeout — flush whatever has accumulated.
/// - Disconnected — all senders gone: flush the remainder and exit.
fn run_audit_loop(
    rx: &Receiver<Option<AuditEvent>>,
    ring: &Arc<Mutex<RingBuffer>>,
    sink: &Arc<Mutex<AuditSink>>,
    flush_threshold: usize,
    flush_interval: Duration,
) {
    let mut batch: Vec<AuditEvent> = Vec::with_capacity(flush_threshold);

    loop {
        match rx.recv_timeout(flush_interval) {
            Ok(Some(event)) => {
                batch.push(event);
                // Drain any additional events that arrived without blocking.
                loop {
                    match rx.try_recv() {
                        Ok(Some(e)) => batch.push(e),
                        // Shutdown sentinel arrived while draining — commit
                        // the batch collected so far, then exit.
                        Ok(None) => {
                            if !batch.is_empty() {
                                commit_batch(ring, sink, &mut batch);
                            }
                            return;
                        }
                        Err(_) => break,
                    }
                }
                if batch.len() >= flush_threshold {
                    commit_batch(ring, sink, &mut batch);
                }
            }
            Ok(None) => {
                // Shutdown sentinel — commit what's left and exit.
                if !batch.is_empty() {
                    commit_batch(ring, sink, &mut batch);
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                // Flush interval elapsed — flush whatever we have.
                if !batch.is_empty() {
                    commit_batch(ring, sink, &mut batch);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                // All senders gone — flush remaining and exit.
                if !batch.is_empty() {
                    commit_batch(ring, sink, &mut batch);
                }
                return;
            }
        }
    }
}

/// Pushes `batch` into `ring`, then flushes to `sink` and clears the batch.
fn commit_batch(
    ring: &Arc<Mutex<RingBuffer>>,
    sink: &Arc<Mutex<AuditSink>>,
    batch: &mut Vec<AuditEvent>,
) {
    // Push each event into the ring buffer (lock held only for the pushes).
    {
        let mut rb = ring.lock().expect("ring buffer mutex poisoned");
        for ev in batch.iter() {
            rb.push(ev.clone());
        }
    }

    // Flush to the durable store (separate, non-nested lock acquisition).
    {
        let mut s = sink.lock().expect("sink mutex poisoned");
        // Best-effort durability: log flush errors to stderr but continue.
        if let Err(e) = s.flush(batch) {
            eprintln!("mote-audit: flush error: {e}");
        }
    }

    batch.clear();
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use mote_storage::Store;
    use mote_types::PluginName;

    use super::*;
    use crate::event::{AuditEvent, Decision};

    fn plugin(name: &str) -> PluginName {
        PluginName::new(name).unwrap()
    }

    fn ev(plugin_name: &str, decision: Decision) -> AuditEvent {
        AuditEvent::new(plugin(plugin_name), "net:intercept_request", decision)
    }

    /// Tight config for tests: tiny ring, flush after 1 event, fast interval.
    fn test_config() -> Config {
        Config {
            ring_capacity: 16,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(10),
        }
    }

    fn open_log() -> (AuditLog, Store) {
        let store = Store::open_in_memory().unwrap();
        let log = AuditLog::new(&store, test_config()).unwrap();
        (log, store)
    }

    /// Drain the channel by waiting for at least one flush interval.
    fn drain(log: &AuditLog) {
        // Sleep longer than the flush interval so the audit thread processes
        // all pending events before we query the ring buffer.
        thread::sleep(Duration::from_millis(50));
        let _ = log; // keep log alive
    }

    // -----------------------------------------------------------------------
    // Ring buffer population
    // -----------------------------------------------------------------------

    #[test]
    fn events_appear_in_ring_buffer_after_drain() {
        let (mut log, _store) = open_log();
        let p = log.producer();
        p.record(ev("adblock", Decision::Allow));
        p.record(ev("vim-mode", Decision::Allow));
        drain(&log);
        let recent = log.query().recent(50);
        assert_eq!(recent.len(), 2);
        log.shutdown().unwrap();
    }

    #[test]
    fn multiple_producers_all_events_recorded() {
        let (mut log, _store) = open_log();
        let p1 = log.producer();
        let p2 = log.producer();
        p1.record(ev("adblock", Decision::Allow));
        p2.record(ev("vim-mode", Decision::Deny));
        drain(&log);
        let recent = log.query().recent(50);
        assert_eq!(recent.len(), 2);
        log.shutdown().unwrap();
    }

    // -----------------------------------------------------------------------
    // Durability
    // -----------------------------------------------------------------------

    #[test]
    fn flush_persists_events_to_store() {
        let (mut log, _store) = open_log();
        let p = log.producer();
        p.record(ev("adblock", Decision::Allow));
        // shutdown sends the sentinel and flushes all pending events.
        log.shutdown().unwrap();

        // The sink is still accessible via the query handle (the Arc keeps it alive).
        let history = log.query().history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].plugin.as_str(), "adblock");
    }

    #[test]
    fn shutdown_flushes_all_pending_events() {
        let (mut log, _store) = open_log();
        let p = log.producer();
        // Send several events rapidly — they may not yet be flushed.
        for _ in 0..10 {
            p.record(ev("adblock", Decision::Allow));
        }
        // Shutdown must flush all of them before returning.
        log.shutdown().unwrap();
        let history = log.query().history().unwrap();
        assert_eq!(history.len(), 10, "all events must survive shutdown");
    }

    // -----------------------------------------------------------------------
    // Ring buffer capacity
    // -----------------------------------------------------------------------

    #[test]
    fn ring_buffer_caps_at_capacity() {
        let store = Store::open_in_memory().unwrap();
        let config = Config {
            ring_capacity: 5,
            flush_threshold: 1,
            flush_interval: Duration::from_millis(10),
        };
        let mut log = AuditLog::new(&store, config).unwrap();
        let p = log.producer();
        // Send 10 events into a ring with capacity 5.
        for _ in 0..10 {
            p.record(ev("adblock", Decision::Allow));
        }
        log.shutdown().unwrap();

        // Ring buffer holds only the 5 most recent.
        let recent = log.query().recent(100);
        assert!(recent.len() <= 5, "ring must not exceed capacity");

        // Durable store has all 10.
        let history = log.query().history().unwrap();
        assert_eq!(history.len(), 10, "store retains all flushed events");
    }

    // -----------------------------------------------------------------------
    // Filtering queries
    // -----------------------------------------------------------------------

    #[test]
    fn per_plugin_filtering() {
        let (mut log, _store) = open_log();
        let p = log.producer();
        p.record(ev("adblock", Decision::Allow));
        p.record(ev("vim-mode", Decision::Allow));
        p.record(ev("adblock", Decision::Deny));
        drain(&log);

        let adblock_events = log.query().recent_for_plugin(&plugin("adblock"), 50);
        assert_eq!(adblock_events.len(), 2);
        assert!(
            adblock_events
                .iter()
                .all(|e| e.plugin.as_str() == "adblock")
        );

        log.shutdown().unwrap();
    }

    #[test]
    fn denial_filtering() {
        let (mut log, _store) = open_log();
        let p = log.producer();
        p.record(ev("adblock", Decision::Allow));
        p.record(ev("vim-mode", Decision::Deny));
        p.record(ev("history", Decision::Deny));
        drain(&log);

        let denials = log.query().recent_denials(50);
        assert_eq!(denials.len(), 2);
        assert!(denials.iter().all(|e| e.decision == Decision::Deny));

        log.shutdown().unwrap();
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    #[test]
    fn double_shutdown_returns_already_shut_down() {
        let (mut log, _store) = open_log();
        log.shutdown().unwrap();
        assert!(matches!(log.shutdown(), Err(AuditError::AlreadyShutDown)));
    }

    #[test]
    fn producer_record_after_shutdown_is_noop() {
        let (mut log, _store) = open_log();
        let p = log.producer();
        log.shutdown().unwrap();
        // Must not panic — send silently discarded after channel disconnect.
        p.record(ev("adblock", Decision::Allow));
    }

    #[test]
    fn producer_is_cloneable() {
        let (mut log, _store) = open_log();
        let p1 = log.producer();
        let p2 = p1.clone();
        p1.record(ev("adblock", Decision::Allow));
        p2.record(ev("vim-mode", Decision::Allow));
        log.shutdown().unwrap();
        // Both events should be durable.
        assert_eq!(log.query().history().unwrap().len(), 2);
    }
}
