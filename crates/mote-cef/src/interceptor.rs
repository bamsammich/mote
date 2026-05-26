//! Resource-request interception — the seam ad-block / privacy plugins ride on.
//!
//! This is the single hardest plugin API in the design (DESIGN.md §Engine — CEF:
//! "Network interception at the right layer via `CefResourceRequestHandler` —
//! this is the single hardest plugin API to implement"). CEF exposes it through
//! `RequestHandler::get_resource_request_handler` →
//! `ResourceRequestHandler::on_before_resource_load`. We surface it as a plain
//! Rust trait so the permission-dispatch layer (not plugins directly) can
//! implement it, with **zero** `cef::` types in the signature.
//!
//! The v0.1 implementation observes and decides per-request synchronously (the
//! "tight, sync filter chain" contract of DISCIPLINES.md §3). Async deferral and
//! response-body filtering map onto `CONTINUE_ASYNC` /
//! `get_resource_response_filter` and are deliberately left as a future
//! extension behind this same trait.

use std::fmt;

/// A resource request observed before it is loaded.
///
/// A read-only, CEF-free snapshot of the fields a filter needs to decide. More
/// fields (headers, POST body, resource type) can be added without breaking the
/// trait, because [`ResourceInterceptor`] takes `&RequestInfo`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RequestInfo {
    /// The fully-qualified request URL.
    pub url: String,
    /// The HTTP method (`GET`, `POST`, …).
    pub method: String,
    /// Whether this request is a top-level navigation (vs. a subresource).
    pub is_navigation: bool,
    /// Whether this request is a download.
    pub is_download: bool,
}

/// The decision a [`ResourceInterceptor`] returns for a request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RequestDecision {
    /// Let the request proceed unchanged.
    #[default]
    Allow,
    /// Block the request outright (maps to CEF `RV_CANCEL`).
    Block,
}

/// Implemented by the permission-dispatch layer to observe and gate network
/// requests. Mote's `net:intercept_request` filter chain fans out from here.
///
/// Implementors MUST be cheap and non-blocking: this runs synchronously on
/// CEF's IO thread for every resource load. Long work belongs in an async
/// deferral path (a future extension), not inline here.
///
/// Implementors must be `Send + Sync` because CEF invokes the handler from its
/// own threads; the wrapper shares a single `Arc<dyn ResourceInterceptor>`
/// across browsers.
pub trait ResourceInterceptor: Send + Sync + fmt::Debug {
    /// Called before each resource load. Return [`RequestDecision::Block`] to
    /// cancel the request, or [`RequestDecision::Allow`] to let it proceed.
    fn on_before_request(&self, request: &RequestInfo) -> RequestDecision;
}

/// A no-op interceptor that allows every request. The default when a browser is
/// created without an explicit interceptor; also the baseline the forwarding
/// implementation degrades to.
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAll;

impl ResourceInterceptor for AllowAll {
    fn on_before_request(&self, _request: &RequestInfo) -> RequestDecision {
        RequestDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct BlockHosts(Vec<&'static str>);

    impl ResourceInterceptor for BlockHosts {
        fn on_before_request(&self, request: &RequestInfo) -> RequestDecision {
            if self.0.iter().any(|h| request.url.contains(h)) {
                RequestDecision::Block
            } else {
                RequestDecision::Allow
            }
        }
    }

    fn req(url: &str) -> RequestInfo {
        RequestInfo {
            url: url.to_string(),
            method: "GET".into(),
            is_navigation: false,
            is_download: false,
        }
    }

    #[test]
    fn allow_all_allows() {
        assert_eq!(
            AllowAll.on_before_request(&req("https://example.com")),
            RequestDecision::Allow
        );
    }

    #[test]
    fn custom_interceptor_blocks_matching_host() {
        let interceptor = BlockHosts(vec!["ads.example.com"]);
        assert_eq!(
            interceptor.on_before_request(&req("https://ads.example.com/x.js")),
            RequestDecision::Block
        );
        assert_eq!(
            interceptor.on_before_request(&req("https://cdn.example.com/x.js")),
            RequestDecision::Allow
        );
    }

    #[test]
    fn decision_default_is_allow() {
        assert_eq!(RequestDecision::default(), RequestDecision::Allow);
    }
}
