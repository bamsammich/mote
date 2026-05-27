//! Permission model and enforcement primitives for Mote.
//!
//! This crate owns everything between a raw permission string and a runtime
//! allow/deny decision:
//!
//! - **Grammar** — the `domain:action[:resource]` syntax, parsed into
//!   [`Permission`] with a thorough [`PermissionParseError`].
//! - **Effective grants** — a [`GrantSet`] holds one plugin's effective
//!   permissions (per `(domain, action)` pair) as a [`GlobSet`] over resource
//!   patterns, supporting **deny-precedence** matching and narrowing.
//! - **Narrowing** — [`GrantSet::narrow`] turns a requested `*`-scope into the
//!   user-approved union of narrower patterns (DESIGN §User narrowing at install
//!   time).
//! - **Resource normalization** — [`normalize_resource`] converts a raw
//!   operation resource (typically a full URL) into the **canonical form** the
//!   gatekeeper matches against, per domain.  The runtime MUST call this before
//!   every [`Gatekeeper::check`] to prevent substring-evasion attacks (e.g.
//!   `https://attacker.com/x.bank.com/y` must NOT satisfy `!*.bank.com`).
//! - **Gatekeeper** — the [`Gatekeeper`] trait is the enforcement seam the
//!   runtime queries: "does this plugin's grant set permit
//!   `domain:action:resource`?"
//!
//! ## Grammar recap (DESIGN §Permission Primitives)
//!
//! ```text
//! domain:action                        # resource defaults to "*" (everything)
//! domain:action:resource-glob          # explicit resource
//! domain:action:!resource-glob         # deny pattern (deny beats allow)
//! ```
//!
//! `domain` and `action` are ASCII identifier segments (`[a-z][a-z0-9_]*`).
//! `resource` is a [`mote_types::Glob`]; absent means `*`.
//!
//! Dynamic-resource forms like `mcp:client:<server-name>` and
//! `secret:read:<name>` are valid — the resource segment is just a glob and
//! carries no special syntactic meaning here; validation that `domain:action` is
//! a *known* registry term is `mote-registry`'s responsibility.
//!
//! ## Enforcement contract
//!
//! [`Gatekeeper::check`] returns a [`Decision`]:
//!
//! - [`Decision::Allow`] — at least one allow pattern matched and no deny
//!   pattern matched.
//! - [`Decision::Deny`] — a deny pattern matched (deny beats allow).
//! - [`Decision::Unmatched`] — no pattern matched; treated as denied by the
//!   runtime.

mod error;
mod gatekeeper;
mod grant;
mod normalize;
mod permission;

pub use error::PermissionParseError;
pub use gatekeeper::{Decision, Gatekeeper, GrantSetGatekeeper};
pub use grant::{EffectiveGrants, GrantSet, GrantSetBuilder};
pub use normalize::{NormalizeError, normalize_resource};
pub use permission::Permission;
