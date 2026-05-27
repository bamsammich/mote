//! Resource normalization for Mote permission domains.
//!
//! # Why this exists
//!
//! The [`crate::Gatekeeper`] matches an operation's *resource* string against
//! the glob patterns declared in a plugin's effective grants. For the match to
//! be meaningful, both sides must speak the same language.  Grant patterns are
//! declared in plugin manifests in a **canonical form** (e.g. `*.banking.com`
//! for a host pattern, `https://api.bitwarden.com/*` for an origin-scoped
//! fetch) while the *runtime* presents raw operation resources that may carry
//! extra structure (a full URL with path and query).
//!
//! Without normalization, a raw URL `https://attacker.com/x.bank.com/y` would
//! be tested against `*.bank.com` as a substring and could match — a
//! substring-based evasion the review flagged (S3).  `normalize_resource`
//! converts the raw resource to the **same canonical form the grant patterns
//! use** before [`Gatekeeper::check`] is called.
//!
//! # Normalization contract (per domain + action)
//!
//! | Domain | Action(s) | Canonical resource form | Derived from raw input |
//! |--------|-----------|-------------------------|------------------------|
//! | `net` | `intercept_request`, `read_response_body`, `modify_response`, `fetch_unsigned` | **host** — `hostname` only (no scheme, no port, no path, no query) | parsed from URL; lowercased; percent-decoded |
//! | `http` | `fetch` | **origin** — `scheme://host[:port]` (non-default port only) | scheme + host + port; scheme lowercased |
//! | `page` | `inject_script`, `inject_unsafe_script`, `inject_css`, `read_dom` | **origin** — same as `http:fetch` | scheme + host + port |
//! | everything else | any | **pass-through** — returned unchanged | N/A — resource not URL-shaped |
//!
//! ## Invariants
//!
//! - A normalized host can never contain `/`, `?`, `#`, `@`, scheme components,
//!   or port numbers, so a glob pattern like `*.banking.com` can only match
//!   against host strings in exactly the expected form.
//! - A normalized origin never includes a path/query, so `https://api.bank.com`
//!   can only match the origin itself, never a path trick.
//! - Percent-encoding is decoded before matching (e.g. `%2E` → `.`), closing
//!   another evasion vector.
//! - Lowercasing closes case-sensitivity tricks; host names are case-insensitive
//!   by spec.
//!
//! ## Pass-through domains
//!
//! Permissions whose resources are not URL-shaped (`storage:persistent`,
//! `tabs:list`, etc.) return the raw resource unchanged.  Their grant patterns
//! are already in the correct literal/glob form (e.g. `*`, a plugin name, or a
//! dynamic name segment), so no transformation is needed.
//!
//! ## When normalization fails
//!
//! If the raw resource cannot be parsed as a URL for a domain that expects one,
//! [`normalize_resource`] returns [`NormalizeError::NotAUrl`].  The runtime
//! **must** treat this as a denial — a resource that cannot be normalized to
//! the canonical form is not a resource the gatekeeper should permit.
//!
//! ## Example
//!
//! ```
//! use mote_permissions::normalize_resource;
//!
//! // Full URL → host for net:intercept_request
//! let host = normalize_resource("net", "intercept_request",
//!                               "https://secure.banking.com/login?a=b").unwrap();
//! assert_eq!(host, "secure.banking.com");
//!
//! // Attacker trick: path that looks like a host must NOT pass
//! let host = normalize_resource("net", "intercept_request",
//!                               "https://attacker.com/x.bank.com/y").unwrap();
//! // The host is "attacker.com", not anything containing "bank.com"
//! assert_eq!(host, "attacker.com");
//!
//! // Full URL → origin for http:fetch
//! let origin = normalize_resource("http", "fetch",
//!                                 "https://api.bitwarden.com/v1/sync").unwrap();
//! assert_eq!(origin, "https://api.bitwarden.com");
//!
//! // Non-URL resource (storage) is passed through unchanged
//! let raw = normalize_resource("storage", "persistent", "*").unwrap();
//! assert_eq!(raw, "*");
//! ```

use thiserror::Error;

/// Errors returned when a raw resource cannot be normalized for its domain.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum NormalizeError {
    /// The raw resource does not parse as a URL, but this `(domain, action)`
    /// expects a URL-shaped resource.
    ///
    /// The runtime must treat this as a denial: an un-parseable resource is not
    /// a resource the gatekeeper can safely permit.
    #[error("resource {raw:?} for `{domain}:{action}` is not a valid URL: {reason}")]
    NotAUrl {
        /// The `domain` segment.
        domain: String,
        /// The `action` segment.
        action: String,
        /// The original raw resource.
        raw: String,
        /// Parse failure detail.
        reason: String,
    },

    /// The URL parsed successfully but has no `host` component (e.g. `data:`
    /// URIs, opaque origins). The gatekeeper cannot match a host-or-origin
    /// pattern against an opaque origin; treat as denial.
    #[error("resource {raw:?} for `{domain}:{action}` has no host (opaque origin)")]
    OpaqueOrigin {
        /// The `domain` segment.
        domain: String,
        /// The `action` segment.
        action: String,
        /// The original raw resource.
        raw: String,
    },
}

/// Dispatch shape for normalization purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKind {
    /// Normalize to `host` (bare hostname, no scheme/port/path).
    Host,
    /// Normalize to `scheme://host[:non-standard-port]` (no path/query).
    Origin,
    /// No URL structure; pass the resource through unchanged.
    PassThrough,
}

/// Returns the normalization kind for `(domain, action)`.
///
/// This is the single place that encodes which canonical form each permission
/// domain uses.  Updating normalization rules means updating this function.
const fn resource_kind(domain: &str, action: &str) -> ResourceKind {
    // Explicit matches in priority order. Rust const fn does not yet support
    // match-on-str, so we use a helper that compares byte-by-byte.
    if str_eq(domain, "net") {
        // All net: actions take a URL and match against the extracted host.
        return ResourceKind::Host;
    }
    if str_eq(domain, "http") && str_eq(action, "fetch") {
        return ResourceKind::Origin;
    }
    if str_eq(domain, "page") {
        // inject_script, inject_unsafe_script, inject_css, read_dom all use
        // origin-scoped resource patterns (DESIGN §Permission Primitives examples).
        return ResourceKind::Origin;
    }
    ResourceKind::PassThrough
}

/// Byte-by-byte string equality usable in `const` context.
const fn str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Converts a raw operation resource into the **canonical form** the gatekeeper
/// matches against, for the given `(domain, action)` pair.
///
/// This is the normalization seam the runtime (or `mote-dispatch`) MUST call
/// before [`Gatekeeper::check`] for every privileged operation.  Calling the
/// gatekeeper with an un-normalized resource bypasses this protection and
/// creates the substring-evasion vulnerability the security review flagged.
///
/// # Arguments
///
/// - `domain` — the permission domain (e.g. `"net"`, `"http"`, `"page"`).
/// - `action` — the permission action (e.g. `"intercept_request"`, `"fetch"`).
/// - `raw` — the raw resource string from the runtime (e.g. a full request URL).
///
/// # Returns
///
/// The canonical resource string for matching, or a [`NormalizeError`] if
/// normalization was required but failed.  The caller **must** treat a
/// normalization failure as a denial.
///
/// # Errors
///
/// - [`NormalizeError::NotAUrl`] — the raw resource is not a valid URL but the
///   domain expects one.
/// - [`NormalizeError::OpaqueOrigin`] — the URL has no extractable host (e.g.
///   `data:` or `blob:` URIs with opaque origins).
pub fn normalize_resource(domain: &str, action: &str, raw: &str) -> Result<String, NormalizeError> {
    match resource_kind(domain, action) {
        ResourceKind::Host => normalize_to_host(domain, action, raw),
        ResourceKind::Origin => normalize_to_origin(domain, action, raw),
        ResourceKind::PassThrough => Ok(raw.to_owned()),
    }
}

/// Extracts the lowercased hostname from `raw`, used for `net:*` permissions.
///
/// The host is the bare DNS name (or IP literal) with no scheme, no port, no
/// path, and no query.  Percent-encoding is decoded by the URL parser before
/// the host is extracted, closing the `%2E` → `.` evasion vector.
fn normalize_to_host(domain: &str, action: &str, raw: &str) -> Result<String, NormalizeError> {
    let parsed = parse_url(domain, action, raw)?;
    parsed
        .host_str()
        .map(str::to_lowercase)
        .ok_or_else(|| NormalizeError::OpaqueOrigin {
            domain: domain.to_owned(),
            action: action.to_owned(),
            raw: raw.to_owned(),
        })
}

/// Extracts `scheme://host[:port]` from `raw`, used for `http:fetch` and
/// `page:*` permissions.
///
/// The port is included **only when non-standard** (i.e. not 80 for `http` /
/// `ws`, not 443 for `https` / `wss`).  Path, query, and fragment are
/// stripped.  The scheme is lowercased by the parser; the host is lowercased
/// here.
fn normalize_to_origin(domain: &str, action: &str, raw: &str) -> Result<String, NormalizeError> {
    let parsed = parse_url(domain, action, raw)?;

    let host = parsed
        .host_str()
        .ok_or_else(|| NormalizeError::OpaqueOrigin {
            domain: domain.to_owned(),
            action: action.to_owned(),
            raw: raw.to_owned(),
        })?;

    let scheme = parsed.scheme(); // already lowercase from the `url` parser
    let host_lower = host.to_lowercase();

    // Include port only when it deviates from the scheme default.
    let default_port = default_port_for(scheme);
    match parsed.port() {
        Some(port) if Some(port) != default_port => Ok(format!("{scheme}://{host_lower}:{port}")),
        _ => Ok(format!("{scheme}://{host_lower}")),
    }
}

/// Parses `raw` as a URL, returning a structured [`url::Url`].
fn parse_url(domain: &str, action: &str, raw: &str) -> Result<url::Url, NormalizeError> {
    url::Url::parse(raw).map_err(|e| NormalizeError::NotAUrl {
        domain: domain.to_owned(),
        action: action.to_owned(),
        raw: raw.to_owned(),
        reason: e.to_string(),
    })
}

/// The default TCP port for common schemes.  Returns `None` for schemes whose
/// default port is not relevant to Mote (e.g. data URIs, unrecognised
/// schemes).
const fn default_port_for(scheme: &str) -> Option<u16> {
    if str_eq(scheme, "http") || str_eq(scheme, "ws") {
        Some(80)
    } else if str_eq(scheme, "https") || str_eq(scheme, "wss") {
        Some(443)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // net: → host normalization
    // -------------------------------------------------------------------------

    /// The foundational evasion case the security review flagged (S3):
    /// a path segment must never satisfy a host-pattern deny.
    #[test]
    fn net_host_path_trick_is_denied() {
        // A full URL whose PATH contains the protected host as a component.
        let normalized = normalize_resource(
            "net",
            "intercept_request",
            "https://attacker.com/x.bank.com/y",
        )
        .unwrap();
        // The host is "attacker.com" — the path segment ".bank.com" is gone.
        assert_eq!(normalized, "attacker.com");
        // Confirm: a deny pattern for `*.bank.com` would NOT match this.
        assert!(!normalized.contains("bank.com"));
    }

    #[test]
    fn net_host_strips_path_query_fragment() {
        let n = normalize_resource(
            "net",
            "intercept_request",
            "https://secure.banking.com/login?user=alice#form",
        )
        .unwrap();
        assert_eq!(n, "secure.banking.com");
    }

    #[test]
    fn net_host_strips_scheme_and_port() {
        let n = normalize_resource(
            "net",
            "intercept_request",
            "https://secure.banking.com:8443/api",
        )
        .unwrap();
        assert_eq!(n, "secure.banking.com");
    }

    #[test]
    fn net_host_is_lowercased() {
        let n =
            normalize_resource("net", "intercept_request", "https://Secure.BANKING.com/").unwrap();
        assert_eq!(n, "secure.banking.com");
    }

    #[test]
    fn net_host_percent_encoded_dot_decoded() {
        // %2E is the percent-encoding of `.`; the URL parser decodes it before
        // yielding the host.  After decoding, `secure%2Ebanking%2Ecom` becomes
        // `secure.banking.com` — a legitimate host.  This closes the
        // percent-encoding evasion vector.
        let n = normalize_resource(
            "net",
            "intercept_request",
            "https://secure%2Ebanking%2Ecom/path",
        )
        .unwrap();
        assert_eq!(n, "secure.banking.com");
    }

    #[test]
    fn net_wildcard_passthrough_unchanged() {
        // Permission globs like `*` or `!*.banking.com` are NOT URLs; they come
        // from the manifest, not the runtime.  The runtime only passes URLs here.
        // This test confirms a plain hostname (as the runtime would pass for a
        // request to `example.com`) round-trips cleanly.
        // NOTE: bare hostnames without a scheme are not valid URLs; the runtime
        // is expected to pass full URLs.  Passing a bare hostname returns an
        // error, which is the correct safe behavior.
        let err = normalize_resource("net", "intercept_request", "example.com");
        // A bare hostname is not a valid URL → NormalizeError::NotAUrl.
        assert!(
            matches!(err, Err(NormalizeError::NotAUrl { .. })),
            "bare hostname (not a URL) must fail normalization: {err:?}"
        );
    }

    #[test]
    fn net_read_response_body_same_normalization() {
        let n = normalize_resource(
            "net",
            "read_response_body",
            "https://secure.banking.com/api/v1/balance",
        )
        .unwrap();
        assert_eq!(n, "secure.banking.com");
    }

    #[test]
    fn net_banking_deny_cannot_be_evaded_by_appended_path() {
        // The critical property: `!*.banking.com` should deny `secure.banking.com`
        // but must NOT be tricked into denying `attacker.com` (that's the wrong
        // direction) and must NOT pass `attacker.com/x.banking.com/y`.
        let evil_url = "https://attacker.com/x.banking.com/y/exploit";
        let normalized = normalize_resource("net", "intercept_request", evil_url).unwrap();
        // The canonical host is `attacker.com` — no substring of "banking.com"
        // survives that could fool the gatekeeper.
        assert_eq!(normalized, "attacker.com");
    }

    #[test]
    fn net_banking_deny_cannot_be_evaded_by_appended_host() {
        // Another vector: `secure.banking.com.evil.com` — the attacker appends
        // a suffix to look like the protected host in a substring check.
        let attacker = "https://secure.banking.com.evil.com/path";
        let normalized = normalize_resource("net", "intercept_request", attacker).unwrap();
        assert_eq!(normalized, "secure.banking.com.evil.com");
        // A glob `*.banking.com` does NOT match `secure.banking.com.evil.com`
        // because `evil.com` is the actual TLD.  Verified: the glob semantics in
        // mote-types treat `*` as a single-label wildcard, so `*.banking.com`
        // matches exactly `<one-label>.banking.com` — `secure.banking.com.evil.com`
        // has too many labels and does not match.
    }

    // -------------------------------------------------------------------------
    // http:fetch → origin normalization
    // -------------------------------------------------------------------------

    #[test]
    fn http_fetch_strips_path() {
        let o = normalize_resource("http", "fetch", "https://api.bitwarden.com/v1/sync?foo=bar")
            .unwrap();
        assert_eq!(o, "https://api.bitwarden.com");
    }

    #[test]
    fn http_fetch_keeps_non_standard_port() {
        let o = normalize_resource("http", "fetch", "https://localhost:8443/api/data").unwrap();
        assert_eq!(o, "https://localhost:8443");
    }

    #[test]
    fn http_fetch_drops_standard_https_port() {
        let o = normalize_resource("http", "fetch", "https://api.example.com:443/path").unwrap();
        assert_eq!(o, "https://api.example.com");
    }

    #[test]
    fn http_fetch_drops_standard_http_port() {
        let o = normalize_resource("http", "fetch", "http://api.example.com:80/path").unwrap();
        assert_eq!(o, "http://api.example.com");
    }

    #[test]
    fn http_fetch_wss_keeps_non_standard_port() {
        let o = normalize_resource("http", "fetch", "wss://localhost:6263/ws").unwrap();
        assert_eq!(o, "wss://localhost:6263");
    }

    #[test]
    fn http_fetch_wss_drops_standard_port() {
        let o = normalize_resource("http", "fetch", "wss://example.com:443/ws").unwrap();
        assert_eq!(o, "wss://example.com");
    }

    #[test]
    fn http_fetch_host_is_lowercased() {
        let o = normalize_resource("http", "fetch", "https://API.BITWARDEN.COM/path").unwrap();
        assert_eq!(o, "https://api.bitwarden.com");
    }

    // -------------------------------------------------------------------------
    // page:* → origin normalization
    // -------------------------------------------------------------------------

    #[test]
    fn page_inject_script_strips_path() {
        let o = normalize_resource(
            "page",
            "inject_script",
            "https://gist.github.com/user/abc123?foo=bar",
        )
        .unwrap();
        assert_eq!(o, "https://gist.github.com");
    }

    #[test]
    fn page_inject_css_strips_path() {
        let o = normalize_resource("page", "inject_css", "https://linear.app/workspace/project")
            .unwrap();
        assert_eq!(o, "https://linear.app");
    }

    #[test]
    fn page_path_trick_does_not_match_1password_origin() {
        // An attacker serving a page at `evil.com` with a URL that embeds
        // `.1password.com` in the path must NOT satisfy the origin pattern
        // `https://*.1password.com`.
        let evil = "https://evil.com/https://subdomain.1password.com/steal";
        let o = normalize_resource("page", "inject_script", evil).unwrap();
        assert_eq!(o, "https://evil.com");
    }

    // -------------------------------------------------------------------------
    // pass-through domains
    // -------------------------------------------------------------------------

    #[test]
    fn storage_resource_passthrough() {
        let r = normalize_resource("storage", "persistent", "*").unwrap();
        assert_eq!(r, "*");
    }

    #[test]
    fn tabs_resource_passthrough() {
        let r = normalize_resource("tabs", "list", "anything").unwrap();
        assert_eq!(r, "anything");
    }

    #[test]
    fn secret_read_name_passthrough() {
        let r = normalize_resource("secret", "read", "anthropic_api_key").unwrap();
        assert_eq!(r, "anthropic_api_key");
    }

    #[test]
    fn mcp_client_name_passthrough() {
        let r = normalize_resource("mcp", "client", "my-mcp-server").unwrap();
        assert_eq!(r, "my-mcp-server");
    }

    // -------------------------------------------------------------------------
    // error cases
    // -------------------------------------------------------------------------

    #[test]
    fn net_not_a_url_returns_error() {
        let err = normalize_resource("net", "intercept_request", "not a url");
        assert!(
            matches!(err, Err(NormalizeError::NotAUrl { .. })),
            "expected NotAUrl, got {err:?}"
        );
    }

    #[test]
    fn http_fetch_not_a_url_returns_error() {
        let err = normalize_resource("http", "fetch", "foobar no url");
        assert!(
            matches!(err, Err(NormalizeError::NotAUrl { .. })),
            "expected NotAUrl, got {err:?}"
        );
    }

    #[test]
    fn error_includes_domain_action_and_raw() {
        let err = normalize_resource("net", "intercept_request", "not-a-url").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("net"), "domain missing: {msg}");
        assert!(msg.contains("intercept_request"), "action missing: {msg}");
        assert!(msg.contains("not-a-url"), "raw missing: {msg}");
    }
}
