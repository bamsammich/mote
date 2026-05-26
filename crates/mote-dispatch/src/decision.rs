//! The filter-chain decision vocabulary (DESIGN §Hook dispatch patterns).

/// A single handler's decision in a **filter chain** (`net:intercept_request`,
/// response interception).
///
/// The four values and their composition rules are DESIGN's middleware
/// semantics, recognizable from Express / Koa / Tower:
///
/// - [`Decision::Block`] — short-circuits the chain. First block wins; later
///   handlers are still notified for observability but cannot override the
///   block.
/// - [`Decision::Modify`] — returns a transformed payload that **cascades** to
///   the next handler, which sees the modified version.
/// - [`Decision::Allow`] — an explicit positive vote. Does not short-circuit;
///   later handlers can still block or modify.
/// - [`Decision::Defer`] — no opinion. The default when a handler returns
///   nothing, errors, or times out.
///
/// `P` is the payload type carried through the chain (e.g. an intercepted
/// request). It is generic so the policy engine is testable without Lua.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<P> {
    /// Short-circuit: the operation is denied. First block wins.
    Block {
        /// Human-readable reason, surfaced in the audit log / integrity panel.
        reason: String,
    },
    /// Replace the payload; the next handler sees this version.
    Modify {
        /// The transformed payload.
        payload: P,
    },
    /// Explicit positive vote; the chain continues.
    Allow,
    /// No opinion; the chain continues. The default for nothing / error /
    /// timeout.
    Defer,
}

/// The resolved outcome of running a whole filter chain.
///
/// This is what the runtime acts on after composing every handler's
/// [`Decision`] per the resolution rule (first-block-wins, modify-cascades,
/// allow/defer continue, empty → defer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainResolution<P> {
    /// The chain blocked the operation. Carries the blocking reason and the
    /// payload as it stood when the block occurred (already-applied modifies
    /// upstream of the block are reflected).
    Blocked {
        /// The reason from the first blocking handler.
        reason: String,
        /// The payload at the point of the block.
        payload: P,
    },
    /// The chain allowed the operation; carries the final (possibly modified)
    /// payload. This is the result for `allow`, `defer`, and `modify`-only
    /// chains, and for empty chains (treated as `defer`).
    Allowed {
        /// The payload after all upstream modifications.
        payload: P,
    },
}
