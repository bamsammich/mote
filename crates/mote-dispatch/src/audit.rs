//! Audit attribution for dispatched chains (DESIGN §Observability;
//! risks-and-inconsistencies.md D4).
//!
//! Every dispatched handler decision is recorded with **performer
//! attribution**: the [`AuditEvent::plugin`] field is the plugin whose handler
//! actually ran and whose permissions gated the action (D4). For a capability
//! API invocation the performer is the *fulfiller* (whose permissions the call
//! runs under), and the caller/capability is carried in the event detail as the
//! invocation chain.
//!
//! The engine writes through the [`DispatchAudit`] seam so policy tests can use
//! a capturing fake without standing up the real `mote-audit` thread + storage
//! pipeline. The production path is the blanket implementation over
//! [`mote_audit::EventProducer`].

use mote_audit::{AuditEvent, Decision as AuditDecision, EventProducer};
use mote_types::PluginName;

/// One line of a dispatched chain's audit trail.
///
/// Mirrors the per-handler rows DESIGN §Observability shows
/// (`adblock → block (1.2ms) [easylist: ...]`).
#[derive(Debug, Clone)]
pub struct ChainStep {
    /// The performer: whose handler ran and whose permissions gated it (D4).
    pub performer: PluginName,
    /// The hook/operation in `domain:action` form (e.g.
    /// `net:intercept_request`).
    pub operation: String,
    /// The audit decision recorded for this step.
    pub decision: AuditDecision,
    /// Measured latency in microseconds, if timed.
    pub latency_us: Option<u64>,
    /// Free-form detail: the block reason, the timeout budget exceeded, or — for
    /// a capability invocation — the `invoked_via (caller -> capability)` chain
    /// (D4).
    pub detail: Option<String>,
}

/// The audit seam the dispatch engine writes through.
///
/// One method per recorded step. Implemented by the real
/// [`mote_audit::EventProducer`] (blanket impl below) and by test fakes.
pub trait DispatchAudit {
    /// Records one step of a dispatched chain.
    fn record_step(&self, step: ChainStep);
}

impl DispatchAudit for EventProducer {
    fn record_step(&self, step: ChainStep) {
        let mut event = AuditEvent::new(step.performer, step.operation, step.decision);
        if let Some(us) = step.latency_us {
            event = event.with_latency(us);
        }
        if let Some(detail) = step.detail {
            event = event.with_detail(detail);
        }
        self.record(event);
    }
}

/// An audit sink that drops every step. Useful when a caller wants dispatch
/// without an audit pipeline wired up (e.g. early boot, or focused tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NullAudit;

impl DispatchAudit for NullAudit {
    fn record_step(&self, _step: ChainStep) {}
}
