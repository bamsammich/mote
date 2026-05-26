//! Lock-free permission and network audit log for Mote.
//!
//! The append-only audit pipeline feeds every gatekeeper check and dispatch
//! decision into a dedicated audit thread with zero mutex contention on the
//! hot path:
//!
//! ```text
//! caller A ─┐                    ┌── ring buffer (recent-N, in-memory)
//! caller B ─┼─ crossbeam MPSC ──►│   audit thread
//! caller C ─┘  (unbounded)       └── periodic flush ──► mote-storage namespace
//! ```
//!
//! Logging a permission call is a single [`crossbeam_channel::Sender::send`]
//! — a lock-free atomic append. The audit thread is the only writer to both
//! the ring buffer and the durable [`mote_storage::Store`] namespace; no
//! external lock is required.
//!
//! # Quick start
//!
//! ```no_run
//! use mote_audit::{AuditLog, AuditEvent, Decision};
//! use mote_storage::Store;
//! use mote_types::PluginName;
//!
//! let store = Store::open_in_memory()?;
//! let mut log = AuditLog::new(&store, mote_audit::Config::default())?;
//! let producer = log.producer();
//!
//! // Cheap, non-blocking — no mutex on the hot path.
//! let plugin = PluginName::new("adblock")?;
//! producer.record(AuditEvent::new(plugin, "net:intercept_request", Decision::Allow));
//!
//! // Integrity panel reads — served from the ring buffer (fast) or
//! // flushed history from the Store namespace (durable).
//! let recent = log.query().recent(50);
//! let counts = log.query().counts_per_plugin();
//! let denials = log.query().recent_denials(20);
//!
//! log.shutdown()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Modules
//!
//! - [`event`] — [`AuditEvent`] and [`Decision`] types.
//! - [`ring`] — fixed-capacity in-memory ring buffer.
//! - [`sink`] — durable persistence layer over [`mote_storage::Namespace`].
//! - [`producer`] — cloneable sender handle.
//! - [`auditor`] — background audit thread lifecycle.
//! - [`query`] — query API for the integrity panel.
//! - [`error`] — [`AuditError`].

pub(crate) mod auditor;
pub(crate) mod error;
pub(crate) mod event;
pub(crate) mod producer;
pub(crate) mod query;
pub(crate) mod ring;
pub(crate) mod sink;

pub use auditor::{AuditLog, Config};
pub use error::AuditError;
pub use event::{AuditEvent, Decision};
pub use producer::EventProducer;
pub use query::QueryHandle;
pub use ring::RingBuffer;
pub use sink::AuditSink;
