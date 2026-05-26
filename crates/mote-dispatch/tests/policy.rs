//! Dispatch policy tests with a MOCK `HookInvoker` — no Lua needed.
//!
//! These prove the composition/policy layer (DESIGN §Plugin Dispatch and
//! Composition; DISCIPLINES §3): filter-chain resolution, budget-timeout →
//! defer, broadcast error isolation, priority ordering + user override,
//! per-plugin auto-disable with keybind exemption, and keybind input
//! coalescing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use mote_dispatch::{
    AUTO_DISABLE_THRESHOLD, ChainResolution, ChainStep, Clock, Decision, DispatchAudit,
    DispatchEngine, HookInvoker, HookOutcome, HookType, InvokeError, KeybindQueue, Registration,
};
use mote_types::PluginName;

fn plugin(name: &str) -> PluginName {
    PluginName::new(name).unwrap()
}

// --- A mock invoker ---------------------------------------------------------

/// A programmed response for one `(plugin, key)` handler.
#[derive(Clone)]
enum Behavior {
    /// Return a fixed decision.
    Decide(Decision<TestPayload>),
    /// A broadcast/keybind handler that completes.
    Done,
    /// Time out.
    Timeout,
    /// Raise an error with this message.
    Error(String),
    /// Modify: append a tag to the payload's `tags`, then defer to cascade the
    /// modified payload to the next handler. (Used to prove modify cascades.)
    AppendTag(String),
}

/// The test payload: an ordered list of tags so modify-cascade is observable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TestPayload {
    tags: Vec<String>,
}

#[derive(Default)]
struct MockInvoker {
    /// Behavior keyed by `(plugin, key)`.
    behaviors: HashMap<(String, String), Behavior>,
}

impl MockInvoker {
    fn program(&mut self, plugin: &str, key: &str, behavior: Behavior) {
        self.behaviors
            .insert((plugin.to_owned(), key.to_owned()), behavior);
    }
}

impl HookInvoker<TestPayload> for MockInvoker {
    fn invoke(
        &self,
        plugin: &PluginName,
        key: &str,
        payload: TestPayload,
        _deadline: Instant,
    ) -> Result<HookOutcome<TestPayload>, InvokeError> {
        match self.behaviors.get(&(plugin.to_string(), key.to_owned())) {
            Some(Behavior::Decide(d)) => Ok(HookOutcome::Decision(d.clone())),
            Some(Behavior::Done) => Ok(HookOutcome::Done),
            Some(Behavior::Timeout) => Err(InvokeError::Timeout),
            Some(Behavior::Error(msg)) => Err(InvokeError::Lua(msg.clone())),
            Some(Behavior::AppendTag(tag)) => {
                let mut next = payload;
                next.tags.push(tag.clone());
                Ok(HookOutcome::Decision(Decision::Modify { payload: next }))
            }
            None => Ok(HookOutcome::Decision(Decision::Defer)),
        }
    }
}

// --- A capturing audit sink -------------------------------------------------

#[derive(Default, Clone)]
struct CapturingAudit {
    steps: std::rc::Rc<RefCell<Vec<ChainStep>>>,
}

impl CapturingAudit {
    fn performers(&self) -> Vec<String> {
        self.steps
            .borrow()
            .iter()
            .map(|s| s.performer.to_string())
            .collect()
    }
}

impl DispatchAudit for CapturingAudit {
    fn record_step(&self, step: ChainStep) {
        self.steps.borrow_mut().push(step);
    }
}

// --- A manual clock ---------------------------------------------------------

#[derive(Clone)]
struct ManualClock {
    now: std::rc::Rc<RefCell<Instant>>,
}

impl ManualClock {
    fn new() -> Self {
        Self {
            now: std::rc::Rc::new(RefCell::new(Instant::now())),
        }
    }
    fn advance(&self, by: Duration) {
        let mut g = self.now.borrow_mut();
        *g += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.now.borrow()
    }
}

// --- helpers ----------------------------------------------------------------

fn empty() -> TestPayload {
    TestPayload::default()
}

// ===========================================================================
// Filter chain
// ===========================================================================

#[test]
fn filter_chain_first_block_wins() {
    let mut inv = MockInvoker::default();
    inv.program(
        "ph",
        "net:intercept_request",
        Behavior::Decide(Decision::Allow),
    );
    inv.program(
        "adblock",
        "net:intercept_request",
        Behavior::Decide(Decision::Block {
            reason: "easylist".into(),
        }),
    );
    // A second blocker that would set a different reason — must not override.
    inv.program(
        "blocker2",
        "net:intercept_request",
        Behavior::Decide(Decision::Block {
            reason: "second".into(),
        }),
    );

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());
    // priorities: ph=70, adblock=50, blocker2=30 → ph, adblock, blocker2.
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("ph"), 70),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("adblock"), 50),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("blocker2"), 30),
        )
        .unwrap();

    let out = engine.dispatch_filter_chain("net:intercept_request", empty());
    match out.resolution {
        ChainResolution::Blocked { reason, .. } => assert_eq!(reason, "easylist"),
        ChainResolution::Allowed { .. } => panic!("expected block, got allowed"),
    }
    // All three were still invoked for observability (later handlers notified
    // but cannot override the first block).
    assert_eq!(
        audit.performers(),
        vec!["ph", "adblock", "blocker2"],
        "later handlers still notified after the block"
    );
}

#[test]
fn filter_chain_modify_cascades_to_next_handler() {
    let mut inv = MockInvoker::default();
    inv.program(
        "a",
        "net:intercept_request",
        Behavior::AppendTag("a".into()),
    );
    inv.program(
        "b",
        "net:intercept_request",
        Behavior::AppendTag("b".into()),
    );

    // Capture the payload each handler saw via the mock's call log: build the
    // invoker, run, then inspect. Because the engine owns the invoker, we read
    // the cascade effect from the final resolution payload instead.
    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit);
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("a"), 70),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("b"), 50),
        )
        .unwrap();

    let out = engine.dispatch_filter_chain("net:intercept_request", empty());
    match out.resolution {
        ChainResolution::Allowed { payload } => {
            assert_eq!(
                payload.tags,
                vec!["a".to_string(), "b".to_string()],
                "b must have seen a's modification and appended after it"
            );
        }
        ChainResolution::Blocked { .. } => panic!("expected allowed, got blocked"),
    }
}

#[test]
fn filter_chain_allow_and_defer_continue() {
    let mut inv = MockInvoker::default();
    inv.program("a", "h", Behavior::Decide(Decision::Allow));
    inv.program("b", "h", Behavior::Decide(Decision::Defer));
    inv.program("c", "h", Behavior::Decide(Decision::Allow));

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());
    for (name, pri) in [("a", 70), ("b", 50), ("c", 30)] {
        engine
            .register(
                "h",
                HookType::FilterChain,
                Registration::with_priority(plugin(name), pri),
            )
            .unwrap();
    }

    let out = engine.dispatch_filter_chain("h", empty());
    assert!(matches!(out.resolution, ChainResolution::Allowed { .. }));
    assert_eq!(audit.performers(), vec!["a", "b", "c"], "all ran in order");
}

#[test]
fn empty_chain_resolves_to_allowed_defer() {
    let inv = MockInvoker::default();
    let audit = CapturingAudit::default();
    let mut engine: DispatchEngine<TestPayload, _, _> = DispatchEngine::new(inv, audit);
    // No registrations for this key.
    let out = engine.dispatch_filter_chain("unregistered", empty());
    assert!(matches!(out.resolution, ChainResolution::Allowed { .. }));
}

#[test]
fn filter_chain_timeout_is_treated_as_defer_not_block() {
    let mut inv = MockInvoker::default();
    inv.program("slow", "h", Behavior::Timeout);
    inv.program("after", "h", Behavior::Decide(Decision::Allow));

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit);
    engine
        .register(
            "h",
            HookType::FilterChain,
            Registration::with_priority(plugin("slow"), 70),
        )
        .unwrap();
    engine
        .register(
            "h",
            HookType::FilterChain,
            Registration::with_priority(plugin("after"), 50),
        )
        .unwrap();

    let out = engine.dispatch_filter_chain("h", empty());
    // A timeout must NOT block and must NOT modify: the chain proceeds, ends
    // allowed.
    assert!(
        matches!(out.resolution, ChainResolution::Allowed { .. }),
        "timeout must be treated as defer, got {:?}",
        out.resolution
    );
    // The timed-out handler counts as a failure (one).
    assert_eq!(
        out.auto_disabled.len(),
        0,
        "one timeout does not yet disable"
    );
}

// ===========================================================================
// Broadcast
// ===========================================================================

#[test]
fn broadcast_runs_all_handlers_and_isolates_errors() {
    let mut inv = MockInvoker::default();
    inv.program("ok1", "tabs:on_change", Behavior::Done);
    inv.program("boom", "tabs:on_change", Behavior::Error("kaboom".into()));
    inv.program("ok2", "tabs:on_change", Behavior::Done);

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());
    for (name, pri) in [("ok1", 70), ("boom", 50), ("ok2", 30)] {
        engine
            .register(
                "tabs:on_change",
                HookType::Broadcast,
                Registration::with_priority(plugin(name), pri),
            )
            .unwrap();
    }

    let _ = engine.dispatch_broadcast("tabs:on_change", empty());
    // All three handlers ran despite the middle one erroring.
    assert_eq!(audit.performers(), vec!["ok1", "boom", "ok2"]);
}

// ===========================================================================
// Ordering
// ===========================================================================

#[test]
fn priority_orders_high_to_low_with_alpha_tiebreak() {
    let mut inv = MockInvoker::default();
    for name in ["zebra", "alpha", "mid"] {
        inv.program(name, "h", Behavior::Decide(Decision::Allow));
    }

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());
    // zebra & alpha both priority 50 (tie → alpha before zebra); mid priority 90.
    engine
        .register(
            "h",
            HookType::FilterChain,
            Registration::with_priority(plugin("zebra"), 50),
        )
        .unwrap();
    engine
        .register(
            "h",
            HookType::FilterChain,
            Registration::with_priority(plugin("alpha"), 50),
        )
        .unwrap();
    engine
        .register(
            "h",
            HookType::FilterChain,
            Registration::with_priority(plugin("mid"), 90),
        )
        .unwrap();

    let _ = engine.dispatch_filter_chain("h", empty());
    assert_eq!(audit.performers(), vec!["mid", "alpha", "zebra"]);
}

#[test]
fn user_override_pins_order_absolutely() {
    let mut inv = MockInvoker::default();
    for name in ["adblock", "privacy-headers", "request-logger"] {
        inv.program(
            name,
            "net:intercept_request",
            Behavior::Decide(Decision::Allow),
        );
    }

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());
    // Register with priorities that would otherwise order adblock first.
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("adblock"), 90),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("privacy-headers"), 50),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("request-logger"), 30),
        )
        .unwrap();

    // User pins a different order (DESIGN example).
    engine.set_user_order(
        "net:intercept_request",
        vec![
            plugin("privacy-headers"),
            plugin("adblock"),
            plugin("request-logger"),
        ],
    );

    let _ = engine.dispatch_filter_chain("net:intercept_request", empty());
    assert_eq!(
        audit.performers(),
        vec!["privacy-headers", "adblock", "request-logger"],
        "user order wins over priority"
    );
}

#[test]
fn pre_registration_order_pin_does_not_constrain_hook_type() {
    // S6: pinning order for a not-yet-registered hook must not lock the hook
    // type to FilterChain. A later real registration as a Broadcast must
    // succeed (no HookTypeMismatch) and honor the pinned order.
    let mut inv = MockInvoker::default();
    inv.program("late", "tabs:on_change", Behavior::Done);
    inv.program("early", "tabs:on_change", Behavior::Done);

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());

    // Pin the order BEFORE any handler registers.
    engine.set_user_order("tabs:on_change", vec![plugin("late"), plugin("early")]);

    // Now register the hook as a Broadcast — a different model than the old
    // hardcoded FilterChain stub. This must not error.
    engine
        .register(
            "tabs:on_change",
            HookType::Broadcast,
            Registration::with_priority(plugin("early"), 90),
        )
        .expect("registering a Broadcast after an order pin must succeed");
    engine
        .register(
            "tabs:on_change",
            HookType::Broadcast,
            Registration::with_priority(plugin("late"), 10),
        )
        .expect("second Broadcast handler registers");

    // The broadcast actually dispatches (type was fixed to Broadcast), and the
    // pinned order is honored over priority.
    let _ = engine.dispatch_broadcast("tabs:on_change", empty());
    assert_eq!(
        audit.performers(),
        vec!["late", "early"],
        "pinned order honored and broadcast dispatched"
    );
}

// ===========================================================================
// Auto-disable (D3) — per plugin, keybinds exempt
// ===========================================================================

#[test]
fn three_failures_in_window_auto_disables_plugin() {
    let mut inv = MockInvoker::default();
    inv.program("flaky", "h", Behavior::Error("nope".into()));

    let audit = CapturingAudit::default();
    let clock = ManualClock::new();
    let mut engine = DispatchEngine::with_clock(inv, audit, clock.clone());
    engine
        .register(
            "h",
            HookType::FilterChain,
            Registration::new(plugin("flaky")),
        )
        .unwrap();

    // Failure 1 and 2: no disable.
    for _ in 0..(AUTO_DISABLE_THRESHOLD - 1) {
        let out = engine.dispatch_filter_chain("h", empty());
        assert!(out.auto_disabled.is_empty());
        clock.advance(Duration::from_mins(1));
    }
    // Failure 3 within the window: auto-disable fires.
    let out = engine.dispatch_filter_chain("h", empty());
    assert_eq!(out.auto_disabled.len(), 1, "third failure disables");
    assert_eq!(out.auto_disabled[0].plugin, plugin("flaky"));
    assert!(engine.is_disabled(&plugin("flaky")));

    // Once disabled the handler no longer runs.
    let out = engine.dispatch_filter_chain("h", empty());
    assert!(out.auto_disabled.is_empty(), "no repeat signal");
}

#[test]
fn keybind_failures_do_not_count_toward_auto_disable() {
    let mut inv = MockInvoker::default();
    inv.program("vim-mode", "keys:bind", Behavior::Timeout);

    let audit = CapturingAudit::default();
    let clock = ManualClock::new();
    let mut engine = DispatchEngine::with_clock(inv, audit, clock);
    engine
        .register(
            "keys:bind",
            HookType::Keybind,
            Registration::new(plugin("vim-mode")),
        )
        .unwrap();

    // Many keybind failures, well over the threshold.
    for _ in 0..(AUTO_DISABLE_THRESHOLD * 3) {
        let out = engine.dispatch_keybind("keys:bind", empty());
        assert!(out.handled, "handler was invoked");
        assert!(out.failed, "and it failed");
    }
    assert!(
        !engine.is_disabled(&plugin("vim-mode")),
        "keybinds are EXEMPT from auto-disable"
    );
}

#[test]
fn failures_outside_window_do_not_accumulate() {
    let mut inv = MockInvoker::default();
    inv.program("p", "h", Behavior::Error("e".into()));

    let audit = CapturingAudit::default();
    let clock = ManualClock::new();
    let mut engine = DispatchEngine::with_clock(inv, audit, clock.clone());
    engine
        .register("h", HookType::FilterChain, Registration::new(plugin("p")))
        .unwrap();

    engine.dispatch_filter_chain("h", empty()); // failure 1
    clock.advance(mote_dispatch::WINDOW + Duration::from_secs(1)); // age it out
    engine.dispatch_filter_chain("h", empty()); // failure 1 (again)
    let out = engine.dispatch_filter_chain("h", empty()); // failure 2 in window
    assert!(
        out.auto_disabled.is_empty(),
        "old failure pruned; only 2 in window"
    );
    assert!(!engine.is_disabled(&plugin("p")));
}

// ===========================================================================
// Keybind input-coalescing
// ===========================================================================

#[test]
fn keybind_queue_coalesces_burst_to_latest() {
    let mut q: KeybindQueue<u32> = KeybindQueue::new();

    // A burst arrives while a handler is "running": only the latest survives.
    q.mark_running();
    q.push(1);
    q.push(2);
    q.push(3);
    assert!(q.has_pending());
    assert_eq!(q.coalesced_count(), 2, "two older inputs discarded");

    q.mark_idle();
    assert_eq!(q.take_latest(), Some(3), "only the latest is handled");
    assert!(!q.has_pending());
    assert_eq!(q.take_latest(), None);
}

#[test]
fn keybind_queue_single_input_passes_through() {
    let mut q: KeybindQueue<&str> = KeybindQueue::new();
    q.push("a");
    assert_eq!(q.coalesced_count(), 0);
    assert_eq!(q.take_latest(), Some("a"));
}

// ===========================================================================
// Hook-type enforcement & audit attribution (D4)
// ===========================================================================

#[test]
fn registering_same_key_under_different_type_is_rejected() {
    let inv = MockInvoker::default();
    let audit = CapturingAudit::default();
    let mut engine: DispatchEngine<TestPayload, _, _> = DispatchEngine::new(inv, audit);
    engine
        .register("k", HookType::FilterChain, Registration::new(plugin("a")))
        .unwrap();
    let err = engine
        .register("k", HookType::Broadcast, Registration::new(plugin("b")))
        .unwrap_err();
    assert!(matches!(
        err,
        mote_dispatch::RegisterError::HookTypeMismatch { .. }
    ));
}

#[test]
fn audit_records_performer_for_each_step() {
    let mut inv = MockInvoker::default();
    inv.program(
        "ph",
        "net:intercept_request",
        Behavior::AppendTag("dnt".into()),
    );
    inv.program(
        "adblock",
        "net:intercept_request",
        Behavior::Decide(Decision::Block {
            reason: "easylist".into(),
        }),
    );

    let audit = CapturingAudit::default();
    let mut engine = DispatchEngine::new(inv, audit.clone());
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("ph"), 70),
        )
        .unwrap();
    engine
        .register(
            "net:intercept_request",
            HookType::FilterChain,
            Registration::with_priority(plugin("adblock"), 50),
        )
        .unwrap();

    let _ = engine.dispatch_filter_chain("net:intercept_request", empty());
    // Each step attributes the performer (D4): the plugin whose handler ran.
    assert_eq!(audit.performers(), vec!["ph", "adblock"]);
}
