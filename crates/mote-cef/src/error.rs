//! Error vocabulary for `mote-cef`.
//!
//! Every fallible boundary returns [`CefError`] so callers never have to reason
//! about raw CEF return codes (`-1`, `0`, `1` and friends) or `cef::` types.

use thiserror::Error;

/// Errors surfaced by the CEF wrapper.
///
/// Variants are intentionally coarse: the rest of Mote needs to know *what kind*
/// of failure occurred (init vs. browser-create vs. lifecycle misuse), not the
/// CEF-internal detail, which is logged here and contained.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CefError {
    /// `cef::initialize` returned failure. The CEF runtime could not start.
    #[error("CEF failed to initialize (check libcef.so resolution and resources)")]
    Initialize,

    /// Creating an off-screen browser host failed.
    #[error("failed to create off-screen browser for {url}")]
    BrowserCreate {
        /// The URL the browser was being created for.
        url: String,
    },

    /// A lifecycle method was called in the wrong order (e.g. creating a browser
    /// before [`crate::Engine::init`], or after [`crate::Engine::shutdown`]).
    #[error("CEF lifecycle misuse: {0}")]
    Lifecycle(&'static str),

    /// Creating or configuring a per-identity profile (`RequestContext`) failed,
    /// or an [`crate::IdentityId`] was malformed.
    #[error("CEF profile error: {0}")]
    Profile(&'static str),

    /// `execute_process` indicated this invocation is a CEF subprocess. This is
    /// not a true error — it is the signal the caller must exit immediately;
    /// see [`crate::ProcessRole`].
    #[error("process is a CEF subprocess; the caller must exit")]
    IsSubprocess,
}

/// Convenience result alias for the crate's public API.
pub type Result<T> = std::result::Result<T, CefError>;
