//! Per-plugin error/timeout counting and auto-disable (DESIGN §Runtime
//! guarantees; risks-and-inconsistencies.md D3).
//!
//! Three timeouts or errors in a rolling 24-hour window auto-disables the
//! plugin. The count is **per-plugin** (D3) — across all of its non-keybind
//! hooks — not per-hook-registration. Keybind handlers never reach here
//! (they are exempt at the call site).
//!
//! The shell surfaces the system notification; this module only produces the
//! [`AutoDisable`] signal when the threshold is crossed.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use mote_types::PluginName;

/// Number of timeouts/errors within [`WINDOW`] that trips auto-disable.
pub const AUTO_DISABLE_THRESHOLD: usize = 3;

/// The rolling window over which failures accumulate (24 hours).
pub const WINDOW: Duration = Duration::from_hours(24);

/// Emitted when a plugin crosses the auto-disable threshold.
///
/// The runtime/shell consumes this to disable the plugin and raise the system
/// notification (DISCIPLINES §3: "treat 'your plugin stopped working and you
/// don't know why' as a P0 UX failure"). This crate only signals; it does not
/// own the notification surface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AutoDisable {
    /// The plugin that crossed the threshold.
    pub plugin: PluginName,
    /// How many failures were counted in the window at the trip point.
    pub failures_in_window: usize,
}

/// Tracks per-plugin failure timestamps and decides auto-disable.
///
/// A clock is injected so tests can advance time deterministically without
/// sleeping; production uses [`Instant::now`].
#[derive(Debug, Default)]
pub struct FailureCounter {
    /// Failure timestamps per plugin, pruned to the window on each record.
    failures: HashMap<PluginName, Vec<Instant>>,
    /// Plugins already auto-disabled, so the signal fires exactly once.
    disabled: HashSet<PluginName>,
}

impl FailureCounter {
    /// A fresh counter with no recorded failures.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one failure (timeout or error) for `plugin` at time `now`,
    /// returning [`Some`] exactly once — when this failure crosses the
    /// threshold within the window.
    ///
    /// Subsequent failures after a plugin is already disabled return [`None`]
    /// (the signal is idempotent; re-enabling is the runtime's concern via
    /// [`Self::reset`]).
    pub fn record_failure(&mut self, plugin: &PluginName, now: Instant) -> Option<AutoDisable> {
        if self.disabled.contains(plugin) {
            return None;
        }

        let window_start = now.checked_sub(WINDOW);
        let entry = self.failures.entry(plugin.clone()).or_default();
        // Prune anything older than the window.
        if let Some(start) = window_start {
            entry.retain(|&t| t >= start);
        }
        entry.push(now);

        if entry.len() >= AUTO_DISABLE_THRESHOLD {
            let failures_in_window = entry.len();
            self.disabled.insert(plugin.clone());
            Some(AutoDisable {
                plugin: plugin.clone(),
                failures_in_window,
            })
        } else {
            None
        }
    }

    /// The number of failures currently within the window for `plugin` as of
    /// `now` (prunes stale entries as a side effect).
    pub fn failures_in_window(&mut self, plugin: &PluginName, now: Instant) -> usize {
        let window_start = now.checked_sub(WINDOW);
        self.failures.get_mut(plugin).map_or(0, |entry| {
            if let Some(start) = window_start {
                entry.retain(|&t| t >= start);
            }
            entry.len()
        })
    }

    /// Whether `plugin` has been auto-disabled.
    #[must_use]
    pub fn is_disabled(&self, plugin: &PluginName) -> bool {
        self.disabled.contains(plugin)
    }

    /// Clears all failure history and the disabled flag for `plugin` (used when
    /// the runtime re-enables a plugin after the user's intervention or a
    /// reload).
    pub fn reset(&mut self, plugin: &PluginName) {
        self.failures.remove(plugin);
        self.disabled.remove(plugin);
    }
}
