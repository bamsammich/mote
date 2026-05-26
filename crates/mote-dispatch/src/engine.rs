//! The differentiated dispatch engine.
//!
//! Owns hook registration (requiring the hook type), priority ordering with a
//! user-override mechanism, and the three dispatch models — filter chains,
//! broadcasts, and keybind input-coalescing — plus per-plugin error/timeout
//! counting → auto-disable and performer-attributed audit.
//!
//! The engine is generic over the payload `P` and a [`HookInvoker`] so the
//! whole policy layer is testable with a mock invoker (no Lua).

use std::collections::HashMap;
use std::time::Instant;

use mote_audit::Decision as AuditDecision;
use mote_types::PluginName;

use crate::audit::{ChainStep, DispatchAudit};
use crate::counter::{AutoDisable, FailureCounter};
use crate::decision::{ChainResolution, Decision};
use crate::hook::{HookType, Registration};
use crate::invoker::{HookInvoker, HookOutcome, InvokeError};

/// A clock the engine reads to derive deadlines and window timestamps.
///
/// Injected so tests can drive time deterministically. Production passes
/// [`SystemClock`].
pub trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// The real wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// One registered hook: its type and the ordered handler registrations.
#[derive(Debug)]
struct HookEntry {
    /// The dispatch model fixed by the **first real registration**.
    ///
    /// `None` until a handler registers: an ordering-only stub created by
    /// [`set_user_order`](DispatchEngine::set_user_order) for a not-yet-registered
    /// hook records the pinned order without committing to a type, so the first
    /// real registration of *any* type still succeeds (it is not pre-constrained
    /// to `FilterChain`). All dispatch paths treat a `None` type as "no matching
    /// hook" — an order pin alone runs no handlers.
    hook_type: Option<HookType>,
    registrations: Vec<Registration>,
    /// User-pinned plugin order (DESIGN §Dispatch ordering, "User config wins
    /// absolutely"). When present, registrations are ordered by this list;
    /// plugins not named fall to the end in default priority order.
    user_order: Option<Vec<PluginName>>,
}

/// The dispatch engine.
///
/// `P` is the filter-chain payload. `I` is the [`HookInvoker`]. `A` is the
/// audit sink. `C` is the [`Clock`].
#[derive(Debug)]
pub struct DispatchEngine<P, I, A, C = SystemClock> {
    hooks: HashMap<String, HookEntry>,
    invoker: I,
    audit: A,
    clock: C,
    counter: FailureCounter,
    _payload: std::marker::PhantomData<fn(P)>,
}

impl<P, I, A> DispatchEngine<P, I, A, SystemClock>
where
    I: HookInvoker<P>,
    A: DispatchAudit,
{
    /// Builds an engine on the system clock.
    pub fn new(invoker: I, audit: A) -> Self {
        Self::with_clock(invoker, audit, SystemClock)
    }
}

impl<P, I, A, C> DispatchEngine<P, I, A, C>
where
    I: HookInvoker<P>,
    A: DispatchAudit,
    C: Clock,
{
    /// Builds an engine with an explicit clock (for tests).
    pub fn with_clock(invoker: I, audit: A, clock: C) -> Self {
        Self {
            hooks: HashMap::new(),
            invoker,
            audit,
            clock,
            counter: FailureCounter::new(),
            _payload: std::marker::PhantomData,
        }
    }

    /// Registers a handler for `hook_key`, **requiring** the hook type
    /// (DISCIPLINES §3). The first registration for a key fixes its hook type;
    /// a later registration with a mismatched type is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterError::HookTypeMismatch`] if `hook_key` was already
    /// registered under a different [`HookType`].
    pub fn register(
        &mut self,
        hook_key: impl Into<String>,
        hook_type: HookType,
        registration: Registration,
    ) -> Result<(), RegisterError> {
        let key = hook_key.into();
        let entry = self.hooks.entry(key.clone()).or_insert_with(|| HookEntry {
            hook_type: None,
            registrations: Vec::new(),
            user_order: None,
        });
        match entry.hook_type {
            // First real registration fixes the dispatch model (even if an
            // ordering-only stub already pinned the order with no type).
            None => entry.hook_type = Some(hook_type),
            Some(existing) if existing != hook_type => {
                return Err(RegisterError::HookTypeMismatch {
                    hook_key: key,
                    existing,
                    requested: hook_type,
                });
            }
            Some(_) => {}
        }
        entry.registrations.push(registration);
        Ok(())
    }

    /// Pins the handler execution order for `hook_key` to `order` (DESIGN
    /// §Dispatch ordering: "User config wins absolutely"). Plugins not named in
    /// `order` run after the pinned ones, in default priority order.
    ///
    /// Has no effect if `hook_key` is not registered (the override is recorded
    /// and applies once handlers register).
    pub fn set_user_order(&mut self, hook_key: impl Into<String>, order: Vec<PluginName>) {
        let key = hook_key.into();
        let entry = self.hooks.entry(key).or_insert_with(|| HookEntry {
            // An ordering-only pin before any registration cannot know the hook
            // type, so it is left `None`. The first real registration fixes the
            // type (of whatever model it is); until then this stub runs no
            // handlers. This avoids poisoning the type — a prior bug pinned it to
            // `FilterChain`, which then rejected a later real registration of a
            // different model with `HookTypeMismatch`.
            hook_type: None,
            registrations: Vec::new(),
            user_order: None,
        });
        entry.user_order = Some(order);
    }

    /// Whether `plugin` is currently auto-disabled.
    #[must_use]
    pub fn is_disabled(&self, plugin: &PluginName) -> bool {
        self.counter.is_disabled(plugin)
    }

    /// Re-enables `plugin`, clearing its failure history (runtime concern after
    /// user intervention / reload).
    pub fn reset_plugin(&mut self, plugin: &PluginName) {
        self.counter.reset(plugin);
    }

    /// Computes the ordered registrations for a hook: user-pinned plugins first
    /// (in pin order), then the rest by priority (higher first), ties broken
    /// alphabetically by plugin name (DESIGN §Dispatch ordering).
    ///
    /// Auto-disabled plugins are filtered out.
    fn ordered_handlers(&self, entry: &HookEntry) -> Vec<Registration> {
        let mut live: Vec<Registration> = entry
            .registrations
            .iter()
            .filter(|r| !self.counter.is_disabled(&r.plugin))
            .cloned()
            .collect();

        if let Some(order) = &entry.user_order {
            // Index of a plugin in the user order, if pinned.
            let rank = |p: &PluginName| order.iter().position(|q| q == p);
            live.sort_by(|a, b| match (rank(&a.plugin), rank(&b.plugin)) {
                (Some(ra), Some(rb)) => ra.cmp(&rb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => default_order(a, b),
            });
        } else {
            live.sort_by(default_order);
        }
        live
    }

    /// Records a failure for `plugin` if the hook type counts, returning any
    /// resulting auto-disable signal.
    fn note_failure(&mut self, plugin: &PluginName, hook_type: HookType) -> Option<AutoDisable> {
        if hook_type.counts_toward_auto_disable() {
            let now = self.clock.now();
            self.counter.record_failure(plugin, now)
        } else {
            None
        }
    }
}

/// Default ordering: higher priority first, ties alphabetical by plugin name.
fn default_order(a: &Registration, b: &Registration) -> std::cmp::Ordering {
    b.priority
        .cmp(&a.priority)
        .then_with(|| a.plugin.as_str().cmp(b.plugin.as_str()))
}

/// The result of dispatching a filter chain.
#[derive(Debug)]
pub struct FilterChainOutcome<P> {
    /// The resolved chain result the runtime acts on.
    pub resolution: ChainResolution<P>,
    /// Any plugins auto-disabled during this dispatch (DESIGN §Runtime
    /// guarantees). The shell surfaces the system notification.
    pub auto_disabled: Vec<AutoDisable>,
}

/// The result of dispatching a broadcast.
#[derive(Debug)]
pub struct BroadcastOutcome {
    /// Plugins auto-disabled during this dispatch.
    pub auto_disabled: Vec<AutoDisable>,
}

/// The result of dispatching a keybind.
#[derive(Debug)]
pub struct KeybindOutcome {
    /// Whether a handler ran (vs. the key being unbound).
    pub handled: bool,
    /// Whether the handler errored or timed out. Keybinds never auto-disable,
    /// so this is informational only.
    pub failed: bool,
}

impl<P, I, A, C> DispatchEngine<P, I, A, C>
where
    P: Clone,
    I: HookInvoker<P>,
    A: DispatchAudit,
    C: Clock,
{
    /// Dispatches a **filter chain** for `hook_key` with `payload`.
    ///
    /// Resolution: handlers run in order; the first [`Decision::Block`] wins
    /// (later handlers are still notified for observability but cannot override
    /// the block); [`Decision::Modify`] cascades the new payload to the next
    /// handler; [`Decision::Allow`] / [`Decision::Defer`] continue. Nothing,
    /// an error, or a timeout is treated as `defer`. A timeout/error counts
    /// toward auto-disable (DESIGN §Runtime guarantees).
    ///
    /// Each handler is given a fresh 10ms deadline (the per-handler budget;
    /// DESIGN's table is per-handler — the audit example times each handler
    /// independently).
    ///
    /// This never panics on handler misbehavior. If `hook_key` is unregistered
    /// or registered under a non-filter-chain type, the chain runs no handlers
    /// and resolves as allowed/unmodified (the runtime should not route a
    /// broadcast key here).
    pub fn dispatch_filter_chain(
        &mut self,
        hook_key: &str,
        mut payload: P,
    ) -> FilterChainOutcome<P> {
        let mut auto_disabled = Vec::new();

        let Some(handlers) = self
            .hooks
            .get(hook_key)
            .filter(|e| e.hook_type == Some(HookType::FilterChain))
            .map(|e| self.ordered_handlers(e))
        else {
            return FilterChainOutcome {
                resolution: ChainResolution::Allowed { payload },
                auto_disabled,
            };
        };

        let budget = crate::hook::FILTER_CHAIN_BUDGET;
        let mut blocked: Option<(String, P)> = None;

        for reg in handlers {
            let start = self.clock.now();
            let deadline = start + budget;
            let result = self
                .invoker
                .invoke(&reg.plugin, hook_key, payload.clone(), deadline);
            let latency_us = elapsed_us(start, self.clock.now());

            match result {
                Ok(HookOutcome::Decision(decision)) => match decision {
                    Decision::Block { reason } => {
                        self.audit.record_step(ChainStep {
                            performer: reg.plugin.clone(),
                            operation: hook_key.to_owned(),
                            decision: AuditDecision::Deny,
                            latency_us: Some(latency_us),
                            detail: Some(reason.clone()),
                        });
                        if blocked.is_none() {
                            // First block wins; capture payload as it stands.
                            blocked = Some((reason, payload.clone()));
                        }
                        // Continue the loop for observability of later handlers.
                    }
                    Decision::Modify { payload: next } => {
                        self.audit.record_step(ChainStep {
                            performer: reg.plugin.clone(),
                            operation: hook_key.to_owned(),
                            decision: AuditDecision::Allow,
                            latency_us: Some(latency_us),
                            detail: Some("modify".to_owned()),
                        });
                        // Cascade only while not yet blocked; once blocked the
                        // action is fixed but we still observe.
                        if blocked.is_none() {
                            payload = next;
                        }
                    }
                    Decision::Allow => {
                        self.audit.record_step(ChainStep {
                            performer: reg.plugin.clone(),
                            operation: hook_key.to_owned(),
                            decision: AuditDecision::Allow,
                            latency_us: Some(latency_us),
                            detail: None,
                        });
                    }
                    Decision::Defer => {
                        self.audit
                            .record_step(step_defer(&reg.plugin, hook_key, latency_us, None));
                    }
                },
                Ok(HookOutcome::Done) => {
                    // A filter-chain handler should yield a decision; treat a
                    // bare completion as defer.
                    self.audit
                        .record_step(step_defer(&reg.plugin, hook_key, latency_us, None));
                }
                Err(err) => {
                    // Timeout or Lua error → treat as defer, count toward
                    // auto-disable, and record.
                    let detail = match &err {
                        InvokeError::Timeout => "timeout → defer".to_owned(),
                        InvokeError::Lua(msg) => format!("error → defer: {msg}"),
                    };
                    self.audit.record_step(step_defer(
                        &reg.plugin,
                        hook_key,
                        latency_us,
                        Some(detail),
                    ));
                    if let Some(sig) = self.note_failure(&reg.plugin, HookType::FilterChain) {
                        auto_disabled.push(sig);
                    }
                }
            }
        }

        let resolution = match blocked {
            Some((reason, payload)) => ChainResolution::Blocked { reason, payload },
            None => ChainResolution::Allowed { payload },
        };
        FilterChainOutcome {
            resolution,
            auto_disabled,
        }
    }

    /// Dispatches a **broadcast** for `hook_key` with `payload`.
    ///
    /// All registered handlers run sequentially with no shared state and no
    /// return semantics; an error or timeout in one handler is isolated and
    /// does not stop the others (DESIGN §Hook dispatch patterns). Failures
    /// count toward auto-disable.
    pub fn dispatch_broadcast(&mut self, hook_key: &str, payload: P) -> BroadcastOutcome {
        let mut auto_disabled = Vec::new();

        let Some(handlers) = self
            .hooks
            .get(hook_key)
            .filter(|e| e.hook_type == Some(HookType::Broadcast))
            .map(|e| self.ordered_handlers(e))
        else {
            return BroadcastOutcome { auto_disabled };
        };

        let budget = crate::hook::BROADCAST_BUDGET;

        for reg in handlers {
            let start = self.clock.now();
            let deadline = start + budget;
            let result = self
                .invoker
                .invoke(&reg.plugin, hook_key, payload.clone(), deadline);
            let latency_us = elapsed_us(start, self.clock.now());

            match result {
                Ok(_) => {
                    self.audit.record_step(ChainStep {
                        performer: reg.plugin.clone(),
                        operation: hook_key.to_owned(),
                        decision: AuditDecision::Allow,
                        latency_us: Some(latency_us),
                        detail: None,
                    });
                }
                Err(err) => {
                    let detail = match &err {
                        InvokeError::Timeout => "timeout (isolated)".to_owned(),
                        InvokeError::Lua(msg) => format!("error (isolated): {msg}"),
                    };
                    self.audit.record_step(step_defer(
                        &reg.plugin,
                        hook_key,
                        latency_us,
                        Some(detail),
                    ));
                    if let Some(sig) = self.note_failure(&reg.plugin, HookType::Broadcast) {
                        auto_disabled.push(sig);
                    }
                    // Isolated: continue to the next handler regardless.
                }
            }
        }

        BroadcastOutcome { auto_disabled }
    }

    /// Dispatches a **keybind** handler for `hook_key`.
    ///
    /// A keybind hook has at most one live handler (the bound plugin). It runs
    /// with no raw-timeout auto-disable: a timeout or error is reported in the
    /// outcome but never counts toward auto-disable (DESIGN §Runtime
    /// guarantees). Input-coalescing across rapid presses is the caller's
    /// responsibility via [`crate::KeybindQueue`] — this method handles one
    /// resolved press.
    pub fn dispatch_keybind(&mut self, hook_key: &str, payload: P) -> KeybindOutcome {
        let Some(handlers) = self
            .hooks
            .get(hook_key)
            .filter(|e| e.hook_type == Some(HookType::Keybind))
            .map(|e| self.ordered_handlers(e))
        else {
            return KeybindOutcome {
                handled: false,
                failed: false,
            };
        };

        let Some(reg) = handlers.into_iter().next() else {
            return KeybindOutcome {
                handled: false,
                failed: false,
            };
        };

        // Keybinds have no raw timeout; give a generous safety deadline so a
        // genuinely-stuck handler still yields rather than hanging the engine,
        // but never count it.
        let start = self.clock.now();
        let deadline = start + crate::hook::BROADCAST_BUDGET;
        let result = self
            .invoker
            .invoke(&reg.plugin, hook_key, payload, deadline);
        let latency_us = elapsed_us(start, self.clock.now());

        match result {
            Ok(_) => {
                self.audit.record_step(ChainStep {
                    performer: reg.plugin,
                    operation: hook_key.to_owned(),
                    decision: AuditDecision::Allow,
                    latency_us: Some(latency_us),
                    detail: None,
                });
                KeybindOutcome {
                    handled: true,
                    failed: false,
                }
            }
            Err(err) => {
                let detail = match &err {
                    InvokeError::Timeout => "keybind timeout (no auto-disable)".to_owned(),
                    InvokeError::Lua(msg) => format!("keybind error (no auto-disable): {msg}"),
                };
                self.audit
                    .record_step(step_defer(&reg.plugin, hook_key, latency_us, Some(detail)));
                // Intentionally NOT counted: keybinds are exempt.
                KeybindOutcome {
                    handled: true,
                    failed: true,
                }
            }
        }
    }
}

/// Builds a `defer`-decision audit step.
fn step_defer(
    plugin: &PluginName,
    operation: &str,
    latency_us: u64,
    detail: Option<String>,
) -> ChainStep {
    ChainStep {
        performer: plugin.clone(),
        operation: operation.to_owned(),
        decision: AuditDecision::Defer,
        latency_us: Some(latency_us),
        detail,
    }
}

/// Elapsed microseconds between two instants, saturating.
fn elapsed_us(start: Instant, end: Instant) -> u64 {
    u64::try_from(end.saturating_duration_since(start).as_micros()).unwrap_or(u64::MAX)
}

/// Error registering a handler.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegisterError {
    /// `hook_key` was already registered under a different [`HookType`]; the
    /// dispatch model for a hook is fixed by its first registration.
    #[error(
        "hook `{hook_key}` is registered as {existing:?} but a handler requested {requested:?}"
    )]
    HookTypeMismatch {
        /// The hook whose type was contradicted.
        hook_key: String,
        /// The type fixed by the first registration.
        existing: HookType,
        /// The conflicting requested type.
        requested: HookType,
    },
}
