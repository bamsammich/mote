//! Keybind input-coalescing (DESIGN §Runtime guarantees; DISCIPLINES §3).
//!
//! Keybind handlers do not use a raw timeout. Their protection against a slow
//! handler under bursty input is **input-coalescing**: if input arrives while a
//! handler is running, the queued inputs are discarded and only the *latest* is
//! handled next. This is what lets vim-mode survive a burst of keypresses
//! without auto-disabling (it is exempt from the counter) and without backing
//! up a queue of stale events.
//!
//! [`KeybindQueue`] models exactly that policy as a tiny state machine over a
//! single "pending latest" slot, independent of any threading model so it can
//! be unit-tested deterministically. The runtime drives it: it `push`es each
//! raw input, and between handler runs it `take`s the latest pending input to
//! dispatch.

/// A coalescing slot for one keybind's pending input.
///
/// Holds at most one input — the most recent. Pushing a newer input while one
/// is already pending **discards** the older one (coalescing). While a handler
/// is marked running, pushes still only retain the latest, so a burst collapses
/// to a single follow-up dispatch.
#[derive(Debug)]
pub struct KeybindQueue<E> {
    /// The most recent input not yet handled, if any.
    pending: Option<E>,
    /// Whether a handler is currently running. Informational for the caller's
    /// loop; the coalescing behavior (keep-latest) holds regardless.
    running: bool,
    /// Count of inputs discarded by coalescing (observability / tests).
    coalesced: u64,
}

impl<E> Default for KeybindQueue<E> {
    fn default() -> Self {
        Self {
            pending: None,
            running: false,
            coalesced: 0,
        }
    }
}

impl<E> KeybindQueue<E> {
    /// A new, empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a raw input. If one was already pending it is discarded in favor
    /// of `input` (coalescing): only the latest is ever kept.
    pub fn push(&mut self, input: E) {
        if self.pending.is_some() {
            self.coalesced += 1;
        }
        self.pending = Some(input);
    }

    /// Marks that a handler has started running. Used by the driving loop to
    /// reflect state; coalescing does not depend on it.
    pub const fn mark_running(&mut self) {
        self.running = true;
    }

    /// Marks that the running handler has finished.
    pub const fn mark_idle(&mut self) {
        self.running = false;
    }

    /// Whether a handler is currently marked running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Takes the latest pending input to dispatch, if any, clearing the slot.
    ///
    /// The caller dispatches the returned input and, when the handler finishes,
    /// calls `take` again: if a burst arrived during the handler, only the
    /// newest survived and is dispatched next.
    pub const fn take_latest(&mut self) -> Option<E> {
        self.pending.take()
    }

    /// Whether there is a pending input awaiting dispatch.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// How many inputs have been discarded by coalescing so far.
    #[must_use]
    pub const fn coalesced_count(&self) -> u64 {
        self.coalesced
    }
}
