//! Hook types, their budgets, and handler registration.
//!
//! Registration **requires** the hook type (DISCIPLINES §3): the engine then
//! enforces the matching dispatch model and budget. There is no way to register
//! a handler without declaring whether it is a filter-chain, broadcast, or
//! keybind handler.

use std::time::Duration;

use mote_types::PluginName;

/// The default dispatch priority when a plugin declares none (DESIGN §Dispatch
/// ordering).
pub const DEFAULT_PRIORITY: u8 = 50;

/// The filter-chain hard timeout (DESIGN §Runtime guarantees).
pub const FILTER_CHAIN_BUDGET: Duration = Duration::from_millis(10);

/// The broadcast budget (DESIGN §Runtime guarantees). "Async-allowed" means
/// off the synchronous critical path with a generous budget — **not** tokio
/// `await` (risks-and-inconsistencies.md D2).
pub const BROADCAST_BUDGET: Duration = Duration::from_millis(100);

/// Which of the three differentiated dispatch models a hook uses.
///
/// The dispatch contract varies by hook type, not by plugin (DESIGN §Runtime
/// guarantees). Registration carries this so the engine enforces the right
/// model and budget — and so keybinds are exempted from the auto-disable
/// counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HookType {
    /// Filter chain: 10ms sync hard timeout, first-block-wins, modify-cascades,
    /// timeout → `defer`. Counts toward auto-disable.
    FilterChain,
    /// Broadcast: 100ms budget, all handlers run, errors isolated, no return
    /// semantics. Counts toward auto-disable.
    Broadcast,
    /// Keybind: input-coalescing, no raw-timeout auto-disable, EXEMPT from the
    /// error/timeout counter.
    Keybind,
}

impl HookType {
    /// The per-call time budget for this hook type.
    ///
    /// Keybinds have no raw timeout (their protection is input-coalescing), so
    /// they report [`None`]; the engine still passes a deadline to the invoker
    /// derived from a generous safety bound, but a keybind overrun never
    /// auto-disables.
    #[must_use]
    pub const fn budget(self) -> Option<Duration> {
        match self {
            Self::FilterChain => Some(FILTER_CHAIN_BUDGET),
            Self::Broadcast => Some(BROADCAST_BUDGET),
            Self::Keybind => None,
        }
    }

    /// Whether timeouts/errors on this hook type count toward the per-plugin
    /// auto-disable counter. Keybinds are exempt (DESIGN §Runtime guarantees).
    #[must_use]
    pub const fn counts_toward_auto_disable(self) -> bool {
        !matches!(self, Self::Keybind)
    }
}

/// One registered handler on a hook: which plugin, at what priority.
///
/// The handler body itself lives behind the [`HookInvoker`](crate::HookInvoker)
/// — this is only the routing/ordering metadata the engine composes.
#[derive(Debug, Clone)]
pub struct Registration {
    /// The plugin that owns this handler. Used for ordering tiebreak, error
    /// counting, and audit attribution.
    pub plugin: PluginName,
    /// Flat integer priority; higher runs earlier (DESIGN §Dispatch ordering).
    pub priority: u8,
}

impl Registration {
    /// A registration at the default priority ([`DEFAULT_PRIORITY`]).
    #[must_use]
    pub const fn new(plugin: PluginName) -> Self {
        Self {
            plugin,
            priority: DEFAULT_PRIORITY,
        }
    }

    /// A registration at an explicit priority.
    #[must_use]
    pub const fn with_priority(plugin: PluginName, priority: u8) -> Self {
        Self { plugin, priority }
    }
}
